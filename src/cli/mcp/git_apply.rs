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

// The process-execution half of this module now lives in `git_apply_exec`
// (this file passed the 2000-line limit).
use super::git_apply_exec::{
    MutationOutput, git_command_sync, run_mutation, run_mutation_with_env,
};
use super::git_diff_handler::MAX_DIFF_BYTES;
use super::git_handler::{argument_warnings_for, plan_argv_for, preview_token};
use super::helpers::validate_file_within_project;
use super::protocol::JsonRpcError;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Operations the `confirm: true` shortcut may apply without a prior plan.
/// `pub(crate)`: the git_handler test pins the plan hint to exactly this set.
pub(crate) const SAFE_TO_CONFIRM: [&str; 3] = ["add", "commit", "push"];

/// Operations that only a prior plan's token may apply.
const TOKEN_ONLY: [&str; 5] = ["checkout", "stash", "tag", "merge", "rebase"];

/// Deadline for the `check-ignore` probe. It is a rules lookup, not a tree
/// walk, so it is fast; the deadline exists so a wedged git cannot hang a
/// staging request with no timeout running at all.
const IGNORE_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

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
            component.as_os_str().to_str().is_some_and(|c| {
                c.eq_ignore_ascii_case("credentials") || c.eq_ignore_ascii_case("secrets")
            })
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

/// Paths among `candidates` that git's ignore rules match.
///
/// `git status --porcelain=v2` reports ignored entries only when `--ignored`
/// is passed, and the shared status call does not pass it — so
/// `GitStatus::ignored` is empty in practice and filtering on it silently
/// filtered nothing (round-8 review, id 4691119051). Asking git directly is
/// exact and cheap: `check-ignore` answers from the rules alone, without
/// walking the worktree. It also skips TRACKED files by design, which is what
/// we want — a tracked file that matches an ignore pattern (`app.log` under
/// `*.log`) stages fine and must not be dropped from the argv.
///
/// Both directions are NUL-delimited, so a path containing a newline cannot be
/// read as two records. Exit 1 means "none of these are ignored" — a normal
/// answer, not a failure. Any other non-zero exit yields an empty set: the
/// bulk add then proceeds un-filtered, which is the pre-fix behaviour, instead
/// of failing the whole add because the ignore query itself broke.
/// Paths among `candidates` that git's ignore rules match.
///
/// `Err` means the ignore answer is UNKNOWN (git failed, or timed out): the
/// caller must refuse the staging request rather than proceed un-filtered.
/// Treating an unknown as "nothing is ignored" was itself a hole — it let a
/// broken probe stage exactly the files the filter exists to exclude.
///
/// Both directions are pumped CONCURRENTLY under one deadline (CodeRabbit
/// round 10, id 4697090116): `check-ignore` writes matches to stdout while it
/// is still reading stdin, so writing a multi-hundred-KB payload to
/// completion before reading stdout deadlocks once both pipes fill — and it
/// deadlocks BEFORE any timeout would start.
///
/// NUL-delimited both ways, so a path containing a newline cannot be read as
/// two records. Exit 1 means "none of these are ignored" — a normal answer.
/// Feed `payload` to `stdin` and drain `stdout` CONCURRENTLY, then wait out the
/// child under one deadline.
///
/// Both directions must run at once: a single thread writing while the child
/// fills its stdout pipe would deadlock. Both threads therefore REPORT their
/// result (CodeRabbit round 11, id 4698955155) instead of discarding it — a
/// partial reply must never look like "nothing is ignored".
///
/// A wedged git is killed and the request refused: an unknown ignore answer
/// must never stage anything.
fn run_check_ignore(
    mut child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout_pipe: Option<std::process::ChildStdout>,
    payload: Vec<u8>,
) -> Result<String, JsonRpcError> {
    // Dropping the handle closes stdin -> `check-ignore` sees EOF and exits.
    // The reply is read uncapped: it is bounded by the request (one record per
    // ignored candidate), and capping it would silently drop ignored paths and
    // then stage them.
    let feeder = std::thread::spawn(move || {
        use std::io::Write;
        if let Some(mut input) = stdin {
            let mut payload = &payload[..];
            std::io::copy(&mut payload, &mut input)?;
            input.flush()?;
        }
        Ok::<(), std::io::Error>(())
    });
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout_pipe {
            pipe.read_to_end(&mut buf)?;
        }
        Ok::<Vec<u8>, std::io::Error>(buf)
    });

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < IGNORE_QUERY_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(JsonRpcError::internal_error(
                    "git check-ignore exceeded its deadline; the staging request was \
                     refused because the ignore answer is unknown"
                        .to_string(),
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(JsonRpcError::internal_error(format!(
                    "git check-ignore wait failed: {error}"
                )));
            }
        }
    };
    let _ = feeder.join();
    let stdout: String = match reader.join() {
        Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok(Err(error)) => {
            return Err(JsonRpcError::internal_error(format!(
                "reading git check-ignore's reply failed ({error}); the staging request \
                 was refused because the ignore answer is incomplete"
            )));
        }
        Err(_) => {
            return Err(JsonRpcError::internal_error(
                "git check-ignore's reply could not be read; the staging request was \
                 refused because the ignore answer is unknown"
                    .to_string(),
            ));
        }
    };

    // Exit 1 is a normal answer ("none of these are ignored"), not a failure.
    if !status.success() && status.code() != Some(1) {
        return Err(JsonRpcError::internal_error(format!(
            "git check-ignore failed ({}); the staging request was refused because the \
             ignore answer is unknown",
            status.code().unwrap_or(-1)
        )));
    }
    Ok(stdout)
}

fn ignored_paths(
    root: &Path,
    candidates: &[String],
) -> Result<std::collections::BTreeSet<String>, JsonRpcError> {
    let mut ignored = std::collections::BTreeSet::new();
    if candidates.is_empty() {
        return Ok(ignored);
    }
    let mut payload = Vec::new();
    for candidate in candidates {
        payload.extend_from_slice(candidate.as_bytes());
        payload.push(0);
    }
    // The spawn uses the SAME scrubbed builder as every other git child, so an
    // inherited GIT_DIR/GIT_WORK_TREE cannot redirect the probe.
    let mut command = git_command_sync(root);
    command
        .args(["check-ignore", "-z", "--stdin"])
        // stdin MUST be piped: `check-ignore --stdin` blocks reading it until
        // EOF. Without this the child inherited a null/terminal stdin, the
        // feeder thread had nothing to write to, and every staging call sat
        // for the full deadline before failing.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().map_err(|error| {
        JsonRpcError::internal_error(format!("failed to spawn git check-ignore: {error}"))
    })?;
    let stdin = child.stdin.take();
    let stdout_pipe = child.stdout.take();
    let stdout = run_check_ignore(child, stdin, stdout_pipe, payload)?;
    for record in stdout.as_bytes().split(|byte: &u8| *byte == 0) {
        if let Ok(text) = std::str::from_utf8(record)
            && !text.is_empty()
        {
            ignored.insert(text.replace('\\', "/"));
        }
    }
    Ok(ignored)
}

