#![cfg(feature = "cli")]

use leindex::cli::git::parse_status;
use std::path::PathBuf;

#[test]
fn parses_nul_delimited_porcelain_v2_records_without_splitting_paths() {
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
fn initial_or_detached_headers_have_no_branch_or_head_oid() {
    let status = parse_status(b"# branch.oid (initial)\0# branch.head (detached)\0? a b\0");

    assert_eq!(status.branch, None);
    assert_eq!(status.head_oid, None);
    assert_eq!(status.untracked, vec![PathBuf::from("a b")]);
}
