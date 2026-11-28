//! Regression tests extracted from `engine.rs` to keep that file under the
//! 2000-line budget (the `_test.rs` sibling convention). Declared from
//! `engine.rs` as `mod engine_test` so they compile inside the engine module
//! and reuse the fixtures defined in `engine::tests`.

use super::tests::{paths, q, spec, write};
use super::*;

/// #092: a continuation page must not advertise has_more when the final
/// hit was delivered — reaching the page limit alone is not evidence that
/// more results exist.
#[test]
fn test_continuation_final_hit_does_not_advertise_an_empty_page() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.rs", "first needle\nsecond needle\n");
    let compiled = q("needle").compile().unwrap();
    let first = search(
        &[spec(dir.path(), None)],
        &compiled,
        &SearchOptions {
            offset: 0,
            limit: Some(1),
            collect_hits: true,
            ..Default::default()
        },
    );
    assert_eq!(first.returned, 1);
    assert!(first.has_more, "page 1 holds one of two hits");
    let key = "0:src/a.rs".to_string();
    let second = search(
        &[spec(dir.path(), None)],
        &compiled,
        &SearchOptions {
            offset: 0,
            limit: Some(1),
            collect_hits: true,
            skip_in_file: BTreeMap::from([(key, 1)]),
            ..Default::default()
        },
    );
    assert_eq!(second.returned, 1, "the final hit is delivered");
    assert!(!second.has_more, "no empty third page may be advertised");
    assert!(second.complete);
}

/// #091: window overflow is the next page's business — it must never be
/// reported as per-file-cap withholding, which means something different.
#[test]
fn test_window_overflow_is_not_reported_as_per_file_cap_withholding() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.rs", "n1 needle\nn2 needle\nn3 needle\n");
    write(dir.path(), "src/b.rs", "n4 needle\n");
    let compiled = q("needle").compile().unwrap();
    let out = search(
        &[spec(dir.path(), None)],
        &compiled,
        &SearchOptions {
            offset: 0,
            limit: Some(1),
            collect_hits: true,
            per_file_cap: 1,
            ..Default::default()
        },
    );
    assert!(out.has_more);
    assert_eq!(
        out.cap_withheld, 0,
        "window overflow must not inflate cap_withheld"
    );
    let capped = search(
        &[spec(dir.path(), None)],
        &compiled,
        &SearchOptions {
            offset: 0,
            limit: None,
            collect_hits: true,
            per_file_cap: 1,
            ..Default::default()
        },
    );
    assert!(capped.has_more);
    assert_eq!(
        capped.cap_withheld, 2,
        "two a.rs hits sit past the per-file cap"
    );
}

/// #090: same-named methods in separate impl blocks keep distinct symbol
/// ranges, and each range resolves to the correct 1-based line (#093).
#[test]
fn test_same_named_methods_keep_distinct_symbol_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let src = "impl One {\n    fn run() {}\n}\nimpl Two {\n    fn run() {}\n}\n";
    write(dir.path(), "src/lib.rs", src);
    let first = src.find("fn run").unwrap() as u32;
    let second = first as usize + src[first as usize + 1..].find("fn run").unwrap() + 1;
    let symbols = HashMap::from([(
        "src/lib.rs".to_string(),
        vec![
            SymbolSpan {
                start: first,
                end: first + 6,
                kind: 0,
                name: "run".into(),
            },
            SymbolSpan {
                start: second as u32,
                end: second as u32 + 6,
                kind: 0,
                name: "run".into(),
            },
        ],
    )]);
    let out = dir.path().join(".leindex/textindex/index.bin");
    build_index(dir.path(), &out, symbols).unwrap();
    let index = Arc::new(TextIndex::open(&out).unwrap());
    let compiled = q("run").compile().unwrap();
    let result = search(
        &[spec(dir.path(), Some(index))],
        &compiled,
        &SearchOptions {
            want_symbols: true,
            ..Default::default()
        },
    );
    let file = &result.roots[0].files[0];
    assert_eq!(
        file.symbol_ranges.len(),
        2,
        "each impl block keeps its own range"
    );
    let names: Vec<&str> = file
        .symbol_ranges
        .iter()
        .map(|(n, _, _)| n.as_str())
        .collect();
    assert_eq!(names, ["run", "run"]);
    let line_of = |at: usize| {
        1 + src[..at.min(src.len())]
            .bytes()
            .filter(|b| *b == b'\n')
            .count() as u32
    };
    let starts: Vec<u32> = file.symbol_ranges.iter().map(|(_, s, _)| *s).collect();
    assert_eq!(starts, [line_of(first as usize), line_of(second)]);
    assert_eq!(
        file.symbol_ranges[0].2,
        line_of((first as usize + 5).min(src.len()))
    );
}

