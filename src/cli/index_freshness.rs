// Index Freshness — staleness detection logic extracted from LeIndex

use super::leindex::{ProjectFileScan, DEPENDENCY_MANIFEST_NAMES};
use crate::cli::memory::CacheEntry;
use crate::storage::schema::Storage;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    let indexed_files = crate::storage::pdg_store::get_indexed_files(ctx.storage, ctx.project_id)
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
pub(crate) fn check_manifest_stale(
    ctx: &FreshnessContext<'_>,
    scan_fn: impl Fn() -> Result<ProjectFileScan>,
) -> bool {
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
pub(crate) fn is_stale_fast(
    ctx: &FreshnessContext<'_>,
    scan_fn: impl Fn() -> Result<ProjectFileScan>,
) -> bool {
    let indexed_files = crate::storage::pdg_store::get_indexed_files(ctx.storage, ctx.project_id)
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
            } else if let Ok(CacheEntry::Binary {
                serialized_data, ..
            }) = ctx.cache_spiller.store().load_from_disk(&cache_key)
            {
                if let Ok(scan) = bincode::deserialize::<ProjectFileScan>(&serialized_data) {
                    cached_manifest_paths = Some(scan.manifest_paths.clone());
                    cached_scan = Some(scan.clone());
                    source_count = Some(scan.source_paths.len());
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
    let sample_size = (indexed_files.len() / 20).clamp(50, 500);
    for (checked, indexed_path) in indexed_files.keys().enumerate() {
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

    // The list-based check below covers every manifest already
    // discovered by the project scan (Cargo.toml, package.json,
    // pyproject.toml, etc.). The historical walkdir block that
    // re-walked the project tree here was both redundant with this
    // check AND responsible for adding hundreds of stat() calls
    // on every tool call when the stale cache TTL expired.
    //
    // However, the cached list only carries manifests that
    // existed during the previous scan. A user who adds a brand-
    // new root manifest after the index was built would not be
    // caught by either the source-count check (no new source
    // file was indexed) or the directory-mtime check (the
    // manifest's directory is unlikely to be in `source_dirs`).
    // Detect this case with a focused stat of the well-known
    // manifest filenames at the project root — O(few) stats, no
    // walkdir. A new manifest is reported as stale because the
    // dependency / external-resolution metadata in the index
    // pre-dates it.
    const ROOT_MANIFEST_NAMES: &[&str] = &[
        "Cargo.toml",
        "Cargo.lock",
        "package.json",
        "package-lock.json",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "go.mod",
        "go.sum",
        "build.gradle",
        "build.gradle.kts",
        "pom.xml",
        "requirements.txt",
        "Pipfile",
    ];
    // Canonicalize the cached list on both sides of the membership
    // check. The cached `manifest_paths` are produced by walkdir
    // against whatever `project_path` the index builder received
    // (absolute when the user passed an absolute path, relative
    // when they passed `.` or similar). The current `ctx.project_path`
    // may be in a different shape, so a raw `Path::join` + `HashSet`
    // membership check would always miss when the two forms differ.
    // We canonicalize both sides, falling back to the original
    // path on canonicalize failure (path no longer exists on disk,
    // permission denied, etc.) so a transient FS error does not
    // silently turn into a "stale" verdict.
    let mut already_listed: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::with_capacity(manifest_paths.len());
    for p in &manifest_paths {
        already_listed.insert(p.canonicalize().unwrap_or_else(|_| p.clone()));
    }
    for name in ROOT_MANIFEST_NAMES {
        let candidate = ctx.project_path.join(name);
        if !candidate.exists() {
            continue;
        }
        let candidate_canon = candidate.canonicalize().unwrap_or(candidate);
        if !already_listed.contains(&candidate_canon) {
            // Newly added root manifest — not in the cached
            // manifest list, but the dependency / external-
            // resolution data in the index pre-dates it.
            return true;
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
