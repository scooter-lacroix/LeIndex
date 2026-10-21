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

/// Why a file listing stopped short of the whole tree.
///
/// The cause is not cosmetic. A structural search reports `stopped_by_deadline`
/// to say "the time budget cut this scan, retry or raise it" — a claim that must
/// not be made on the strength of a page limit, which is a deliberate bound the
/// caller asked for. Collapsing both into one "incomplete" boolean makes a large
/// tree look time-starved, and (worse) lets a scan that already skipped files on
/// the deadline report `stopped_by_deadline: false` merely because the listing
/// itself was cut by the limit instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InventoryTruncation {
    /// A file budget was reached with tree still unlisted — either the
    /// `limit` that bounds the result, or the larger work budget that bounds
    /// the traversal (see `WORK_BUDGET_PER_ADMITTED_FILE`). The two are
    /// reported through one flag because they mean the same thing to every
    /// caller: files remain unexamined and this is not time pressure.
    pub limit: bool,
    /// The wall-clock deadline stopped the walk.
    pub deadline: bool,
}

impl InventoryTruncation {
    /// Either cause: the listing does not describe the whole tree.
    pub fn truncated(self) -> bool {
        self.limit || self.deadline
    }
}

/// How many files the walk may *see* per file it is allowed to return.
///
/// The result budget (`limit`) and the work budget must be separate counters.
/// Bounding the traversal by admitted files alone means a selective predicate
/// never reaches `limit`, so the walk reads every dirent in the tree: language
/// inference asks for 10_000 files with `deadline: None` and a scope predicate,
/// and structural search asks for a language-filtered listing with an optional
/// deadline that is `None` whenever the caller passes `timeout_ms: 0`. Both
/// would go from O(limit) work to O(tree) on every call.
///
/// A fixed multiple keeps the budget proportional to what the caller asked for
/// — the traversal still stops early on a tree whose matches sort late — while
/// leaving a selective predicate enough room to find its files. It is
/// deliberately generous: a scope filter that admits one file in a hundred
/// still completes, and the caller's deadline remains the real bound.
const WORK_BUDGET_PER_ADMITTED_FILE: usize = 64;

/// Walk `root`, returning the files `admit` accepts and why the walk stopped
/// short. A truncated walk must be distinguishable from a tree with no files:
/// otherwise an expired deadline yields "zero candidates, nothing matched".
///
/// `admit` runs as each file is seen, so `limit` bounds the files that MATCH
/// rather than the first `limit` paths in tree order: filtering afterwards puts
/// the matches behind an earlier wall of rejects out of reach entirely.
///
/// The traversal is separately bounded by `limit * WORK_BUDGET_PER_ADMITTED_FILE`
/// files SEEN — see that constant for why one counter cannot serve both.
fn walk_inventory(
    root: &Path,
    limit: usize,
    deadline: Option<Instant>,
    admit: impl Fn(&str) -> bool,
) -> (Vec<String>, InventoryTruncation) {
    let work_budget = limit.saturating_mul(WORK_BUDGET_PER_ADMITTED_FILE);
    let mut seen = 0usize;
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let expired = deadline.is_some_and(|d| Instant::now() >= d);
        let hit_limit = out.len() >= limit;
        let hit_work = seen >= work_budget;
        if hit_limit || hit_work || expired {
            // Files are still unlisted, so there is unexplored tree left. Sort
            // before returning: the walk yields in stack-traversal order, and
            // `list_files` promises a sorted listing on EVERY path, not just
            // the complete one.
            //
            // `hit_work` alone does not mean tree is left: an exhausted stack is
            // a COMPLETE walk, and `work_budget` can fire at zero files seen
            // (`limit == 0`). Reporting `limit: true` there would turn a clean
            // scan into a false partial — the same misreport, in the opposite
            // direction, that the post-batch deadline check below guards against.
            out.sort_unstable();
            return (
                out,
                InventoryTruncation {
                    limit: hit_limit || (hit_work && !stack.is_empty()),
                    deadline: expired,
                },
            );
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
                seen += 1;
                if admit(&rel) {
                    out.push(rel);
                }
            }
        }
        // A deadline that expires exactly as the queue empties is NOT a
        // truncation: every directory was walked, so `deadline` here would mark a
        // complete scan as partial. The check runs after the work, and the final
        // batch of entries can easily push elapsed time past a deadline that was
        // met — `stopped_by_deadline` is a claim about unexamined tree, so it may
        // only be raised when tree is actually left.
        if deadline.is_some_and(|d| Instant::now() >= d) && !stack.is_empty() {
            out.sort_unstable();
            return (
                out,
                InventoryTruncation {
                    limit: false,
                    deadline: true,
                },
            );
        }
    }
    out.sort_unstable();
    (out, InventoryTruncation::default())
}

/// Files under `dir`, relative to it, sorted, forward slashes. Uses git's own
/// ignore rules inside a repository; elsewhere a plain walk that skips VCS
/// metadata and package caches.
pub fn list_files(dir: &Path, limit: usize, deadline: Option<Instant>) -> Vec<String> {
    list_files_reporting(dir, limit, deadline).0
}

/// [`list_files`], plus why the listing stopped short.
///
/// The cause exists because a truncated listing is otherwise indistinguishable
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
) -> (Vec<String>, InventoryTruncation) {
    list_files_matching(dir, limit, deadline, |_| true)
}