/// #010: an explicit directory root searches the raw filesystem (git
/// ignores bypassed), and the caller's glob filters still apply to it.
#[test]
fn test_explicit_directory_root_searches_raw_inventory_and_honors_globs() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), ".gitignore", "hidden.txt\n");
    write(dir.path(), "hidden.txt", "needle hidden\n");
    write(dir.path(), "visible.txt", "needle visible\n");
    // A real Git worktree, so the default inventory consults git ignore rules.
    git_worktree(dir.path());

    let listed = list_files(dir.path(), 1000, None);
    assert!(
        !listed.contains(&"hidden.txt".to_string()),
        "default inventory omits git-ignored files"
    );
    let compiled = q("needle").compile().unwrap();
    let raw = search(
        &[RootSpec {
            root: dir.path().to_path_buf(),
            index: None,
            filter: FileFilter::default(),
            bypass_ignore: true,
        }],
        &compiled,
        &SearchOptions::default(),
    );
    let raw_paths = paths(&raw);
    assert!(
        raw_paths.contains(&"hidden.txt".to_string()),
        "raw inventory sees ignored files"
    );
    assert!(raw_paths.contains(&"visible.txt".to_string()));
    let filtered = search(
        &[RootSpec {
            root: dir.path().to_path_buf(),
            index: None,
            filter: FileFilter::new(&[], &["hidden.txt".to_string()], None),
            bypass_ignore: true,
        }],
        &compiled,
        &SearchOptions::default(),
    );
    let filtered_paths = paths(&filtered);
    assert!(
        !filtered_paths.contains(&"hidden.txt".to_string()),
        "caller globs still apply to raw roots"
    );
}

/// Turn `dir` into a committed Git worktree so the default inventory consults
/// real ignore rules. Panics (with context) when git is unavailable: every
/// #010 assertion depends on the ignore rules actually being in force.
fn git_worktree(dir: &std::path::Path) {
    let run = |args: &[&str], what: &str| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .is_ok_and(|status| status.success());
        assert!(
            ok,
            "`git {what}` must succeed for the worktree inventory test"
        );
    };
    run(&["init", "-q"], "init");
    run(
        &["config", "user.email", "test@example.com"],
        "config user.email",
    );
    run(&["config", "user.name", "test"], "config user.name");
    run(&["add", "-A"], "add");
    run(&["commit", "-qm", "init"], "commit");
}

/// #010: an explicitly named directory root must be inventoried RAW. A file
/// Git ignores is still returned when its directory is named as an explicit
/// root — `list_files_reporting` (git-aware) omits it, `list_files_raw`
/// (plain walk) must not. Both halves are asserted so the contrast, not just
/// the raw behaviour, is what the test pins.
#[test]
fn test_list_files_raw_returns_git_ignored_files_that_reporting_omits() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), ".gitignore", "hidden.txt\n");
    write(dir.path(), "hidden.txt", "needle hidden\n");
    write(dir.path(), "visible.txt", "needle visible\n");
    git_worktree(dir.path());

    let reported = list_files_reporting(dir.path(), 1000, None).0;
    assert!(
        !reported.contains(&"hidden.txt".to_string()),
        "the git-aware inventory omits the ignored file — the premise of this test: {reported:?}"
    );
    assert!(reported.contains(&"visible.txt".to_string()));

    let raw = list_files_raw(dir.path(), 1000, None);
    assert!(
        raw.contains(&"hidden.txt".to_string()),
        "the raw inventory must return a git-ignored file when its directory is the explicit root: {raw:?}"
    );
    assert!(raw.contains(&"visible.txt".to_string()));
    // The raw walk still skips VCS metadata: `.git` must never surface as a file.
    assert!(
        !raw.iter().any(|p| p == ".git" || p.starts_with(".git/")),
        "raw walk must still skip VCS metadata: {raw:?}"
    );
    // And the ignore FILE itself is an ordinary file on disk — present in the
    // raw listing, which is exactly why an ignore-aware caller must not use it.
    assert!(raw.contains(&".gitignore".to_string()));
}
