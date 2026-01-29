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

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufReader, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use jwalk::WalkDir;
use memmap2::Mmap;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use std::io::Write; // Import Write trait
use smallvec::SmallVec;

use crate::blob::IO_BUF_LARGE;
use crate::GlobalConfig;

pub const PAX_HEADER_SHA256: &str = "freedesktopsdk.checksum.sha256";
pub const PAX_HEADER_XATTR: &str = "SCHILY.xattr.";

fn file_sha256(path: &Path) -> Result<String> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::with_capacity(IO_BUF_LARGE, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; IO_BUF_LARGE];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Lower layer entry metadata - field order optimized to reduce struct padding
#[derive(Debug, Clone)]
pub struct LowerEntry {
    // 8-byte aligned fields first (pointer-based types)
    pub pax_headers: HashMap<String, String>,
    pub symlink_target: Option<String>,
    // 8-byte aligned primitives
    pub uid: u64,
    pub gid: u64,
    pub mtime: u64,
    pub size: u64,
    // 4-byte aligned, followed by 1-byte - packs efficiently
    pub mode: u32,
    pub entry_type: u8,
}

pub struct LowerAnalysis {
    pub files: BTreeMap<String, LowerEntry>,
    // Use SmallVec for directory contents as most dirs have few entries
    pub dir_contents: FxHashMap<String, SmallVec<[String; 4]>>,
}

/// Represents parsed entries from a single tar archive before merging
struct ArchiveEntries {
    /// Regular entries (non-whiteout)
    entries: Vec<(String, LowerEntry)>,
    /// Opaque whiteouts - directories whose contents should be deleted
    opaque_whiteouts: Vec<String>,
    /// File whiteouts - specific files to delete
    file_whiteouts: Vec<String>,
}

/// Parse a single tar archive into entries (can run in parallel)
fn parse_archive<R: Read>(archive: &mut tar::Archive<R>) -> Result<ArchiveEntries> {
    let mut entries = Vec::with_capacity(1024);
    let mut opaque_whiteouts = Vec::new();
    let mut file_whiteouts = Vec::new();

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;

        let entry_type = entry.header().entry_type().as_byte();
        let uid = entry.header().uid()?;
        let gid = entry.header().gid()?;
        let mode = entry.header().mode()?;
        let mtime = entry.header().mtime()?;
        let size = entry.header().size()?;
        let path_str = entry.path()?.to_string_lossy().to_string();
        let (dirname, basename) = split_path(&path_str);

        if basename == ".wh..wh..opq" {
            opaque_whiteouts.push(dirname.into_owned());
        } else if let Some(real_name_str) = basename.strip_prefix(".wh.") {
            let real_name = real_name_str;
            let full_path = if dirname.is_empty() {
                real_name.to_string()
            } else {
                format!("{}/{}", dirname, real_name)
            };
            file_whiteouts.push(full_path);
        } else {
            let mut pax_headers = HashMap::with_capacity(8);
            if let Some(pax) = entry.pax_extensions()? {
                for ext in pax.flatten() {
                    let key = ext.key().unwrap_or_default().to_string();
                    let val = ext.value().unwrap_or_default().to_string();
                    pax_headers.insert(key, val);
                }
            }

            // Cache symlink target to avoid re-reading later
            let symlink_target = if entry_type == tar::EntryType::Symlink.as_byte() {
                entry
                    .header()
                    .link_name()?
                    .map(|p| p.to_string_lossy().to_string())
            } else {
                None
            };

            let le = LowerEntry {
                pax_headers,
                symlink_target,
                uid,
                gid,
                mtime,
                size,
                mode,
                entry_type,
            };
            entries.push((path_str, le));
        }
    }

    Ok(ArchiveEntries {
        entries,
        opaque_whiteouts,
        file_whiteouts,
    })
}

