//! File inventory for the text search: which files a root actually contains.
//!
//! Extracted from `engine.rs` by pure code motion (Large File Detection gate:
//! `engine.rs` crossed 2000 lines). Re-exported from `engine` so
//! `engine::list_files` / `engine::list_files_reporting` keep their paths.
//!
//! Two sources, in order of preference: `git ls-files` inside a worktree (so
//! the project's own ignore rules decide what exists, and a build directory is
//! never walked), else a plain walk that skips VCS metadata and package
//! caches. Both are budgeted — the walk takes a file limit and a deadline, and
//! either branch can report that it stopped short. A truncated listing must
//! never be mistaken for a tree with no files, so every way a listing can come
//! back incomplete — the deadline, a file budget, or a subtree the OS refused
//! to open — is reported through `InventoryTruncation` rather than swallowed.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

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

/// First interval between liveness checks on a child process.
///
/// The interval BACKS OFF to `POLL_MAX` rather than being fixed at 5 ms. A fixed
/// interval ran the sleep even when the child had already exited — a fast
/// `git ls-files` in a small worktree paid 5 ms on every inventory call — and
/// noticed an expiry up to a full interval late. A short first tick keeps the
/// fast path cheap; the backoff keeps a long listing from spinning.
const POLL_MIN: Duration = Duration::from_millis(1);

/// Ceiling the liveness-check interval backs off to.
const POLL_MAX: Duration = Duration::from_millis(20);

/// Entries a directory walk or git listing may pass between deadline checks.
///
/// `Instant::now()` is cheap but not free, and these loops run once per file —
/// tens of millions of times for a structural search. Checking EVERY entry would
/// cost more than the work it bounds. Checking once per directory, which is what
/// the walk did, let one directory holding 100k files run to exhaustion long
/// after the budget expired and then report the cut as `limit: true`, telling the
/// caller to raise a limit that was never the problem.
const DEADLINE_CHECK_STRIDE: usize = 64;

/// Run `command` to completion capturing stdout, or kill it once `deadline`
/// expires. `None` when it was killed, could not be waited on, or failed.
///
/// This is the part of `git_inventory` that can hang, so it lives here where a
/// unit test can drive it with any child process instead of needing a git
/// worktree slow enough to overrun.
///
/// stdout is drained on a reader thread rather than left in a pipe nobody reads:
/// output larger than the pipe buffer would fill it, block the child, and make
/// the expiry check below unreachable. The thread OUTLIVES the kill — abandoned,
/// never joined — because a process killed mid-write leaves its reader waiting
/// for an EOF that never comes, and joining would reintroduce exactly the hang
/// this function exists to prevent. It owns nothing but the pipe and a buffer.
fn run_captured(mut command: std::process::Command, deadline: Option<Instant>) -> Option<Vec<u8>> {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        // Never leave a piped stderr unread: a chatty child must not block on it
        // before the expiry check can reap it.
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut raw = Vec::new();
        let _ = pipe.read_to_end(&mut raw);
        raw
    });
    let mut poll = POLL_MIN;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(poll);
        poll = (poll * 2).min(POLL_MAX);
    };
    if !status.is_some_and(|status| status.success()) {
        return None;
    }
    reader.join().ok()
}

/// `git ls-files`, or `None` when this is not a worktree, when the listing
/// could not be read, or when `deadline` expired first.
///
/// The deadline is enforced HERE, and not only around the walk: `git ls-files`
/// over a large worktree takes real time, and `Command::output()` blocks for all
/// of it. A structural search with `timeout_ms: 50` and an explicit `lang` —
/// which skips language inference and so reaches this call immediately — would
/// spend seconds past its budget before the first file was ever scanned. `None`
/// from an expired deadline routes the caller to `walk_inventory`, which sees
/// the same expired deadline and reports it, so the result is a correctly
/// labelled partial listing rather than a silent overrun.
///
/// Killing the subprocess is NOT enough, which is why this returns `Partial`
/// rather than `Option`: canonicalizing every candidate and applying `admit` to
/// the whole listing is also unbounded work, and a worktree with a large listing
/// blows the same budget there. An expiry during post-processing yields the
/// candidates resolved so far WITH `deadline: true` — never a clean default,
/// which would report a scan that ran long past its budget as complete.
fn sorted_unique(mut files: Vec<String>) -> Vec<String> {
    files.sort_unstable();
    files.dedup();
    files
}

