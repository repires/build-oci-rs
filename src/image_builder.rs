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
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, LazyLock};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use gzp::deflate::Gzip;
use gzp::par::compress::ParCompress;
use gzp::ZWriter;
use rayon::prelude::*;
use zstd::stream::read::Decoder as ZstdDecoder;
use zstd::stream::write::Encoder as ZstdEncoder;

use crate::util::{advise_sequential, get_source_date_epoch, HashingWriter, SharedHashWriter};

use crate::blob::{Blob, IO_BUF_SMALL, IO_BUF_MEDIUM};
use crate::layer_builder::{analyze_lowers, create_layer};
use crate::{Compression, GlobalConfig};

/// Result type for extract_oci_image_info to reduce type complexity
type OciImageInfo = (Vec<serde_json::Value>, Vec<PathBuf>, Vec<String>, Vec<serde_json::Value>);

/// Cache key for extracted OCI images
type ExtractCacheKey = (PathBuf, usize, Compression);

/// Type alias to reduce clippy::type_complexity warning
/// Uses Arc<OciImageInfo> to share cached data without full clones
type ExtractCache = LazyLock<Mutex<FxHashMap<ExtractCacheKey, Arc<OciImageInfo>>>>;
type AnalysisCache = LazyLock<Mutex<FxHashMap<Vec<PathBuf>, Arc<crate::layer_builder::LowerAnalysis>>>>;

static EXTRACT_CACHE: ExtractCache = LazyLock::new(|| Mutex::new(FxHashMap::default()));

static ANALYSIS_CACHE: AnalysisCache = LazyLock::new(|| Mutex::new(FxHashMap::default()));

