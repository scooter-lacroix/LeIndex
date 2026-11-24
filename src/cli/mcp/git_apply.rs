//! Apply side for `leindex_git` mutations — the executor behind the plan.
//!
//! Policy (brief §5):
//! * A plan plus its `preview_token` is the default. The token binds the exact
//!   argv AND the repository state (HEAD oid + a hash of the index), so a plan
//!   reviewed against one state cannot be replayed against another.
//! * `confirm: true` skips the plan round-trip for the SAFE subset only
//!   (`add`, `commit`, `push` without force).
//! * Destructive shapes are never reachable on `confirm` alone: force-push
//!   additionally needs `force: true` plus a `confirm_text` equal to the
//!   string the plan printed; `reset --hard`, `clean -fd` and branch delete
//!   need their explicit flags and are token-only (enforced in
//!   [`super::git_handler::plan_argv`], so plan and apply cannot disagree).
//! * Every mutation runs with stdin closed, `GIT_TERMINAL_PROMPT=0`,
//!   `GIT_EDITOR=true`, no pager, and a hard timeout. Failures — hook
//!   rejections included — are surfaced verbatim, bounded, with a truncation
//!   count.
//!
//! Staging guardrails (bulk adds only — paths named explicitly are always
//! staged but warned about): files under a directory path that look like
//! secrets or runtime artefacts are EXCLUDED and listed, unless
//! `allow_sensitive: true`.

use super::git_diff_handler::MAX_DIFF_BYTES;
use super::git_handler::{plan_argv_for, preview_token};
use super::helpers::validate_file_within_project;
use super::protocol::JsonRpcError;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Hard timeout for one mutating git command. A hung credential prompt or a
/// stuck hook must not pin the MCP server; the process is killed at the cap.
const MUTATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Ceiling on captured stdout/stderr of a mutation, per stream.
const MAX_MUTATION_OUTPUT_BYTES: usize = 256 * 1024;

/// Operations the `confirm: true` shortcut may apply without a prior plan.
/// `pub(crate)`: the git_handler test pins the plan hint to exactly this set.
pub(crate) const SAFE_TO_CONFIRM: [&str; 3] = ["add", "commit", "push"];

/// Operations that only a prior plan's token may apply.
const TOKEN_ONLY: [&str; 5] = ["checkout", "stash", "tag", "merge", "rebase"];

/// Char budget for a surfaced failure before truncation is counted.
const FAILURE_CHARS: usize = 4_000;

// ---------------------------------------------------------------------------
// Staging guardrails
// ---------------------------------------------------------------------------

/// What a bulk `add` will stage, and what it deliberately will not.
///
/// `staged` is the exact path list the mutation argv is built from, so the
/// plan's "what will be staged" claim and the executor's behaviour cannot
/// drift. Deterministic on a fixed tree: identical inputs yield the same
/// sorted list, which is what makes token binding over the rebuilt argv work.
pub(crate) struct StagingOutcome {
    /// Paths the `git add` argv carries (post-exclusion, sorted, deduped).
    pub staged: Vec<String>,
    /// (path, reason) for directory-discovered files that were NOT staged.
    pub excluded: Vec<(String, &'static str)>,
    /// Human-readable warnings: secrets named explicitly, oversized and
    /// binary files, and anything excluded.
    pub warnings: Vec<String>,
}

/// Files whose names look like secrets.
///
/// basename patterns: `.env*`, `*.pem`, `*id_rsa*`, `*.key`,
/// `credentials*.json`, and anything with `secret` in the name; anything
/// under a `credentials`/`secrets` directory is also treated as one
/// (`credentials/google.json` is a secret even though its basename is not).
fn is_secret_path(relative: &str) -> bool {
    let path = Path::new(relative);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let basename_looks_secret = lower.starts_with(".env")
        || lower.ends_with(".pem")
        || lower.contains("id_rsa")
        || lower.ends_with(".key")
        || lower.contains("secret")
        || (lower.starts_with("credentials") && lower.ends_with(".json"));
    basename_looks_secret
        || path.components().rev().skip(1).any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|c| c.eq_ignore_ascii_case("credentials"))
        })
}