fn git_inventory(
    root: &Path,
    deadline: Option<Instant>,
    admit: &impl Fn(&str) -> bool,
) -> GitInventory {
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return GitInventory::Absent;
    }
    let mut command = std::process::Command::new("git");
    command
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root);
    let Some(raw) = run_captured(command, deadline) else {
        return GitInventory::Absent;
    };
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut files: Vec<String> = Vec::new();
    let mut truncation = InventoryTruncation::default();
    for (index, entry) in raw
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .enumerate()
    {
        if index % DEADLINE_CHECK_STRIDE == 0 && deadline.is_some_and(|d| Instant::now() >= d) {
            truncation.deadline = true;
            return GitInventory::Present(sorted_unique(files), truncation);
        }
        let Ok(rel) = std::str::from_utf8(entry) else {
            truncation.skip = true;
            continue;
        };
        if rel.split('/').any(|seg| seg == ".leindex" || seg == ".git") {
            continue;
        }
        // A tracked or untracked symlink can point anywhere — including
        // outside the project — and so can an INTERMEDIATE directory
        // component: `git ls-files --cached` still emits `dir/leak` after
        // `dir` is replaced by a symlink to /outside, and leaf-only
        // `symlink_metadata` follows `dir` and reports the far-side target
        // as a plain regular file. Canonicalize EVERY candidate and require
        // containment in the canonical root — that covers leaf symlinks,
        // intermediate symlinks, and vanished paths with one check.
        let path = root.join(rel);
        if !std::fs::canonicalize(&path)
            .is_ok_and(|resolved| resolved.starts_with(&canon_root) && resolved.is_file())
        {
            continue;
        }
        if admit(rel) {
            files.push(rel.to_string());
        }
    }
    GitInventory::Present(sorted_unique(files), truncation)
}

/// Result of asking git for a listing.
///
/// `Absent` is not "the tree is empty" — it means git did not answer (no
/// worktree, no git, or the deadline expired), so the caller must walk instead.
#[derive(Debug)]
enum GitInventory {
    /// Not a worktree, git missing, or reaped on the deadline.
    Absent,
    /// The listing was resolved from git, with any truncation (deadline, skip) recorded.
    Present(Vec<String>, InventoryTruncation),
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
    /// A subtree was dropped because the OS would not let us read it — `read_dir`
    /// denied, a `file_type()` failure, a non-UTF-8 name. These files were never
    /// candidates and never will be, so this is not time pressure and not a
    /// page limit; but it does mean the listing does not describe the whole
    /// tree, and claiming otherwise is the `has_more: false` misreport this
    /// type exists to prevent.
    pub skip: bool,
}

impl InventoryTruncation {
    /// Either cause: the listing does not describe the whole tree.
    pub fn truncated(self) -> bool {
        self.limit || self.deadline || self.skip
    }
}