pub fn analyze_lowers<R: Read + Send>(lowers: &mut [tar::Archive<R>]) -> Result<LowerAnalysis> {
    // Parse all archives in parallel
    let parsed: Result<Vec<ArchiveEntries>> = lowers
        .par_iter_mut()
        .map(|archive| parse_archive(archive))
        .collect();
    let parsed = parsed?;

    // Merge results sequentially to maintain overlay semantics
    let mut lower_files: BTreeMap<String, LowerEntry> = BTreeMap::new();

    for archive_entries in parsed {
        // Apply opaque whiteouts from this layer using O(n) retain
        // More efficient than collecting keys and removing one by one
        if !archive_entries.opaque_whiteouts.is_empty() {
            // Build prefixes once for all whiteouts in this layer
            let prefixes: Vec<String> = archive_entries
                .opaque_whiteouts
                .iter()
                .map(|dirname| format!("{}/", dirname))
                .collect();

            // Single O(n) pass through the map
            lower_files.retain(|k, _| {
                !prefixes.iter().any(|prefix| k.starts_with(prefix))
            });
        }

        // Apply file whiteouts from this layer
        for path in &archive_entries.file_whiteouts {
            lower_files.remove(path);
        }

        // Add/override entries from this layer
        for (path, entry) in archive_entries.entries {
            lower_files.insert(path, entry);
        }
    }

    let mut dir_contents: FxHashMap<String, SmallVec<[String; 4]>> = FxHashMap::default();
    for file in lower_files.keys() {
        let (dirname, basename) = split_path(file);
        dir_contents
            .entry(dirname.into_owned())
            .or_default()
            .push(basename.into_owned());
    }

    Ok(LowerAnalysis {
        files: lower_files,
        dir_contents,
    })
}

#[inline]
fn split_path(path: &str) -> (Cow<str>, Cow<str>) {
    let p = Path::new(path);
    let basename = p
        .file_name()
        .map(|f| f.to_string_lossy())
        .unwrap_or_default();
    let dirname = p
        .parent()
        .map(|d| d.to_string_lossy())
        .unwrap_or_default();
    (dirname, basename)
}


/// Threshold for using mmap vs reading into memory
const MMAP_THRESHOLD: u64 = 64 * 1024; // 64KB

/// Cached file contents - either in-memory or memory-mapped
#[derive(Clone, Debug)]
pub(crate) enum FileContents {
    InMemory(Vec<u8>),
    Mapped(Arc<Mmap>),
}

