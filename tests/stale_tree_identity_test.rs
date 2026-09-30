//! A commit that moves HEAD without changing any indexed file must not leave
//! the index flagged stale.
//!
//! Regression: the "No changes detected, skipping indexing" path marked health
//! `Fresh` but copied the previous `tree_oid`, so `is_stale_fast` saw tree
//! drift on every check. The registry then re-ran a background incremental scan
//! (~140 ms of CPU) after *every* tool call for as long as the session lived.

#![cfg(feature = "cli")]

use leindex::cli::leindex::LeIndex;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn test_commit_of_unindexed_file_does_not_leave_index_stale() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(
        root.join("lib.rs"),
        "pub fn alpha(x: u32) -> u32 {\n    x + 1\n}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);

    let mut index = LeIndex::new(root).unwrap();
    index.index_project(true).unwrap();
    assert!(
        !index.is_stale_fast(),
        "a freshly indexed, committed tree must not be stale"
    );

    // HEAD moves; no indexed source changes.
    std::fs::write(root.join("payload.dat"), [0u8, 1, 2, 3]).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "binary payload only"]);
    assert!(
        index.is_stale_fast(),
        "precondition: the moved HEAD tree is reported as drift until a scan records it"
    );

    // The scan proves the content is unchanged and records the new tree.
    index.index_project(false).unwrap();
    assert!(
        !index.is_stale_fast(),
        "index must be fresh after a no-op refresh of a moved HEAD"
    );
}

#[test]
fn test_real_source_change_is_still_detected_as_stale() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);

    let mut index = LeIndex::new(root).unwrap();
    index.index_project(true).unwrap();

    std::fs::write(root.join("lib.rs"), "pub fn alpha() {}\npub fn beta() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "add beta"]);

    assert!(
        index.is_stale_fast(),
        "a changed indexed source file must still be reported stale"
    );
}

#[test]
fn test_new_unindexed_file_in_source_directory_is_acknowledged_by_a_clean_scan() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);

    let mut index = LeIndex::new(root).unwrap();
    index.index_project(true).unwrap();

    // A file nothing indexes (and git ignores nothing): only the directory
    // mtime sentinel notices it.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(root.join("scratch.dat"), [9u8]).unwrap();
    assert!(
        index.is_stale_fast(),
        "precondition: a changed source directory is reported until a scan records it"
    );

    index.index_project(false).unwrap();
    assert!(
        !index.is_stale_fast(),
        "a scan that finds nothing to index must acknowledge the directory change"
    );
}

#[test]
fn test_fixture_manifests_do_not_keep_the_index_stale() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    // The scanner excludes tests/fixtures/**; the freshness check must agree.
    std::fs::create_dir_all(root.join("tests/fixtures/app")).unwrap();
    std::fs::write(root.join("tests/fixtures/app/Cargo.toml"), "[package]\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);

    let mut index = LeIndex::new(root).unwrap();
    index.index_project(true).unwrap();
    assert!(
        !index.is_stale_fast(),
        "a fixture manifest the scanner ignores must not make the index stale"
    );
}
