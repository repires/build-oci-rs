// Copyright (c) 2019, 2020 Codethink Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use gzp::deflate::Gzip;
use gzp::par::compress::ParCompress;
use gzp::ZWriter;
use rayon::prelude::*;
use sha2::{Digest, Sha256};

/// A writer wrapper that computes SHA256 hash while writing.
/// This eliminates a separate hashing pass over the data.
struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        HashingWriter {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> (W, String) {
        let digest = format!("{:x}", self.hasher.finalize());
        (self.inner, digest)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

use crate::blob::Blob;
use crate::layer_builder::{analyze_lowers, create_layer};
use crate::{Compression, GlobalConfig};

const IO_BUF_SIZE: usize = 128 * 1024;

fn get_source_date_epoch() -> Option<u64> {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
}

pub fn extract_oci_image_info(
    path: &Path,
    index: usize,
    global_conf: &GlobalConfig,
) -> Result<(Vec<serde_json::Value>, Vec<PathBuf>, Vec<String>, Vec<serde_json::Value>)> {
    let index_path = path.join("index.json");
    let index_data: serde_json::Value =
        serde_json::from_reader(fs::File::open(&index_path).context("Opening index.json")?)?;

    let image_desc = &index_data["manifests"][index];
    let digest_str = image_desc["digest"].as_str().unwrap();
    let (algo, digest) = digest_str.split_once(':').unwrap();

    let manifest_path = path.join("blobs").join(algo).join(digest);
    let image_manifest: serde_json::Value =
        serde_json::from_reader(fs::File::open(&manifest_path)?)?;

    let config_digest_str = image_manifest["config"]["digest"].as_str().unwrap();
    let (algo2, digest2) = config_digest_str.split_once(':').unwrap();
    let config_path = path.join("blobs").join(algo2).join(digest2);
    let image_config: serde_json::Value = serde_json::from_reader(fs::File::open(&config_path)?)?;

    let diff_ids: Vec<String> = image_config["rootfs"]["diff_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    let history: Vec<serde_json::Value> = image_config
        .get("history")
        .and_then(|h| h.as_array())
        .cloned()
        .unwrap_or_default();

    let mut layer_descs = Vec::new();
    let mut layer_files = Vec::new();

    let layers = image_manifest["layers"].as_array().unwrap();
    for (i, layer) in layers.iter().enumerate() {
        let layer_digest_str = layer["digest"].as_str().unwrap();
        let (lalgo, ldigest) = layer_digest_str.split_once(':').unwrap();
        let origfile = path.join("blobs").join(lalgo).join(ldigest);

        let layer_media_type = layer["mediaType"].as_str().unwrap();
        let is_gzipped = layer_media_type.ends_with("+gzip");

        let (_, _diff_id) = diff_ids[i].split_once(':').unwrap();

        let out_media_type = if global_conf.compression == Compression::Gzip {
            "application/vnd.oci.image.layer.v1.tar+gzip"
        } else {
            "application/vnd.oci.image.layer.v1.tar"
        };

        let mut output_blob = Blob::new(global_conf, Some(out_media_type));

        output_blob.create(|tmp_file| {
            let inp = fs::File::open(&origfile)?;
            let mut reader = BufReader::new(inp);

            if is_gzipped {
                if global_conf.compression == Compression::Gzip {
                    // gzip -> gzip: just copy
                    io::copy(&mut reader, tmp_file)?;
                } else {
                    // gzip -> uncompressed: decompress
                    let mut decoder = GzDecoder::new(reader);
                    io::copy(&mut decoder, tmp_file)?;
                }
            } else {
                if global_conf.compression == Compression::Gzip {
                    // uncompressed -> gzip: compress
                    let level = flate2::Compression::new(
                        global_conf.compression_level.unwrap_or(5) as u32,
                    );
                    let mut encoder = GzEncoder::new(std::io::Write::by_ref(tmp_file), level);
                    io::copy(&mut reader, &mut encoder)?;
                    encoder.finish()?;
                } else {
                    // uncompressed -> uncompressed: just copy
                    io::copy(&mut reader, tmp_file)?;
                }
            }
            Ok(())
        })?;

        layer_descs.push(output_blob.descriptor.as_ref().unwrap().to_json());
        layer_files.push(output_blob.filename.unwrap());
    }

    Ok((layer_descs, layer_files, diff_ids, history))
}

pub fn build_layer(
    upper: &Path,
    lowers: &[PathBuf],
    global_conf: &GlobalConfig,
) -> Result<(Vec<serde_json::Value>, Vec<String>)> {
    let tmp_dir = Path::new("/var/tmp");
    fs::create_dir_all(tmp_dir).ok();

    // Open lower tars for deduplication analysis
    let mut lower_archives: Vec<tar::Archive<Box<dyn Read>>> = Vec::new();
    for lower_path in lowers {
        let f = fs::File::open(lower_path)?;
        let reader: Box<dyn Read> = if global_conf.compression == Compression::Gzip {
            Box::new(GzDecoder::new(BufReader::new(f)))
        } else {
            Box::new(BufReader::new(f))
        };
        lower_archives.push(tar::Archive::new(reader));
    }
    let lower_analysis = analyze_lowers(&mut lower_archives)?;

    let mut new_layer_descs = Vec::new();

    if global_conf.compression == Compression::Gzip {
        // STREAMING: tar -> hash -> gzip -> blob (single pass, no temp tar file)
        let compressed_tmp = tempfile::NamedTempFile::new_in(tmp_dir)?;
        let level = global_conf.compression_level.unwrap_or(5) as u32;

        let tar_hexdigest = {
            let parz: ParCompress<Gzip> = ParCompress::<Gzip>::builder()
                .num_threads(global_conf.workers)
                .map_err(|e| anyhow::anyhow!("gzp thread config: {}", e))?
                .compression_level(gzp::Compression::new(level))
                .from_writer(BufWriter::new(compressed_tmp.reopen()?));

            // Stack: tar -> HashingWriter -> gzp -> compressed file
            let hashing_writer = HashingWriter::new(parz);
            let mut tar_builder = tar::Builder::new(hashing_writer);
            tar_builder.follow_symlinks(false);

            create_layer(&mut tar_builder, upper, &lower_analysis)?;

            let (mut parz, digest) = tar_builder.into_inner()?.finish();
            parz.finish().map_err(|e| anyhow::anyhow!("parallel gzip: {}", e))?;
            digest
        };

        let mut blob = Blob::new(
            global_conf,
            Some("application/vnd.oci.image.layer.v1.tar+gzip"),
        );
        blob.create_from_path(compressed_tmp.path())?;
        new_layer_descs.push(blob.descriptor.as_ref().unwrap().to_json());

        let new_diff_ids = vec![format!("sha256:{}", tar_hexdigest)];
        return Ok((new_layer_descs, new_diff_ids));
    }

    // No compression: tar -> hash -> temp file -> blob
    let mut tmp_file = tempfile::tempfile_in(tmp_dir)
        .or_else(|_| tempfile::tempfile())?;

    let tar_hexdigest = {
        let hashing_writer = HashingWriter::new(&mut tmp_file);
        let mut tar_builder = tar::Builder::new(hashing_writer);
        tar_builder.follow_symlinks(false);

        create_layer(&mut tar_builder, upper, &lower_analysis)?;
        tar_builder.into_inner()?.finish().1
    };

    tmp_file.seek(SeekFrom::Start(0))?;

    let mut blob = Blob::new(
        global_conf,
        Some("application/vnd.oci.image.layer.v1.tar"),
    );
    blob.create(|out| {
        io::copy(&mut tmp_file, out)?;
        Ok(())
    })?;
    new_layer_descs.push(blob.descriptor.as_ref().unwrap().to_json());

    let new_diff_ids = vec![format!("sha256:{}", tar_hexdigest)];

    Ok((new_layer_descs, new_diff_ids))
}

pub fn build_image(
    global_conf: &GlobalConfig,
    image: &serde_json::Value,
) -> Result<serde_json::Value> {
    let mut layer_descs: Vec<serde_json::Value> = Vec::new();
    let mut layer_files: Vec<PathBuf> = Vec::new();
    let mut diff_ids: Vec<String> = Vec::new();
    let mut history: Option<Vec<serde_json::Value>> = None;

    // Create config
    let epoch = get_source_date_epoch();
    let created = if let Some(ep) = epoch {
        chrono::DateTime::from_timestamp(ep as i64, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    } else {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    };

    let mut config = serde_json::json!({
        "created": created,
    });

    if let Some(author) = image.get("author") {
        config["author"] = author.clone();
    }
    config["architecture"] = image["architecture"].clone();
    config["os"] = image["os"].clone();
    if let Some(img_config) = image.get("config") {
        config["config"] = img_config.clone();
    }

    // Handle parent image
    if let Some(parent) = image.get("parent") {
        let parent_image = parent["image"].as_str().unwrap();
        let parent_index = parent.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let (pld, plf, pdi, ph) =
            extract_oci_image_info(Path::new(parent_image), parent_index, global_conf)?;
        layer_descs = pld;
        layer_files = plf;
        diff_ids = pdi;
        history = Some(ph);
    }

    // Build layer
    if let Some(layer_path) = image.get("layer").and_then(|v| v.as_str()) {
        let (new_descs, new_diffs) = build_layer(Path::new(layer_path), &layer_files, global_conf)?;
        layer_descs.extend(new_descs);
        diff_ids.extend(new_diffs);
    }

    // History
    let mut hist = history.unwrap_or_default();
    let mut hist_entry = serde_json::Map::new();
    if image.get("layer").is_none() {
        hist_entry.insert("empty_layer".to_string(), serde_json::Value::Bool(true));
    }
    if let Some(author) = image.get("author") {
        hist_entry.insert("author".to_string(), author.clone());
    }
    if let Some(comment) = image.get("comment") {
        hist_entry.insert("comment".to_string(), comment.clone());
    }
    hist.push(serde_json::Value::Object(hist_entry));

    config["rootfs"] = serde_json::json!({
        "type": "layers",
        "diff_ids": diff_ids,
    });
    config["history"] = serde_json::Value::Array(hist);

    // Write config blob
    let mut config_blob = Blob::new(
        global_conf,
        Some("application/vnd.oci.image.config.v1+json"),
    );
    config_blob.create(|f| {
        let json_bytes = serde_json::to_vec(&config)?;
        f.write_all(&json_bytes)?;
        Ok(())
    })?;

    // Write manifest blob
    let mut manifest = serde_json::json!({
        "schemaVersion": 2,
        "layers": layer_descs,
        "config": config_blob.descriptor.as_ref().unwrap().to_json(),
    });
    if let Some(annotations) = image.get("annotations") {
        manifest["annotations"] = annotations.clone();
    }

    let mut manifest_blob = Blob::new(
        global_conf,
        Some("application/vnd.oci.image.manifest.v1+json"),
    );
    manifest_blob.create(|f| {
        let json_bytes = serde_json::to_vec(&manifest)?;
        f.write_all(&json_bytes)?;
        Ok(())
    })?;

    let mut desc = manifest_blob.descriptor.as_ref().unwrap().to_json();

    // Platform
    let mut platform = serde_json::json!({
        "os": image["os"],
        "architecture": image["architecture"],
    });
    if let Some(v) = image.get("os.version") {
        platform["os.version"] = v.clone();
    }
    if let Some(v) = image.get("os.features") {
        platform["os.features"] = v.clone();
    }
    if let Some(v) = image.get("variant") {
        platform["variant"] = v.clone();
    }
    desc["platform"] = platform;

    if let Some(idx_ann) = image.get("index-annotations") {
        desc["annotations"] = idx_ann.clone();
    }

    Ok(desc)
}

pub fn build_images(
    global_conf: &GlobalConfig,
    images: &[serde_json::Value],
    annotations: Option<&serde_json::Value>,
) -> Result<()> {
    // Ensure blob output directory exists before parallel work
    let blob_dir = Path::new(&global_conf.output).join("blobs").join("sha256");
    fs::create_dir_all(&blob_dir)?;

    let manifests: Result<Vec<serde_json::Value>> = if images.len() > 1 && global_conf.workers > 1
    {
        // Build images in parallel
        images
            .par_iter()
            .map(|image| build_image(global_conf, image))
            .collect()
    } else {
        // Single image or single worker — sequential
        images
            .iter()
            .map(|image| build_image(global_conf, image))
            .collect()
    };
    let manifests = manifests?;

    let mut index = serde_json::json!({
        "schemaVersion": 2,
        "manifests": manifests,
    });
    if let Some(ann) = annotations {
        index["annotations"] = ann.clone();
    }

    let index_path = Path::new(&global_conf.output).join("index.json");
    let index_file = BufWriter::new(fs::File::create(&index_path)?);
    serde_json::to_writer(index_file, &index)?;

    let layout = serde_json::json!({
        "imageLayoutVersion": "1.0.0",
    });
    let layout_path = Path::new(&global_conf.output).join("oci-layout");
    let layout_file = BufWriter::new(fs::File::create(&layout_path)?);
    serde_json::to_writer(layout_file, &layout)?;

    Ok(())
}