/// Expand the requested `paths` into concrete files.
///
/// Directories are walked (`.git` and build-output directories skipped)
/// with a hard entry budget, so a caller pointing at the repo root cannot
/// blow the response; the skipped remainder is returned for the warning.
fn expand_paths(
    root: &Path,
    requested: &[String],
) -> Result<(Vec<String>, usize, Vec<String>, bool), JsonRpcError> {
    const MAX_DISCOVERED: usize = 2_000;

    let mut files = Vec::new();
    let mut skipped = 0usize;
    // Set when the walk stops on its BUDGET or DEPTH limit instead of
    // exhausting the tree. `skipped` counts files it saw and dropped; a
    // subtree it never entered contributes nothing, so without this flag the
    // response could claim a complete bulk add that was actually partial
    // (CodeRabbit round 8, id 4691119046).
    let mut incomplete = false;
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
            &mut incomplete,
        )?;
    }
    // Bulk discovery must not hand git the name of an IGNORED file: `git add`
    // rejects an explicitly named ignored path ("Use -f if you really want to
    // add it"), so an ordinary `paths:["."]` failed outright, and any file it
    // had already staged stayed staged (CodeRabbit round 8, id 4691119051).
    // Git's own ignore rules know exactly which files those are; the status
    // dump does NOT (it omits ignored entries unless `--ignored` is passed),
    // so the filter asks the rules directly.
    if !dirs.is_empty() {
        let ignored = ignored_paths(root, &files)?;
        files.retain(|file| !ignored.contains(&lexical_relative(file)));
        // The filesystem walk can only see files that exist. A tracked file
        // deleted from the worktree is invisible to it, so a bulk
        // `paths:["."]` used to stage every surviving change and silently leave
        // the deletions unstaged. Git's own inventory knows about them.
        if let Ok(status) = crate::cli::git::status(root) {
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
    }
    files.sort();
    files.dedup();
    Ok((files, skipped, dir_rels, incomplete))
}

/// Recursive worker for [`expand_paths`]. `budget` is shared per directory
/// request; when it runs out the walk stops, `skipped` counts the rest and
/// `incomplete` is set.
#[allow(clippy::too_many_arguments)]
fn walk_dir(
    root: &Path,
    rel: &str,
    absolute: &Path,
    depth: usize,
    files: &mut Vec<String>,
    skipped: &mut usize,
    budget: &mut usize,
    incomplete: &mut bool,
) -> Result<(), JsonRpcError> {
    const SKIP_DIR_NAMES: [&str; 2] = [".git", "node_modules"];
    if depth > 16 || *budget == 0 {
        *incomplete = true;
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
                incomplete,
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
    staging_outcome_inner(root, args)
}

/// Run staging preparation on a BLOCKING thread (CodeRabbit round 11, id
/// 4698955145).
///
/// `staging_outcome` polls a `check-ignore` child for up to
/// [`IGNORE_QUERY_TIMEOUT`] and walks the tree synchronously. Called from an
/// async handler it parks a worker for that whole time, so a handful of
/// concurrent bulk adds can exhaust the runtime BEFORE the mutation timeout
/// has even started. The async entry points go through this instead; the
/// synchronous one stays for the CLI/sync callers and the tests.
pub(crate) async fn staging_outcome_async(
    root: &Path,
    args: &Value,
) -> Result<StagingOutcome, JsonRpcError> {
    let root = root.to_path_buf();
    let args = args.clone();
    tokio::task::spawn_blocking(move || staging_outcome_inner(&root, &args))
        .await
        .map_err(|error| {
            JsonRpcError::internal_error(format!("staging preparation failed: {error}"))
        })?
}

/// Resolve the requested `paths` into the list to stage, refusing anything
/// outside the project.
///
/// Tracked files deleted from the worktree are stageable (`git add` records the
/// removal) but no longer exist on disk, so the canonicalizing containment
/// check cannot resolve them. They are accepted when git reports them deleted
/// AND their lexical, normalised form stays inside the project — a deletion can
/// never escape the root it is reported relative to.
fn requested_paths(root: &Path, args: &Value) -> Result<Vec<String>, JsonRpcError> {
    let paths = args.get("paths").and_then(Value::as_array).ok_or_else(|| {
        JsonRpcError::invalid_params_with_suggestion(
            "git add requires a non-empty 'paths' array".to_string(),
            "Provide paths: project-relative files to stage".to_string(),
        )
    })?;
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
        if validate_file_within_project(raw, root).is_err()
            && !deleted.contains(&lexical_relative(raw))
        {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                format!("'{raw}' is not a path inside this project"),
                "Stage project-relative paths only".to_string(),
            ));
        }
        requested.push(raw.to_string());
    }
    if requested.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "git add requires at least one path".to_string(),
        ));
    }
    Ok(requested)
}