/// Runtime/generated artefacts the brief names for this workspace.
fn is_runtime_artifact(relative: &str) -> bool {
    let path = Path::new(relative);
    if path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("target")
                | Some("node_modules")
                | Some("selection.json")
                | Some("initializer.json")
        )
    }) {
        return true;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    // The operation lock file is named `lock` (the parent dir is the
    // operation id, e.g. `.maestro/operations/1-lock/lock`).
    (relative.starts_with(".maestro/operations/") && (lower == "lock" || lower.ends_with(".lock")))
        || lower.ends_with(".log")
}

/// Whether the first bytes of a file look binary (NUL byte in 8 KiB).
fn looks_binary(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut head = [0u8; 8192];
    let read = file.read(&mut head).unwrap_or(0);
    head[..read].contains(&0)
}

/// Normalise `raw` to a forward-slash project-relative path without
/// consulting the filesystem (so it works for files that no longer exist):
/// strips `./`, collapses `.` segments and rejects `..` by returning the
/// raw text unchanged for the caller to reject.
fn lexical_relative(raw: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in raw.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => return raw.to_string(),
            other => out.push(other),
        }
    }
    out.join("/")
}

/// Expand the requested `paths` into concrete files.
///
/// Directories are walked (`.git` and build-output directories skipped)
/// with a hard entry budget, so a caller pointing at the repo root cannot
/// blow the response; the skipped remainder is returned for the warning.
fn expand_paths(
    root: &Path,
    requested: &[String],
) -> Result<(Vec<String>, usize, Vec<String>), JsonRpcError> {
    const MAX_DISCOVERED: usize = 2_000;

    let mut files = Vec::new();
    let mut skipped = 0usize;
    let mut dirs = Vec::new();
    for raw in requested {
        let absolute = root.join(raw);
        if absolute.is_dir() {
            // A whole-tree request (`.`) is the common bulk case: normalize it
            // so discovered paths come back project-relative (`f.txt`), not
            // `./f.txt` — the plan and the exclusion report use these strings.
            let rel = if raw == "." { "" } else { raw.as_str() };
            dirs.push((rel.to_string(), absolute));
        } else {
            files.push(raw.clone());
        }
    }
    let dir_rels = dirs.iter().map(|(rel, _)| rel.clone()).collect::<Vec<_>>();
    for (rel, absolute) in &dirs {
        let mut budget = MAX_DISCOVERED;
        walk_dir(
            root,
            rel,
            absolute,
            0,
            &mut files,
            &mut skipped,
            &mut budget,
        )?;
    }
    // The filesystem walk can only see files that exist. A tracked file
    // deleted from the worktree is invisible to it, so a bulk
    // `paths:["."]` used to stage every surviving change and silently leave
    // the deletions unstaged. Git's own inventory knows about them: add
    // every deleted tracked path under a requested directory.
    if !dirs.is_empty()
        && let Ok(status) = crate::cli::git::status(root)
    {
        for deleted in &status.deleted {
            let rel = deleted.to_string_lossy().replace('\\', "/");
            let under_requested_dir = dirs.iter().any(|(dir, _)| {
                dir.is_empty() || rel.starts_with(&format!("{dir}/")) || &rel == dir
            });
            if under_requested_dir {
                files.push(rel);
            }
        }
    }
    files.sort();
    files.dedup();
    Ok((files, skipped, dir_rels))
}

/// Recursive worker for [`expand_paths`]. `budget` is shared per directory
/// request; when it runs out the walk stops and `skipped` counts the rest.
fn walk_dir(
    root: &Path,
    rel: &str,
    absolute: &Path,
    depth: usize,
    files: &mut Vec<String>,
    skipped: &mut usize,
    budget: &mut usize,
) -> Result<(), JsonRpcError> {
    const SKIP_DIR_NAMES: [&str; 2] = [".git", "node_modules"];
    if depth > 16 || *budget == 0 {
        return Ok(());
    }
    let entries = std::fs::read_dir(absolute).map_err(|error| {
        JsonRpcError::invalid_params(format!("cannot read directory '{rel}': {error}"))
    })?;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let child_abs = root.join(&child_rel);
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            // `target/` stays out of discovery entirely (walk cost), but is
            // still reported by the caller's own explicit-path check.
            if SKIP_DIR_NAMES.contains(&name.as_str()) || name == "target" {
                continue;
            }
            walk_dir(
                root,
                &child_rel,
                &child_abs,
                depth + 1,
                files,
                skipped,
                budget,
            )?;
        } else if *budget > 0 {
            *budget -= 1;
            files.push(child_rel);
        } else {
            *skipped += 1;
        }
    }
    Ok(())
}