pub fn extract_oci_image_info(
    path: &Path,
    index: usize,
    global_conf: &GlobalConfig,
) -> Result<Arc<OciImageInfo>> {
    let cache_key = (path.to_path_buf(), index, global_conf.compression);
    {
        let cache = EXTRACT_CACHE
            .lock()
            .map_err(|e| anyhow::anyhow!("Extract cache lock poisoned: {}", e))?;
        if let Some(cached) = cache.get(&cache_key) {
            return Ok(Arc::clone(cached)); // Cheap Arc clone instead of full data clone
        }
    }

    let index_path = path.join("index.json");
    let index_data: serde_json::Value =
        serde_json::from_reader(fs::File::open(&index_path).context("Opening index.json")?)?;

    let image_desc = &index_data["manifests"][index];
    let digest_str = image_desc["digest"]
        .as_str()
        .context("Missing 'digest' in manifest descriptor")?;
    let (algo, digest) = digest_str
        .split_once(':')
        .context("Invalid digest format: expected 'algorithm:hash'")?;

    let manifest_path = path.join("blobs").join(algo).join(digest);
    let image_manifest: serde_json::Value =
        serde_json::from_reader(fs::File::open(&manifest_path)?)?;

    let config_digest_str = image_manifest["config"]["digest"]
        .as_str()
        .context("Missing 'config.digest' in image manifest")?;
    let (algo2, digest2) = config_digest_str
        .split_once(':')
        .context("Invalid config digest format: expected 'algorithm:hash'")?;
    let config_path = path.join("blobs").join(algo2).join(digest2);
    let image_config: serde_json::Value = serde_json::from_reader(fs::File::open(&config_path)?)?;

    let diff_ids_array = image_config["rootfs"]["diff_ids"]
        .as_array()
        .context("Missing 'rootfs.diff_ids' array in image config")?;
    let diff_ids: Vec<String> = diff_ids_array
        .iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow::anyhow!("Invalid diff_id at index {}: expected string", i))
        })
        .collect::<Result<Vec<_>>>()?;

    let history: Vec<serde_json::Value> = image_config
        .get("history")
        .and_then(|h| h.as_array())
        .cloned()
        .unwrap_or_default();

    let mut layer_descs = Vec::new();
    let mut layer_files = Vec::new();

    let layers = image_manifest["layers"]
        .as_array()
        .context("Missing 'layers' array in image manifest")?;

    // Validate that diff_ids and layers arrays have matching lengths
    if diff_ids.len() != layers.len() {
        anyhow::bail!(
            "Malformed OCI image: diff_ids count ({}) does not match layers count ({})",
            diff_ids.len(),
            layers.len()
        );
    }

    let results: Result<Vec<_>> = layers
        .par_iter()
        .enumerate()
        .map(|(i, layer)| {
            let layer_digest_str = layer["digest"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing 'digest' in layer {}", i))?;
            let (lalgo, ldigest) = layer_digest_str
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("Invalid layer digest format at index {}", i))?;
            let origfile = path.join("blobs").join(lalgo).join(ldigest);

            let layer_media_type = layer["mediaType"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing 'mediaType' in layer {}", i))?;
            let is_gzipped = layer_media_type.ends_with("+gzip");
            let is_zstd = layer_media_type.ends_with("+zstd");

            // diff_ids are read-only, safe to access (already bounds-checked above)
            let (_, _diff_id) = diff_ids[i]
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("Invalid diff_id format at index {}", i))?;

            let out_media_type = match global_conf.compression {
                Compression::Gzip => "application/vnd.oci.image.layer.v1.tar+gzip",
                Compression::Zstd => "application/vnd.oci.image.layer.v1.tar+zstd",
                Compression::Disabled => "application/vnd.oci.image.layer.v1.tar",
            };

            let mut output_blob = Blob::new(global_conf, Some(out_media_type));

            output_blob.create(|tmp_file| {
                let inp = fs::File::open(&origfile)?;
                advise_sequential(&inp); // Hint kernel for sequential layer reading
                // Increase buffer size for I/O performance
                let reader = BufReader::with_capacity(IO_BUF_SMALL, inp);

                // First, get an uncompressed reader if needed
                let mut decompressed: Box<dyn Read> = if is_gzipped {
                    Box::new(GzDecoder::new(reader))
                } else if is_zstd {
                    Box::new(ZstdDecoder::new(reader)?)
                } else {
                    Box::new(reader)
                };

                // Now compress to the target format AND compute digest on the fly
                // This avoids reading the file back to hash it.
                //
                // We write the COMPRESSED stream to the temp file, but we need
                // the digest of that compressed stream.
                //
                // Reader -> Decompress -> Compress -> HashingWriter -> TempFile

                let mut hashing_writer = HashingWriter::new(tmp_file);

                match global_conf.compression {
                    Compression::Gzip => {
                        if is_gzipped {
                            // gzip -> gzip: reopen and copy directly (optimized path)
                            let inp = fs::File::open(&origfile)?;
                            advise_sequential(&inp);
                            let mut reader = BufReader::with_capacity(IO_BUF_MEDIUM, inp);
                            io::copy(&mut reader, &mut hashing_writer)?;
                        } else {
                            let level = flate2::Compression::new(
                                global_conf.compression_level.unwrap_or(5),
                            );
                            let mut encoder =
                                GzEncoder::new(&mut hashing_writer, level);
                            io::copy(&mut decompressed, &mut encoder)?;
                            encoder.finish()?;
                        }
                    }
                    Compression::Zstd => {
                        if is_zstd {
                            // zstd -> zstd: reopen and copy directly
                            let inp = fs::File::open(&origfile)?;
                            advise_sequential(&inp);
                            let mut reader = BufReader::with_capacity(IO_BUF_MEDIUM, inp);
                            io::copy(&mut reader, &mut hashing_writer)?;
                        } else {
                            let level = global_conf.compression_level.unwrap_or(3) as i32;
                            let mut encoder = ZstdEncoder::new(&mut hashing_writer, level)?;
                            encoder.multithread(global_conf.compression_threads as u32)?;
                            io::copy(&mut decompressed, &mut encoder)?;
                            encoder.finish()?;
                        }
                    }
                    Compression::Disabled => {
                        if !is_gzipped && !is_zstd {
                            let inp = fs::File::open(&origfile)?;
                            advise_sequential(&inp);
                            let mut reader = BufReader::with_capacity(IO_BUF_MEDIUM, inp);
                            io::copy(&mut reader, &mut hashing_writer)?;
                        } else {
                            io::copy(&mut decompressed, &mut hashing_writer)?;
                        }
                    }
                }

                // Return the computed digest so Blob can use it (avoid re-reading)
                let (_, digest) = hashing_writer.finish()?;
                Ok(Some(digest))
            })?;

            Ok((
                output_blob
                    .descriptor
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Missing descriptor after layer extraction"))?
                    .to_json(),
                output_blob
                    .filename
                    .ok_or_else(|| anyhow::anyhow!("Missing filename after layer extraction"))?,
            ))
        })
        .collect();

    let results = results?;

    for (desc, file) in results {
        layer_descs.push(desc);
        layer_files.push(file);
    }

    let out = Arc::new((layer_descs, layer_files, diff_ids, history));
    EXTRACT_CACHE
        .lock()
        .map_err(|e| anyhow::anyhow!("Extract cache lock poisoned: {}", e))?
        .insert(cache_key, Arc::clone(&out));
    Ok(out)
}

