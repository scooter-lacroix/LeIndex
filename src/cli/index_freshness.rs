// Index Freshness — staleness detection logic extracted from LeIndex

use crate::cli::memory::CacheEntry;
use crate::storage::schema::Storage;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use super::leindex::{ProjectFileScan, ALWAYS_SKIP_DIRS, DEPENDENCY_MANIFEST_NAMES};

/// Read-only context passed to freshness functions to avoid borrow conflicts.
pub(crate) struct FreshnessContext<'a> {
    pub project_path: &'a Path,
    pub storage_path: &'a Path,
    pub project_id: &'a str,
    pub storage: &'a Storage,
    pub project_scan: Option<&'a ProjectFileScan>,
    pub cache_spiller: &'a crate::cli::memory::CacheSpiller,
}

/// Check which source files have changed since last index.
/// Returns (changed_paths, deleted_paths).
pub(crate) fn check_freshness(
    ctx: &FreshnessContext<'_>,
    scan_fn: impl Fn() -> Result<ProjectFileScan>,
    hash_fn: impl Fn(&Path) -> Result<String>,
) -> Result<(Vec<PathBuf>, Vec<String>)> {
    let indexed_files =
        crate::storage::pdg_store::get_indexed_files(ctx.storage, ctx.project_id)
            .unwrap_or_default();

    let scan = scan_fn()?;
    let current: Vec<(PathBuf, String)> = scan
        .source_paths
        .iter()
        .map(|path| Ok((path.clone(), hash_fn(path)?)))
        .collect::<Result<_>>()?;
    let current_map: HashMap<String, String> = current
        .iter()
        .map(|(p, h)| (p.display().to_string(), h.clone()))
        .collect();

    let changed: Vec<PathBuf> = current
        .iter()
        .filter(|(p, h)| indexed_files.get(&p.display().to_string()) != Some(h))
        .map(|(p, _)| p.clone())
        .collect();

    let deleted: Vec<String> = indexed_files
        .keys()
        .filter(|k| !current_map.contains_key(*k))
        .cloned()
        .collect();

    Ok((changed, deleted))
}

/// Check if any dependency manifest/lockfile has changed since last index.
pub(crate) fn check_manifest_stale(ctx: &FreshnessContext<'_>, scan_fn: impl Fn() -> Result<ProjectFileScan>) -> bool {
    let db_time = match ctx
        .storage_path
        .join("leindex.db")
        .metadata()
        .and_then(|m| m.modified())
    {
        Ok(t) => t,
        Err(_) => return true,
    };

    let scan = ctx.project_scan;
    let paths_to_check: Vec<PathBuf> = if let Some(scan) = scan {
        scan.manifest_paths.clone()
    } else {
        match scan_fn() {
            Ok(scan) => scan.manifest_paths,
            Err(_) => return true,
        }
    };

    let original_scan_paths: std::collections::HashSet<PathBuf> =
        paths_to_check.iter().cloned().collect();

    let mut all_paths: std::collections::HashSet<PathBuf> = paths_to_check.into_iter().collect();
    for name in DEPENDENCY_MANIFEST_NAMES {
        all_paths.insert(ctx.project_path.join(name));
    }

    for manifest_path in &all_paths {
        match std::fs::metadata(manifest_path) {
            Ok(metadata) => {
                if let Ok(modified) = metadata.modified() {
                    if modified > db_time {
                        return true;
                    }
                }
            }
            Err(_) => {
                if original_scan_paths.contains(manifest_path) {
                    return true;
                }
            }
        }
    }
    false
}

