//! Preview-token BINDING tests: object-id acceptance and target tracking.
//!
//! Split out of `git_apply_test.rs`, which is at the 2000-line cap. The
//! SHA-256 case needs a real `--object-format=sha256` fixture, so it lives in
//! its own module rather than growing the main test file.

use super::*;

/// A git child isolated from whatever launched the test binary.
///
/// The variable list is the shared [`super::super::git_apply_exec::REPOSITORY_LOCATION_OVERRIDES`]
/// constant, reached through `scrub_std`; a fixture that cleared a different
/// set could pass while the code under test inherited a variable.
fn git(dir: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    super::super::git_apply_exec::scrub_std(&mut command);
    let status = command
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .args(args)
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}

/// A SHA-256 repository must resolve its target, and the binding must track it.
/// `resolve_oid` tested `oid.len() == 40` by hand, so every SHA-256 commit id
/// was rejected and the target bound as `unresolved:<name>` — hashed into the
/// preview token, so a moved branch could no longer invalidate it.
#[test]
fn test_accepted_oid_and_binding_work_on_a_sha256_repository() {
    // The predicate accepts both hash lengths and still rejects non-oids.
    let sha1 = "a".repeat(40);
    let sha256 = "0123456789abcdef".repeat(4);
    assert_eq!(accepted_oid(&sha1), Some(sha1));
    assert_eq!(accepted_oid(&format!(" {sha256}\n")), Some(sha256));
    for junk in ["", "HEAD", "unresolved:x", &"a".repeat(39), &"g".repeat(64)] {
        assert_eq!(accepted_oid(junk), None, "must reject {junk:?}");
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(
        root,
        &["init", "-q", "--object-format=sha256", "-b", "master"],
    );
    git(root, &["config", "user.name", "t"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "commit.gpgSign", "false"]);
    std::fs::write(root.join("f.txt"), "a\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "one"]);
    git(root, &["branch", "moved"]);

    let argv = plan_argv_for("checkout", root, &json!({ "target": "moved" })).unwrap();
    let before = state_binding(root, "checkout", &argv).expect("binding");
    assert!(
        !before.contains("unresolved:"),
        "a SHA-256 target must RESOLVE, not bind as unresolved: {before}"
    );
    assert_eq!(
        before,
        state_binding(root, "checkout", &argv).expect("binding"),
        "an unmoved target must bind identically"
    );

    // Re-point `moved` only; HEAD stays on master and the index is clean.
    git(root, &["checkout", "-q", "moved"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "move"]);
    git(root, &["checkout", "-q", "master"]);
    assert_ne!(
        before,
        state_binding(root, "checkout", &argv).expect("binding"),
        "moving the target branch must invalidate the preview token"
    );
}
