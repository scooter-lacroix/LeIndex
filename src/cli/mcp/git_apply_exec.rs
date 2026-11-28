//! Process execution for `leindex_git` mutations: the hardened git child,
//! its bounded pipe draining, and the failure surface.
//!
//! Split out of `git_apply.rs` (which had grown past the 2000-line limit) so
//! the publisher keeps its policy code and this module keeps the process
//! mechanics. Nothing here decides WHAT to run — only how a child is spawned,
//! drained, timed out and reported.

use super::protocol::JsonRpcError;
use std::path::Path;
use std::time::Duration;

/// Hard timeout for one mutating git command. A hung credential prompt or a
/// stuck hook must not pin the MCP server; the process is killed at the cap.
const MUTATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Ceiling on captured stdout/stderr of a mutation, per stream.
pub(crate) const MAX_MUTATION_OUTPUT_BYTES: usize = 256 * 1024;

/// Char budget for a surfaced failure before truncation is counted.
const FAILURE_CHARS: usize = 4_000;

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

pub(crate) struct MutationOutput {
    pub(crate) stdout: String,
    /// Characters dropped from the combined output by the byte cap.
    pub(crate) truncated_chars: usize,
}

/// Read a child's pipe to EOF, keeping at most [`MAX_MUTATION_OUTPUT_BYTES`]
/// bytes and DISCARDING the rest.
///
/// Draining does not stop at the cap: a child whose output exceeds it must
/// still be able to finish writing, or it blocks on a full pipe forever.
/// Returns the kept text (lossy UTF-8) and the number of characters in the
/// discarded tail. The tail is decoded chunk by chunk with a carry for a
/// multibyte character split across two reads, so the count is exact and the
/// discarded text is never held in memory.
pub(crate) async fn drain_capped<R: tokio::io::AsyncRead + Unpin>(
    pipe: Option<R>,
) -> (String, usize) {
    use tokio::io::AsyncReadExt;
    let Some(mut pipe) = pipe else {
        return (String::new(), 0);
    };
    let mut buffer = Vec::new();
    let mut dropped_chars = 0usize;
    let mut carry: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 8192];
    loop {
        let read = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let room = MAX_MUTATION_OUTPUT_BYTES.saturating_sub(buffer.len());
        let keep = room.min(read);
        buffer.extend_from_slice(&chunk[..keep]);
        if keep < read {
            // Only `[keep..read]` is real data; the rest of `chunk` is stale
            // bytes from earlier reads and must not be counted.
            dropped_chars += count_chars_streaming(&mut carry, &chunk[keep..read]);
        }
    }
    // A trailing unfinished sequence is one replacement character in the
    // text the caller would have seen.
    dropped_chars += usize::from(!carry.is_empty());
    let text = String::from_utf8_lossy(&buffer).into_owned();
    (text, dropped_chars)
}

/// Count the characters in one piece of a UTF-8 stream, carrying an
/// unfinished multibyte sequence (up to 3 bytes) into the next call so a
/// character split across two reads is counted once.
pub(crate) fn count_chars_streaming(carry: &mut Vec<u8>, piece: &[u8]) -> usize {
    let mut stream = std::mem::take(carry);
    stream.extend_from_slice(piece);
    let mut rest = stream.as_slice();
    let mut count = 0usize;
    // Decode the whole piece: `from_utf8` reports only the first error, so a
    // single call would defer every later invalid byte to the next read and
    // lose them if the stream ends first. An invalid byte is one replacement
    // character and is consumed; an unfinished sequence (error_len() ==
    // None) is the tail, and is carried.
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                count += text.chars().count();
                rest = &[];
            }
            Err(error) => {
                let valid = &rest[..error.valid_up_to()];
                count += std::str::from_utf8(valid).unwrap_or("").chars().count();
                match error.error_len() {
                    Some(bad) => {
                        count += 1;
                        rest = &rest[error.valid_up_to() + bad..];
                    }
                    // Truncated tail: carry exactly the unfinished bytes, not
                    // everything before the error (those were counted above).
                    None => {
                        rest = &rest[error.valid_up_to()..];
                        break;
                    }
                }
            }
        }
    }
    *carry = rest.to_vec();
    count
}