/// What one expanded file does to the staging report.
enum Disposition {
    /// Excluded from the argv, with the reason shown to the caller.
    Exclude(&'static str),
    /// Staged, possibly with a warning.
    Stage(Vec<String>),
}

/// Classify one expanded file:
/// * secret/runtime found under a DIRECTORY path — excluded (and listed)
///   unless `allow_sensitive: true`, in which case it is staged with a warning;
/// * secret/runtime named EXPLICITLY — always staged (the caller asked for
///   exactly this path) with a warning;
/// * everything else — staged, with warnings for files over 1 MiB and probable
///   binaries.
fn classify_staged_file(
    root: &Path,
    relative: &str,
    is_explicit: bool,
    allow_sensitive: bool,
) -> Disposition {
    let secret = is_secret_path(relative);
    let sensitive = secret || is_runtime_artifact(relative);
    if sensitive && !is_explicit && !allow_sensitive {
        return Disposition::Exclude(if secret {
            "likely secret"
        } else {
            "runtime artefact"
        });
    }
    let mut warnings = Vec::new();
    if sensitive {
        warnings.push(format!("'{relative}' looks like a secret or runtime artefact; staging it because it was named explicitly or allow_sensitive is set"));
    }
    let absolute = root.join(relative);
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
    Disposition::Stage(warnings)
}

/// Announce a directory walk that did not cover the whole tree.
///
/// `skipped` counts entries it saw and dropped. The walk can also stop BEFORE
/// entering a subtree, in which case it never saw the files there and
/// `skipped` stays 0 — say so anyway, because silence would claim a complete
/// bulk add (CodeRabbit round 8, id 4691119046).
fn enumeration_warnings(skipped: usize, incomplete: bool) -> Vec<String> {
    if skipped > 0 {
        return vec![format!(
            "directory walk hit the {}-entry budget; {} files were not enumerated \
             — pass narrower paths to cover them",
            2_000, skipped
        )];
    }
    if incomplete {
        return vec![
            "directory walk stopped at its depth/entry limit; enumeration is INCOMPLETE \
             (unknown remainder) — pass narrower paths to cover the rest"
                .to_string(),
        ];
    }
    Vec::new()
}

fn staging_outcome_inner(root: &Path, args: &Value) -> Result<StagingOutcome, JsonRpcError> {
    let requested = requested_paths(root, args)?;
    let allow_sensitive = args
        .get("allow_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let explicit = requested
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let (discovered, skipped, dirs, incomplete) = expand_paths(root, &requested)?;

    let mut staged = Vec::new();
    let mut excluded = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for relative in discovered {
        if !seen.insert(relative.clone()) {
            continue;
        }
        let is_explicit = explicit.contains(&relative);
        match classify_staged_file(root, &relative, is_explicit, allow_sensitive) {
            Disposition::Exclude(reason) => excluded.push((relative, reason)),
            Disposition::Stage(mut notes) => {
                warnings.append(&mut notes);
                staged.push(relative);
            }
        }
    }
    staged.sort();
    staged.dedup();
    warnings.extend(enumeration_warnings(skipped, incomplete));
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
///
/// `--literal-pathspecs` is a GLOBAL git option, so it goes before the
/// subcommand. `--` stops option parsing but NOT pathspec expansion: a
/// directory containing a file literally named `*` would otherwise be handed
/// to git as a wildcard, and `git add -- "*"` stages everything — including
/// the `.env` the guardrail just excluded (CodeRabbit round 8, id
/// 4691119058). Literal mode makes every enumerated name mean exactly itself.
pub(crate) fn add_argv(outcome: &StagingOutcome) -> Vec<String> {
    let mut argv = vec![
        "--literal-pathspecs".to_string(),
        "add".to_string(),
        "--".to_string(),
    ];
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
///
/// The inventory is taken at the WORKTREE ROOT, not at `root`. `git ls-files`
/// run from a subdirectory lists only that subtree, yet a `commit` with no
/// `paths` commits the whole staged set — so staging a sibling after the plan
/// left HEAD and this digest untouched and let the old token authorize a
/// commit containing changes the plan never showed. Verified: from a member
/// directory the listing is byte-identical before and after staging a sibling,
/// while `git diff --cached` there names that sibling.
fn index_digest(root: &Path) -> Result<String, JsonRpcError> {
    let (text, truncated) = super::git_diff_handler::git_output_capped(
        &index_root(root),
        &["ls-files".to_string(), "-s".to_string()],
        MAX_DIFF_BYTES,
    )
    .map_err(|error| JsonRpcError::internal_error(format!("index probe failed: {error}")))?;
    digest_listing(&text, truncated)
}

/// Hash one `git ls-files -s` listing, refusing a TRUNCATED one.
///
/// A truncated listing hashes only a prefix: entries past the cap could be
/// staged or unstaged without moving the token, so a reviewed plan could
/// authorize a commit carrying content it never showed. Refusing to bind
/// state we cannot see is the only honest answer — the alternative is a token
/// that silently certifies a partial view.
fn digest_listing(listing: &str, truncated: bool) -> Result<String, JsonRpcError> {
    if truncated {
        return Err(JsonRpcError::invalid_params_with_suggestion(
            format!(
                "index inventory exceeds {MAX_DIFF_BYTES} bytes; cannot bind the mutation token \
                 to the whole staged set"
            ),
            "Commit from a narrower repository, or unstage the paths the plan did not cover"
                .to_string(),
        ));
    }
    let mut digest = blake3::Hasher::new();
    for line in listing.lines() {
        digest.update(line.as_bytes());
        digest.update(b"\n");
    }
    Ok(digest.finalize().to_hex().to_string())
}

/// The directory whose index `index_digest` must hash: the worktree root that
/// `git` resolves for `root`, or `root` itself when git cannot say (not a
/// repository, or `rev-parse` unavailable).
fn index_root(root: &Path) -> PathBuf {
    super::git_diff_handler::git_output_capped(
        root,
        &["rev-parse".to_string(), "--show-toplevel".to_string()],
        4096,
    )
    .ok()
    .map(|(text, _)| PathBuf::from(text.trim()))
    .filter(|toplevel| !toplevel.as_os_str().is_empty() && toplevel.is_dir())
    .unwrap_or_else(|| root.to_path_buf())
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

/// The live state a mutation token binds.
///
/// ONE binding, used at both ends of the seam: `mutation_plan` issues the
/// advertised token against it, `validate_before_apply` recomputes it before
/// spawning git. Issuing against an empty string (the pre-fix behaviour) made
/// every plan-issued token unmatchable, which silently disabled every
/// `TOKEN_ONLY` operation and every destructive shape.
///
/// ## What "state" has to mean (CodeRabbit round 8, ids 4691119074 and
/// 4691119087)
///
/// HEAD + the index are necessary but NOT sufficient. Git re-resolves several
/// things when the command runs, and each is state the plan reviewed:
///
/// * a `checkout`/`merge`/`rebase`/`reset` TARGET NAME. If `master` moves
///   after planning, HEAD and the index do not change, the old token still
///   matched, and git resolved the name again at run time — acting on a
///   commit the plan never showed. The commit the name resolves to is bound.
/// * the REMOTE branch a force-push overwrites. `--force-with-lease` with no
///   expected value is an implicit lease against the CURRENT remote-tracking
///   ref, so a background `git fetch` that advanced it made the lease
///   satisfied by exactly the commit the plan never saw, and the push
///   silently replaced someone else's work (verified against git 2.56.1:
///   `+ 1a989c8...027b451 (forced update)`). The remote-tracking oid is bound
///   and the lease carries it explicitly (see `git_handler::force_push_argv`).
///
/// The name stays in the argv on purpose — `git checkout <branch>` attaches
/// HEAD while `<oid>` detaches it, and a merge by name records the name in its
/// default message — so the oid goes in the STATE, not the argv. A moved name
/// then changes the state, and the reviewed token no longer applies.
///
/// Branch deletion (`-d <branch>`) and `clean -fd` name no revision, so they
/// bind nothing extra: for them HEAD + index already cover what the command
/// acts on.
pub(crate) fn state_binding(
    root: &Path,
    op: &str,
    argv: &[String],
) -> Result<String, JsonRpcError> {
    let mut state = format!(
        "{}\u{0}{}",
        head_oid(root).unwrap_or_else(|| "none".to_string()),
        index_digest(root)?
    );
    // The SYMBOLIC target of HEAD is bound too (CodeRabbit round 10, id
    // 4697090161). Two branches can name the SAME commit, so HEAD's OID alone
    // cannot tell them apart: switching `main` -> `topic` at one commit left
    // every other part of the state identical, the token still matched, and
    // the commit advanced whichever branch HEAD pointed at by publish time.
    // Binding the branch name makes the switch itself invalidate the token.
    // Detached HEAD has no branch to bind; it is recorded as `detached` so it
    // is still distinguishable from "attached to some branch".
    state.push('\u{0}');
    state.push_str(&head_symbolic_ref(root));
    // The PENDING MERGE is bound too (CodeRabbit round 11, id 4698955164).
    // `HEAD` + index digest cannot see a merge: `git merge --quit` forgets
    // MERGE_HEAD and leaves both untouched, so a plan reviewed during a merge
    // still validated and then published a SINGLE-parent commit — the merge
    // was silently dropped. Binding the parent ids makes the quit invalidate
    // the token, and the capture below re-checks them before publishing.
    state.push('\u{0}');
    state.push_str(&merge_parent_ids(root).join(","));
    for oid in resolved_target_oids(root, op, argv) {
        state.push('\u{0}');
        state.push_str(&oid);
    }
    Ok(state)
}

/// Resolve a revision name to the commit it names, as a full oid.
///
/// Scrubbed like every other probe: an inherited `GIT_DIR` would otherwise
/// resolve the name in a DIFFERENT repository. Fails loudly (not `None`) so a
/// bad target is the caller's error, not a silently-empty response.
pub(crate) fn resolve_to_oid(root: &Path, target: &str) -> Result<String, JsonRpcError> {
    let (text, _) = git_read(
        root,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "--quiet".to_string(),
            format!("{target}^{{commit}}"),
        ],
    )?;
    let oid = text.trim();
    if is_oid(oid) {
        Ok(oid.to_string())
    } else {
        Err(JsonRpcError::invalid_params(format!(
            "'{target}' does not name a commit"
        )))
    }
}

/// The branch HEAD is attached to, as a full ref — or `detached` when it is
/// not attached to one.
///
/// Bound into the token state so that checking out a different branch — even
/// one pointing at the same commit — invalidates the plan.
fn head_symbolic_ref(root: &Path) -> String {
    git_read(
        root,
        &[
            "symbolic-ref".to_string(),
            "--quiet".to_string(),
            "HEAD".to_string(),
        ],
    )
    .ok()
    .map(|(text, _)| text.trim().to_string())
    .filter(|r| r.starts_with("refs/"))
    .unwrap_or_else(|| "detached".to_string())
}

/// The oids a plan has to bind beyond HEAD and the index, in a stable order.
///
/// A name that does not resolve is bound as the literal `unresolved:<name>`
/// rather than skipped: a target that did not resolve at plan time must not
/// become resolvable later under the same token. The unresolved fact is bound
/// too, so the token cannot be reused for a different resolution.
fn resolved_target_oids(root: &Path, op: &str, argv: &[String]) -> Vec<String> {
    match op {
        // The four revision-taking operations: bind the commit each name
        // resolves to NOW, so a moved branch invalidates the token.
        "checkout" | "merge" | "rebase" | "reset" => {
            // The name is the LAST argument for every revision form:
            // `checkout --no-guess <t>`, `merge --no-edit <t>`,
            // `rebase <t>`, `reset --hard|--mixed <t>`.
            let Some(name) = argv.last() else {
                return Vec::new();
            };
            // Flag-shaped targets (`reset -d <branch>`, `reset clean -fd`)
            // are not revisions; `-d`'s branch is a deletion, not a commit.
            if name.starts_with('-') || argv.iter().any(|arg| arg == "-d") {
                Vec::new()
            } else {
                vec![resolve_oid(root, name)]
            }
        }
        // Force-push: bind the remote branch it will overwrite. The lease
        // (`git_handler::force_push_argv`) is built from the SAME resolution,
        // so the argv the token binds and the oid the state binds agree.
        "force" => match force_refspec(argv) {
            Some((remote, branch)) => vec![resolve_oid(
                root,
                &format!("refs/remotes/{remote}/{branch}"),
            )],
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The `<remote>, <branch>` pair a force-push argv targets, when it names one.
///
/// Mirrors `git_handler::force_push_argv`: `push --force-with-lease[=…]
/// <remote> HEAD:refs/heads/<branch>`. A bare remote (`push … origin`) keeps
/// git's own upstream resolution and names no branch to bind.
///
/// The remote is the argument immediately BEFORE the `HEAD:refs/heads/…`
/// refspec, read from the argv itself — never assumed to be `origin`, since
/// the plan may name any configured remote.
fn force_refspec(argv: &[String]) -> Option<(String, String)> {
    let refspec_at = argv
        .iter()
        .position(|arg| arg.starts_with("HEAD:refs/heads/"))?;
    let branch = argv[refspec_at]
        .strip_prefix("HEAD:refs/heads/")?
        .to_string();
    let remote = argv.get(refspec_at.checked_sub(1)?)?;
    if remote.is_empty() || remote.starts_with('-') {
        return None;
    }
    Some((remote.clone(), branch))
}

/// The remote-tracking oid a force-push lease must pin, when it names a
/// remote branch.
///
/// ONE resolution shared by the argv builder (`force_push_argv`) and the
/// token state (`resolved_target_oids`): the lease the command carries and the
/// oid the token binds are read from the same place, so the two cannot
/// disagree about what "reviewed" means.
///
/// `None` when the shape names no branch (bare remote) or the branch does not
/// resolve yet (unborn) — the caller then falls back to git's implicit lease.
pub(crate) fn force_lease_oid(root: &Path, argv: &[String]) -> Option<String> {
    let (remote, branch) = force_refspec(argv)?;
    let oid = resolve_oid(root, &format!("refs/remotes/{remote}/{branch}"));
    // `is_oid` accepts both hash lengths (CodeRabbit round 11, id 4698955173):
    // a SHA-256 repository's 64-digit oid used to yield NO lease at all, so a
    // force-push there fell back to the implicit (unsafe) lease.
    is_oid(&oid).then_some(oid)
}

/// Accept git's `rev-parse` answer as a commit id, or reject it.
///
/// The length test MUST come from [`is_oid`], not a literal 40: in a SHA-256
/// repository every valid object id is 64 hex digits, so a hardcoded 40
/// rejected all of them and bound the target as `unresolved:<name>`. That is
/// not a cosmetic label — the binding is what makes a stale preview token
/// detect a moved target. With the target unresolved, moving the branch left
/// every hashed input (HEAD, index, argv) unchanged, so `checkout`/`merge`/
/// `rebase`/`reset` would apply against a commit the caller never previewed.
fn accepted_oid(text: &str) -> Option<String> {
    let oid = text.trim();
    is_oid(oid).then(|| oid.to_string())
}

/// Resolve a revision name to the commit it names RIGHT NOW.
///
/// `--verify --quiet` makes an unknown name a failure instead of echoing the
/// input back, so a typo can never bind as if it were a commit.
fn resolve_oid(root: &Path, name: &str) -> String {
    let resolved = super::git_diff_handler::git_output_capped(
        root,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "--quiet".to_string(),
            format!("{name}^{{commit}}"),
        ],
        4096,
    );
    match resolved {
        Ok((text, _)) => accepted_oid(&text).unwrap_or_else(|| format!("unresolved:{name}")),
        Err(_) => format!("unresolved:{name}"),
    }
}

/// Recompute the preview token for `(op, argv)` against the CURRENT state.
///
/// The token is the plan's identity AND its validity window: it changes when
/// the argv changes (caller typo, tree grew) and when the state changes
/// (HEAD moved, index changed, a bound target moved), so a stale token can
/// never apply.
///
/// `pub(crate)` so `git_handler::mutation_plan` issues its advertised token
/// through this exact function — a plan token that is not the apply token is
/// a dead seam.
pub(crate) fn token_now(root: &Path, op: &str, argv: &[String]) -> Result<String, JsonRpcError> {
    Ok(preview_token(
        root,
        op,
        argv,
        &state_binding(root, op, argv)?,
    ))
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

/// Everything the publish step is allowed to know, captured ONCE at
/// validation time. Nothing here is re-read later.
#[derive(Debug)]
pub(super) struct ValidatedCommit {
    /// Frozen copy of the validated index (removed on drop).
    pub index: tempfile::TempPath,
    /// Message with trailers composed in git's trailer-block form.
    pub message: String,
    /// The ref the commit must advance: `refs/heads/<branch>` when HEAD is
    /// attached, or `HEAD` when detached. Never re-resolved.
    pub target_ref: String,
    /// The OID `target_ref` must still name. `None` only when the branch is
    /// unborn AND validation saw it unborn.
    pub expected_old: Option<String>,
    /// Parents for `commit-tree`: HEAD plus every `MERGE_HEAD`, so a merge
    /// commit keeps both parents and the merge is COMPLETED, not bypassed.
    pub parents: Vec<String>,
    /// The `MERGE_HEAD` ids captured at validation, in file order.
    ///
    /// Kept SEPARATELY from `parents` so the publish step can compare ids, not
    /// a count (CodeRabbit round 12, id 4700055376): quitting merge A and
    /// starting merge B leaves `MERGE_HEAD` with one entry, so a count check
    /// passes and the commit is published with A while B is what is pending.
    pub merge_parents: Vec<String>,
    /// `Some(..)` when a merge was in progress, so the publish step clears it.
    pub merge_head: Option<PathBuf>,
}

/// Whether this apply is a commit that reads the WHOLE index.
///
/// `commit -- <paths>` commits the worktree contents of exactly those paths
/// and ignores what is staged for others, so the index is not what it
/// consumes. Only the pathspec-free form commits the staged set, and only
/// that form needs the validated-tree commit.
pub(super) fn commits_whole_index(op: &str, argv: &[String]) -> bool {
    op == "commit" && !argv.iter().any(|arg| arg == "--")
}

/// Every oid a pending merge would add as a parent, in order.
///
/// Empty when no merge is in progress, so the value is also a faithful
/// "merge or not" signal in the token state.
fn merge_parent_ids(root: &Path) -> Vec<String> {
    let Some(path) = merge_head_path(root) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| is_oid(line))
        .map(str::to_string)
        .collect()
}

/// A marker file's path inside the repository, when it exists.
///
/// `--git-path` answers relative to the repository root, so it must be
/// anchored on `root` before it is tested — a bare `exists()` resolves
/// against the process CWD. Same helper shape as [`merge_head_path`], shared
/// so a new marker cannot reintroduce the CWD bug.
fn git_marker_path(root: &Path, marker: &str) -> Option<PathBuf> {
    let (text, _) = git_read(
        root,
        &[
            "rev-parse".to_string(),
            "--git-path".to_string(),
            marker.to_string(),
        ],
    )
    .ok()?;
    let path = PathBuf::from(text.trim());
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    path.is_file().then_some(path)
}

/// The `MERGE_HEAD` path for `root`'s repository, when a merge is pending.
///
/// Resolved through git rather than assumed to be `.git/MERGE_HEAD`: in a
/// linked worktree `.git` is a FILE, and the real git dir lives elsewhere.
fn merge_head_path(root: &Path) -> Option<PathBuf> {
    let (text, _) = git_read(
        root,
        &[
            "rev-parse".to_string(),
            "--git-path".to_string(),
            "MERGE_HEAD".to_string(),
        ],
    )
    .ok()?;
    // `--git-path` answers RELATIVE to the repository root (`.git/MERGE_HEAD`),
    // so it must be anchored on `root` before it is tested: a bare `exists()`
    // resolves against the process CWD and silently reported "no merge" from
    // any other directory, which dropped the merge's second parent.
    let path = PathBuf::from(text.trim());
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    path.exists().then_some(path)
}

/// Turn a planned whole-index `git commit` into a commit of an IMMUTABLE tree.
///
/// Every mutable thing the publish step would otherwise re-read is captured
/// here, while the token's state is the state being validated:
///
/// * the HEAD OID — used BOTH as the parent and as the compare-and-swap old
///   value, so a HEAD that moved between validation and publish is rejected
///   instead of silently re-parented (4697090139);
/// * the symbolic target of HEAD — so the branch that gets advanced is the
///   branch that was reviewed, not whichever branch HEAD points at by the
///   time the update runs (4697090161);
/// * `MERGE_HEAD` — a pending merge is either completed with all its parents
///   or refused; a single-parent commit during a merge is never published
///   (4697090148);
/// * unborn-ness — when the branch has no commit yet, the update carries
///   git's must-not-exist expectation instead of no expectation (4697090175).
///
/// The index is resolved THROUGH GIT (`rev-parse --git-path index`), so a
/// linked worktree works; and a whole-index commit that cannot snapshot its
/// index is REFUSED rather than falling back to a plain `git commit`, because
/// that fallback reads the live index and runs hooks — the exact window this
/// path exists to close (4697090130).
pub(super) fn validated_tree_commit(
    root: &Path,
    argv: &[String],
) -> Result<ValidatedCommit, JsonRpcError> {
    let Some((message, trailers)) = commit_message_and_trailers(argv) else {
        return Err(JsonRpcError::internal_error(
            "whole-index commit plan carried no message".to_string(),
        ));
    };

    // Refuse while a cherry-pick or revert is in progress (CodeRabbit round
    // 11, id 4698955186). Publishing from the frozen index would move the
    // branch and leave CHERRY_PICK_HEAD / REVERT_HEAD behind, so git keeps
    // reporting the operation as in progress for a commit that already
    // landed. Resolved through git so linked worktrees are covered, and
    // the markers are NOT deleted here — the user finishes with git.
    for marker in ["CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        if let Some(path) = git_marker_path(root, marker) {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                format!(
                    "a {} is in progress ({}); commit it with git so the operation \
                     completes, or abort it, then re-plan",
                    if marker == "CHERRY_PICK_HEAD" {
                        "cherry-pick"
                    } else {
                        "revert"
                    },
                    path.display()
                ),
                format!(
                    "Run `git {} --continue` (or `--abort`), then re-plan the commit",
                    if marker == "CHERRY_PICK_HEAD" {
                        "cherry-pick"
                    } else {
                        "revert"
                    }
                ),
            ));
        }
    }

    // Resolve the real index through git: `.git/index` is wrong in a linked
    // worktree, where `.git` is a file pointing at the real git dir.
    let index_path = git_index_path(root)?;
    let bytes = std::fs::read(&index_path).map_err(|error| {
        JsonRpcError::invalid_params_with_suggestion(
            format!(
                "cannot snapshot the index at '{}': {error} — a whole-index commit \
                 must commit exactly the validated content",
                index_path.display()
            ),
            "Check that the index is readable and owned by you (a stale lock or a \
             permission change shows up here). To recover the staged set, stash and \
             re-stage, or restore the index from a backup; then re-plan. Committing with \
             explicit `paths` reads the WORKTREE instead, so it does not recover \
             staged-only changes"
                .to_string(),
        )
    })?;
    let mut snapshot = tempfile::NamedTempFile::new()
        .map_err(|error| JsonRpcError::internal_error(format!("index snapshot failed: {error}")))?;
    std::io::Write::write_all(&mut snapshot, &bytes)
        .map_err(|error| JsonRpcError::internal_error(format!("index snapshot failed: {error}")))?;
    let (_, index) = snapshot.into_parts();

    // Capture the publication facts NOW, against the validated state.
    let head = git_read(
        root,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "--quiet".to_string(),
            "HEAD".to_string(),
        ],
    )
    .ok()
    .map(|(text, _)| text.trim().to_string())
    .filter(|oid| is_oid(oid));
    let symbolic = git_read(
        root,
        &[
            "symbolic-ref".to_string(),
            "--quiet".to_string(),
            "HEAD".to_string(),
        ],
    )
    .ok()
    .map(|(text, _)| text.trim().to_string())
    .filter(|r| r.starts_with("refs/"));
    let merge_head = merge_head_path(root);

    // A failed HEAD probe is NOT the same as an unborn branch: a repository
    // that cannot answer is refused, not treated as empty.
    let (target_ref, expected_old) = match (&symbolic, &head) {
        (Some(branch), Some(oid)) => (branch.clone(), Some(oid.clone())),
        // Unborn: HEAD names a branch that has no commit yet.
        (Some(branch), None) => (branch.clone(), None),
        // Detached: no branch to bind, so the OID itself is the target.
        (None, Some(oid)) => ("HEAD".to_string(), Some(oid.clone())),
        (None, None) => {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                "cannot determine where this commit would be published: HEAD is neither \
                 a branch nor a commit"
                    .to_string(),
                "Repair HEAD (e.g. `git checkout <branch>`) and re-plan".to_string(),
            ));
        }
    };

    // Parents: HEAD, then every MERGE_HEAD line — the same parents
    // `git commit` would record, so a merge is completed rather than lost.
    let mut parents = head.into_iter().collect::<Vec<_>>();
    let mut merge_parents: Vec<String> = Vec::new();
    if let Some(merge) = &merge_head {
        // An AUTOSTASH merge is refused rather than silently mishandled
        // (CodeRabbit round 11, id 4698955200). Plain `git commit` applies the
        // saved edits when the merge completes; this path moves the branch and
        // clears MERGE_HEAD without touching the autostash, so the caller was
        // told "done" while their working-tree edits were still in the stash.
        if git_marker_path(root, "MERGE_AUTOSTASH").is_some()
            || git_marker_path(root, ".git/MERGE_AUTOSTASH").is_some()
        {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                "this merge was started with autostash; finishing it here would leave \
                 the stashed edits unapplied"
                    .to_string(),
                "Finish the merge with `git commit` (which applies the autostash), then \
                 re-plan if further commits are needed"
                    .to_string(),
            ));
        }
        let text = std::fs::read_to_string(merge).map_err(|error| {
            JsonRpcError::internal_error(format!("cannot read MERGE_HEAD: {error}"))
        })?;
        // The merge ids are kept as their OWN list as well as folded into
        // `parents`: the publish step compares the ids, and a count comparison
        // cannot tell "merge A" from "merge B replaced it" (CodeRabbit round
        // 12, id 4700055376).
        for line in text.lines() {
            let oid = line.trim();
            if is_oid(oid) {
                merge_parents.push(oid.to_string());
                parents.push(oid.to_string());
            }
        }
    }

    Ok(ValidatedCommit {
        index,
        message: compose_message(&message, &trailers),
        target_ref,
        expected_old,
        parents,
        merge_parents,
        merge_head,
    })
}

