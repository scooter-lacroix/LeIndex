// Index Freshness — staleness detection logic extracted from LeIndex

use super::leindex::{ProjectFileScan, DEPENDENCY_MANIFEST_NAMES};
use crate::cli::memory::CacheEntry;
use crate::cli::skip_dirs::SKIP_DIRS;
use crate::storage::schema::Storage;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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
/// 4. Root-manifest stat (O(14) — catches `Cargo.toml` /
///    `package.json` / `pyproject.toml` at the project root)
/// 5. Bounded-depth nested-manifest walkdir (`max_depth(5)`,
///    skipping dotfile dirs and SKIP_DIRS) — catches monorepo
///    cases like `packages/api/package.json` where the new
///    manifest is not at the project root.
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
    // new manifest after the index was built (whether at the
    // project root or in a monorepo subdirectory such as
    // `packages/api/package.json`) would not be caught by the
    // source-count check (no new source file was indexed), the
    // directory-mtime check (the manifest's parent dir may not
    // be in `source_dirs`), the source-file sample check, or the
    // cached manifest list. The check below is a bounded-depth
    // walkdir (`max_depth(5)`, skipping dotfile dirs and the
    // shared SKIP_DIRS list) that looks only for files whose
    // name is in `DEPENDENCY_MANIFEST_NAMES`. The walk short-
    // circuits at the first new manifest, so the common case
    // (no new manifest) costs O(number of directories up to
    // depth 5) which is on the order of a few thousand
    // directory entries for a large monorepo, comparable to
    // one `cargo build` directory traversal. We also keep the
    // root-only fast path above (it does the same canonicalize
    // check in O(14) without touching walkdir) for the common
    // case of a single-package project.
    //
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
    // Root-level fast path: O(14) stat, no walkdir. Common case
    // for single-package projects.
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
    let mut new_root_manifest = false;
    for name in ROOT_MANIFEST_NAMES {
        let candidate = ctx.project_path.join(name);
        if !candidate.exists() {
            continue;
        }
        let candidate_canon = candidate.canonicalize().unwrap_or(candidate);
        if !already_listed.contains(&candidate_canon) {
            new_root_manifest = true;
            break;
        }
    }
    if new_root_manifest {
        return true;
    }
    // Nested-manifest slow path: bounded-depth walkdir that
    // catches monorepo cases like `packages/api/package.json`
    // where the manifest is not at the project root. We walk
    // up to depth 5 (covers repos like
    // `repo/services/auth/config/Cargo.toml`) and skip
    // dotfile / build / cache / VCS directories via the shared
    // SKIP_DIRS list. The walk short-circuits at the first
    // new manifest, so the common case (no new manifest)
    // completes after visiting every directory entry up to
    // depth 5 — on the order of a few thousand `metadata()`
    // calls, comparable to one `cargo build` traversal. A
    // walkdir iteration error is treated the same as the full
    // scan in `index_builder::scan_project_files`: skipped
    // silently and we move to the next entry. Permission
    // errors or vanished paths do not flip the verdict to
    // stale by themselves; we only return `true` when a
    // candidate manifest is actually present and not in the
    // cached list.
    if find_new_nested_manifest(ctx.project_path, &already_listed) {
        return true;
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

/// Walk `project_path` up to depth 5 looking for dependency
/// manifests that are NOT in the supplied `already_listed` set.
///
/// The set is expected to contain canonicalized absolute paths
/// (built by the caller via `Path::canonicalize`). This helper
/// exists as a separate function so it can be unit-tested in
/// isolation against a tempdir fixture without spinning up
/// the full `Storage` / `CacheSpiller` context that
/// `is_stale_fast` requires.
///
/// Returns `true` on the first new manifest found (short-
/// circuits to keep the common "no new manifest" case fast).
/// Walkdir iteration errors are skipped silently — the same
/// policy as the full project scan in
/// `index_builder::scan_project_files`.
pub(crate) fn find_new_nested_manifest(
    project_path: &Path,
    already_listed: &std::collections::HashSet<PathBuf>,
) -> bool {
    let walker = WalkDir::new(project_path)
        .min_depth(1)
        .max_depth(5)
        .into_iter();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Skip dotfile-prefixed entries and the shared skip list
        // (build outputs, venv, node_modules, .git, etc.). We
        // re-apply the same rules as the full scan in
        // `index_builder::scan_project_files` so a new manifest
        // hidden under, e.g., `node_modules` is not flagged.
        // Note: the full scan skips SKIP_DIRS at directory
        // traversal time via `walker.skip_current_dir()`, which
        // means the file is never visited. In the per-file
        // helper here we have to walk up the parent chain and
        // check every ancestor directory against SKIP_DIRS to
        // get the same coverage.
        if file_name.starts_with('.') {
            continue;
        }
        let mut ancestor = path.parent();
        let mut in_skip_dir = false;
        while let Some(dir) = ancestor {
            if dir == project_path {
                break;
            }
            if let Some(dname) = dir.file_name().and_then(|n| n.to_str()) {
                if SKIP_DIRS.contains(&dname) {
                    in_skip_dir = true;
                    break;
                }
            }
            ancestor = dir.parent();
        }
        if in_skip_dir {
            continue;
        }
        if !DEPENDENCY_MANIFEST_NAMES.contains(&file_name) {
            continue;
        }
        let candidate_canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !already_listed.contains(&candidate_canon) {
            return true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Build a tempdir fixture for the bounded-walkdir tests.
    /// Returns `(tempdir_path, already_listed_set)` — the set is
    /// pre-populated with the canonicalized absolute paths of
    /// any manifests that the test wants the helper to treat as
    /// "already known". Tests that want to exercise the "new
    /// manifest" case simply leave the set empty.
    fn make_fixture() -> (tempfile::TempDir, std::collections::HashSet<PathBuf>) {
        let tmp = tempfile::tempdir().unwrap();
        (tmp, std::collections::HashSet::new())
    }

    #[test]
    fn find_new_nested_manifest_detects_monorepo_package_json() {
        // Regression: a new `packages/api/package.json` (a
        // monorepo nested manifest) must be flagged as stale
        // by the bounded walkdir. The previous root-only check
        // missed this case entirely.
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        fs::create_dir_all(root.join("packages/api/src")).unwrap();
        fs::write(root.join("packages/api/package.json"), "{}").unwrap();
        fs::write(root.join("packages/api/src/main.rs"), "fn main() {}").unwrap();
        assert!(
            find_new_nested_manifest(root, &listed),
            "monorepo package.json at depth 2 must be flagged"
        );
    }

    #[test]
    fn find_new_nested_manifest_detects_cargo_toml_at_depth_3() {
        // Depth-3 monorepo case: `services/auth/config/Cargo.toml`.
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        fs::create_dir_all(root.join("services/auth/config/src")).unwrap();
        fs::write(
            root.join("services/auth/config/Cargo.toml"),
            "[package]\nname = \"auth\"\n",
        )
        .unwrap();
        assert!(
            find_new_nested_manifest(root, &listed),
            "depth-3 Cargo.toml must be flagged"
        );
    }

    #[test]
    fn find_new_nested_manifest_skips_node_modules() {
        // A new `node_modules/foo/package.json` must NOT be
        // flagged — node_modules is in SKIP_DIRS and would
        // otherwise produce a false-positive stale verdict on
        // every `npm install`.
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules/foo/src")).unwrap();
        fs::write(root.join("node_modules/foo/package.json"), "{}").unwrap();
        assert!(
            !find_new_nested_manifest(root, &listed),
            "node_modules/package.json must be skipped"
        );
    }

    #[test]
    fn find_new_nested_manifest_skips_target() {
        // A new `target/Cargo.toml` (e.g. via a build script)
        // must NOT be flagged — `target` is in SKIP_DIRS.
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        fs::create_dir_all(root.join("target/some-build/src")).unwrap();
        fs::write(root.join("target/some-build/Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert!(
            !find_new_nested_manifest(root, &listed),
            "target/Cargo.toml must be skipped"
        );
    }

    #[test]
    fn find_new_nested_manifest_respects_already_listed() {
        // When the nested manifest IS in `already_listed`, the
        // helper must return false (no new manifest). The
        // canonicalize() form of the path is what the caller
        // uses, so we match that here.
        let (tmp, mut listed) = make_fixture();
        let root = tmp.path();
        fs::create_dir_all(root.join("packages/api/src")).unwrap();
        let manifest = root.join("packages/api/package.json");
        fs::write(&manifest, "{}").unwrap();
        let canon = manifest.canonicalize().unwrap();
        listed.insert(canon);
        assert!(
            !find_new_nested_manifest(root, &listed),
            "manifest already in listed set must not be flagged"
        );
    }

    #[test]
    fn find_new_nested_manifest_returns_false_when_no_manifests() {
        // A project with no manifests (e.g. a fresh source
        // tree with only `.rs` files) must return false — the
        // bounded walkdir must not produce false positives.
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src/lib.rs"), "// lib\n").unwrap();
        assert!(
            !find_new_nested_manifest(root, &listed),
            "no manifests present, must return false"
        );
    }

    #[test]
    fn find_new_nested_manifest_respects_max_depth_5() {
        // max_depth=5 means depth 6 is NOT visited. A manifest
        // at depth 6 (e.g. a/b/c/d/e/f/Cargo.toml) must NOT
        // be flagged — the bounded walkdir is a deliberate
        // performance/perf trade-off that the index builder's
        // full scan covers at index-build time.
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        let deep = root.join("a/b/c/d/e/f");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("Cargo.toml"), "[package]\nname=\"deep\"\n").unwrap();
        assert!(
            !find_new_nested_manifest(root, &listed),
            "depth-6 manifest must not be flagged (max_depth=5)"
        );
    }

    #[test]
    fn find_new_nested_manifest_ignores_dotfile_dirs() {
        // A new `.cargo/config.toml` (an oddball dotfile
        // directory) must NOT be flagged — dotfile-prefixed
        // directories are skipped to keep the walkdir cost
        // bounded and to match the full scan's behaviour.
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(root.join(".cargo/config.toml"), "[net]\n").unwrap();
        assert!(
            !find_new_nested_manifest(root, &listed),
            ".cargo/config.toml must not be flagged"
        );
    }
}