/// Repository-location variables that OVERRIDE `current_dir(root)`.
///
/// `Command::current_dir` alone does NOT redirect a git child that inherited
/// one of these — from a git hook that launched the MCP process, for instance.
/// They win over the working directory, so a caller asking about repository B
/// reads (or mutates) repository A. CodeRabbit round 10 (id 4697090106) found
/// this on the WRITE path; the READ path and the indexer's tree probe had the
/// same hole and were missed, so an inherited `GIT_DIR` also made the preview
/// token bind a DIFFERENT repository's HEAD and index (every mutation then
/// rejected on a token that could never match) and recorded an index tree oid
/// belonging to another worktree.
///
/// One list, every spawn site, so a variable cannot be scrubbed in one place
/// and forgotten in another.
pub(crate) const REPOSITORY_LOCATION_OVERRIDES: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_COMMON_DIR",
];

/// Remove the inherited repository-location overrides from a blocking child.
pub(crate) fn scrub_std(command: &mut std::process::Command) {
    for variable in REPOSITORY_LOCATION_OVERRIDES {
        command.env_remove(variable);
    }
}

/// Tokio twin of [`scrub_std`], for the async mutation path.
fn scrub_async(command: &mut tokio::process::Command) {
    for variable in REPOSITORY_LOCATION_OVERRIDES {
        command.env_remove(variable);
    }
}

/// Every git child this module spawns starts here: `current_dir(root)` AND
/// the inherited repository-location overrides removed.
///
/// Those variables are cleared for every probe and mutation; the validated
/// index snapshot is then set EXPLICITLY and nothing else is.
pub(super) fn git_command(root: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    scrub_async(&mut command);
    command
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null());
    // Own process group, so a timed-out mutation can be killed as a TREE.
    // Without this, `start_kill()` reaches only git itself: a pre-commit hook
    // (or anything the hook spawned) survives, keeps writing the worktree or
    // holding `.git/index.lock` while the caller is told the operation was
    // killed. The pgid is the leader's own pid because `process_group(0)`
    // makes the child its own group leader.
    #[cfg(unix)]
    command.process_group(0);
    command
}

