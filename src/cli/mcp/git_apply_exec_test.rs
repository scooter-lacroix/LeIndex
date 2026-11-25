//! Tests for the process-execution half of the git mutations
//! ([`super::git_apply_exec`]): the hardened child, bounded pipe draining,
//! the mutation runner's failure surface, and credential redaction.
//!
//! Split out of `git_apply_test.rs`, which passed the 2000-line limit. These
//! reach the executor directly, so they live beside it rather than behind the
//! publisher's re-exports.

use super::*;
use std::path::Path;

/// A git child isolated from whatever launched the test binary.
fn isolated_git(dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

fn git(dir: &Path, args: &[&str]) {
    let status = isolated_git(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}

fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "master"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "commit.gpgSign", "false"]);
    std::fs::write(dir.path().join("f.txt"), "a\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "one"]);
    dir
}

/// A hook that floods stderr well past the OS pipe buffer (64 KiB on
/// Linux). Before the fix, `run_mutation` read stdout to EOF first and
/// only then stderr, so git blocked on the full stderr pipe, stdout never
/// reached EOF, and the call hung until the 300s timeout.
#[cfg(unix)]
#[tokio::test]
async fn test_run_mutation_drains_both_pipes_when_a_hook_floods_stderr() {
    let repo = scratch();
    let hooks = repo.path().join(".git/hooks");
    std::fs::write(
        hooks.join("pre-commit"),
        "#!/bin/sh\nhead -c 300000 /dev/zero | tr '\\0' 'x' >&2\necho hook-stderr >&2\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        hooks.join("pre-commit"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(repo.path().join("f.txt"), "b\n").unwrap();
    // `git commit` refuses an empty commit (nothing staged), which is
    // what the hook-flood commit would be without this. Stage through the
    // executor under test so the setup is itself exercised end-to-end.
    run_mutation(repo.path(), &["add".to_string(), "f.txt".to_string()])
        .await
        .expect("staging the fixture must succeed");

    // Well under MUTATION_TIMEOUT: a regression hangs here, and the
    // assertion fails the test instead of stalling the suite for 300s.
    let started = std::time::Instant::now();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run_mutation(
            repo.path(),
            &[
                "commit".to_string(),
                "-q".to_string(),
                "-m".to_string(),
                "two".to_string(),
            ],
        ),
    )
    .await
    .expect("run_mutation must not deadlock on a hook that floods stderr")
    .expect("commit with a passing hook succeeds");
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
    // git routes hook output to ITS stderr, so the marker never reaches
    // stdout; what the success path proves is that the 300 KB flood was
    // drained concurrently (no deadlock) and that the overflow past the
    // per-stream cap was counted rather than lost.
    assert!(output.stdout.trim().is_empty(), "{}", output.stdout);
    // 300,000 bytes in, 262,144 kept: the difference is the drop.
    assert!(
        output.truncated_chars >= 300_000 - MAX_MUTATION_OUTPUT_BYTES,
        "stderr past the cap must be reported as dropped, got {}",
        output.truncated_chars
    );
}

#[tokio::test]
async fn test_drain_capped_keeps_the_cap_and_reports_dropped_characters() {
    // A 64-byte duplex forces many small reads, so the cap is crossed
    // mid-stream rather than in one shot.
    let (mut tx, rx) = tokio::io::duplex(64);
    let payload: Vec<u8> = std::iter::repeat_n(b'x', MAX_MUTATION_OUTPUT_BYTES + 1000).collect();
    let write = async move {
        use tokio::io::AsyncWriteExt;
        tx.write_all(&payload).await.unwrap();
        tx.shutdown().await.unwrap();
    };
    let ((text, dropped), ()) = tokio::join!(drain_capped(Some(rx)), write);
    assert_eq!(text.len(), MAX_MUTATION_OUTPUT_BYTES, "kept text is capped");
    assert_eq!(
        dropped, 1000,
        "exactly the characters past the cap are counted"
    );
}

#[test]
fn test_count_chars_streaming_is_exact_for_multibyte_split_across_reads() {
    // 2-byte characters ("é" is U+00E9 = 0xC3 0xA9), so a split can land
    // in the middle of one.
    let bytes = "éééééééééé".as_bytes();
    for cut in 1..bytes.len() {
        let (a, b) = bytes.split_at(cut);
        let mut carry = Vec::new();
        let counted = count_chars_streaming(&mut carry, a)
            + count_chars_streaming(&mut carry, b)
            + usize::from(!carry.is_empty());
        assert_eq!(
            counted, 10,
            "split at byte {cut} must still count 10 characters"
        );
    }
    // An invalid byte is one replacement character and is consumed, so
    // the following valid byte is counted on its own.
    let mut carry = Vec::new();
    assert_eq!(count_chars_streaming(&mut carry, &[0xFF, b'a']), 2);
    assert!(carry.is_empty());
}

#[test]
fn test_failure_error_redacts_credentials_from_git_output() {
    // F3: git echoes remote URLs and hook chatter verbatim. A push that
    // fails against `https://user:token@host/…` must not put the token in
    // an MCP error message.
    let error = failure_error(
        &["push".to_string()],
        "",
        "fatal: unable to access 'https://scooter:s3cr3t-token@github.com/o/r.git/': 403\n\
         remote: Authorization: Bearer abcdef123456\n",
        0,
    );
    let message = error.message_with_hint();
    assert!(
        !message.contains("s3cr3t-token"),
        "URL credentials must be redacted: {message}"
    );
    assert!(
        !message.contains("abcdef123456"),
        "bearer tokens must be redacted: {message}"
    );
    assert!(
        message.contains("[REDACTED]"),
        "the redaction is visible, not silently dropped: {message}"
    );
    // The words the hook printed that matter are still there.
    assert!(message.contains("403"), "{message}");
}
