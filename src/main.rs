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

mod blob;
mod image_builder;
mod layer_builder;

use std::io::Read;

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Compression {
    Gzip,
    Disabled,
}

#[derive(Debug, Clone)]
pub struct GlobalConfig {
    pub compression: Compression,
    pub compression_level: Option<u32>,
    pub output: String,
}

fn main() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let data: serde_json::Value = serde_yaml::from_str(&input)?;

    let compression_str = data
        .get("compression")
        .and_then(|v| v.as_str())
        .unwrap_or("gzip");

    let compression = match compression_str {
        "gzip" => Compression::Gzip,
        "disabled" => Compression::Disabled,
        other => bail!("Compression must be gzip or disabled, got: {}", other),
    };

    let compression_level = data
        .get("compression-level")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .or_else(|| {
            if compression == Compression::Gzip {
                Some(5)
            } else {
                None
            }
        });

    let output = std::env::current_dir()?
        .to_string_lossy()
        .to_string();

    let global_conf = GlobalConfig {
        compression,
        compression_level,
        output,
    };

    let images = data
        .get("images")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let annotations = data.get("annotations");

    image_builder::build_images(&global_conf, &images, annotations)?;

    Ok(())
}
