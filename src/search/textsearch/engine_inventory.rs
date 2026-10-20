//! File inventory for the text search: which files a root actually contains.
//!
//! Extracted from `engine.rs` by pure code motion (Large File Detection gate:
//! `engine.rs` crossed 2000 lines). Re-exported from `engine` so
//! `engine::list_files` / `engine::list_files_reporting` keep their paths.
//!
//! Two sources, in order of preference: `git ls-files` inside a worktree (so
//! the project's own ignore rules decide what exists, and a build directory is
//! never walked), else a plain walk that skips VCS metadata and package
//! caches. Both are budgeted: the walk takes a file limit and a deadline, and
//! reports whether it was cut short — a truncated listing must never be
//! mistaken for a tree with no files.

use std::path::Path;
use std::time::Instant;

/// Directories never worth searching, at any depth.
const NEVER_WALK: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".leindex",
    "node_modules",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
];

fn git_inventory(root: &Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut files: Vec<String> = output
        .stdout
        .split(|b| *b == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| std::str::from_utf8(raw).ok())
        .filter(|rel| !rel.split('/').any(|seg| seg == ".leindex" || seg == ".git"))
        // A tracked or untracked symlink can point anywhere — including
        // outside the project — and so can an INTERMEDIATE directory
        // component: `git ls-files --cached` still emits `dir/leak` after
        // `dir` is replaced by a symlink to /outside, and leaf-only
        // `symlink_metadata` follows `dir` and reports the far-side target
        // as a plain regular file. Canonicalize EVERY candidate and require
        // containment in the canonical root — that covers leaf symlinks,
        // intermediate symlinks, and vanished paths with one check.
        .filter(|rel| {
            let path = root.join(rel);
            std::fs::canonicalize(&path)
                .is_ok_and(|resolved| resolved.starts_with(&canon_root) && resolved.is_file())
        })
        .map(str::to_string)
        .collect();
    files.sort_unstable();
    files.dedup();
    Some(files)
}

/// Walk `root`, returning the files seen and whether the budget cut the walk
/// short. A truncated walk must be distinguishable from a tree with no files:
/// otherwise an expired deadline yields "zero candidates, nothing matched".
fn walk_inventory(root: &Path, limit: usize, deadline: Option<Instant>) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        if out.len() >= limit || deadline.is_some_and(|d| Instant::now() >= d) {
            // Directories are still queued (or the file budget is exhausted),
            // so there is unexplored tree left.
            return (out, true);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() {
                if !NEVER_WALK.contains(&name) {
                    stack.push((entry.path(), rel));
                }
            } else if kind.is_file() {
                out.push(rel);
            }
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return (out, true);
        }
    }
    out.sort_unstable();
    (out, false)
}

/// Files under `dir`, relative to it, sorted, forward slashes. Uses git's own
/// ignore rules inside a repository; elsewhere a plain walk that skips VCS
/// metadata and package caches.
pub fn list_files(dir: &Path, limit: usize, deadline: Option<Instant>) -> Vec<String> {
    list_files_reporting(dir, limit, deadline).0
}

/// [`list_files`], plus whether the listing is known to be incomplete.
///
/// The flag exists because a truncated listing is otherwise indistinguishable
/// from a tree that simply has no matching files: `walk_inventory` stops at the
/// deadline and returns what it saw, so a deadline-expired structural search
/// once reported zero candidates and `has_more: false` — a clean-looking
/// "nothing matched" for a scan that never looked (Codex P2). Callers that
/// report completeness (`search` in `structural`) must use this.
///
/// `git ls-files` does not accept the budget, so inside a worktree the
/// inventory is always complete; only the non-git walk can be cut short.
pub fn list_files_reporting(
    dir: &Path,
    limit: usize,
    deadline: Option<Instant>,
) -> (Vec<String>, bool) {
    let (mut files, mut incomplete) = match git_inventory(dir) {
        Some(files) => (files, false),
        None => walk_inventory(dir, limit, deadline),
    };
    if files.len() > limit {
        incomplete = true;
    }
    files.truncate(limit);
    (files, incomplete)
}