pub fn build_layer(
    upper: &Path,
    lowers: &[PathBuf],
    global_conf: &GlobalConfig,
) -> Result<(Vec<serde_json::Value>, Vec<String>)> {
    // Use a temp dir inside the output dir to ensure same-filesystem moves
    let output_path = Path::new(&global_conf.output);
    let tmp_dir = output_path.join(".tmp");
    fs::create_dir_all(&tmp_dir).ok();

    let lower_cache_key = lowers.to_vec();
    let lower_analysis = {
        let cached = ANALYSIS_CACHE
            .lock()
            .map_err(|e| anyhow::anyhow!("Analysis cache lock poisoned: {}", e))?
            .get(&lower_cache_key)
            .cloned();
        if let Some(cached) = cached {
            cached
        } else {
            // Open lower tars for deduplication analysis
            let mut lower_archives: Vec<tar::Archive<Box<dyn Read + Send>>> = Vec::new();
            for lower_path in lowers {
                let f = fs::File::open(lower_path)?;
                advise_sequential(&f); // Hint kernel for sequential tar reading
                let reader: Box<dyn Read + Send> = match global_conf.compression {
                    Compression::Gzip => Box::new(GzDecoder::new(BufReader::new(f))),
                    Compression::Zstd => Box::new(ZstdDecoder::new(BufReader::new(f))?),
                    Compression::Disabled => Box::new(BufReader::new(f)),
                };
                lower_archives.push(tar::Archive::new(reader));
            }
            let analysis = Arc::new(analyze_lowers(&mut lower_archives)?);
            ANALYSIS_CACHE
                .lock()
                .map_err(|e| anyhow::anyhow!("Analysis cache lock poisoned: {}", e))?
                .insert(lower_cache_key, analysis.clone());
            analysis
        }
    };

    let mut new_layer_descs = Vec::new();

    match global_conf.compression {
        Compression::Gzip => {
            let compressed_tmp = tempfile::NamedTempFile::new_in(&tmp_dir)?;
            let level = global_conf.compression_level.unwrap_or(5);

            // OPTIMIZATION: Use SharedHashWriter to compute blob digest on the fly.
            // ParCompress consumes the writer, so we share the hasher via Arc<Mutex>.
            let blob_hasher = Arc::new(Mutex::new(Sha256::new()));
            let shared_writer = SharedHashWriter::new(BufWriter::new(compressed_tmp.reopen()?), blob_hasher.clone());

            let parz: ParCompress<Gzip> = ParCompress::<Gzip>::builder()
                    .num_threads(global_conf.compression_threads)
                    .map_err(|e| anyhow::anyhow!("gzp thread config: {}", e))?
                    .compression_level(gzp::Compression::new(level))
                    .from_writer(shared_writer);

            // Stack: tar -> BufWriter -> HashingWriter(diff_id) -> gzp -> SharedHashWriter(blob) -> file
            let diff_hasher = HashingWriter::new(parz);
            let mut tar_builder = tar::Builder::new(BufWriter::new(diff_hasher));
            tar_builder.follow_symlinks(false);

            create_layer(&mut tar_builder, upper, &lower_analysis, global_conf)?;

            let buf_writer = tar_builder.into_inner()?;
            let hashing_writer = buf_writer.into_inner().map_err(|e| anyhow::anyhow!("bufwriter: {}", e))?;
            let (mut parz_writer, diff_digest) = hashing_writer.finish()?;
            parz_writer.finish().map_err(|e| anyhow::anyhow!("parallel gzip: {}", e))?;

            // Retrieve blob digest from shared hasher (no re-reading needed)
            let blob_digest = format!(
                "{:x}",
                blob_hasher
                    .lock()
                    .map_err(|e| anyhow::anyhow!("Blob hasher lock poisoned: {}", e))?
                    .clone()
                    .finalize()
            );

            let mut blob = Blob::new(
                global_conf,
                Some("application/vnd.oci.image.layer.v1.tar+gzip"),
            );

            let size = compressed_tmp.as_file().metadata()?.len();
            blob.create_from_temp_with_digest(compressed_tmp, size, &blob_digest)?;

            new_layer_descs.push(
                blob.descriptor
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Missing blob descriptor after gzip layer creation"))?
                    .to_json(),
            );

            let new_diff_ids = vec![format!("sha256:{}", diff_digest)];
            Ok((new_layer_descs, new_diff_ids))
        }
        Compression::Zstd => {
            // STREAMING: tar -> hash(diff_id) -> zstd(multithread) -> hash(blob) -> file
            // Unlike ParCompress, ZstdEncoder's finish() returns the inner writer,
            // so we can properly chain HashingWriters.

            let compressed_tmp = tempfile::NamedTempFile::new_in(&tmp_dir)?;
            let level = global_conf.compression_level.unwrap_or(3) as i32;

            // Outer hasher for BLOB digest (compressed)
            let blob_hasher = HashingWriter::new(BufWriter::new(compressed_tmp.reopen()?));

            let mut zstd_encoder = ZstdEncoder::new(blob_hasher, level)?;
            zstd_encoder.multithread(global_conf.compression_threads as u32)?;

            // Stack: tar -> BufWriter -> HashingWriter(diff_id) -> zstd -> HashingWriter(blob) -> file
            let diff_hasher = HashingWriter::new(zstd_encoder);
            let mut tar_builder = tar::Builder::new(BufWriter::new(diff_hasher));
            tar_builder.follow_symlinks(false);

            create_layer(&mut tar_builder, upper, &lower_analysis, global_conf)?;

            let buf_writer_diff = tar_builder.into_inner()?;
            let hashing_writer = buf_writer_diff.into_inner().map_err(|e| anyhow::anyhow!("bufwriter: {}", e))?;
            let (zstd_writer, diff_digest) = hashing_writer.finish()?;
            let blob_hasher = zstd_writer.finish()?;

            let (mut buf_writer, blob_digest) = blob_hasher.finish()?;
            buf_writer.flush()?;

            let mut blob = Blob::new(
                global_conf,
                Some("application/vnd.oci.image.layer.v1.tar+zstd"),
            );

            let size = compressed_tmp.as_file().metadata()?.len();
            blob.create_from_temp_with_digest(compressed_tmp, size, &blob_digest)?;

            new_layer_descs.push(
                blob.descriptor
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Missing blob descriptor after zstd layer creation"))?
                    .to_json(),
            );

            let new_diff_ids = vec![format!("sha256:{}", diff_digest)];
            Ok((new_layer_descs, new_diff_ids))
        }
        Compression::Disabled => {
            // No compression: tar -> hash -> file
            // Since uncompressed, diff_id == blob_id, compute once

            let tar_tmp = tempfile::NamedTempFile::new_in(&tmp_dir)?;

            let tar_hexdigest = {
                // Hash while writing - this IS the blob digest too (no compression)
                let hashing_writer = HashingWriter::new(BufWriter::new(tar_tmp.reopen()?));
                let mut tar_builder = tar::Builder::new(BufWriter::new(hashing_writer));
                tar_builder.follow_symlinks(false);

                create_layer(&mut tar_builder, upper, &lower_analysis, global_conf)?;
                let buf_writer_tar = tar_builder.into_inner()?;
                let hashing_writer = buf_writer_tar.into_inner().map_err(|e| anyhow::anyhow!("bufwriter: {}", e))?;
                let (mut buf_writer_file, digest) = hashing_writer.finish()?;
                buf_writer_file.flush()?;
                digest
            };

            let size = tar_tmp.as_file().metadata()?.len();

            let mut blob = Blob::new(
                global_conf,
                Some("application/vnd.oci.image.layer.v1.tar"),
            );
            // Use pre-computed digest - avoids re-reading the file
            blob.create_from_temp_with_digest(tar_tmp, size, &tar_hexdigest)?;
            new_layer_descs.push(
                blob.descriptor
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Missing blob descriptor after uncompressed layer creation"))?
                    .to_json(),
            );

            let new_diff_ids = vec![format!("sha256:{}", tar_hexdigest)];
            Ok((new_layer_descs, new_diff_ids))
        }
    }
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
            .ok_or_else(|| anyhow::anyhow!("Invalid SOURCE_DATE_EPOCH timestamp: {}", ep))?
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
        let parent_image = parent["image"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'image' in parent"))?;
        let parent_index = parent.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let parent_info =
            extract_oci_image_info(Path::new(parent_image), parent_index, global_conf)?;
        // Clone out of Arc - necessary since we modify these later
        let (pld, plf, pdi, ph) = parent_info.as_ref();
        layer_descs = pld.clone();
        layer_files = plf.clone();
        diff_ids = pdi.clone();
        history = Some(ph.clone());
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
        
        // Compute digest of small JSON config in-memory
        let mut hasher = Sha256::new();
        hasher.update(&json_bytes);
        Ok(Some(format!("{:x}", hasher.finalize())))
    })?;

    // Write manifest blob
    let mut manifest = serde_json::json!({
        "schemaVersion": 2,
        "layers": layer_descs,
        "config": config_blob
            .descriptor
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing config blob descriptor"))?
            .to_json(),
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

        // Compute digest of manifest in-memory
        let mut hasher = Sha256::new();
        hasher.update(&json_bytes);
        Ok(Some(format!("{:x}", hasher.finalize())))
    })?;

    let mut desc = manifest_blob
        .descriptor
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing manifest blob descriptor"))?
        .to_json();

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
