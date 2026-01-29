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
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::Result;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::GlobalConfig;

/// Buffer sizes for I/O operations, tuned for modern SSD performance
pub const IO_BUF_SMALL: usize = 64 * 1024;   // 64KB - for metadata/small files
pub const IO_BUF_MEDIUM: usize = 128 * 1024; // 128KB - for streaming compression
pub const IO_BUF_LARGE: usize = 256 * 1024;  // 256KB - for file hashing
pub const IO_BUF_HUGE: usize = 1024 * 1024;  // 1MB - for blob copying

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
        F: FnOnce(&mut NamedTempFile) -> Result<Option<String>>,
    {
        // Create temp file in the target directory directly to allow atomic rename (persist)
        // We can't predict the filename yet, so we trust NamedTempFile to pick a safe one.
        // Note: NamedTempFile::new_in ensures the file is on the same filesystem.
        let blob_dir = self.output_dir.join("blobs").join("sha256");
        fs::create_dir_all(&blob_dir)?;
        
        // We write to a temp file in the FINAL directory.
        // This avoids cross-filesystem link errors and allows simple renaming.
        let mut tmp = NamedTempFile::new_in(&blob_dir)?;
        let tmp_path = tmp.path().to_path_buf();

        let result = (|| -> Result<()> {
            let provided_digest = writer_fn(&mut tmp)?;

            // Get file size
            let size = tmp.as_file().metadata()?.len();

            let hexdigest = if let Some(d) = provided_digest {
                // Trust the provided digest (avoid re-reading)
                d
            } else {
                // Fallback: Hash the file (requires reading it back)
                // Since we are already in the target dir, we just read and hash, no copy needed.
                tmp.seek(SeekFrom::Start(0))?;
                let mut reader = BufReader::with_capacity(IO_BUF_HUGE, tmp.reopen()?);
                let mut hasher = Sha256::new();
                let mut buf = [0u8; IO_BUF_HUGE];
                loop {
                    let n = reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                }
                format!("{:x}", hasher.finalize())
            };

            self.descriptor = Some(BlobDescriptor {
                media_type: self.media_type.clone(),
                size,
                digest: format!("sha256:{}", hexdigest),
                platform: None,
                annotations: None,
            });

            let dest = blob_dir.join(&hexdigest);
            self.filename = Some(dest.clone());
            
            // Atomic rename to final digest name
            tmp.persist(&dest).map_err(|e| anyhow::anyhow!("persist blob: {}", e))?;

            Ok(())
        })();

        if result.is_err() {
            // Attempt cleanup if persist failed (though tempfile usually handles this)
            let _ = fs::remove_file(&tmp_path);
        }

        result
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
