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
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::GlobalConfig;

const IO_BUF_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct BlobDescriptor {
    pub media_type: Option<String>,
    pub size: u64,
    pub digest: String,
    pub platform: Option<serde_json::Value>,
    pub annotations: Option<serde_json::Value>,
}

impl BlobDescriptor {
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        if let Some(ref mt) = self.media_type {
            map.insert("mediaType".to_string(), serde_json::Value::String(mt.clone()));
        }
        map.insert("size".to_string(), serde_json::Value::Number(self.size.into()));
        map.insert("digest".to_string(), serde_json::Value::String(self.digest.clone()));
        if let Some(ref p) = self.platform {
            map.insert("platform".to_string(), p.clone());
        }
        if let Some(ref a) = self.annotations {
            map.insert("annotations".to_string(), a.clone());
        }
        serde_json::Value::Object(map)
    }
}

pub struct Blob {
    pub descriptor: Option<BlobDescriptor>,
    pub filename: Option<PathBuf>,
    media_type: Option<String>,
    output_dir: PathBuf,
}

impl Blob {
    pub fn new(global_conf: &GlobalConfig, media_type: Option<&str>) -> Self {
        Blob {
            descriptor: None,
            filename: None,
            media_type: media_type.map(|s| s.to_string()),
            output_dir: PathBuf::from(&global_conf.output),
        }
    }

    pub fn create<F>(&mut self, writer_fn: F) -> Result<()>
    where
        F: FnOnce(&mut NamedTempFile) -> Result<()>,
    {
        let mut tmp = NamedTempFile::new_in(&self.output_dir)?;
        let tmp_path = tmp.path().to_path_buf();

        let result = (|| -> Result<()> {
            writer_fn(&mut tmp)?;

            // Get file size
            let size = tmp.seek(SeekFrom::End(0))?;
            tmp.seek(SeekFrom::Start(0))?;

            let blob_dir = self.output_dir.join("blobs").join("sha256");
            fs::create_dir_all(&blob_dir)?;

            // Hash and copy in a single pass (eliminates one full read)
            let dest_tmp = NamedTempFile::new_in(&blob_dir)?;
            let mut hasher = Sha256::new();
            {
                let mut dest_writer = BufWriter::new(dest_tmp.reopen()?);
                let mut buf = [0u8; IO_BUF_SIZE];
                loop {
                    let n = tmp.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    dest_writer.write_all(&buf[..n])?;
                }
                dest_writer.flush()?;
            }
            let hexdigest = format!("{:x}", hasher.finalize());

            self.descriptor = Some(BlobDescriptor {
                media_type: self.media_type.clone(),
                size,
                digest: format!("sha256:{}", hexdigest),
                platform: None,
                annotations: None,
            });

            let dest = blob_dir.join(&hexdigest);
            self.filename = Some(dest.clone());
            dest_tmp.persist(&dest).map_err(|e| anyhow::anyhow!("persist blob: {}", e))?;

            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }

        result
    }

    pub fn create_from_path(&mut self, source_path: &Path) -> Result<()> {
        let blob_dir = self.output_dir.join("blobs").join("sha256");
        fs::create_dir_all(&blob_dir)?;

        let mut file = fs::File::open(source_path)?;
        let size = file.metadata()?.len();

        // Hash and copy in a single pass
        let dest_tmp = NamedTempFile::new_in(&blob_dir)?;
        let mut hasher = Sha256::new();
        {
            let mut dest_writer = BufWriter::new(dest_tmp.reopen()?);
            let mut buf = [0u8; IO_BUF_SIZE];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                dest_writer.write_all(&buf[..n])?;
            }
            dest_writer.flush()?;
        }
        let hexdigest = format!("{:x}", hasher.finalize());

        self.descriptor = Some(BlobDescriptor {
            media_type: self.media_type.clone(),
            size,
            digest: format!("sha256:{}", hexdigest),
            platform: None,
            annotations: None,
        });

        let dest = blob_dir.join(&hexdigest);
        self.filename = Some(dest.clone());
        dest_tmp.persist(&dest).map_err(|e| anyhow::anyhow!("persist blob: {}", e))?;

        Ok(())
    }

    /// Create blob from a temp file with a pre-computed digest.
    /// This avoids re-reading the file to compute the hash (zero-copy move).
    pub fn create_from_temp_with_digest(
        &mut self,
        temp_file: NamedTempFile,
        size: u64,
        hexdigest: &str,
    ) -> Result<()> {
        let blob_dir = self.output_dir.join("blobs").join("sha256");
        fs::create_dir_all(&blob_dir)?;

        self.descriptor = Some(BlobDescriptor {
            media_type: self.media_type.clone(),
            size,
            digest: format!("sha256:{}", hexdigest),
            platform: None,
            annotations: None,
        });

        let dest = blob_dir.join(hexdigest);
        self.filename = Some(dest.clone());
        temp_file.persist(&dest).map_err(|e| anyhow::anyhow!("persist blob: {}", e))?;

        Ok(())
    }
}