/// Build the staging report for `action=add`.
///
/// Classification per expanded file:
/// * secret/runtime found under a DIRECTORY path — excluded from the argv
///   (and listed) unless `allow_sensitive: true`, in which case it is staged
///   with a warning;
/// * secret/runtime named EXPLICITLY — always staged (the caller asked for
///   exactly this path) with a warning;
/// * everything else — staged, with warnings for files over 1 MiB and
///   probable binaries.
pub(crate) fn staging_outcome(root: &Path, args: &Value) -> Result<StagingOutcome, JsonRpcError> {
    let paths = args.get("paths").and_then(Value::as_array).ok_or_else(|| {
        JsonRpcError::invalid_params_with_suggestion(
            "git add requires a non-empty 'paths' array".to_string(),
            "Provide paths: project-relative files to stage".to_string(),
        )
    })?;
    // Tracked files deleted from the worktree are stageable (`git add`
    // records the removal) but no longer exist on disk, so the
    // canonicalizing containment check cannot resolve them. They are
    // accepted when git reports them deleted AND their lexical, normalised
    // form stays inside the project — a deletion can never escape the
    // root it is reported relative to.
    let deleted: std::collections::BTreeSet<String> = crate::cli::git::status(root)
        .map(|status| {
            status
                .deleted
                .iter()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .collect()
        })
        .unwrap_or_default();
    let mut requested = Vec::new();
    for path in paths {
        let raw = path.as_str().unwrap_or_default();
        if validate_file_within_project(raw, root).is_err() {
            let lexed = lexical_relative(raw);
            if !deleted.contains(&lexed) {
                return Err(JsonRpcError::invalid_params_with_suggestion(
                    format!("'{raw}' is not a path inside this project"),
                    "Stage project-relative paths only".to_string(),
                ));
            }
        }
        requested.push(raw.to_string());
    }
    if requested.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "git add requires at least one path".to_string(),
        ));
    }
    let allow_sensitive = args
        .get("allow_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let explicit = requested
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let (discovered, skipped, dirs) = expand_paths(root, &requested)?;

    let mut staged = Vec::new();
    let mut excluded = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for relative in discovered {
        if !seen.insert(relative.clone()) {
            continue;
        }
        let sensitive = is_secret_path(&relative) || is_runtime_artifact(&relative);
        let is_explicit = explicit.contains(&relative);
        if sensitive && !is_explicit && !allow_sensitive {
            let reason = if is_secret_path(&relative) {
                "likely secret"
            } else {
                "runtime artefact"
            };
            excluded.push((relative, reason));
            continue;
        }
        if sensitive {
            warnings.push(format!("'{relative}' looks like a secret or runtime artefact; staging it because it was named explicitly or allow_sensitive is set"));
        }
        let absolute = root.join(&relative);
        let metadata = std::fs::metadata(&absolute);
        if metadata.as_ref().is_ok_and(|m| m.len() > 1024 * 1024) {
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            warnings.push(format!(
                "'{relative}' is {:.1} MiB (over the 1 MiB warning threshold)",
                size as f64 / (1024.0 * 1024.0)
            ));
        }
        if looks_binary(&absolute) {
            warnings.push(format!(
                "'{relative}' looks binary; check before committing"
            ));
        }
        staged.push(relative);
    }
    staged.sort();
    staged.dedup();
    if skipped > 0 {
        warnings.push(format!(
            "directory walk hit the {}-entry budget; {} files were not enumerated \
             — pass narrower paths to cover them",
            2_000, skipped
        ));
    }
    for (relative, reason) in &excluded {
        warnings.push(format!(
            "excluded '{relative}' from bulk add: {reason} (stage it explicitly or set allow_sensitive:true)"
        ));
    }
    let _ = dirs;
    if staged.is_empty() && !excluded.is_empty() {
        return Err(JsonRpcError::invalid_params_with_suggestion(
            "nothing left to stage: every candidate was excluded as a secret or runtime artefact"
                .to_string(),
            "Name the files explicitly or set allow_sensitive:true".to_string(),
        ));
    }
    Ok(StagingOutcome {
        staged,
        excluded,
        warnings,
    })
}

