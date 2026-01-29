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

use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use sha2::{Digest, Sha256};

/// Hint to the kernel for sequential file access (Linux optimization).
/// This tells the kernel to aggressively prefetch file contents.
#[cfg(target_os = "linux")]
pub fn advise_sequential(file: &File) {
    use std::os::unix::io::AsRawFd;
    // POSIX_FADV_SEQUENTIAL = 2 - enables aggressive readahead
    unsafe {
        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_SEQUENTIAL);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn advise_sequential(_file: &File) {
    // No-op on non-Linux platforms
}

/// A writer wrapper that computes SHA256 hash while writing.
/// This eliminates a separate hashing pass over the data.
///
/// Uses an owned Sha256 hasher (no mutex) since each instance is used
/// single-threaded. This avoids lock acquisition overhead on every write.
pub struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        HashingWriter {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// Consume the writer and return the inner writer along with the computed digest.
    /// This consumes the hasher directly without cloning.
    pub fn finish(mut self) -> io::Result<(W, String)> {
        self.inner.flush()?;
        let digest = format!("{:x}", self.hasher.finalize());
        Ok((self.inner, digest))
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

/// A writer that updates a shared SHA256 hasher.
/// Used when the writer ownership is consumed by a third-party library (like gzp)
/// but we still need the hash of the data written to it.
pub struct SharedHashWriter<W: Write> {
    inner: W,
    hasher: Arc<Mutex<Sha256>>,
}

impl<W: Write> SharedHashWriter<W> {
    pub fn new(inner: W, hasher: Arc<Mutex<Sha256>>) -> Self {
        Self { inner, hasher }
    }
}

impl<W: Write> Write for SharedHashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        // Handle poisoned mutex gracefully - in I/O context, convert to io::Error
        self.hasher
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "hasher mutex poisoned"))?
            .update(&buf[..n]);
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
