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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tar;

pub const PAX_HEADER_SHA256: &str = "freedesktopsdk.checksum.sha256";
pub const PAX_HEADER_XATTR: &str = "SCHILY.xattr.";

pub fn xattr_sha256(path: &Path) -> Option<String> {
    match xattr::get(path, "user.checksum.sha256") {
        Ok(Some(val)) => Some(String::from_utf8_lossy(&val).to_string()),
        _ => None,
    }
}

pub fn get_all_xattr(path: &Path) -> Vec<(String, String)> {
    let mut result = Vec::new();
    if let Ok(attrs) = xattr::list(path) {
        for attr_name in attrs {
            let attr_str = attr_name.to_string_lossy().to_string();
            if let Ok(Some(val)) = xattr::get(path, &attr_name) {
                let val_str = String::from_utf8_lossy(&val).to_string();
                result.push((attr_str, val_str));
            }
        }
    }
    result
}

pub fn file_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn file_sha256_from_reader<R: Read>(reader: &mut R) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Debug, Clone)]
pub struct LowerEntry {
    pub tar_index: usize,
    pub name: String,
    pub entry_type: u8,
    pub uid: u64,
    pub gid: u64,
    pub mode: u32,
    pub mtime: u64,
    pub size: u64,
    pub linkname: String,
    pub pax_headers: HashMap<String, String>,
}

pub struct LowerAnalysis {
    pub files: BTreeMap<String, LowerEntry>,
    pub dir_contents: HashMap<String, Vec<String>>,
}