/// How many files the walk may *see* per file it is allowed to return.
///
/// The result budget (`limit`) and the work budget must be separate counters.
/// Bounding the traversal by admitted files alone means a selective predicate
/// never reaches `limit`, so the walk reads every dirent in the tree. Language
/// inference is exactly that shape: it asks for 10_000 files with
/// `deadline: None` and a scope predicate, so a narrow scope went from O(limit)
/// work to O(tree) on every call.
///
/// What this bounds, precisely, so nobody budgets against a promise the code
/// does not keep: only FILES it walks, applied inside the entry loop. It binds
/// language inference (10_000 x 64 = 640k files, reachable) and does NOT bind
/// structural search (300_000 x 64 = 19.2M, which no repository reaches) — that
/// call is *meant* to enumerate the whole tree, since a match can be anywhere
/// in it. Directories are never counted, so a tree that is mostly directories costs
/// nothing against the budget; directory traversal has no depth limit, though
/// `NEVER_WALK` skips common metadata and package caches (and symlinked
/// directories are not followed). Inside a git worktree the budget does not
/// apply at all — see `list_files_matching`.
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
    let mut truncation = InventoryTruncation::default();
    // Finish the partial listing the same way the complete one is finished: the
    // walk yields in stack-traversal order, and `list_files` promises a sorted
    // listing on EVERY path, not just the complete one.
    fn finish(
        mut out: Vec<String>,
        truncation: InventoryTruncation,
    ) -> (Vec<String>, InventoryTruncation) {
        out.sort_unstable();
        (out, truncation)
    }
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            truncation.deadline = true;
            return finish(out, truncation);
        }
        // A subtree the OS will not open is unexamined tree, not an empty one.
        // Dropping it silently is how a listing that never looked at `src/`
        // reported `has_more: false`.
        let Ok(entries) = std::fs::read_dir(&dir) else {
            truncation.skip = true;
            continue;
        };
        let mut since_check = 0usize;
        for entry in entries {
            let Ok(entry) = entry else {
                truncation.skip = true;
                continue;
            };
            // Polled per STRIDE entries rather than once per directory: one
            // directory holding 100k files used to run to exhaustion long after
            // the budget expired. `deadline: true`, not `limit: true` — the
            // limit was never the reason the scan stopped, and blaming it sends
            // the caller to raise a bound that is not the constraint.
            since_check += 1;
            if since_check >= DEADLINE_CHECK_STRIDE {
                since_check = 0;
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    truncation.deadline = true;
                    return finish(out, truncation);
                }
            }
            let Ok(kind) = entry.file_type() else {
                truncation.skip = true;
                continue;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                truncation.skip = true;
                continue;
            };
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if kind.is_dir() {
                if !NEVER_WALK.contains(&name) {
                    stack.push((entry.path(), rel));
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            // The work budget is tested PER FILE, not once per directory. Tested
            // between directories it was an advisory cap: a single directory
            // holding a million files overshot a budget of 64 by a million, so
            // "bounded by `limit * WORK_BUDGET_PER_ADMITTED_FILE` files SEEN"
            // was never true. Here it is the ceiling that sentence promises, and
            // `seen` only ever equals the budget on return from this branch — so
            // tree is unexamined by construction and `limit: true` is never a
            // false partial, for a queued directory or an unread remainder of
            // this one alike.
            if seen >= work_budget {
                truncation.limit = true;
                return finish(out, truncation);
            }
            seen += 1;
            if out.len() >= limit {
                truncation.limit = true;
                return finish(out, truncation);
            }
            if admit(&rel) {
                out.push(rel);
            }
        }
    }
    finish(out, truncation)
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
/// `git ls-files` does not accept the file budget, so inside a worktree the
/// listing is bounded by `limit` alone. Both branches honour `deadline` at every
/// stage that can run long: the subprocess is killed when it overruns, post-
/// processing stops when it expires, and the walk then reports the truncation.
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
    // The two branches are NOT symmetric, and the asymmetry is observable, so
    // do not "fix" it: inside a worktree the listing is git's, sorted and
    // truncated, so a `limit` cut keeps the alphabetically-first matching
    // files; outside one the walk stops admitting in `read_dir` order, so it
    // keeps an arbitrary subset. Both honour `limit` (it means "this many
    // matching files" either way), neither is a corruption, and the difference
    // is inherent to ls-files being a single sorted subprocess versus the walk
    // being a streaming traversal. Sorting the walk's whole output before
    // truncating would mean reading the entire tree to serve a `limit` of ten.
    //
    // Nor are they symmetric on WORK. `git ls-files` returns the whole listing in
    // one shot, so the work budget does not apply there — only `limit` bounds
    // the result — which is acceptable because the O(tree) cost
    // `WORK_BUDGET_PER_ADMITTED_FILE` exists to bound is the non-git walk's.
    //
    // Both branches honour the deadline, and both report it: the subprocess is
    // killed when it overruns, post-processing stops when it expires, and an
    // `Absent` result sends the walk through with the same already-expired
    // deadline so the truncation is reported instead of the overrun hidden.
    let (mut files, mut truncation) = match git_inventory(dir, deadline, &admit) {
        GitInventory::Present(files, trunc) => (files, trunc),
        GitInventory::Absent => walk_inventory(dir, limit, deadline, admit),
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

    /// A command that exits immediately, so `run_captured` has something cheap
    /// to drive that is not git.
    fn exits_now() -> std::process::Command {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf 'a\\0b\\0'"]);
        command
    }

    #[test]
    fn test_run_captured_reads_a_command_that_exits() {
        let out = run_captured(exits_now(), None).expect("a quick command must be captured");
        assert_eq!(out, b"a\0b\0");
    }

    /// The kill path — the riskiest code in this module and, until this test, the
    /// only part of it nothing exercised.
    ///
    /// `run_captured` is a separate function precisely so this can be tested
    /// with a process that overruns on purpose instead of needing a git worktree
    /// slow enough to miss a deadline. `sleep 30` against a 0 ms deadline proves
    /// the child is killed rather than waited on: without the kill this test
    /// would take 30 seconds, and with the join instead of an abandoned reader it
    /// would hang forever.
    #[test]
    fn test_run_captured_kills_a_child_that_overruns_the_deadline() {
        let started = Instant::now();
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        let out = run_captured(command, Some(Instant::now()));
        assert!(out.is_none(), "an overrunning child must report no output");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the child must be KILLED, not waited out; took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn test_run_captured_reports_a_failing_command_as_none() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 3"]);
        assert!(
            run_captured(command, None).is_none(),
            "a non-zero exit is not a listing"
        );
    }

    /// A subtree the OS refuses to open is unexamined tree. Reporting the
    /// listing as complete once it has been dropped is the `has_more: false`
    /// misreport `InventoryTruncation` exists to prevent.
    ///
    /// The root must hold no admitted file of its own, or the walk would return
    /// on a budget path before reaching the directory it cannot read.
    #[test]
    fn test_walk_reports_an_unreadable_subtree_as_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("inner.rs"), "fn inner() {}").unwrap();
        // chmod 0 is the only way to make `read_dir` fail as a non-root user;
        // root ignores the mode, so the assertion is skipped there rather than
        // asserting a skip that cannot happen.
        let mut perms = std::fs::metadata(&locked).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o000);
        std::fs::set_permissions(&locked, perms).unwrap();
        let unreadable = std::fs::read_dir(&locked).is_err();

        let (out, truncation) = walk_inventory(dir.path(), 100, None, |rel| rel.ends_with(".rs"));

        let mut restore = std::fs::metadata(&locked).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut restore, 0o755);
        std::fs::set_permissions(&locked, restore).unwrap();

        if !unreadable {
            return; // running as root: the mode does not apply.
        }
        assert!(out.is_empty(), "the only file is inside the locked subtree");
        assert!(
            truncation.skip,
            "a dropped subtree must be reported, not silently swallowed"
        );
        assert!(truncation.truncated());
        assert!(
            !truncation.deadline,
            "the OS refused the directory; the clock did not stop it"
        );
    }

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
        // WHICH matching file survives is not specified and must not be pinned:
        // the walk admits as it goes, so the first one it sees wins, and
        // `read_dir` order is filesystem-dependent. (Asserting `zzz.rs` here was
        // an artifact of the walk once admitting everything and truncating a
        // sorted list afterwards — the very behaviour that made `limit` mean
        // "first N paths" instead of "first N matches".) The contract is that
        // the ONE file returned is a match and that the cut is reported.
        assert_eq!(one.len(), 1, "the limit bounds the files returned");
        assert!(
            one[0].ends_with(".rs"),
            "the one file returned must be an admitted match, got {:?}",
            one
        );
        assert!(
            truncation.limit,
            "a second matching file exists, so the listing is incomplete"
        );
    }

    #[test]
    fn test_list_files_reporting_admits_everything() {
        // Pin walk via direct walk_inventory so this test is isolated from
        // whether TMPDIR sits inside a git checkout.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x").unwrap();
        let (files, truncation) = walk_inventory(dir.path(), 10, None, |_| true);
        assert_eq!(files, vec!["a.rs".to_string(), "b.txt".to_string()]);
        assert!(!truncation.truncated());
    }

    #[test]
    fn test_walk_stops_on_a_work_budget_a_selective_predicate_never_spends() {
        // The regression this guard exists for: bounding the walk by ADMITTED
        // files alone means a predicate that admits nothing never reaches
        // `limit`, so the walk reads every dirent in the tree. `infer_language`
        // asks for 10_000 files with `deadline: None` and a scope predicate, so
        // a narrow scope used to cost a full traversal per call.
        //
        // `walk_inventory` is called DIRECTLY rather than through
        // `list_files_matching`: the latter tries `git ls-files` first and only
        // falls back to the walk outside a worktree, and `tempfile::tempdir()`
        // honours TMPDIR — so on a machine whose TMPDIR sits inside a checkout
        // the assertion below would be made against the git branch and fail,
        // while the budget path under test was never entered.
        //
        // ONE flat directory, deliberately: the budget is spent inside the
        // entry loop now, so a single directory larger than the budget proves
        // it binds. A budget tested once per directory passes this test too,
        // which is why the count of examined files is asserted exactly.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..WORK_BUDGET_PER_ADMITTED_FILE + 1 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }

        // `limit: 1` gives a work budget of `WORK_BUDGET_PER_ADMITTED_FILE`, and
        // this predicate admits nothing at all.
        let examined = std::cell::Cell::new(0usize);
        let (matched, truncation) = walk_inventory(dir.path(), 1, None, |_| {
            examined.set(examined.get() + 1);
            false
        });
        assert!(
            matched.is_empty(),
            "nothing is admitted, so nothing may be returned"
        );
        assert_eq!(
            examined.get(),
            WORK_BUDGET_PER_ADMITTED_FILE,
            "the walk must examine exactly the work budget, not one directory's worth: \
             without it the walk reads the whole tree"
        );
        assert!(
            truncation.limit,
            "the work budget cut the traversal, so files remain unlisted"
        );
        assert!(
            !truncation.deadline,
            "no deadline was given, so this is a file budget, not time pressure"
        );
    }

    #[test]
    fn test_walk_stops_once_the_result_budget_is_admitted() {
        // `limit` is the RESULT budget: it bounds admitted files. Which file is
        // admitted first depends on `read_dir` order, so only the count is
        // asserted — the returned list is sorted on the way out regardless.
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let (matched, truncation) = walk_inventory(dir.path(), 2, None, |_| true);
        assert_eq!(matched.len(), 2, "the limit bounds the files returned");
        assert!(
            truncation.limit,
            "a third file exists, so the listing is incomplete"
        );
        assert!(!truncation.deadline);
    }

    #[test]
    fn test_walk_of_exactly_the_admitted_budget_is_complete() {
        // The opposite misreport, and the one the old `!stack.is_empty()` guard
        // was reaching for: a walk that admits exactly `limit` files from a tree
        // that holds exactly that many must NOT claim to be cut short. Nothing
        // is left unexamined, so `has_more` must stay false.
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.rs", "b.rs"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let (matched, truncation) = walk_inventory(dir.path(), 2, None, |_| true);
        assert_eq!(matched, vec!["a.rs".to_string(), "b.rs".to_string()]);
        assert!(
            !truncation.truncated(),
            "every file in the tree was admitted, so nothing was cut short"
        );
    }

    #[test]
    fn test_walk_with_a_zero_limit_reports_truncation() {
        // `limit: 0` makes the work budget zero, so the first file seen stops
        // the walk. Reporting completion here would hide a whole tree.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("only.rs"), "x").unwrap();
        let (matched, truncation) = walk_inventory(dir.path(), 0, None, |_| true);
        assert!(matched.is_empty(), "a zero limit admits nothing");
        assert!(truncation.limit, "the tree was never listed");
    }

    #[test]
    fn test_sorted_unique_deduplicates_and_sorts() {
        let input = vec![
            "b.rs".to_string(),
            "a.rs".to_string(),
            "b.rs".to_string(),
            "a.rs".to_string(),
        ];
        assert_eq!(
            sorted_unique(input),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_git_inventory_non_utf8_sets_skip() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let bad_name = std::ffi::OsStr::from_bytes(b"bad\xffname.txt");
        std::fs::write(dir.path().join(bad_name), "x").unwrap();
        std::fs::write(dir.path().join("good.txt"), "x").unwrap();

        let (files, truncation) = list_files_matching(dir.path(), 100, None, |_| true);
        assert_eq!(files, vec!["good.txt".to_string()]);
        assert!(
            truncation.skip,
            "non-UTF-8 git entry must record truncation.skip, not claim clean completion"
        );
        assert!(truncation.truncated());
        assert!(!truncation.deadline);
    }

    #[test]
    fn test_git_inventory_deterministic_repo() {
        let dir = tempfile::tempdir().unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        std::fs::write(dir.path().join("a.rs"), "x").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x").unwrap();
        std::fs::write(dir.path().join("c.rs"), "x").unwrap();

        let (matched, truncation) =
            list_files_matching(dir.path(), 10, None, |p| p.ends_with(".rs"));
        assert_eq!(matched, vec!["a.rs".to_string(), "c.rs".to_string()]);
        assert!(!truncation.truncated());

        let (limited, truncation) =
            list_files_matching(dir.path(), 1, None, |p| p.ends_with(".rs"));
        assert_eq!(limited.len(), 1);
        assert!(limited[0].ends_with(".rs"));
        assert!(truncation.limit);
    }
}