/// The `git add` command line for a staging outcome.
///
/// ONE definition: the plan's advertised argv, the argv the token binds, and
/// the argv the executor runs all come from here, so the three cannot drift.
pub(crate) fn add_argv(outcome: &StagingOutcome) -> Vec<String> {
    let mut argv = vec!["add".to_string(), "--".to_string()];
    argv.extend(outcome.staged.iter().cloned());
    argv
}

// ---------------------------------------------------------------------------
// Repository state binding
// ---------------------------------------------------------------------------

/// Hash of the staged set: one line per index entry (mode, blob oid, path).
///
/// Covers both "the index hash" and "the staged set" from the brief in one
/// deterministic digest; any stage/unstage/rm changes it.
fn index_digest(root: &Path) -> Result<String, JsonRpcError> {
    let (text, _) = super::git_diff_handler::git_output_capped(
        root,
        &["ls-files".to_string(), "-s".to_string()],
        MAX_DIFF_BYTES,
    )
    .map_err(|error| JsonRpcError::internal_error(format!("index probe failed: {error}")))?;
    let mut digest = blake3::Hasher::new();
    for line in text.lines() {
        digest.update(line.as_bytes());
        digest.update(b"\n");
    }
    Ok(digest.finalize().to_hex().to_string())
}

/// HEAD commit oid, when the repository has one.
fn head_oid(root: &Path) -> Option<String> {
    super::git_diff_handler::git_output_capped(
        root,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "--quiet".to_string(),
            "HEAD".to_string(),
        ],
        4096,
    )
    .ok()
    .map(|(text, _)| text.trim().to_string())
    .filter(|oid| !oid.is_empty())
}

/// The live state a mutation token binds: HEAD oid + the index digest.
///
/// ONE binding, used at both ends of the seam: `mutation_plan` issues the
/// advertised token against it, `validate_before_apply` recomputes it before
/// spawning git. Issuing against an empty string (the pre-fix behaviour) made
/// every plan-issued token unmatchable, which silently disabled every
/// `TOKEN_ONLY` operation and every destructive shape.
pub(crate) fn state_binding(root: &Path) -> Result<String, JsonRpcError> {
    Ok(format!(
        "{}\u{0}{}",
        head_oid(root).unwrap_or_else(|| "none".to_string()),
        index_digest(root)?,
    ))
}

/// Recompute the preview token for `(op, argv)` against the CURRENT state.
///
/// The token is the plan's identity AND its validity window: it changes when
/// the argv changes (caller typo, tree grew) and when the state changes
/// (HEAD moved, index changed), so a stale token can never apply.
///
/// `pub(crate)` so `git_handler::mutation_plan` issues its advertised token
/// through this exact function — a plan token that is not the apply token is
/// a dead seam.
pub(crate) fn token_now(root: &Path, op: &str, argv: &[String]) -> Result<String, JsonRpcError> {
    Ok(preview_token(root, op, argv, &state_binding(root)?))
}

// ---------------------------------------------------------------------------
// Rails
// ---------------------------------------------------------------------------