impl FileContents {
    fn as_slice(&self) -> &[u8] {
        match self {
            FileContents::InMemory(v) => v.as_slice(),
            FileContents::Mapped(m) => m.as_ref(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CachedMetadata {
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub mtime: i64,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    Regular {
        checksum: String,
        contents: Option<FileContents>,
    },
    Directory,
    Symlink {
        target: String,
    },
    Hardlink {
        target_path: String,
    },
    Other,
}

#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub metadata: CachedMetadata,
    pub kind: EntryKind,
    pub xattrs: Vec<(String, String)>,
}

/// Pre-calculated data for the entire layer, mapping relative paths to entry info.
pub struct LayerData {
    pub entries: FxHashMap<PathBuf, EntryInfo>,
    /// Map from relative directory path to list of child basenames.
    pub children: FxHashMap<PathBuf, Vec<String>>,
}

use dashmap::DashMap;

/// Collect and pre-calculate all data for a directory tree in parallel.
fn precalculate_layer_data(upper: &Path, config: &GlobalConfig) -> LayerData {
    // Use saturating_mul to prevent overflow on large prefetch limits
    let memory_limit = config.prefetch_limit_mb.saturating_mul(1024).saturating_mul(1024);
    let memory_used = Arc::new(AtomicUsize::new(0));
    let skip_xattrs = config.skip_xattrs;

    // Map of (dev, ino) -> first seen relative path for hardlink detection
    // Use DashMap for wait-free concurrent access
    let inode_map: Arc<DashMap<(u64, u64), String>> = Arc::new(DashMap::default());

    // Use jwalk to collect all entries (dirs, files, symlinks)
    let all_entries: Vec<jwalk::DirEntry<((), ())>> = WalkDir::new(upper)
        .skip_hidden(false)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .collect();

    let results: FxHashMap<PathBuf, EntryInfo> = all_entries
        .par_iter()
        .filter_map(|entry| {
            let full_path = entry.path();
            if full_path == upper {
                return None; // Skip root, handled specially or as part of traversal
            }
            
            let meta = entry.metadata().ok()?;
            let file_type = meta.file_type();
            
            let metadata = CachedMetadata {
                mode: meta.permissions().mode(),
                uid: meta.uid() as u64,
                gid: meta.gid() as u64,
                mtime: meta.mtime(),
                size: meta.len(),
            };

            // SYSCALL OPTIMIZATION:
            // Single pass to get all xattrs, diverting checksum if found.
            // This avoids redundant listxattr + getxattr calls.
            let mut xattrs = Vec::new();
            let mut xattr_checksum = None;

            if !skip_xattrs {
                if let Ok(attrs_list) = xattr::list(&full_path) {
                    for attr_name in attrs_list {
                        let attr_str = attr_name.to_string_lossy().to_string();
                        // Only fetch value if we care about it
                        if let Ok(Some(val)) = xattr::get(&full_path, &attr_name) {
                            if attr_str == "user.checksum.sha256" {
                                xattr_checksum = Some(String::from_utf8_lossy(&val).to_string());
                            } else {
                                let val_str = String::from_utf8_lossy(&val).to_string();
                                xattrs.push((attr_str, val_str));
                            }
                        }
                    }
                }
            }

            let rel_path = pathdiff(&full_path, upper).into_owned();

            let kind = if file_type.is_dir() {
                EntryKind::Directory
            } else if file_type.is_symlink() {
                let target = fs::read_link(&full_path).ok()?
                    .to_string_lossy().to_string();
                EntryKind::Symlink { target }
            } else if file_type.is_file() {
                // Hardlink detection using DashMap for atomic check-and-insert without manual locking
                let dev_ino = (meta.dev(), meta.ino());
                
                use dashmap::mapref::entry::Entry;
                match inode_map.entry(dev_ino) {
                    Entry::Occupied(e) => {
                        // Another file with the same inode was already seen - this is a hardlink
                        EntryKind::Hardlink { target_path: e.get().clone() }
                    }
                    Entry::Vacant(e) => {
                        // First time seeing this inode - insert our path and compute hash
                        e.insert(rel_path.clone());
                        
                        // No lock to drop, DashMap handles it per-shard

                        let file_size = meta.len();

                        let current_memory = memory_used.load(Ordering::Relaxed);
                        // Use saturating_add to prevent overflow when checking cache capacity
                        let can_cache = current_memory.saturating_add(file_size as usize) <= memory_limit;

                        let (contents, checksum) = if file_size >= MMAP_THRESHOLD {
                            let file = fs::File::open(&full_path).ok()?;
                            // SAFETY: The source filesystem is expected to be stable during OCI builds.
                            // Files should not be modified or deleted while we hold the mmap.
                            let mmap = unsafe { Mmap::map(&file).ok()? };
                            let checksum = xattr_checksum.unwrap_or_else(|| {
                                let mut hasher = Sha256::new();
                                hasher.update(&mmap[..]);
                                format!("{:x}", hasher.finalize())
                            });
                            (Some(FileContents::Mapped(Arc::new(mmap))), checksum)
                        } else if can_cache {
                            let data = fs::read(&full_path).ok()?;
                            memory_used.fetch_add(data.len(), Ordering::Relaxed);
                            let checksum = xattr_checksum.unwrap_or_else(|| {
                                let mut hasher = Sha256::new();
                                hasher.update(&data);
                                format!("{:x}", hasher.finalize())
                            });
                            (Some(FileContents::InMemory(data)), checksum)
                        } else {
                            let checksum = xattr_checksum.unwrap_or_else(|| {
                                file_sha256(&full_path).unwrap_or_default()
                            });
                            (None, checksum)
                        };

                        EntryKind::Regular { checksum, contents }
                    }
                }
            } else {
                EntryKind::Other
            };

            Some((full_path, EntryInfo { metadata, kind, xattrs }))
        })
        .collect();

    let mut children: FxHashMap<PathBuf, Vec<String>> = FxHashMap::default();
    for path in results.keys() {
        if let Some(parent) = path.parent() {
            if let Some(file_name) = path.file_name() {
                let name = file_name.to_string_lossy().to_string();
                children.entry(parent.to_path_buf()).or_default().push(name);
            }
        }
    }

    // Sort children for deterministic output
    for child_list in children.values_mut() {
        child_list.sort();
    }

    LayerData { entries: results, children }
}

pub fn create_layer<W: std::io::Write>(
    output: &mut tar::Builder<W>,
    upper: &Path,
    lower_analysis: &LowerAnalysis,
    config: &GlobalConfig,
) -> Result<()> {
    let epoch = crate::util::get_source_date_epoch();

    // Pre-calculate all data in parallel
    let layer_data = precalculate_layer_data(upper, config);

    let mut stack: Vec<PathBuf> = vec![upper.to_path_buf()];
    let mut path_scratch = String::with_capacity(256);

    while let Some(root) = stack.pop() {
        let root_rel = pathdiff(&root, upper);

        let rel_prefix = if root_rel == "." {
            Cow::Borrowed("./")
        } else {
            Cow::Owned(format!("./{}/", root_rel))
        };

        // Add directory entry (root use root_meta, others from layer_data)
        let mut dir_header = tar::Header::new_gnu();
        dir_header.set_entry_type(tar::EntryType::Directory);

        let metadata = if root == upper {
            let meta = fs::symlink_metadata(&root)?;
            CachedMetadata {
                mode: meta.permissions().mode(),
                uid: meta.uid() as u64,
                gid: meta.gid() as u64,
                mtime: meta.mtime(),
                size: 0,
            }
        } else {
            match layer_data.entries.get(&root) {
                Some(entry) => entry.metadata.clone(),
                None => {
                    anyhow::bail!("Missing entry in layer data for path: {:?}", root);
                }
            }
        };

        dir_header.set_mode(metadata.mode);
        dir_header.set_uid(metadata.uid);
        dir_header.set_gid(metadata.gid);
        dir_header.set_mtime(if let Some(ep) = epoch { ep } else { metadata.mtime as u64 });
        dir_header.set_size(0);
        dir_header.set_cksum();
        output.append_data(&mut dir_header, &*rel_prefix, &[] as &[u8])?;

        let empty_vec: Vec<String> = Vec::new();
        let child_names = layer_data.children.get(&root).unwrap_or(&empty_vec);
        // child_names is already sorted from precalculate_layer_data
        
        // Push subdirs to stack in reverse for DFS
        for name in child_names.iter().rev() {
            let path = root.join(name);
            if let Some(info) = layer_data.entries.get(&path) {
                if let EntryKind::Directory = info.kind {
                    stack.push(path);
                }
            }
        }

        // Handle whiteouts
        let lookup_prefix = if root_rel == "." {
            Cow::Borrowed(".")
        } else {
            Cow::Owned(format!("./{}", root_rel))
        };

        if let Some(old_files) = lower_analysis.dir_contents.get(lookup_prefix.as_ref()) {
            // Build HashSet for O(1) lookups instead of O(log n) binary_search
            let child_set: std::collections::HashSet<&str> =
                child_names.iter().map(|s| s.as_str()).collect();

            for old_file in old_files {
                // Check if missing in current layer - O(1) with HashSet
                if !child_set.contains(old_file.as_str()) {
                    path_scratch.clear();
                    path_scratch.push_str(&rel_prefix);
                    path_scratch.push_str(old_file);
                    
                    if let Some(old_entry) = lower_analysis.files.get(&path_scratch) {
                        // Build whiteout name in scratch buffer
                        path_scratch.clear();
                        path_scratch.push_str(&rel_prefix);
                        path_scratch.push_str(".wh.");
                        path_scratch.push_str(old_file);
                        
                        let mut wh_header = tar::Header::new_gnu();
                        wh_header.set_entry_type(tar::EntryType::Regular);
                        wh_header.set_uid(old_entry.uid);
                        wh_header.set_gid(old_entry.gid);
                        wh_header.set_mode(old_entry.mode);
                        wh_header.set_mtime(old_entry.mtime);
                        wh_header.set_size(0);
                        wh_header.set_cksum();
                        output.append_data(&mut wh_header, &path_scratch, &[] as &[u8])?;
                    }
                }
            }
        }

        // Process non-directory files
        for name in child_names {
            let path = root.join(name);
            let info = match layer_data.entries.get(&path) {
                Some(entry) => entry,
                None => {
                    anyhow::bail!("Missing entry in layer data for file: {:?}", path);
                }
            };

            if let EntryKind::Directory = info.kind {
                continue;
            }

            path_scratch.clear();
            path_scratch.push_str(&rel_prefix);
            path_scratch.push_str(name);
            let rel = &path_scratch;

            let mut header = tar::Header::new_gnu();
            header.set_uid(info.metadata.uid);
            header.set_gid(info.metadata.gid);
            header.set_mode(info.metadata.mode);
            header.set_mtime(if let Some(ep) = epoch { ep } else { info.metadata.mtime as u64 });

            let mut pax_headers: HashMap<String, String> = HashMap::with_capacity(8);

            match &info.kind {
                EntryKind::Regular { checksum, .. } => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(info.metadata.size);
                    for (attr, value) in &info.xattrs {
                        pax_headers.insert(format!("{}{}", PAX_HEADER_XATTR, attr), value.clone());
                    }
                    pax_headers.insert(PAX_HEADER_SHA256.to_string(), checksum.clone());
                    
                    // Deduplication check - short-circuit on checksum first (most discriminating, O(1))
                    if let Some(lower_entry) = lower_analysis.files.get(rel.as_str()) {
                        // Check checksum FIRST - most selective, avoids allocations if mismatch
                        let checksum_matches = lower_entry
                            .pax_headers
                            .get(PAX_HEADER_SHA256)
                            .map(|other| checksum == other)
                            .unwrap_or(false);

                        if checksum_matches
                            && lower_entry.entry_type == tar::EntryType::Regular.as_byte()
                            && lower_entry.size == info.metadata.size
                            && lower_entry.mode == info.metadata.mode
                            && lower_entry.uid == info.metadata.uid
                            && lower_entry.gid == info.metadata.gid
                            && lower_entry.mtime == (if let Some(ep) = epoch { ep } else { info.metadata.mtime as u64 })
                        {
                            // Short-circuit xattr comparison: count first to avoid allocation if counts differ
                            let my_xattr_count = pax_headers
                                .keys()
                                .filter(|k| k.starts_with(PAX_HEADER_XATTR))
                                .count();
                            let lower_xattr_count = lower_entry
                                .pax_headers
                                .keys()
                                .filter(|k| k.starts_with(PAX_HEADER_XATTR))
                                .count();

                            if my_xattr_count == lower_xattr_count {
                                // Only allocate if counts match
                                let mut my_xattrs: Vec<(&String, &String)> = pax_headers
                                    .iter()
                                    .filter(|(k, _)| k.starts_with(PAX_HEADER_XATTR))
                                    .collect();
                                my_xattrs.sort();

                                let mut lower_xattrs: Vec<(&String, &String)> = lower_entry
                                    .pax_headers
                                    .iter()
                                    .filter(|(k, _)| k.starts_with(PAX_HEADER_XATTR))
                                    .collect();
                                lower_xattrs.sort();

                                if my_xattrs == lower_xattrs {
                                    continue; // Skip! File is identical to lower layer
                                }
                            }
                        }
                    }
                }
                EntryKind::Symlink { target } => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    header.set_link_name(target)?;

                    // Deduplication check for symlinks
                    if let Some(lower_entry) = lower_analysis.files.get(rel.as_str()) {
                         if lower_entry.entry_type == tar::EntryType::Symlink.as_byte()
                            && lower_entry.mode == info.metadata.mode
                            && lower_entry.uid == info.metadata.uid
                            && lower_entry.gid == info.metadata.gid
                        {
                            if let Some(lower_target) = &lower_entry.symlink_target {
                                if target == lower_target {
                                    continue;
                                }
                            }
                        }
                    }
                }
                EntryKind::Hardlink { target_path } => {
                    header.set_entry_type(tar::EntryType::Link);
                    header.set_size(0);
                    // tar links are relative to the archive root usually or absolute depending on builder.
                    // tar::Builder::append_link usually handles this.
                    // We need the relative path of the first seen file.
                    // rel_prefix + name is the current rel.
                    // target_path is already relative to archive root (starting with ./ or otherwise consistent).
                    // Actually, our pathdiff returns paths like "bin/run". 
                    // Our rel_prefix is "./bin/".
                    // Let's ensure target_path is formatted correctly.
                    let formatted_target = if target_path.starts_with("./") {
                        target_path.clone()
                    } else {
                        format!("./{}", target_path)
                    };
                    header.set_link_name(&formatted_target)?;
                }
                _ => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(info.metadata.size);
                }
            }

            // Write PAX headers
            if !pax_headers.is_empty() {
                let mut pax_data = Vec::with_capacity(512);
                let mut sorted_keys: Vec<_> = pax_headers.keys().collect();
                sorted_keys.sort();

                for key in sorted_keys {
                    let value = &pax_headers[key];
                    let entry_str_len = key.len() + value.len() + 2;
                    let mut digits = 1; 
                    let mut total_len = digits + 1 + entry_str_len; 
                    if total_len >= 10 {
                        digits = count_digits(total_len);
                        total_len = digits + 1 + entry_str_len;
                        if count_digits(total_len) != digits { total_len += 1; }
                    }
                    writeln!(pax_data, "{} {}={}", total_len, key, value)?;
                }
                let mut pax_header = tar::Header::new_ustar();
                pax_header.set_entry_type(tar::EntryType::XHeader);
                pax_header.set_size(pax_data.len() as u64);
                pax_header.set_cksum();
                output.append_data(&mut pax_header, rel, &pax_data[..])?;
            }

            header.set_cksum();
            if let EntryKind::Regular { contents: Some(ref c), .. } = info.kind {
                output.append_data(&mut header, rel, c.as_slice())?;
            } else if let EntryKind::Regular { .. } = info.kind {
                let f = fs::File::open(&path)?;
                output.append_data(&mut header, rel, f)?;
            } else {
                output.append_data(&mut header, rel, &[] as &[u8])?;
            }
        }
    }

    Ok(())
}

#[inline]
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

#[inline]
fn pathdiff<'a>(path: &'a Path, base: &Path) -> Cow<'a, str> {
    match path.strip_prefix(base) {
        Ok(rel) => {
            let s = rel.to_string_lossy();
            if s.is_empty() {
                Cow::Borrowed(".")
            } else {
                s
            }
        }
        Err(_) => path.to_string_lossy(),
    }
}
