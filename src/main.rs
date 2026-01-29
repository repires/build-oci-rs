// Copyright (c) 2019 Codethink Ltd.
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

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod blob;
mod image_builder;
mod layer_builder;
pub mod util;

use std::io::Read;

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Compression {
    Gzip,
    Zstd,
    Disabled,
}

#[derive(Debug, Clone)]
pub struct GlobalConfig {
    pub compression: Compression,
    pub compression_level: Option<u32>,
    pub output: String,
    pub workers: usize,
    pub compression_threads: usize,
    pub skip_xattrs: bool,
    pub prefetch_limit_mb: usize,
}

fn parse_workers_arg() -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "-j" || args[i] == "--workers" {
            if i + 1 < args.len() {
                return args[i + 1].parse::<usize>().ok();
            }
        } else if args[i].starts_with("-j") {
            // Handle -j4 (no space)
            return args[i][2..].parse::<usize>().ok();
        }
        i += 1;
    }
    None
}

fn main() -> Result<()> {
    let workers = parse_workers_arg().unwrap_or_else(num_cpus);

    // Configure rayon thread pool
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build_global()
        .ok(); // Ignore error if already initialized

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let data: serde_json::Value = serde_yaml::from_str(&input)?;

    let compression_str = data
        .get("compression")
        .and_then(|v| v.as_str())
        .unwrap_or("zstd");

    let compression = match compression_str {
        "gzip" => Compression::Gzip,
        "zstd" => Compression::Zstd,
        "disabled" => Compression::Disabled,
        other => bail!("Compression must be gzip, zstd, or disabled, got: {}", other),
    };

    let compression_level = data
        .get("compression-level")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .or(match compression {
            Compression::Gzip => Some(5),
            Compression::Zstd => Some(1), // zstd level 1 for max speed
            Compression::Disabled => None,
        });

    let output = std::env::current_dir()?
        .to_string_lossy()
        .to_string();

    let skip_xattrs = data
        .get("skip-xattrs")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let prefetch_limit_mb = data
        .get("prefetch-limit-mb")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(512); // Default 512MB limit for prefetch cache

    let images = data
        .get("images")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let num_images = if !images.is_empty() { images.len() } else { 1 };
    
    // Avoid thread oversubscription:
    // If we build M images in parallel, and each uses N compression threads, we have M*N threads.
    // We want M*N <= workers approximately.
    let compression_threads = if num_images > 1 {
        std::cmp::max(1, workers / num_images)
    } else {
        workers
    };

    let global_conf = GlobalConfig {
        compression,
        compression_level,
        output,
        workers,
        compression_threads,
        skip_xattrs,
        prefetch_limit_mb,
    };

    let annotations = data.get("annotations");

    image_builder::build_images(&global_conf, &images, annotations)?;

    Ok(())
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}
