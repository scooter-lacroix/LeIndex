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
use std::path::{Path, PathBuf};
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

/// Deadline for the `check-ignore` probe. It is a rules lookup, not a tree
/// walk, so it is fast; the deadline exists so a wedged git cannot hang a
/// staging request with no timeout running at all.
const IGNORE_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

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
fn ignored_paths(
    root: &Path,
    candidates: &[String],
) -> Result<std::collections::BTreeSet<String>, JsonRpcError> {
    use std::io::Write;

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
    let mut stdin = child.stdin.take();
    let stdout_pipe = child.stdout.take();

    // Feed the request on a helper thread while THIS thread drains the reply:
    // the two directions run concurrently, so neither pipe can fill and
    // deadlock the other. Dropping the handle closes stdin -> `check-ignore`
    // sees EOF and exits. The reply is read uncapped — it is bounded by the
    // request (one record per ignored candidate), and capping it would
    // silently drop ignored paths and then stage them.
    let feeder = std::thread::spawn(move || {
        if let Some(mut input) = stdin.take() {
            let _ = input.write_all(&payload);
        }
    });
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout_pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    // One deadline for the WHOLE exchange. A wedged git is killed and the
    // request refused: an unknown ignore answer must never stage anything.
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
    let stdout: String = reader.join().unwrap_or_default();

    if !status.success() && status.code() != Some(1) {
        return Err(JsonRpcError::internal_error(format!(
            "git check-ignore failed ({}); the staging request was refused because the \
             ignore answer is unknown",
            status.code().unwrap_or(-1)
        )));
    }
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
    let (discovered, skipped, dirs, incomplete) = expand_paths(root, &requested)?;

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
    // The walk can also stop BEFORE entering a subtree, in which case it never
    // saw the files there and `skipped` stays 0. Say the enumeration was
    // incomplete anyway: the count is unknown, but silence would claim a
    // complete bulk add (CodeRabbit round 8, id 4691119046).
    if incomplete && skipped == 0 {
        warnings.push(
            "directory walk stopped at its depth/entry limit; enumeration is INCOMPLETE \
             (unknown remainder) — pass narrower paths to cover the rest"
                .to_string(),
        );
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
    if oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()) {
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
    (oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit())).then_some(oid)
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
        Ok((text, _)) => {
            let oid = text.trim();
            if oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                oid.to_string()
            } else {
                format!("unresolved:{name}")
            }
        }
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
            "Commit with explicit `paths` (a pathspec commit reads the worktree), or \
             repair the repository and re-plan"
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
    if let Some(merge) = &merge_head {
        let text = std::fs::read_to_string(merge).map_err(|error| {
            JsonRpcError::internal_error(format!("cannot read MERGE_HEAD: {error}"))
        })?;
        for line in text.lines() {
            let oid = line.trim();
            if is_oid(oid) {
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

fn is_oid(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
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

/// Publish the validated commit and return the OID that was published.
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
) -> Result<String, JsonRpcError> {
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
    let old = plan.expected_old.clone().unwrap_or_else(|| "0".repeat(40));
    let update = vec![
        "update-ref".to_string(),
        plan.target_ref.clone(),
        commit.clone(),
        old,
    ];
    let published = git_run(root, &update, None).await?;
    let _ = published;

    // A merge is complete only once MERGE_HEAD is cleared, exactly as
    // `git commit` would. Clearing AFTER the ref moved keeps a failed
    // publish from leaving a half-finished merge state.
    if let Some(merge) = &plan.merge_head {
        let _ = std::fs::remove_file(merge);
    }
    Ok(commit)
}

/// Every git child this module spawns starts here: `current_dir(root)` AND
/// the inherited repository-location overrides removed.
///
/// `current_dir(root)` alone does NOT redirect a git child that inherited
/// `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` — from a hook that launched
/// the MCP process, for instance. Those variables win over the working
/// directory, so a request for repository B could inspect or mutate
/// repository A (CodeRabbit round 10, id 4697090106). They are cleared here
/// for every probe and mutation; the validated index snapshot is then set
/// EXPLICITLY and nothing else is.
fn git_command(root: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "echo")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null());
    command
}

/// The std-process twin of [`git_command`], for the one blocking probe
/// (`check-ignore`) that runs on a plain thread.
fn git_command_sync(root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_COMMON_DIR")
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
async fn run_mutation_with_env(
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

/// Run a mutation with no index pin — the plain form every non-commit
/// operation uses, and what the executor's own tests call.
async fn run_mutation(root: &Path, argv: &[String]) -> Result<MutationOutput, JsonRpcError> {
    run_mutation_with_env(root, argv, None).await
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

    // A whole-index commit reads the LIVE index when git runs, and nothing
    // above holds it between the digest check and the spawn — another process
    // (or a pre-commit hook, which inherits the commit's environment) could
    // stage content in that window and the reviewed token would authorise a
    // commit that includes it (CodeRabbit round 8, id 4691119067). The
    // commit is therefore made from an immutable tree built from the
    // validated index (`validated_tree_commit` explains why a snapshot alone
    // does not suffice). A pathspec commit (`commit -- <paths>`) reads the
    // WORKTREE for its paths, not the index, so it keeps the ordinary command.
    // A whole-index commit publishes from the immutable tree and reports the
    // OID it published. `validated_tree_commit` REFUSES when the index cannot
    // be snapshotted (a linked worktree whose git dir cannot be read): there
    // is deliberately no fallback to a plain `git commit`, because that reads
    // the live index and runs hooks — the exact window this closes.
    let published = if commits_whole_index(op, &argv) {
        let plan = validated_tree_commit(root, &argv)?;
        Some(run_validated_commit(root, &plan).await?)
    } else {
        None
    };
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
            // The commit is already published; the summary is ENRICHMENT. A
            // failed probe must not turn a successful mutation into an error
            // (the caller would retry a commit that already landed). For the
            // validated-tree path the summary describes the published OID,
            // not whatever HEAD is by now.
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
    if output.truncated_chars > 0 {
        payload["output_truncated_chars"] = json!(output.truncated_chars);
    }
    Ok(payload)
}

#[cfg(test)]
#[path = "git_apply_test.rs"]
mod test;