/// Fast-path freshness check: O(1) for indexed files, O(D) for source
/// directories (typically 10-20), and O(M) for manifest files.
///
/// Detection layers (in order):
/// 1. Source count mismatch (cached vs indexed)
/// 2. Directory mtime sentinel — detects new file additions in <1ms
/// 3. Mtime sampling of 50-500 indexed files
/// 4. Manifest walkdir (depth-limited) for dependency changes
pub(crate) fn is_stale_fast(ctx: &FreshnessContext<'_>, scan_fn: impl Fn() -> Result<ProjectFileScan>) -> bool {
    let indexed_files =
        crate::storage::pdg_store::get_indexed_files(ctx.storage, ctx.project_id)
            .unwrap_or_default();

    if indexed_files.is_empty() {
        return true;
    }

    let db_time = match ctx
        .storage_path
        .join("leindex.db")
        .metadata()
        .and_then(|m| m.modified())
    {
        Ok(t) => t,
        Err(_) => return true,
    };

    let mut cold_manifest_paths: Option<Vec<PathBuf>> = None;
    let mut source_count: Option<usize> = None;
    let mut cached_manifest_paths: Option<Vec<PathBuf>> = None;
    let mut cached_scan: Option<ProjectFileScan> = None;
    match ctx.project_scan {
        Some(cache) => {
            source_count = Some(cache.source_paths.len());
        }
        None => {
            let cache_key = crate::cli::memory::project_scan_cache_key(ctx.project_id);
            if let Some(entry) = ctx.cache_spiller.store().peek(&cache_key) {
                if let CacheEntry::Binary {
                    serialized_data, ..
                } = entry
                {
                    if let Ok(scan) = bincode::deserialize::<ProjectFileScan>(serialized_data) {
                        cached_manifest_paths = Some(scan.manifest_paths.clone());
                        cached_scan = Some(scan.clone());
                        source_count = Some(scan.source_paths.len());
                    }
                }
            } else if let Ok(entry) = ctx.cache_spiller.store().load_from_disk(&cache_key) {
                if let CacheEntry::Binary {
                    serialized_data, ..
                } = entry
                {
                    if let Ok(scan) = bincode::deserialize::<ProjectFileScan>(&serialized_data) {
                        cached_manifest_paths = Some(scan.manifest_paths.clone());
                        cached_scan = Some(scan.clone());
                        source_count = Some(scan.source_paths.len());
                    }
                }
            }
            if source_count.is_none() {
                match scan_fn() {
                    Ok(scan) => {
                        cold_manifest_paths = Some(scan.manifest_paths.clone());
                        source_count = Some(scan.source_paths.len());
                    }
                    Err(_) => return true,
                }
            }
        }
    };
    let source_count = source_count.unwrap_or(indexed_files.len());

    if source_count != indexed_files.len() {
        return true;
    }

    // Directory mtime sentinel check
    let source_dirs: Vec<PathBuf> = if let Some(scan) = ctx.project_scan {
        scan.source_directories.clone()
    } else if let Some(scan) = cached_scan.as_ref() {
        scan.source_directories.clone()
    } else {
        let mut dirs: Vec<PathBuf> = indexed_files
            .keys()
            .filter_map(|p| PathBuf::from(p).parent().map(|d| d.to_path_buf()))
            .collect();
        dirs.sort();
        dirs.dedup();
        dirs
    };
    for dir in &source_dirs {
        let full_path = if dir.is_absolute() {
            dir.clone()
        } else {
            ctx.project_path.join(dir)
        };
        match std::fs::metadata(&full_path) {
            Ok(metadata) => {
                if let Ok(modified) = metadata.modified() {
                    if modified > db_time {
                        return true;
                    }
                }
            }
            Err(_) => {
                return true;
            }
        }
    }

    // Quick spot-check: sample indexed files for deletion or modification
    let sample_size = (indexed_files.len() / 20).max(50).min(500);
    let mut checked = 0;
    for indexed_path in indexed_files.keys() {
        if checked >= sample_size {
            break;
        }
        let full_path = ctx.project_path.join(indexed_path);
        if !full_path.exists() {
            return true;
        }
        if let Ok(metadata) = std::fs::metadata(&full_path) {
            if let Ok(modified) = metadata.modified() {
                if modified > db_time {
                    return true;
                }
            }
        }
        checked += 1;
    }

    // Check manifest file mtimes
    let manifest_paths: Vec<PathBuf> = if let Some(scan) = ctx.project_scan {
        scan.manifest_paths.clone()
    } else if let Some(ref paths) = cached_manifest_paths {
        paths.clone()
    } else if let Some(ref paths) = cold_manifest_paths {
        paths.clone()
    } else {
        Vec::new()
    };

    // Walk project tree for ALL manifests
    {
        for entry in walkdir::WalkDir::new(ctx.project_path)
            .max_depth(8)
            .into_iter()
            .filter_entry(|e| {
                if let Some(name) = e.file_name().to_str() {
                    if ALWAYS_SKIP_DIRS.contains(&name) && e.file_type().is_dir() {
                        return false;
                    }
                }
                true
            })
            .filter_map(|e| e.ok())
        {
            let Some(name) = entry.file_name().to_str() else {
                continue;
            };
            if !DEPENDENCY_MANIFEST_NAMES.contains(&name) {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                if let Ok(modified) = metadata.modified() {
                    if modified > db_time {
                        return true;
                    }
                }
            }
        }
    }
    for manifest_path in &manifest_paths {
        match std::fs::metadata(manifest_path) {
            Ok(metadata) => {
                if let Ok(modified) = metadata.modified() {
                    if modified > db_time {
                        return true;
                    }
                }
            }
            Err(_) => {
                return true;
            }
        }
    }

    false
}

/// Extract sorted unique directories from a list of file paths.
pub(crate) fn extract_unique_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            dirs.insert(parent.to_path_buf());
        }
    }
    let mut result: Vec<PathBuf> = dirs.into_iter().collect();
    result.sort();
    result
}