pub fn analyze_lowers<R: Read>(lowers: &mut [tar::Archive<R>]) -> Result<LowerAnalysis> {
    let mut lower_files: BTreeMap<String, LowerEntry> = BTreeMap::new();

    for (idx, lower) in lowers.iter_mut().enumerate() {
        for entry_result in lower.entries()? {
            let mut entry = entry_result?;

            // Extract header data before any mutable borrows
            let entry_type = entry.header().entry_type().as_byte();
            let uid = entry.header().uid()?;
            let gid = entry.header().gid()?;
            let mode = entry.header().mode()?;
            let mtime = entry.header().mtime()?;
            let size = entry.header().size()?;
            let linkname = entry
                .header()
                .link_name()?
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let path_str = entry.path()?.to_string_lossy().to_string();
            let (dirname, basename) = split_path(&path_str);

            if basename == ".wh..wh..opq" {
                let prefix = format!("{}/", dirname);
                let to_delete: Vec<String> = lower_files
                    .keys()
                    .filter(|k| k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for k in to_delete {
                    lower_files.remove(&k);
                }
            } else if basename.starts_with(".wh.") {
                let real_name = &basename[4..];
                let full_path = if dirname.is_empty() {
                    real_name.to_string()
                } else {
                    format!("{}/{}", dirname, real_name)
                };
                lower_files.remove(&full_path);
            } else {
                let mut pax_headers = HashMap::new();
                if let Some(pax) = entry.pax_extensions()? {
                    for ext in pax {
                        if let Ok(ext) = ext {
                            let key = ext.key().unwrap_or_default().to_string();
                            let val = ext.value().unwrap_or_default().to_string();
                            pax_headers.insert(key, val);
                        }
                    }
                }

                let le = LowerEntry {
                    tar_index: idx,
                    name: path_str.clone(),
                    entry_type,
                    uid,
                    gid,
                    mode,
                    mtime,
                    size,
                    linkname,
                    pax_headers,
                };
                lower_files.insert(path_str, le);
            }
        }
    }

    let mut dir_contents: HashMap<String, Vec<String>> = HashMap::new();
    for file in lower_files.keys() {
        let (dirname, basename) = split_path(file);
        dir_contents
            .entry(dirname)
            .or_default()
            .push(basename);
    }

    Ok(LowerAnalysis {
        files: lower_files,
        dir_contents,
    })
}

fn split_path(path: &str) -> (String, String) {
    let p = Path::new(path);
    let basename = p
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let dirname = p
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    (dirname, basename)
}

fn attr_set(pax: &HashMap<String, String>) -> BTreeSet<(String, String)> {
    pax.iter()
        .filter(|(k, _)| k.starts_with(PAX_HEADER_XATTR))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn create_layer<W: std::io::Write>(
    output: &mut tar::Builder<W>,
    upper: &Path,
    lower_analysis: &LowerAnalysis,
) -> Result<()> {
    let epoch = std::env::var("SOURCE_DATE_EPOCH").ok().map(|e| {
        e.parse::<u64>()
            .expect("SOURCE_DATE_EPOCH must be a valid integer")
    });

    let mut stack: Vec<PathBuf> = vec![upper.to_path_buf()];

    while let Some(root) = stack.pop() {
        let root_rel = pathdiff(&root, upper);

        // Add directory entry
        let dir_meta = fs::symlink_metadata(&root)
            .with_context(|| format!("Failed to stat directory: {:?}", root))?;
        let mut dir_header = tar::Header::new_gnu();
        dir_header.set_entry_type(tar::EntryType::Directory);
        dir_header.set_mode(dir_meta.permissions().mode());
        dir_header.set_uid(dir_meta.uid() as u64);
        dir_header.set_gid(dir_meta.gid() as u64);
        dir_header.set_mtime(if let Some(ep) = epoch {
            ep
        } else {
            dir_meta.mtime() as u64
        });
        dir_header.set_size(0);
        let dir_name = if root_rel == "." {
            "./".to_string()
        } else {
            format!("./{}/", root_rel)
        };
        dir_header.set_cksum();
        output.append_data(&mut dir_header, &dir_name, &[] as &[u8])?;

        let mut files: Vec<String> = Vec::new();
        let mut dirs: Vec<String> = Vec::new();

        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                dirs.push(name);
            } else {
                files.push(name);
            }
        }

        dirs.sort();
        dirs.reverse();
        for d in &dirs {
            stack.push(root.join(d));
        }

        // Handle whiteout for deleted files
        let rel_for_lookup = if root_rel == "." {
            ".".to_string()
        } else {
            format!("./{}", root_rel)
        };

        if let Some(old_files) = lower_analysis.dir_contents.get(&rel_for_lookup) {
            for old_file in old_files {
                if !files.contains(old_file) && !dirs.contains(old_file) {
                    let full_path = if root_rel == "." {
                        format!("./{}", old_file)
                    } else {
                        format!("./{}/{}", root_rel, old_file)
                    };

                    if let Some(old_entry) = lower_analysis.files.get(&full_path) {
                        let wh_name = if root_rel == "." {
                            format!("./.wh.{}", old_file)
                        } else {
                            format!("./{}/{}", root_rel, format!(".wh.{}", old_file))
                        };
                        let mut wh_header = tar::Header::new_gnu();
                        wh_header.set_entry_type(tar::EntryType::Regular);
                        wh_header.set_uid(old_entry.uid);
                        wh_header.set_gid(old_entry.gid);
                        wh_header.set_mode(old_entry.mode);
                        wh_header.set_mtime(old_entry.mtime);
                        wh_header.set_size(0);
                        wh_header.set_cksum();
                        output.append_data(&mut wh_header, &wh_name, &[] as &[u8])?;
                    }
                }
            }
        }

        files.sort();
        for file in &files {
            let path = root.join(file);
            let rel = if root_rel == "." {
                format!("./{}", file)
            } else {
                format!("./{}/{}", root_rel, file)
            };

            let meta = fs::symlink_metadata(&path)?;
            let is_symlink = meta.file_type().is_symlink();
            let is_regular = meta.file_type().is_file();

            let mut header = tar::Header::new_gnu();
            let mode = meta.permissions().mode() & 0o7777;
            header.set_uid(meta.uid() as u64);
            header.set_gid(meta.gid() as u64);
            header.set_mode(mode);
            header.set_mtime(if let Some(ep) = epoch {
                ep
            } else {
                meta.mtime() as u64
            });

            let mut checksum = String::new();
            let mut pax_headers: HashMap<String, String> = HashMap::new();

            if is_regular {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(meta.len());

                // Get checksum
                checksum = xattr_sha256(&path).unwrap_or_else(|| {
                    file_sha256(&path).unwrap_or_default()
                });
                pax_headers.insert(PAX_HEADER_SHA256.to_string(), checksum.clone());

                for (attr, value) in get_all_xattr(&path) {
                    pax_headers.insert(format!("{}{}", PAX_HEADER_XATTR, attr), value);
                }
            } else if is_symlink {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                let target = fs::read_link(&path)?;
                header.set_link_name(&target)?;
            } else {
                // Other types (hardlinks, devices, etc.)
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(meta.len());
            }

            // Check against lower layers for deduplication
            if let Some(lower_entry) = lower_analysis.files.get(&rel) {
                let same_info = lower_entry.entry_type == header.entry_type().as_byte()
                    && lower_entry.uid == header.uid().unwrap_or(0)
                    && lower_entry.gid == header.gid().unwrap_or(0)
                    && lower_entry.mode == header.mode().unwrap_or(0)
                    && lower_entry.mtime == header.mtime().unwrap_or(0)
                    && lower_entry.size == header.size().unwrap_or(0);

                if same_info && attr_set(&pax_headers) == attr_set(&lower_entry.pax_headers) {
                    if is_regular {
                        let other_checksum = lower_entry
                            .pax_headers
                            .get(PAX_HEADER_SHA256)
                            .cloned()
                            .unwrap_or_default();
                        if checksum == other_checksum {
                            continue;
                        }
                    } else if is_symlink {
                        let target = fs::read_link(&path)?;
                        if target.to_string_lossy() == lower_entry.linkname {
                            continue;
                        }
                    }
                }
            }

            // Write PAX headers if any
            if !pax_headers.is_empty() {
                let mut pax_data = Vec::new();
                for (key, value) in &pax_headers {
                    let record = format!("{} {}={}\n", key.len() + value.len() + 3 + count_digits(key.len() + value.len() + 3), key, value);
                    // Use proper PAX format: length key=value\n
                    let entry_str = format!("{}={}\n", key, value);
                    let total_len = entry_str.len() + count_digits(entry_str.len() + 1) + 1;
                    let _ = record; // suppress warning
                    let pax_record = format!("{} {}", total_len, entry_str);
                    // Recalculate if length of length digits changed
                    let actual = pax_record.len();
                    let pax_record = if actual != total_len {
                        let total_len2 = entry_str.len() + count_digits(actual) + 1;
                        format!("{} {}", total_len2, entry_str)
                    } else {
                        pax_record
                    };
                    pax_data.extend_from_slice(pax_record.as_bytes());
                }
                let mut pax_header = tar::Header::new_ustar();
                pax_header.set_entry_type(tar::EntryType::XHeader);
                pax_header.set_size(pax_data.len() as u64);
                pax_header.set_cksum();
                output.append_data(&mut pax_header, &rel, &pax_data[..])?;
            }

            header.set_cksum();
            if is_regular {
                let f = fs::File::open(&path)?;
                output.append_data(&mut header, &rel, f)?;
            } else {
                output.append_data(&mut header, &rel, &[] as &[u8])?;
            }
        }
    }

    Ok(())
}

fn count_digits(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut count = 0;
    let mut val = n;
    while val > 0 {
        count += 1;
        val /= 10;
    }
    count
}

fn pathdiff(path: &Path, base: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) => {
            let s = rel.to_string_lossy().to_string();
            if s.is_empty() {
                ".".to_string()
            } else {
                s
            }
        }
        Err(_) => path.to_string_lossy().to_string(),
    }
}
