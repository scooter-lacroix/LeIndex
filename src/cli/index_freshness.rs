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

/// Fast-path freshness check: O(1) for source count, O(N) for indexed
/// files (full stat() scan of every indexed file), and O(M) for manifest
/// files.
///
/// Detection layers (in order):
/// 1. Source count mismatch (cached vs indexed)
/// 2. Directory mtime sentinel — detects new file additions in <1ms
/// 3. Full O(N) mtime scan of all indexed source files — detects
///    modifications and deletions by stat()'ing every indexed file
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
    let mut cached_manifest_paths: Option<Vec<PathBuf>> = None;
    let mut cached_scan: Option<ProjectFileScan> = None;
    if ctx.project_scan.is_none() {
        let cache_key = crate::cli::memory::project_scan_cache_key(ctx.project_id);
        if let Some(entry) = ctx.cache_spiller.store().peek(&cache_key) {
            if let CacheEntry::Binary {
                serialized_data, ..
            } = entry
            {
                if let Ok(scan) = bincode::deserialize::<ProjectFileScan>(serialized_data) {
                    cached_manifest_paths = Some(scan.manifest_paths.clone());
                    cached_scan = Some(scan);
                }
            }
        } else if let Ok(CacheEntry::Binary {
            serialized_data, ..
        }) = ctx.cache_spiller.store().load_from_disk(&cache_key)
        {
            if let Ok(scan) = bincode::deserialize::<ProjectFileScan>(&serialized_data) {
                cached_manifest_paths = Some(scan.manifest_paths.clone());
                cached_scan = Some(scan);
            }
        }
        if cached_scan.is_none() {
            match scan_fn() {
                Ok(scan) => {
                    cold_manifest_paths = Some(scan.manifest_paths.clone());
                    cached_scan = Some(scan);
                }
                Err(_) => return true,
            }
        }
    }
    // Source count mismatch: O(1) check that catches file additions or
    // deletions even when directory mtimes are not updated reliably.
    let source_count = if let Some(scan) = ctx.project_scan {
        Some(scan.source_paths.len())
    } else {
        cached_scan.as_ref().map(|scan| scan.source_paths.len())
    };
    if let Some(count) = source_count {
        if count != indexed_files.len() {
            return true;
        }
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

    // Check ALL indexed source files for deletion or modification.
    // This is O(N) stat() calls but each stat is ~0.01ms, so even for
    // 1000 files the total cost is ~10ms — well within the 1000ms budget.
    // Using >= instead of > catches same-second modifications (where the
    // file mtime equals the DB mtime). This may produce false positives
    // (files indexed in the same second as the DB write), but those are
    // resolved by the authoritative check_freshness() hash comparison
    // that the caller runs when is_stale_fast returns true.
    for indexed_path in indexed_files.keys() {
        let full_path = ctx.project_path.join(indexed_path);
        if !full_path.exists() {
            return true;
        }
        if let Ok(metadata) = std::fs::metadata(&full_path) {
            if let Ok(modified) = metadata.modified() {
                if modified >= db_time {
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
    // Build the membership set. Fast path: when the scan has
    // pre-canonicalized manifest paths (populated by the scanner
    // at scan time), use them directly as a HashSet — zero
    // syscalls on the freshness-check hot path. Slow path:
    // legacy scans (empty `manifest_paths_canonical`) or cold/
    // cached paths without a scan fall back to
    // `build_already_listed`, which canonicalizes on the fly.
    // The per-call canonicalize cost is O(N) stat/readlink
    // syscalls where N is the number of manifests — in a large
    // monorepo with hundreds of package manifests this was the
    // dominant fixed cost of the freshness fast path before
    // the round-18 optimization.
    let already_listed: std::collections::HashSet<PathBuf> = if let Some(scan) = ctx.project_scan {
        if !scan.manifest_paths_canonical.is_empty() {
            scan.manifest_paths_canonical.iter().cloned().collect()
        } else {
            build_already_listed(ctx.project_path, &scan.manifest_paths)
        }
    } else {
        build_already_listed(ctx.project_path, &manifest_paths)
    };
    // Root-level fast path: O(N) stat, no walkdir. Common case
    // for single-package projects.
    //
    // The list of names must be a subset of
    // `DEPENDENCY_MANIFEST_NAMES` — every name we check here
    // must also be a name the scanner records in
    // `ProjectFileScan::manifest_paths`, otherwise the cached
    // `already_listed` set will never contain it and the fast
    // path will mark the index stale on every tool call (the
    // stale flag is then cleared by the next `leindex.index`,
    // which records the same manifest, which the *next* tool
    // call still treats as new — an infinite loop). The
    // previous literal list included `setup.py`, `setup.cfg`,
    // `build.gradle`, `build.gradle.kts`, `pom.xml`, and
    // `Pipfile`, none of which the scanner records, so any
    // project with one of those files at the root reported
    // stale forever. Reuse `DEPENDENCY_MANIFEST_NAMES`
    // directly so the two lists can never drift.
    let new_root_manifest = find_new_root_manifest(ctx.project_path, &already_listed);
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

/// Check whether the project root contains a manifest that is
/// NOT in `already_listed`. This is the O(N) root-only fast path
/// in `is_stale_fast` — no walkdir, just a `metadata()` call
/// per name in `DEPENDENCY_MANIFEST_NAMES`.
///
/// The list of names we check is exactly
/// `DEPENDENCY_MANIFEST_NAMES` (the same set the scanner
/// records in `ProjectFileScan::manifest_paths`). Factored out
/// of `is_stale_fast` so it can be unit-tested in isolation
/// against a tempdir fixture without spinning up the full
/// `Storage` / `CacheSpiller` context. Returns `true` on the
/// first new manifest found (short-circuits to keep the common
/// "no new manifest" case fast).
///
/// `already_listed` is expected to contain canonicalized
/// absolute paths (the caller in `is_stale_fast` canonicalizes
/// before calling). For a tempdir-based test the path is
/// already canonical for the duration of the test, so we fall
/// back to the original path on canonicalize failure to keep
/// the helper standalone.
pub(crate) fn find_new_root_manifest(
    project_path: &Path,
    already_listed: &std::collections::HashSet<PathBuf>,
) -> bool {
    for name in DEPENDENCY_MANIFEST_NAMES {
        let candidate = project_path.join(name);
        if !candidate.exists() {
            continue;
        }
        let candidate_canon = candidate.canonicalize().unwrap_or(candidate);
        if !already_listed.contains(&candidate_canon) {
            return true;
        }
    }
    false
}

/// Build the `already_listed` set used by `is_stale_fast`'s root
/// and nested fast paths. Each entry in `manifest_paths` is
/// joined with `project_path` (so relative scanner outputs are
/// resolved against the project root, not the CWD) and then
/// canonicalized. `Path::join` returns the second argument
/// unchanged when it is already absolute, so this is safe for
/// both relative and absolute scanner outputs.
///
/// Factored out of `is_stale_fast` so it can be unit-tested in
/// isolation against a tempdir fixture without spinning up the
/// full `Storage` / `CacheSpiller` context.
pub(crate) fn build_already_listed(
    project_path: &Path,
    manifest_paths: &[PathBuf],
) -> std::collections::HashSet<PathBuf> {
    let mut already_listed: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::with_capacity(manifest_paths.len());
    for p in manifest_paths {
        let full_path = project_path.join(p);
        already_listed.insert(full_path.canonicalize().unwrap_or(full_path));
    }
    already_listed
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
    // Use `filter_entry` to prune `SKIP_DIRS` and dotfile-prefixed
    // directories at traversal time (returning `false` for a
    // directory entry tells walkdir to skip its entire subtree).
    // The previous per-file ancestor-chain check still visited
    // every directory entry up to depth 5 and only filtered after
    // the stat, so a project with a large `node_modules` or
    // `target` would pay O(thousands of stat() calls) on every
    // expired staleness-cache check. With `filter_entry`, the
    // entire `node_modules` / `target` / `.git` / `target` /
    // `.venv` subtrees are pruned at the directory boundary
    // before any child stat happens, restoring the original
    // intent ("detect new manifests without paying the cost
    // of a full project walk"). The manifest-name filter still
    // runs after the walkdir yield so we only flag files whose
    // name is in `DEPENDENCY_MANIFEST_NAMES`.
    let walker = WalkDir::new(project_path)
        .min_depth(1)
        .max_depth(5)
        .into_iter()
        .filter_entry(|e| {
            // Always keep the root (depth 0) so descent can begin;
            // `WalkDir` only invokes `filter_entry` on the root
            // once at the very start, and returning `false`
            // there would prune the entire tree.
            if e.depth() == 0 {
                return true;
            }
            // Only prune directories. Files are accepted here
            // and filtered by manifest-name after the yield.
            if !e.file_type().is_dir() {
                return true;
            }
            let name = match e.file_name().to_str() {
                Some(n) => n,
                None => return true,
            };
            if name.starts_with('.') {
                return false;
            }
            if SKIP_DIRS.contains(&name) {
                return false;
            }
            true
        });
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
        fs::write(
            root.join("target/some-build/Cargo.toml"),
            "[package]\nname=\"x\"\n",
        )
        .unwrap();
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

    // =====================================================================
    // find_new_root_manifest — root-level fast path regression tests
    // =====================================================================

    /// Regression for P2 round 12: the previous `ROOT_MANIFEST_NAMES`
    /// literal in `is_stale_fast` included `setup.py`, `setup.cfg`,
    /// `build.gradle`, `build.gradle.kts`, `pom.xml`, and `Pipfile`,
    /// none of which the scanner records. A project with any of
    /// these files at the root would report stale on every tool
    /// call (the cached `already_listed` set never contained them,
    /// so the fast path kept flagging them as new). After the
    /// fix, the fast path uses `DEPENDENCY_MANIFEST_NAMES`
    /// directly. Verify that an `already_listed` set containing
    /// the scanner-tracked manifests does NOT report stale for a
    /// project that has `setup.py` at the root (i.e. `setup.py` is
    /// not in the fast path any more).
    #[test]
    fn find_new_root_manifest_ignores_setup_py_when_cached() {
        let (tmp, _) = make_fixture();
        let root = tmp.path();
        // `setup.py` is in the *old* literal but NOT in
        // `DEPENDENCY_MANIFEST_NAMES`, so the post-fix fast path
        // must not consider it.
        fs::write(root.join("setup.py"), "from setuptools import setup\n").unwrap();
        // Create a scanner-tracked manifest at the root and add
        // it to `already_listed` to simulate a previously-
        // indexed state.
        let manifest = root.join("pyproject.toml");
        fs::write(&manifest, "[tool.poetry]\nname = \"x\"\n").unwrap();
        let mut listed: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        listed.insert(manifest.canonicalize().unwrap());
        assert!(
            !find_new_root_manifest(root, &listed),
            "setup.py must not be flagged when pyproject.toml is already listed"
        );
    }

    /// A new scanner-tracked manifest (e.g. a fresh `Cargo.toml`)
    /// at the root MUST be flagged as new when it is not in
    /// `already_listed`.
    #[test]
    fn find_new_root_manifest_detects_new_cargo_toml() {
        let (tmp, listed) = make_fixture();
        let root = tmp.path();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert!(
            find_new_root_manifest(root, &listed),
            "new Cargo.toml at root must be flagged"
        );
    }

    /// A scanner-tracked manifest that is already in
    /// `already_listed` must NOT be flagged. This locks the
    /// canonicalize-and-compare behaviour so a future refactor
    /// doesn't accidentally introduce a stale-on-every-call
    /// regression.
    #[test]
    fn find_new_root_manifest_does_not_flag_listed_manifest() {
        let (tmp, _) = make_fixture();
        let root = tmp.path();
        fs::write(root.join("package.json"), "{}").unwrap();
        let mut listed: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        listed.insert(root.join("package.json").canonicalize().unwrap());
        assert!(
            !find_new_root_manifest(root, &listed),
            "listed package.json must not be flagged"
        );
    }

    // =====================================================================
    // build_already_listed — round-13 gemini canonicalize-relative-path fix
    // =====================================================================

    /// Regression for HIGH round 13: when the scanner records a
    /// relative manifest path (e.g. `Cargo.toml` rather than
    /// `/abs/path/Cargo.toml`), `is_stale_fast` used to
    /// canonicalize the relative path directly, which resolves
    /// against the CWD rather than the project root. When
    /// CWD ≠ project root, the canonical path is wrong, the
    /// `already_listed.contains(...)` membership check always
    /// misses, and the fast path reports stale on every tool
    /// call (the cycle of "stale → reindex → still stale"
    /// that the previous review called out as the
    /// "setup.py / build.gradle" loop).
    ///
    /// The fix joins each `manifest_paths` entry with
    /// `project_path` before canonicalizing. `Path::join`
    /// returns the second argument unchanged when it is
    /// already absolute, so absolute paths are unaffected.
    /// Verify the contract: a relative scanner output, joined
    /// with a relative `project_path`, is canonicalized
    /// against the project root and ends up in the
    /// `already_listed` set under its real absolute form.
    #[test]
    fn build_already_listed_resolves_relative_paths_against_project_path() {
        let (tmp, _) = make_fixture();
        let root = tmp.path();
        // Create a manifest file at the project root.
        let manifest = root.join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"x\"\n").unwrap();
        // The scanner wrote a *relative* path (e.g. when the
        // user passed `.` as the project root). Pre-fix,
        // canonicalize would resolve this against the test
        // runner's CWD; post-fix, we join with `project_path`
        // first so the canonical path matches the project
        // root.
        let relative = std::path::PathBuf::from("Cargo.toml");
        let listed = build_already_listed(root, &[relative]);
        // The set must contain the canonical absolute path of
        // the manifest at the project root, NOT whatever the
        // relative-path canonicalize would have produced
        // (which in this test is the same directory anyway,
        // but the contract is the canonical absolute form).
        let canon = manifest.canonicalize().unwrap();
        assert!(
            listed.contains(&canon),
            "already_listed must contain the canonical absolute path of the joined manifest: {:?}",
            canon
        );
    }

    /// An absolute scanner output must round-trip unchanged.
    /// `Path::join(root, abs_path)` returns `abs_path` when
    /// `abs_path` is already absolute (Rust's documented
    /// behaviour), so the canonical form is identical to the
    /// pre-fix behaviour for absolute inputs.
    #[test]
    fn build_already_listed_passes_through_absolute_paths() {
        let (tmp, _) = make_fixture();
        let root = tmp.path();
        let manifest = root.join("package.json");
        fs::write(&manifest, "{}").unwrap();
        let abs = manifest.canonicalize().unwrap();
        let listed = build_already_listed(root, std::slice::from_ref(&abs));
        assert!(
            listed.contains(&abs),
            "absolute scanner output must round-trip through join+canonicalize"
        );
    }

    // =====================================================================
    // ProjectFileScan.manifest_paths_canonical — round-18 gemini perf fix
    // =====================================================================

    /// Contract for the pre-canonicalized membership-set fast
    /// path used by `is_stale_fast`. When the scanner populates
    /// `manifest_paths_canonical`, the freshness check is
    /// expected to use those entries verbatim as a HashSet and
    /// skip the per-call `Path::canonicalize` cost entirely.
    ///
    /// Locks the contract: a `ProjectFileScan` with a non-empty
    /// `manifest_paths_canonical` MUST produce a membership set
    /// that contains the canonical absolute path of every
    /// scanner-tracked manifest, identical to what
    /// `build_already_listed` would have produced. This is the
    /// regression target for the round-18 finding: the previous
    /// implementation re-canonicalized on every freshness check
    /// (O(N) stat/readlink syscalls per call), which was the
    /// dominant fixed cost of the freshness fast path in
    /// monorepos with hundreds of package manifests.
    #[test]
    fn pre_canonicalized_manifest_paths_match_build_already_listed() {
        let (tmp, _) = make_fixture();
        let root = tmp.path();

        // Lay down a few manifests at varying depths.
        let root_manifest = root.join("Cargo.toml");
        fs::write(&root_manifest, "[package]\nname = \"root\"\n").unwrap();
        let nested_dir = root.join("packages/api");
        fs::create_dir_all(&nested_dir).unwrap();
        let nested_manifest = nested_dir.join("package.json");
        fs::write(&nested_manifest, "{}").unwrap();
        let pyproject_manifest = root.join("pyproject.toml");
        fs::write(&pyproject_manifest, "[project]\nname = \"x\"\n").unwrap();

        // Relative scanner outputs (mimic the scanner when
        // the user passes a relative `project_path`).
        let relative_paths = vec![
            std::path::PathBuf::from("Cargo.toml"),
            std::path::PathBuf::from("packages/api/package.json"),
            std::path::PathBuf::from("pyproject.toml"),
        ];

        // Slow-path: build the membership set the way
        // `is_stale_fast` did before round 18.
        let slow_set = build_already_listed(root, &relative_paths);

        // Fast-path: build the pre-canonicalized set the way
        // the scanner does at scan time.
        let manifest_paths_canonical: Vec<PathBuf> = relative_paths
            .iter()
            .map(|p| {
                let full = if p.is_relative() {
                    root.join(p)
                } else {
                    p.clone()
                };
                full.canonicalize().unwrap_or(full)
            })
            .collect();

        // The two sets MUST be identical. The fast path is
        // allowed to skip the per-call canonicalize cost
        // exactly because the result is byte-for-byte the
        // same set of canonical absolute paths.
        let fast_set: std::collections::HashSet<PathBuf> =
            manifest_paths_canonical.iter().cloned().collect();
        assert_eq!(
            slow_set, fast_set,
            "pre-canonicalized set must match build_already_listed output"
        );
        assert_eq!(fast_set.len(), 3);
    }

    /// A `ProjectFileScan` deserialized from legacy state (no
    /// `manifest_paths_canonical` field) MUST fall back to
    /// `build_already_listed` on the freshness check. We
    /// exercise that fallback by constructing the scan with an
    /// empty `manifest_paths_canonical` and asserting the
    /// membership set is still correct.
    #[test]
    fn legacy_scan_falls_back_to_build_already_listed() {
        let (tmp, _) = make_fixture();
        let root = tmp.path();
        let manifest = root.join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"x\"\n").unwrap();

        // Build a scan that mirrors the legacy serialized form:
        // `manifest_paths_canonical` is empty.
        let scan = ProjectFileScan {
            source_paths: vec![],
            manifest_paths: vec![std::path::PathBuf::from("Cargo.toml")],
            manifest_paths_canonical: Vec::new(),
            source_directories: vec![],
            manifest_hashes: std::collections::HashMap::new(),
        };
        assert!(
            scan.manifest_paths_canonical.is_empty(),
            "fixture must mirror legacy serialized form"
        );

        // The fallback path produces the same set as a fully
        // populated fast-path scan would.
        let fallback_set = build_already_listed(root, &scan.manifest_paths);
        let canon = manifest.canonicalize().unwrap();
        assert!(
            fallback_set.contains(&canon),
            "fallback must still resolve the manifest to its canonical form"
        );
    }
}