/// The std-process twin of [`git_command`], for the one blocking probe
/// (`check-ignore`) that runs on a plain thread.
pub(super) fn git_command_sync(root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    scrub_std(&mut command);
    command
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("GIT_EDITOR", "true")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

/// Run one mutating git command under the hardened environment.
///
/// stdin is closed so hooks cannot block on input; credential prompts are
/// disabled by `GIT_TERMINAL_PROMPT=0` (and `GIT_ASKPASS=echo` as a second
/// latch); `GIT_EDITOR=true` keeps `merge --no-edit`-class operations from
/// opening an editor even where a hook or git path tries.
pub(crate) async fn run_mutation_with_env(
    root: &Path,
    argv: &[String],
    index_snapshot: Option<&Path>,
) -> Result<MutationOutput, JsonRpcError> {
    let mut command = git_command(root);
    command
        .args(argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(snapshot) = index_snapshot {
        command.env("GIT_INDEX_FILE", snapshot);
    }

    let mut child = command
        .spawn()
        .map_err(|error| JsonRpcError::internal_error(format!("failed to spawn git: {error}")))?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    // Captured NOW, not at teardown: `process_group(0)` makes the child its
    // own group leader, so its pid IS the pgid — but once the child is reaped
    // `Child::id()` returns None and the group signal would be skipped.
    #[cfg(unix)]
    let pgid = child.id().map(|id| id as libc::pid_t);

    // Output collection and process execution share ONE deadline. Reading
    // one pipe to EOF before touching the other deadlocks when a hook fills
    // the other pipe: git blocks on the write, the read never sees EOF, and
    // the timeout below would never be reached. So drain both concurrently,
    // and keep draining past the capture cap so git can always make progress.
    let collect = tokio::time::timeout(MUTATION_TIMEOUT, async {
        let stdout = drain_capped(stdout_pipe);
        let stderr = drain_capped(stderr_pipe);
        let (stdout, stderr) = tokio::join!(stdout, stderr);
        let status = child.wait().await;
        (stdout, stderr, status)
    })
    .await;

    let ((stdout, stdout_dropped), (stderr, stderr_dropped), status) = match collect {
        Ok((stdout, stderr, Ok(status))) => (stdout, stderr, status),
        Ok((_, _, Err(error))) => {
            return Err(JsonRpcError::internal_error(format!(
                "git wait failed: {error}"
            )));
        }
        Err(_) => {
            // Kill the GROUP, not just git. A hook that outlived its parent
            // keeps writing the worktree and holding `.git/index.lock`, so the
            // caller is told the mutation was killed while its side effects
            // continue — and a retry then fails on the stale lock.
            #[cfg(unix)]
            if let Some(pgid) = pgid {
                // SAFETY: plain signal delivery to the group created above by
                // `process_group(0)`; no memory is touched.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            // Reap before returning so no zombie is left behind and the group
            // signal lands before the pid can be recycled.
            let _ = child.wait().await;
            return Err(JsonRpcError::internal_error(format!(
                "git {} was killed after the {}s mutation timeout (process group signalled)",
                argv.join(" "),
                MUTATION_TIMEOUT.as_secs()
            )));
        }
    };

    let truncated_chars = stdout_dropped + stderr_dropped;
    if !status.success() {
        return Err(failure_error(argv, &stdout, &stderr, truncated_chars));
    }
    Ok(MutationOutput {
        stdout,
        truncated_chars,
    })
}

/// Run a mutation with no index pin — the plain form every non-commit
/// operation uses, and what the executor's own tests call.
pub(crate) async fn run_mutation(
    root: &Path,
    argv: &[String],
) -> Result<MutationOutput, JsonRpcError> {
    run_mutation_with_env(root, argv, None).await
}

/// Build the error for a failed mutation: the combined output VERBATIM,
/// bounded, with an explicit truncation count.
///
/// Hook rejections and plain git failures are surfaced the same way — the
/// caller needs the words the hook printed, not a paraphrase.
pub(crate) fn failure_error(
    argv: &[String],
    stdout: &str,
    stderr: &str,
    truncated_chars: usize,
) -> JsonRpcError {
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str(stdout.trim_end());
        combined.push('\n');
    }
    if !stderr.trim().is_empty() {
        combined.push_str(stderr.trim_end());
    }
    let mut message = format!("git {} failed", argv.join(" "));
    if !combined.trim().is_empty() {
        // Git surfaces remote output verbatim — including `push` progress and
        // hook chatter, which can carry URL-embedded credentials
        // (`https://user:token@host/…`) or `Bearer` tokens from a CI hook.
        // The same scrubber the log pipeline uses runs here so a mutation
        // failure can never leak a secret into an MCP response.
        let redacted = crate::observability::LogScrubber::redact(&combined);
        let mut body: String = redacted.chars().take(FAILURE_CHARS).collect();
        if body.chars().count() == FAILURE_CHARS {
            body.push('…');
        }
        message.push_str(":\n");
        message.push_str(&body);
        let total = redacted.chars().count();
        let shown = total.min(FAILURE_CHARS);
        if total > shown || truncated_chars > 0 {
            message.push_str(&format!(
                "\n[output truncated: showing {shown} of {total} chars, {} dropped by the capture cap]",
                total - shown + truncated_chars
            ));
        }
    }
    JsonRpcError::internal_error(message)
}

#[cfg(test)]
#[path = "git_apply_exec_test.rs"]
mod test;
