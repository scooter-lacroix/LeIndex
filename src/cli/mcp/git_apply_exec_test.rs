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
///
/// The variable list is the shared [`REPOSITORY_LOCATION_OVERRIDES`] constant
/// (imported here by `use super::*`), not a local copy: a fixture that clears a
/// different set can pass while the code under test inherits a variable.
fn isolated_git(dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    scrub_std(&mut command);
    command
        .current_dir(dir)
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

/// The mutation child must be its OWN process-group leader, or the timeout
/// path has nothing to signal as a group.
///
/// `run_mutation` used to call `start_kill()` only. On Unix that reaps git and
/// leaves everything git spawned — a pre-commit hook's descendants — running
/// after the caller was told the mutation was killed, so a hook could keep
/// writing the worktree or holding `.git/index.lock` while a retry ran. The
/// fix puts the child in a new process group (`process_group(0)`) and signals
/// `-pgid`. This asserts the precondition: without it, `libc::kill(-pgid)`
/// either fails or hits the TEST runner's own group.
#[cfg(unix)]
#[tokio::test]
async fn test_git_command_child_leads_its_own_process_group() {
    let repo = scratch();
    let mut command = git_command(repo.path());
    command
        .args(["rev-parse", "--show-toplevel"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().expect("git available");
    let pid = child.id().expect("child is alive") as libc::pid_t;

    // `getpgid` is portable across unix; /proc/<pid>/stat is Linux-only and
    // would panic this test on macOS, where `#[cfg(unix)]` also compiles.
    let pgid = unsafe { libc::getpgid(pid) };
    assert!(
        pgid >= 0,
        "getpgid({pid}) failed: {}",
        std::io::Error::last_os_error()
    );

    assert_eq!(
        pgid, pid,
        "the mutation child must lead its own group (pgid {pgid}, pid {pid}); a shared group \
         makes the timeout group-kill either a no-op or a hit on the test runner"
    );
    // Our own process must NOT be in that group, or the same kill would take
    // the whole test binary down with a hook.
    let own_pgid = unsafe { libc::getpgid(0) };
    assert_ne!(pgid, own_pgid, "child must not share the parent's group");

    let _ = child.wait().await;
}

/// Signalling `-pgid` must reap a GRANDCHILD that survived its parent, which
/// is exactly the case `start_kill()` alone missed: a hook outliving git.
#[cfg(unix)]
#[tokio::test]
async fn test_process_group_kill_reaps_a_grandchild_that_outlives_its_parent() {
    /// Signal the group on EVERY exit path, including an assertion panic.
    ///
    /// The direct child exits before the assertions run, so without this a
    /// failed poll would leave the background writer looping forever in the
    /// test harness. The explicit kill in the test body is the behaviour under
    /// test; this guard only guarantees nothing survives the test itself.
    struct GroupGuard(libc::pid_t);
    impl GroupGuard {
        fn kill(&self) {
            // SAFETY: plain signal delivery to a group this test created with
            // `process_group(0)`; no memory is touched.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
    impl Drop for GroupGuard {
        fn drop(&mut self) {
            self.kill();
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("hook-writes.log");
    let child_pid_file = dir.path().join("gc.pid");

    // `sh` spawns a background writer (the "hook descendant") and exits
    // immediately, so by the time we kill there is no live direct child —
    // the same shape as a hook that daemonised past its git parent.
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!(
            "sh -c 'while true; do echo tick >> {marker}; sleep 0.05; done' & \
             echo $! > {pid}; exit 0",
            marker = marker.display(),
            pid = child_pid_file.display(),
        ))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.process_group(0);
    let mut child = command.spawn().expect("sh available");
    let pgid = child.id().expect("child alive") as libc::pid_t;
    // Installed BEFORE any assertion: from here on the group is always reaped.
    let guard = GroupGuard(pgid);
    let _ = child.wait().await;

    // Wait for the grandchild to actually start writing, so a pass cannot be
    // a false positive from "it never ran".
    let grandchild: libc::pid_t = (0..100)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::read_to_string(&child_pid_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
        })
        .expect("grandchild pid recorded");
    let mut saw_output = false;
    for _ in 0..100 {
        if std::fs::metadata(&marker)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            saw_output = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(saw_output, "the descendant must be writing before the kill");

    // The production teardown: signal the whole group.
    guard.kill();

    // The grandchild must be gone (or a zombie awaiting its reaper). Liveness
    // is checked with the PORTABLE probe: `/proc` is Linux-only, and reading
    // its absence as "stopped" would make this test vacuously pass on macOS.
    // `kill(pid, 0)` works everywhere POSIX and returns ESRCH once the pid is
    // gone; a zombie still answers, so poll until the reaper collects it or
    // the budget expires.
    let mut stopped = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        // SAFETY: signal 0 performs the permission/existence check only.
        let alive = unsafe { libc::kill(grandchild, 0) } == 0;
        let zombie = is_zombie(grandchild);
        if !alive || zombie {
            stopped = true;
            break;
        }
    }
    assert!(
        stopped,
        "the descendant (pid {grandchild}) must not survive the group kill; \
         killing only the direct child leaves it writing the worktree"
    );

    let size_after = std::fs::metadata(&marker).map(|m| m.len()).unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(150));
    let size_settled = std::fs::metadata(&marker).map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        size_after, size_settled,
        "a killed group must stop writing; the file grew after the signal"
    );
}

/// Is `pid` a zombie awaiting its reaper?
///
/// Linux answers from `/proc/<pid>/stat` (state 'Z'). Elsewhere the honest
/// answer is "cannot tell", which returns `false` so the caller keeps polling
/// `kill(pid, 0)` rather than treating an unreadable interface as proof of
/// death.
#[cfg(unix)]
fn is_zombie(pid: libc::pid_t) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| {
                s.rsplit_once(')')
                    .map(|(_, rest)| rest.split_whitespace().next() == Some("Z"))
            })
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}