/// Enforce the confirm/token rails for one apply.
///
/// `argv` is the effective argv this apply would run; the token comparison
/// covers it, so any drift between the reviewed plan and the current call is
/// a rejection before git is spawned.
async fn validate_before_apply(
    op: &str,
    root: &Path,
    args: &Value,
    argv: &[String],
) -> Result<(), JsonRpcError> {
    let confirm = args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let token = args.get("preview_token").and_then(Value::as_str);

    if op == "force" {
        // force:true + the exact string the plan printed. Required on BOTH
        // paths: the token proves a plan was reviewed, the text proves the
        // reviewer read the destructive shape (and not just the token field).
        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
        let printed = format!(
            "force-push {}",
            args.get("target")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        let supplied = args
            .get("confirm_text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !force {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                "force-push refused: set force:true to allow rewriting the remote".to_string(),
                format!("Set force:true and confirm_text:\"{printed}\""),
            ));
        }
        if supplied != printed {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                format!(
                    "confirm_text does not match the plan's confirmation string ('{supplied}')"
                ),
                format!("Copy it verbatim from the plan: confirm_text:\"{printed}\""),
            ));
        }
    }

    if let Some(token) = token {
        let expected = token_now(root, op, argv)?;
        if token != expected {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                "preview_token does not match this operation: the plan it identifies has \
                 changed (argv, HEAD or the index moved since it was issued)"
                    .to_string(),
                "Request a fresh plan and review it before applying".to_string(),
            ));
        }
        return Ok(());
    }
    if confirm {
        if SAFE_TO_CONFIRM.contains(&op) {
            return Ok(());
        }
        let safe = SAFE_TO_CONFIRM.join(", ");
        let hint = if TOKEN_ONLY.contains(&op) {
            format!(
                "Request the plan first, then resend with its preview_token (confirm works only for: {safe})"
            )
        } else {
            format!(
                "A destructive operation cannot be confirmed directly: request the plan and apply it with its preview_token (confirm works only for: {safe})"
            )
        };
        return Err(JsonRpcError::invalid_params_with_suggestion(
            format!("git {op} cannot be applied with confirm:true alone"),
            hint,
        ));
    }
    Err(JsonRpcError::invalid_params_with_suggestion(
        format!("git {op} needs a preview_token from a plan, or confirm:true"),
        "Request the plan (omit confirm and preview_token), review what it will do, then apply"
            .to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

struct MutationOutput {
    stdout: String,
    /// Characters dropped from the combined output by the byte cap.
    truncated_chars: usize,
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
async fn drain_capped<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> (String, usize) {
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
fn count_chars_streaming(carry: &mut Vec<u8>, piece: &[u8]) -> usize {
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

/// Run one mutating git command under the hardened environment.
///
/// stdin is closed so hooks cannot block on input; credential prompts are
/// disabled by `GIT_TERMINAL_PROMPT=0` (and `GIT_ASKPASS=echo` as a second
/// latch); `GIT_EDITOR=true` keeps `merge --no-edit`-class operations from
/// opening an editor even where a hook or git path tries.
async fn run_mutation(root: &Path, argv: &[String]) -> Result<MutationOutput, JsonRpcError> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(argv)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|error| JsonRpcError::internal_error(format!("failed to spawn git: {error}")))?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

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
            let _ = child.start_kill();
            return Err(JsonRpcError::internal_error(format!(
                "git {} was killed after the {}s mutation timeout",
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

/// Build the error for a failed mutation: the combined output VERBATIM,
/// bounded, with an explicit truncation count.
///
/// Hook rejections and plain git failures are surfaced the same way — the
/// caller needs the words the hook printed, not a paraphrase.
fn failure_error(
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

// ---------------------------------------------------------------------------
// Post-apply payloads
// ---------------------------------------------------------------------------

/// `<shorthash> <oneline>` plus the files-changed count for the new commit.
async fn commit_summary(root: &Path) -> Result<Value, JsonRpcError> {
    let (log, _) = super::git_diff_handler::git_output_capped(
        root,
        &[
            "log".to_string(),
            "-1".to_string(),
            "--no-color".to_string(),
            "--format=%h%x09%s".to_string(),
        ],
        4096,
    )
    .map_err(|error| JsonRpcError::internal_error(format!("commit probe failed: {error}")))?;
    let mut fields = log.trim().splitn(2, '\t');
    let short = fields.next().unwrap_or_default().to_string();
    let subject = fields.next().unwrap_or_default().to_string();
    let (files, _) = super::git_diff_handler::git_output_capped(
        root,
        &[
            "diff-tree".to_string(),
            "--no-commit-id".to_string(),
            "--name-only".to_string(),
            "-r".to_string(),
            "HEAD".to_string(),
        ],
        MAX_DIFF_BYTES,
    )
    .map_err(|error| JsonRpcError::internal_error(format!("commit probe failed: {error}")))?;
    let files_changed = files.lines().filter(|l| !l.trim().is_empty()).count();
    Ok(json!({
        "short": short,
        "subject": subject,
        "files_changed": files_changed,
        "oneline": format!("{short} {subject}"),
    }))
}

/// Remote ref range reported by `git push --porcelain`, plus the upstream
/// ahead/behind AFTER the push.
async fn push_payload(stdout: &str, root: &Path) -> Value {
    let mut range = Value::Null;
    let mut remote_ref = Value::Null;
    let mut note = String::new();
    for line in stdout.lines() {
        // Porcelain rows: `>\trefs/heads/x:refs/heads/x\t[up-to-date|new reference]\told:new`
        if let Some(rest) = line.strip_prefix(">\t") {
            let mut fields = rest.split('\t');
            let refspec = fields.next().unwrap_or_default().to_string();
            let flag = fields.next().unwrap_or_default().to_string();
            let oids = fields.next().unwrap_or_default();
            let mut sides = oids.split(':');
            let (Some(old), Some(new)) = (sides.next(), sides.next()) else {
                continue;
            };
            remote_ref = Value::String(refspec);
            if flag.contains("up-to-date") || old.is_empty() && new.is_empty() {
                note.push_str("already up to date; ");
                continue;
            }
            if !old.is_empty() && !new.is_empty() {
                range = json!(format!("{old}..{new}"));
            } else if new.is_empty() {
                note.push_str("remote ref deleted; ");
            } else {
                range = json!(format!("{new} (new)"));
            }
        }
    }
    let branch = head_branch(root);
    let mut upstream = Value::Null;
    let mut ahead = Value::Null;
    let mut behind = Value::Null;
    if let Some(branch) = branch {
        let query = format!("{branch}@{{upstream}}...{branch}");
        if let Ok((counts, _)) = super::git_diff_handler::git_output_capped(
            root,
            &[
                "rev-list".to_string(),
                "--left-right".to_string(),
                "--count".to_string(),
                query,
            ],
            4096,
        ) {
            let mut fields = counts.split_whitespace();
            let (b, a) = (fields.next(), fields.next());
            upstream = Value::String(format!("{branch}@{{upstream}}"));
            behind = b
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(Value::Null, Value::from);
            ahead = a
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(Value::Null, Value::from);
        }
    }
    let mut payload = json!({
        "remote_ref": remote_ref,
        "range": range,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
    });
    if !note.is_empty() {
        payload["note"] = Value::String(note.trim_end().to_string());
    }
    payload
}

/// Current branch name from the porcelain status probe (`None` when detached).
fn head_branch(root: &Path) -> Option<String> {
    crate::cli::git::status(root)
        .ok()
        .and_then(|status| status.branch)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Plan-or-apply entry for every mutating `leindex_git` action.
pub(crate) async fn handle(op: &str, root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    // `add` is planned over the EXACT path list the guardrails produce, so the
    // plan's staged list is the truth (exclusions included), not a promise.
    // The outcome is computed ONCE and reused for the payload: a second call
    // could disagree with the argv that actually ran (the tree can move
    // between them), which would report a different staged list than the one
    // staged.
    let (argv, staging) = if op == "add" {
        let outcome = staging_outcome(root, args)?;
        (add_argv(&outcome), Some(outcome))
    } else {
        (plan_argv_for(op, root, args)?, None)
    };

    validate_before_apply(op, root, args, &argv).await?;

    // Re-verify the token AFTER the (potentially slow) rail checks: the
    // caller could have raced the window. Cheap; correctness first.
    if args.get("preview_token").and_then(Value::as_str).is_some() {
        let expected = token_now(root, op, &argv)?;
        let supplied = args
            .get("preview_token")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if supplied != expected {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                "preview_token no longer matches: the repository state changed while \
                 the apply was being validated"
                    .to_string(),
                "Request a fresh plan".to_string(),
            ));
        }
    }

    let output = run_mutation(root, &argv).await?;

    let mut payload = json!({
        "action": op,
        "mutating": true,
        "applied": true,
        "dry_run": false,
        "argv": argv,
    });
    match op {
        "add" => {
            let outcome = staging.expect("add always carries its staging outcome");
            payload["staged"] = json!(outcome.staged);
            payload["excluded"] = json!(
                outcome
                    .excluded
                    .iter()
                    .map(|(path, reason)| json!({ "path": path, "reason": reason }))
                    .collect::<Vec<_>>()
            );
            payload["warnings"] = json!(outcome.warnings);
        }
        "commit" => {
            payload["commit"] = commit_summary(root).await?;
        }
        "push" => {
            payload["push"] = push_payload(&output.stdout, root).await;
        }
        "stash" => {
            payload["note"] = Value::String("stash created; use action=stash pop/apply outside this surface or a follow-up plan".to_string());
        }
        _ => {}
    }
    if output.truncated_chars > 0 {
        payload["output_truncated_chars"] = json!(output.truncated_chars);
    }
    Ok(payload)
}

#[cfg(test)]
mod test {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
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
        std::fs::write(dir.path().join("f.txt"), "a\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        dir
    }

    /// A tracked file deleted from the worktree is invisible to the
    /// filesystem walk, so a bulk `paths:["."]` staged every surviving
    /// change and silently left the deletion unstaged — and naming the
    /// deleted file explicitly failed the canonicalizing containment check
    /// because it no longer exists. Both must stage the removal.
    #[test]
    fn test_staging_includes_deleted_tracked_files() {
        let repo = scratch();
        let root = repo.path();
        git(root, &["config", "commit.gpgSign", "false"]);
        std::fs::write(root.join("keep.txt"), "b\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "two"]);
        // Modify one tracked file and delete another from the worktree.
        std::fs::write(root.join("f.txt"), "changed\n").unwrap();
        std::fs::remove_file(root.join("keep.txt")).unwrap();

        // Bulk: the walk finds f.txt; git's inventory must add keep.txt.
        let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["."] })).unwrap();
        assert!(
            outcome.staged.iter().any(|p| p == "f.txt"),
            "modified file staged: {:?}",
            outcome.staged
        );
        assert!(
            outcome.staged.iter().any(|p| p == "keep.txt"),
            "deleted tracked file must be staged for removal: {:?}",
            outcome.staged
        );

        // Explicit: the deleted path is accepted although it no longer exists.
        let outcome =
            staging_outcome(root, &serde_json::json!({ "paths": ["./keep.txt"] })).unwrap();
        assert_eq!(outcome.staged, vec!["./keep.txt".to_string()]);

        // A deleted path scoped to a subdirectory only picks deletions there.
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/g.txt"), "g\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "three"]);
        std::fs::remove_file(root.join("sub/g.txt")).unwrap();
        let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["sub"] })).unwrap();
        assert!(
            outcome.staged.iter().any(|p| p == "sub/g.txt"),
            "{:?}",
            outcome.staged
        );
        assert!(
            !outcome.staged.iter().any(|p| p == "keep.txt"),
            "deletion outside the requested directory must not be staged: {:?}",
            outcome.staged
        );

        // Containment is still enforced: a nonexistent path that is NOT a
        // tracked deletion, or one that traverses out, is rejected.
        let rejected = |paths: serde_json::Value| match staging_outcome(root, &paths) {
            Err(error) => error.message,
            Ok(_) => panic!("{paths} must be rejected"),
        };
        assert!(
            rejected(serde_json::json!({ "paths": ["ghost.txt"] }))
                .contains("not a path inside this project")
        );
        assert!(
            rejected(serde_json::json!({ "paths": ["../outside"] }))
                .contains("not a path inside this project")
        );
    }

    /// A hook that floods stderr well past the OS pipe buffer (64 KiB on
    /// Linux). Before the fix, `run_mutation` read stdout to EOF first and
    /// only then stderr, so git blocked on the full stderr pipe, stdout never
    /// reached EOF, and the call hung until the 300s timeout.
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
        let payload: Vec<u8> =
            std::iter::repeat_n(b'x', MAX_MUTATION_OUTPUT_BYTES + 1000).collect();
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
    fn test_secret_names_are_classified() {
        assert!(is_secret_path(".env"));
        assert!(is_secret_path("config/.env.production"));
        assert!(is_secret_path("server.pem"));
        assert!(is_secret_path("keys/id_rsa"));
        assert!(is_secret_path("client.key"));
        assert!(is_secret_path("credentials/google.json"));
        assert!(is_secret_path("my_secret_notes.txt"));
        assert!(!is_secret_path("src/key.rs"));
        assert!(!is_secret_path("environment.rs"));
    }

    #[test]
    fn test_runtime_artifacts_are_classified() {
        assert!(is_runtime_artifact(".maestro/operations/1-lock/lock"));
        assert!(is_runtime_artifact(".maestro/operations/9x/lock"));
        assert!(is_runtime_artifact("selection.json"));
        assert!(is_runtime_artifact("initializer.json"));
        assert!(is_runtime_artifact("deep/nested/target/debug/blob"));
        assert!(is_runtime_artifact("logs/server.log"));
        assert!(!is_runtime_artifact("src/lock.rs"));
        assert!(!is_runtime_artifact("selection_reader.rs"));
        assert!(!is_runtime_artifact("initializer.rs"));
    }

    #[test]
    fn test_staging_report_excludes_and_warns_on_bulk_add() {
        let repo = scratch();
        let root = repo.path();
        std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
        std::fs::write(root.join("keep.rs"), "fn a() {}\n").unwrap();
        std::fs::create_dir_all(root.join(".maestro/operations/j1")).unwrap();
        std::fs::write(root.join(".maestro/operations/j1/lock"), "1").unwrap();

        let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["."] })).unwrap();
        assert!(outcome.staged.contains(&"keep.rs".to_string()));
        assert!(
            !outcome.staged.iter().any(|p| p == ".env"),
            "{:?}",
            outcome.staged
        );
        assert!(
            !outcome
                .staged
                .iter()
                .any(|p| p.contains(".maestro/operations")),
            "{:?}",
            outcome.staged
        );
        assert!(
            outcome
                .excluded
                .iter()
                .any(|(p, r)| p == ".env" && *r == "likely secret")
        );
        assert!(
            outcome
                .excluded
                .iter()
                .any(|(p, r)| p.ends_with("/lock") && *r == "runtime artefact")
        );
        assert!(outcome.warnings.iter().any(|w| w.contains(".env")));
    }

    #[test]
    fn test_explicit_secret_path_overrides_the_exclusion_with_a_warning() {
        let repo = scratch();
        let root = repo.path();
        std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
        let outcome = staging_outcome(root, &serde_json::json!({ "paths": [".env"] })).unwrap();
        assert_eq!(outcome.staged, vec![".env".to_string()]);
        assert!(outcome.warnings.iter().any(|w| w.contains(".env")));
    }

    #[test]
    fn test_allow_sensitive_stages_directory_discovered_secrets() {
        let repo = scratch();
        let root = repo.path();
        std::fs::write(root.join(".env"), "TOKEN=x\n").unwrap();
        let outcome = staging_outcome(
            root,
            &serde_json::json!({ "paths": ["."], "allow_sensitive": true }),
        )
        .unwrap();
        assert!(outcome.staged.contains(&".env".to_string()));
        assert!(outcome.excluded.is_empty());
    }

    #[test]
    fn test_large_and_binary_files_are_warned() {
        let repo = scratch();
        let root = repo.path();
        std::fs::write(root.join("big.bin"), [0u8; 1024 * 1024 + 42]).unwrap();
        let outcome = staging_outcome(root, &serde_json::json!({ "paths": ["big.bin"] })).unwrap();
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.contains("MiB") && w.contains("big.bin"))
        );
        assert!(outcome.warnings.iter().any(|w| w.contains("binary")));
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

    /// The token binds repository state: once HEAD moves, a plan's token must
    /// be rejected instead of replaying the reviewed argv against a moved
    /// branch (brief §5 token binding).
    ///
    /// The token is derived through the EXPORTED seam ([`token_now`]) — never
    /// by rebuilding the state string here. An earlier version hand-built
    /// `"{head} {index}"` with a SPACE while [`state_binding`] joins with NUL,
    /// so the apply rejected a token that never matched for the WRONG reason
    /// and only the loose message assertion kept it green: it proved the
    /// separator, not the binding.
    #[tokio::test]
    async fn test_apply_rejects_a_token_after_head_moves() {
        let repo = scratch();
        let root = repo.path();
        let args = serde_json::json!({ "target": "HEAD" });
        let argv = plan_argv_for("checkout", root, &args).unwrap();
        let token = token_now(root, "checkout", &argv).unwrap();

        // Sanity: the token IS valid for the state it was issued against, so
        // the rejection below can only come from the HEAD move.
        assert_eq!(
            token_now(root, "checkout", &argv).unwrap(),
            token,
            "an unmoved repository must reproduce the plan's token"
        );

        // Move HEAD after the plan: the token's state is now stale.
        std::fs::write(root.join("f.txt"), "changed\n").unwrap();
        git(root, &["commit", "-aqm", "two"]);
        assert_ne!(
            token_now(root, "checkout", &argv).unwrap(),
            token,
            "the state binding must change when HEAD moves"
        );

        let error = handle(
            "checkout",
            root,
            &serde_json::json!({ "target": "HEAD", "preview_token": token }),
        )
        .await
        .expect_err("a stale token must not apply");
        let message = error.message_with_hint();
        assert!(
            message.contains("no longer matches") || message.contains("fresh plan"),
            "stale token must be refused with guidance: {error}"
        );
    }
}