/// [`list_files_reporting`], keeping only files `admit` allows, with `limit`
/// applied to the ADMITTED files.
///
/// Taking the predicate here rather than letting the caller filter afterwards is
/// the whole point: `git ls-files` and `walk_inventory` return everything, so
/// filtering a `limit`-truncated result silently substitutes "the first `limit`
/// files in the tree" for "the first `limit` matching files". Language inference
/// hit exactly that — it filtered `scope`/include/exclude out of the first
/// `LANGUAGE_INFERENCE_FILES` paths root-wide, so a scope whose files sort after
/// the cutoff looked empty, and the error told the caller to widen a scope that
/// was never examined. Applying the predicate before the cut makes `limit` mean
/// what every caller already assumes it means.
pub fn list_files_matching(
    dir: &Path,
    limit: usize,
    deadline: Option<Instant>,
    admit: impl Fn(&str) -> bool,
) -> (Vec<String>, InventoryTruncation) {
    // `git ls-files` already honors the project's ignore rules, so its listing
    // only needs the predicate; the walk applies it per file as it goes.
    //
    // The two branches are NOT symmetric on work. `git ls-files` returns the
    // whole listing in one shot, so the predicate is applied to all of it and
    // the work budget does not apply here — only `limit` bounds the result.
    // That is acceptable because `git ls-files` is a single fast subprocess
    // over an already-ignore-filtered set, not a dirent walk: the O(tree) cost
    // `WORK_BUDGET_PER_ADMITTED_FILE` exists to bound is the non-git walk's.
    // The walk is also the only branch that can cut short on a deadline.
    let (mut files, mut truncation) = match git_inventory(dir) {
        Some(mut files) => {
            files.retain(|rel| admit(rel));
            (files, InventoryTruncation::default())
        }
        None => walk_inventory(dir, limit, deadline, admit),
    };
    if files.len() > limit {
        truncation.limit = true;
    }
    files.truncate(limit);
    (files, truncation)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_list_files_matching_bounds_the_matching_files() {
        // The regression this guard exists for: `limit` must bound the files
        // that MATCH, not the files in the tree. Filtering a truncated listing
        // afterwards turns `limit` into "the first N paths in the whole tree",
        // so a scope whose files all sort past the cutoff reports nothing —
        // which is exactly how language inference came back empty for a scope
        // full of source, then told the caller to widen it.
        let dir = tempfile::tempdir().unwrap();
        // Alphabetically FIRST are files the filter rejects, so a post-hoc
        // filter over a 2-entry listing would leave nothing.
        for name in ["aaa.txt", "aab.txt", "zzz.rs", "zzz2.rs"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }

        let (matched, truncation) =
            list_files_matching(dir.path(), 2, None, |rel| rel.ends_with(".rs"));
        assert_eq!(
            matched,
            vec!["zzz.rs".to_string(), "zzz2.rs".to_string()],
            "the limit must be spent on admitted files, not consumed by rejects"
        );
        assert!(
            !truncation.truncated(),
            "both matching files fit, so nothing was cut short"
        );

        let (one, truncation) =
            list_files_matching(dir.path(), 1, None, |rel| rel.ends_with(".rs"));
        assert_eq!(one, vec!["zzz.rs".to_string()]);
        assert!(
            truncation.limit,
            "a second matching file exists, so the listing is incomplete"
        );
    }

    #[test]
    fn test_list_files_reporting_admits_everything() {
        // The no-predicate path must stay exactly what it was.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x").unwrap();
        assert_eq!(
            list_files_reporting(dir.path(), 10, None).0,
            vec!["a.rs".to_string(), "b.txt".to_string()]
        );
    }

    #[test]
    fn test_walk_stops_on_a_work_budget_a_selective_predicate_never_spends() {
        // The regression this guard exists for: bounding the walk by ADMITTED
        // files alone means a predicate that admits nothing never reaches
        // `limit`, so the walk reads every dirent in the tree. `infer_language`
        // asks for 10_000 files with `deadline: None` and a scope predicate, so
        // a narrow scope used to cost a full traversal per call.
        //
        // Three directories each larger than the work budget, so the cutoff
        // fires after the FIRST one regardless of the order `read_dir` returns
        // them: with two directories left on the stack the walk cannot mistake
        // this for a finished traversal.
        let dir = tempfile::tempdir().unwrap();
        let per_dir = WORK_BUDGET_PER_ADMITTED_FILE + 1;
        for d in ["d1", "d2", "d3"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
            for i in 0..per_dir {
                std::fs::write(dir.path().join(d).join(format!("f{i}.txt")), "x").unwrap();
            }
        }

        // `limit: 1` gives a work budget of `WORK_BUDGET_PER_ADMITTED_FILE`, and
        // this predicate admits nothing at all.
        let (matched, truncation) = list_files_matching(dir.path(), 1, None, |_| false);
        assert!(
            matched.is_empty(),
            "nothing is admitted, so nothing may be returned"
        );
        assert!(
            truncation.limit,
            "the work budget must cut the traversal short even though no file was admitted; \
             without it the walk reads the whole tree"
        );
        assert!(
            !truncation.deadline,
            "no deadline was given, so this is a file budget, not time pressure"
        );
    }
}
