#![cfg(feature = "cli")]

use leindex::cli::git::{
    changed_source_candidates, parse_status, source_candidates, source_inventory, tree_oid,
};
use std::path::PathBuf;

#[test]
fn test_parses_nul_delimited_porcelain_v2_records_without_splitting_paths() {
    let output = b"# branch.oid 0123456789abcdef\0# branch.head feature/perf\0\
1 .M N... 100644 100644 100644 abc def src/lib.rs\0\
1 M. N... 100644 100644 100644 abc def src/main.rs\0\
2 R. N... 100644 100644 100644 abc def R100 src/new\tname.rs\0src/old name.rs\0\
u UU N... 100644 100644 100644 100644 abc def ghi src/conflict.rs\0\
1 .M S.M. 160000 160000 160000 abc def vendor/engine\0\
? notes.txt\0! ignored file\0";

    let status = parse_status(output);

    assert_eq!(
        status.modified,
        vec![PathBuf::from("src/lib.rs"), PathBuf::from("vendor/engine")]
    );
    assert_eq!(
        status.staged,
        vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/new\tname.rs")
        ]
    );
    assert_eq!(status.untracked, vec![PathBuf::from("notes.txt")]);
    assert_eq!(status.conflicted, vec![PathBuf::from("src/conflict.rs")]);
    assert_eq!(status.ignored, vec![PathBuf::from("ignored file")]);
    assert_eq!(status.submodules[0].path, PathBuf::from("vendor/engine"));
    assert_eq!(status.renames[0].from, PathBuf::from("src/old name.rs"));
    assert_eq!(status.renames[0].to, PathBuf::from("src/new\tname.rs"));
    assert_eq!(status.branch.as_deref(), Some("feature/perf"));
    assert_eq!(status.head_oid.as_deref(), Some("0123456789abcdef"));
}

#[test]
fn test_initial_or_detached_headers_have_no_branch_or_head_oid() {
    let status = parse_status(b"# branch.oid (initial)\0# branch.head (detached)\0? a b\0");

    assert_eq!(status.branch, None);
    assert_eq!(status.head_oid, None);
    assert_eq!(status.untracked, vec![PathBuf::from("a b")]);
}

#[test]
fn test_inventory_uses_git_ignore_and_nested_repo_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let run = |args: &[&str], cwd: &std::path::Path| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {:?}: {:?}", args, output);
    };
    run(&["init", "-q"], root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join(".leindex/jobs/1")).unwrap();
    std::fs::create_dir_all(root.join("scratch/nested-repo")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
    std::fs::write(root.join("target/generated.rs"), "fn ignored() {}\n").unwrap();
    std::fs::write(root.join(".leindex/jobs/1/state.json"), "{}\n").unwrap();
    std::fs::write(root.join("scratch/nested-repo/main.rs"), "fn nested() {}\n").unwrap();
    std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
    run(&["add", "."], root);
    run(
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-qm",
            "fixture",
        ],
        root,
    );
    run(&["init", "-q"], &root.join("scratch/nested-repo"));
    std::fs::write(root.join("src/new.rs"), "pub fn new_source() {}\n").unwrap();

    let paths = source_inventory(root).unwrap();
    assert!(paths.contains(&root.join("src/lib.rs")));
    assert!(paths.contains(&root.join("src/new.rs")));
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(root.join("target")))
    );
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(root.join(".leindex")))
    );
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(root.join("scratch/nested-repo")))
    );
}

#[test]
fn test_inventory_from_subdirectory_does_not_treat_outer_worktree_as_nested_repo() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let project = root.join("projects/app");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {:?}: {:?}", args, output);
    };
    run(&["init", "-q"]);
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=test",
        "commit",
        "-qm",
        "fixture",
    ]);

    let paths = source_inventory(&project).unwrap();
    assert_eq!(paths, vec![project.join("src/lib.rs")]);
}

#[test]
fn test_source_candidates_prefilters_fixed_string_without_full_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {:?}: {:?}", args, output);
    };
    run(&["init", "-q"]);
    std::fs::write(root.join("hit.rs"), "pub fn target_symbol() {}\n").unwrap();
    std::fs::write(root.join("miss.rs"), "pub fn other_symbol() {}\n").unwrap();
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=test",
        "commit",
        "-qm",
        "fixture",
    ]);

    let hits = source_candidates(root, "target_symbol").unwrap();
    assert_eq!(hits, vec![root.join("hit.rs")]);
    assert!(
        source_candidates(root, "does_not_exist")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_changed_source_candidates_only_returns_live_edits() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {:?}: {:?}", args, output);
    };
    run(&["init", "-q"]);
    std::fs::write(root.join("tracked.rs"), "pub fn tracked() {}\n").unwrap();
    std::fs::write(root.join("stable.rs"), "pub fn stable() {}\n").unwrap();
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=test",
        "commit",
        "-qm",
        "fixture",
    ]);
    std::fs::write(root.join("tracked.rs"), "pub fn changed() {}\n").unwrap();
    std::fs::write(root.join("untracked.rs"), "pub fn new_source() {}\n").unwrap();

    let paths = changed_source_candidates(root).unwrap();
    assert_eq!(
        paths,
        vec![root.join("tracked.rs"), root.join("untracked.rs")]
    );
    assert!(tree_oid(root).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn test_live_source_candidates_reject_symlink_escapes() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = temp.path();
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {:?}: {:?}", args, output);
    };
    run(&["init", "-q"]);
    std::fs::write(root.join("stable.rs"), "pub fn stable() {}\n").unwrap();
    run(&["add", "."]);
    run(&[
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=test",
        "commit",
        "-qm",
        "fixture",
    ]);
    let escaped = outside.path().join("escaped.rs");
    std::fs::write(&escaped, "pub fn escaped() {}\n").unwrap();
    std::os::unix::fs::symlink(&escaped, root.join("escaped.rs")).unwrap();

    assert!(source_candidates(root, "escaped").unwrap().is_empty());
    assert!(changed_source_candidates(root).unwrap().is_empty());
}