/// The message and trailer lines a planned commit argv carries.
fn commit_message_and_trailers(argv: &[String]) -> Option<(String, Vec<String>)> {
    let at = argv.iter().position(|arg| arg == "-m")?;
    let message = argv.get(at + 1)?.clone();
    let mut trailers = Vec::new();
    let mut cursor = at + 2;
    while argv.get(cursor).map(String::as_str) == Some("--trailer") {
        trailers.push(argv.get(cursor + 1)?.clone());
        cursor += 2;
    }
    Some((message, trailers))
}

/// Git's trailer block: a blank line, then the trailers, in the order given.
///
/// This is the shape `git commit --trailer` writes, reproduced because
/// `commit-tree` has no `--trailer` flag. Each line is already validated as
/// single-line and non-empty by `commit_argv`.
fn compose_message(message: &str, trailers: &[String]) -> String {
    if trailers.is_empty() {
        return message.to_string();
    }
    let mut composed = String::from(message.trim_end());
    composed.push_str("\n\n");
    composed.push_str(&trailers.join("\n"));
    composed.push('\n');
    composed
}

/// Whether `text` is a full object name in EITHER supported hash algorithm.
///
/// `pub(crate)` so the git handler's log parser applies the same rule instead
/// of repeating a 40-digit literal (CodeRabbit round 11, id 4698955173): a
/// SHA-256 repository's 64-digit commit ids are real, and a parser that
/// hardcodes 40 turns every one of them into a *file* record.
pub(crate) fn is_oid(text: &str) -> bool {
    // 40 hex digits in a SHA-1 repository, 64 in a SHA-256 one (CodeRabbit
    // round 11, id 4698955173). Hardcoding 40 rejected every SHA-256 object:
    // `show` failed with "does not name a commit", `validated_tree_commit`
    // read an existing branch as unborn, and the tree/commit checks rejected
    // objects git had just written.
    (text.len() == 40 || text.len() == 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The all-zero object name in THIS repository's hash algorithm.
///
/// `update-ref`'s "must not exist" expectation is the zero oid, and its
/// LENGTH must match the repository: a 40-zero value is not a valid SHA-256
/// object name, so an unborn branch in a SHA-256 repo could not be published.
/// Asked of git rather than guessed, and defaulting to SHA-1 only when the
/// probe itself fails (which is the overwhelmingly common case).
fn zero_oid(root: &Path) -> String {
    let length = match git_read(
        root,
        &["rev-parse".to_string(), "--show-object-format".to_string()],
    )
    .ok()
    .map(|(text, _)| text.trim().to_ascii_lowercase())
    .as_deref()
    {
        Some("sha256") => 64,
        _ => 40,
    };
    "0".repeat(length)
}

/// The repository's real index path, resolved through git.
fn git_index_path(root: &Path) -> Result<PathBuf, JsonRpcError> {
    let (text, _) = git_read(
        root,
        &[
            "rev-parse".to_string(),
            "--git-path".to_string(),
            "index".to_string(),
        ],
    )
    .map_err(|error| {
        JsonRpcError::invalid_params_with_suggestion(
            format!("cannot locate the git index: {error}"),
            "Run from inside the repository, or commit with explicit `paths`".to_string(),
        )
    })?;
    let raw = text.trim();
    // `--git-path` may return a path relative to `root`; anchor it.
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// Read-only git invocation under the scrubbed environment, capped.
///
/// Synchronous and bounded: these are the small probes the validated-commit
/// planner uses (HEAD, symbolic-ref, --git-path), not mutations.
fn git_read(root: &Path, argv: &[String]) -> Result<(String, bool), JsonRpcError> {
    // The probes the validated-commit planner uses (`rev-parse --git-path`,
    // `symbolic-ref`, `rev-parse HEAD`) must be scrubbed too: an inherited
    // GIT_DIR would otherwise redirect them, and the plan would be built
    // against a DIFFERENT repository than the one being committed to. The
    // shared runner does not clear those, so this goes through the scrubbed
    // builder directly.
    let mut command = git_command_sync(root);
    command
        .args(argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let output = command
        .output()
        .map_err(|error| JsonRpcError::internal_error(format!("git probe failed: {error}")))?;
    if !output.status.success() {
        // A failed probe is reported by the caller through the `Ok`/`Err`
        // distinction it already makes; surface git's own exit as a soft
        // failure so "unborn branch" is distinguishable from "broken repo".
        return Ok((String::new(), false));
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let truncated = text.len() > 4096;
    Ok((text, truncated))
}

/// Run one mutating git command under the hardened environment and return
/// its captured output. `index_snapshot` pins the index a `write-tree` reads.
///
/// This is the executor the validated-commit steps go through, so every
/// publication command gets the same env scrubbing and timeout as any other
/// mutation.
async fn git_run(
    root: &Path,
    argv: &[String],
    index_snapshot: Option<&Path>,
) -> Result<MutationOutput, JsonRpcError> {
    run_mutation_with_env(root, argv, index_snapshot).await
}

/// Publish the validated commit and return the OID that was published, plus
/// any warnings about post-publish cleanup that failed.
///
/// Three steps, each a separate process, so the tree is an immutable object
/// before anything else can touch it:
///
/// 1. `write-tree` from the frozen index;
/// 2. `commit-tree` with the captured parents (HEAD + MERGE_HEAD);
/// 3. `update-ref <target_ref> <new> <old>` — the CAS. When the branch was
///    unborn the old value is the empty OID, git's "must not exist" form, so a
///    first commit created concurrently cannot be replaced.
///
/// On success the published OID is RETURNED (4697090186): the summary must
/// describe this commit, not whatever HEAD happens to be by the time it is
/// read. A pending merge is then cleared so the operation is complete.
pub(super) async fn run_validated_commit(
    root: &Path,
    plan: &ValidatedCommit,
) -> Result<(String, Vec<String>), JsonRpcError> {
    // 1. Freeze the validated index as a tree object.
    let tree_output = git_run(root, &["write-tree".to_string()], Some(&plan.index)).await?;
    let tree = tree_output.stdout.trim();
    if !is_oid(tree) {
        return Err(JsonRpcError::internal_error(format!(
            "git write-tree did not return a tree id: {tree:?}"
        )));
    }

    // 2. Commit that tree with exactly the captured parents.
    let mut commit_argv = vec!["commit-tree".to_string(), tree.to_string()];
    for parent in &plan.parents {
        commit_argv.push("-p".to_string());
        commit_argv.push(parent.clone());
    }
    // `--no-gpg-sign` matches the planned argv: the plan promised an
    // unsigned commit, so the commit must not quietly sign.
    commit_argv.push("--no-gpg-sign".to_string());
    commit_argv.push("-m".to_string());
    commit_argv.push(plan.message.clone());
    let commit_output = git_run(root, &commit_argv, None).await?;
    let commit = commit_output.stdout.trim().to_string();
    if !is_oid(&commit) {
        return Err(JsonRpcError::internal_error(format!(
            "git commit-tree did not return a commit id: {commit:?}"
        )));
    }

    // 3. Advance the CAPTURED ref, conditional on it still naming the
    //    CAPTURED oid. An unborn branch uses the all-zero "must not exist"
    //    expectation, so the update cannot replace a first commit that
    //    appeared after validation.
    //
    //    A merge that vanished or was REPLACED between the capture and here
    //    would publish a merge commit whose recorded parents no longer
    //    describe the repository, so the ids are re-read and compared
    //    EXACTLY — a count comparison lets "quit merge A, start merge B"
    //    through, since both leave one entry (CodeRabbit round 12, ids
    //    4698955164 and 4700055376).
    if merge_parent_ids(root) != plan.merge_parents {
        return Err(JsonRpcError::invalid_params_with_suggestion(
            "the pending merge changed after validation; the captured parents no longer \
             describe the repository"
                .to_string(),
            "Re-plan the commit and review the parents before applying".to_string(),
        ));
    }
    let old = plan.expected_old.clone().unwrap_or_else(|| zero_oid(root));
    let update = vec![
        "update-ref".to_string(),
        plan.target_ref.clone(),
        commit.clone(),
        old,
    ];
    // `update-ref` can run a `reference-transaction` hook AFTER the ref has
    // already moved (CodeRabbit round 12, id 4700055354). A timeout there
    // returns an error for a commit that IS reachable, so the caller would
    // skip merge cleanup and never learn the published oid. The ref itself is
    // the authority: reconcile a failed publish against it before deciding.
    //
    // `warnings` is declared BEFORE this block so the late-publish notice can
    // be recorded on the SAME list the caller receives; a separate list would
    // have been dropped on the floor.
    let mut warnings: Vec<String> = Vec::new();
    if let Err(error) = git_run(root, &update, None).await {
        // The ref is read back: if it names THIS commit, the transaction
        // landed and only its hook is unhappy. A publish whose outcome cannot
        // be established is reported as unknown rather than as a failure, so
        // the caller inspects the ref instead of retrying blind.
        match resolve_oid(root, &plan.target_ref) {
            landed if landed == commit => warnings.push(format!(
                "commit {commit} was published, but `git update-ref` did not report success \
                 ({error}); the ref already names it, so do NOT retry — inspect \
                 `{}` if the repository looks inconsistent",
                plan.target_ref
            )),
            _ => {
                return Err(JsonRpcError::internal_error(format!(
                    "publishing {} could not be confirmed: `git update-ref` failed ({error}) \
                     and the ref does not name the new commit — inspect the ref before \
                     retrying, the write may have been partially applied",
                    plan.target_ref
                )));
            }
        }
    }

    // A merge is complete only once MERGE_HEAD is cleared, exactly as
    // `git commit` would. Clearing AFTER the ref moved keeps a failed
    // publish from leaving a half-finished merge state.
    //
    // A failure here is NOT an error: the commit is already published, and
    // returning `Err` would invite the caller to retry a commit that landed.
    // But a MERGE_HEAD left behind makes git think the merge is still pending,
    // so the failure is REPORTED to the caller instead of discarded (Kilo
    // round 11, id 4698809233).
    if let Some(merge) = &plan.merge_head
        && let Err(error) = std::fs::remove_file(merge)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warnings.push(format!(
            "commit {} was published, but the merge state could not be cleared \
             ({}: {error}); git will still consider a merge pending — run \
             `git merge --quit` or remove the file by hand",
            commit,
            merge.display()
        ));
    }
    Ok((commit, warnings))
}

// ---------------------------------------------------------------------------
// Post-apply payloads
// ---------------------------------------------------------------------------

/// `<shorthash> <oneline>` plus the files-changed count for the new commit.
/// `<shorthash> <oneline>` plus the files-changed count for ONE commit.
///
/// `oid` is the commit this apply published — never re-read from HEAD. If
/// HEAD advances between publish and summary, re-reading it would describe
/// somebody else's commit (CodeRabbit round 10, id 4697090186). The same
/// resolved object feeds both probes, so the short hash, subject and file
/// count always describe the same commit.
async fn commit_summary(root: &Path, oid: &str) -> Result<Value, JsonRpcError> {
    let (log, _) = super::git_diff_handler::git_output_capped(
        root,
        &[
            "log".to_string(),
            "-1".to_string(),
            "--no-color".to_string(),
            "--format=%h%x09%s".to_string(),
            oid.to_string(),
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
            oid.to_string(),
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
        "oid": oid,
    }))
}

/// Remote ref range reported by `git push --porcelain`, plus the upstream
/// ahead/behind AFTER the push.
///
/// Porcelain records are `<flag>\t<refspec>\t<summary>`, where the flag is one
/// of `space` (fast-forward), `+` (forced update), `-` (deleted), `*` (new
/// ref), `=` (up to date) or `!` (rejected). The old parser looked for a `>`
/// prefix — which git never emits — and expected a fourth `<old>:<new>` field,
/// so EVERY successful push reported `remote_ref: null, range: null`
/// (CodeRabbit round 8, id 4691119062). Verified against git 2.56.1: a
/// fast-forward prints ` \tHEAD:refs/heads/main\t7518c48..3f841eb`, an
/// up-to-date push prints `=\t…\t[up to date]`, a new branch prints `*`.
async fn push_payload(stdout: &str, root: &Path) -> Value {
    let mut range = Value::Null;
    let mut remote_ref = Value::Null;
    let mut note = String::new();
    for line in stdout.lines() {
        let mut fields = line.split('\t');
        let Some(flag) = fields.next() else { continue };
        // A record's first field is exactly one flag character (or a word like
        // `To`/`Done`, which land here too and are skipped by the length test).
        if flag.chars().count() != 1 {
            continue;
        }
        let Some(refspec) = fields.next() else {
            continue;
        };
        let summary = fields.next().unwrap_or_default();
        if refspec.is_empty() {
            continue;
        }
        remote_ref = Value::String(refspec.to_string());
        let forced = flag == "+";
        // `[up to date]`, `[new branch]`, `[rejected] (non-fast-forward)`,
        // `<old>..<new>`, or `<old>...<new> (forced update)`. The oid pair is
        // the first whitespace-delimited token; anything after it is an
        // annotation, so splitting on the space keeps `(forced update)` out of
        // the range (verified against git 2.56.1).
        let (oids, annotation) = summary.split_once(' ').unwrap_or((summary, ""));
        if summary.contains("up to date") {
            note.push_str("already up to date; ");
            continue;
        }
        if summary.contains("rejected") {
            note.push_str(&format!("rejected ({summary}); "));
            continue;
        }
        if let Some((old, new)) = oids.split_once("...").or_else(|| oids.split_once("..")) {
            if old.is_empty() || new.is_empty() {
                note.push_str("remote ref deleted; ");
            } else {
                range = json!(if forced {
                    format!("{old}...{new}")
                } else {
                    format!("{old}..{new}")
                });
                if forced {
                    note.push_str("forced update; ");
                }
            }
        } else if flag == "*" {
            range = json!(format!("{summary} (new)"));
        } else if !annotation.is_empty() {
            note.push_str(&format!("{annotation}; "));
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
/// Re-verify a supplied `preview_token` against the repository's CURRENT state.
///
/// Called both after the (potentially slow) rail checks and again right after a
/// whole-index capture: the caller could have raced either window, and a cheap
/// comparison is worth more than the mutation it guards.
fn reverify_token(
    root: &Path,
    op: &str,
    argv: &[String],
    args: &Value,
    hint: &str,
) -> Result<(), JsonRpcError> {
    let Some(supplied) = args.get("preview_token").and_then(Value::as_str) else {
        return Ok(());
    };
    if supplied == token_now(root, op, argv)? {
        return Ok(());
    }
    Err(JsonRpcError::invalid_params_with_suggestion(
        "preview_token no longer matches: the repository state changed while \
         the apply was being validated"
            .to_string(),
        hint.to_string(),
    ))
}

/// Publish a whole-index commit from an immutable tree, returning the OID it
/// published (when this op is such a commit) plus any post-publish warnings.
///
/// A whole-index commit reads the LIVE index when git runs, and nothing holds
/// it between the digest check and the spawn — another process (or a pre-commit
/// hook, which inherits the commit's environment) could stage content in that
/// window and the reviewed token would authorise a commit that includes it
/// (CodeRabbit round 8, id 4691119067). `validated_tree_commit` explains why a
/// snapshot alone does not suffice; it REFUSES when the index cannot be
/// snapshotted, because the alternative is a plain `git commit` that reads the
/// live index and runs hooks.
///
/// A pathspec commit (`commit -- <paths>`) reads the WORKTREE for its paths,
/// so it keeps the ordinary command.
async fn publish_whole_index_commit(
    root: &Path,
    op: &str,
    argv: &[String],
    args: &Value,
) -> Result<(Option<String>, Vec<String>), JsonRpcError> {
    if !commits_whole_index(op, argv) {
        return Ok((None, Vec::new()));
    }
    let plan = validated_tree_commit(root, argv)?;
    // The token was re-verified BEFORE the capture; a `git merge --quit` in
    // that gap would have been invisible to it and the capture would then
    // build a single-parent commit (CodeRabbit round 11, id 4698955164). The
    // state now includes the merge parents, so one more comparison immediately
    // after the capture closes that window.
    reverify_token(
        root,
        op,
        argv,
        args,
        "Request a fresh plan and review it before applying",
    )?;
    let (oid, warnings) = run_validated_commit(root, &plan).await?;
    Ok((Some(oid), warnings))
}

/// Per-action enrichment of the applied-mutation payload. Never turns a
/// successful mutation into an error: the mutation already landed, so a failed
/// probe is reported alongside it, not raised.
async fn describe_applied(
    op: &str,
    root: &Path,
    payload: &mut Value,
    staging: Option<StagingOutcome>,
    published: &Option<String>,
    output: &MutationOutput,
) {
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
            // For the validated-tree path the summary describes the published
            // OID, not whatever HEAD is by now.
            let described = published
                .clone()
                .unwrap_or_else(|| head_oid(root).unwrap_or_default());
            match commit_summary(root, &described).await {
                Ok(summary) => payload["commit"] = summary,
                Err(error) => {
                    payload["commit"] = json!({ "oid": described });
                    payload["commit_summary_error"] = Value::String(error.to_string());
                }
            }
        }
        "push" => {
            payload["push"] = push_payload(&output.stdout, root).await;
        }
        "stash" => {
            payload["note"] = Value::String("stash created; use action=stash pop/apply outside this surface or a follow-up plan".to_string());
        }
        _ => {}
    }
}

pub(crate) async fn handle(op: &str, root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    // Argument hygiene is enforced AT THE MUTATION GATE (CodeRabbit round 10,
    // id 4697090213). The handler validates before the plan/apply split, but
    // this function is the one that mutates: a caller that reaches it with a
    // typo'd `pths` key must be refused here, not trusted to have validated.
    // `plan_argv_for` below ignores keys it does not read, so without this
    // the unknown key is silently dropped and the mutation runs wider than
    // the caller asked. The plan path re-runs the same check for its
    // warnings; here the result is discarded and only refusals matter.
    argument_warnings_for(op, args)?;
    // `add` is planned over the EXACT path list the guardrails produce, so the
    // plan's staged list is the truth (exclusions included), not a promise.
    // The outcome is computed ONCE and reused for the payload: a second call
    // could disagree with the argv that actually ran (the tree can move
    // between them), which would report a different staged list than the one
    // staged.
    let (argv, staging) = if op == "add" {
        let outcome = staging_outcome_async(root, args).await?;
        (add_argv(&outcome), Some(outcome))
    } else {
        (plan_argv_for(op, root, args)?, None)
    };

    validate_before_apply(op, root, args, &argv).await?;

    // Re-verify the token AFTER the (potentially slow) rail checks: the
    // caller could have raced the window. Cheap; correctness first.
    reverify_token(root, op, &argv, args, "Request a fresh plan")?;

    let (published, publish_warnings) = publish_whole_index_commit(root, op, &argv, args).await?;
    let output = match &published {
        Some(_) => MutationOutput {
            stdout: String::new(),
            truncated_chars: 0,
        },
        None => run_mutation(root, &argv).await?,
    };

    let mut payload = json!({
        "action": op,
        "mutating": true,
        "applied": true,
        "dry_run": false,
        "argv": argv,
    });
    describe_applied(op, root, &mut payload, staging, &published, &output).await;
    if output.truncated_chars > 0 {
        payload["output_truncated_chars"] = json!(output.truncated_chars);
    }
    // Post-publish cleanup that failed is reported, never hidden: the
    // mutation succeeded, but the caller needs to know the repository is
    // not in the state a successful commit normally leaves it in.
    if !publish_warnings.is_empty() {
        payload["warnings"] = json!(publish_warnings);
    }
    Ok(payload)
}

#[cfg(test)]
#[path = "git_apply_test.rs"]
mod test;

/// Preview-token binding tests (object-id acceptance, target tracking). Split
/// out of `git_apply_test.rs`, which is at the 2000-line cap.
#[cfg(test)]
#[path = "git_apply_binding_test.rs"]
mod binding_test;
