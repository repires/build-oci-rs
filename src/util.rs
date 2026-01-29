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

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use sha2::{Digest, Sha256};

/// A writer wrapper that computes SHA256 hash while writing.
/// This eliminates a separate hashing pass over the data.
pub struct HashingWriter<W: Write> {
    inner: W,
    hasher: Arc<Mutex<Sha256>>,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> (Self, Arc<Mutex<Sha256>>) {
        let hasher = Arc::new(Mutex::new(Sha256::new()));
        (HashingWriter {
            inner,
            hasher: hasher.clone(),
        }, hasher)
    }

    /// For compatibility when we still own the writer
    pub fn finish(mut self) -> io::Result<(W, String)> {
        self.inner.flush()?;
        let digest = format!("{:x}", self.hasher.lock().unwrap().clone().finalize());
        Ok((self.inner, digest))
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.lock().unwrap().update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn get_source_date_epoch() -> Option<u64> {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
}
