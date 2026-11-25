//! Handler for `leindex_git` — the live Git surface of the MCP server.
//!
//! Every operation shells out to `git` through the shared runner in
//! [`super::git_diff_handler`]: no new dependency, no second subprocess
//! implementation, and the cap-and-drain read keeps a chatty child from
//! deadlocking the transport.
//!
//! ## Mutation policy (this is the security-relevant part)
//!
//! A mutating operation returns a *plan*: whether the operation mutates, the
//! exact argv that would run, what a read-only probe says about the current
//! state, what a bulk add would stage (post-exclusion), and a `preview_token`
//! derived from `(repository, operation, argv, HEAD oid, index digest)`.
//!
//! The plan is the review surface; the executor in [`super::git_apply`] is the
//! apply side. A plan is applied by re-sending the call with its
//! `preview_token`; `confirm: true` skips the plan round-trip for the safe
//! subset only (`add`, `commit`, `push` without force). Destructive shapes are
//! never reachable on `confirm` alone: force-push additionally needs
//! `force: true` plus a `confirm_text` equal to the string the plan printed;
//! `reset --hard`, `clean -fd` and branch delete need their explicit flags and
//! are token-only (`plan_argv` refuses to build them without the flag, so plan
//! and apply cannot disagree about what is destructive).
//!
//! The token binds argv AND repository state, so a plan reviewed against one
//! state cannot be replayed against another (HEAD moved, index changed).
//!
//! ## Ref validation
//!
//! Every operation that takes a revision goes through [`super::git_ref`], which
//! is also what `leindex_git`'s diff branch uses for ranges. One gate, so a new
//! operation cannot pick up a weaker variant.

use super::git_diff_handler::git_output_capped;
use super::helpers::{validate_file_within_project, wrap_live_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Ceiling on bytes read from one git invocation.
const MAX_GIT_BYTES: u64 = 8 * 1024 * 1024;
/// Default characters of git output returned in one response.
const DEFAULT_MAX_CHARS: usize = 20_000;
/// Hard cap on characters of git output returned in one response.
const MAX_MAX_CHARS: usize = 200_000;
/// Default number of history entries returned.
const DEFAULT_COUNT: u64 = 20;
/// Hard cap on number of history entries returned.
const MAX_COUNT: u64 = 200;

/// Handler for the `leindex_git` router branch set.
#[derive(Clone)]
pub struct GitHandler;

/// One `action` of the git router.
struct Op {
    /// Canonical discriminator value.
    name: &'static str,
    /// One-line summary for the guide and the advertised schema.
    summary: &'static str,
    /// Arguments this operation reads (documented in the schema).
    args: &'static [&'static str],
    /// Whether running it changes repository state.
    mutating: bool,
}

/// The git surface, in advertisement order.
const OPS: [Op; 17] = [
    Op {
        name: "status",
        summary: "Working-tree status enriched with changed symbols (wraps leindex_git_status)",
        args: &[],
        mutating: false,
    },
    Op {
        name: "diff",
        summary: "PDG-enriched diff (wraps leindex_git_diff)",
        args: &[],
        mutating: false,
    },
    Op {
        name: "log",
        summary: "Commit history: oid, date, author, subject",
        args: &["target", "path", "max_count"],
        mutating: false,
    },
    Op {
        name: "show",
        summary: "One commit: patch or stat",
        args: &["target", "include_patch"],
        mutating: false,
    },
    Op {
        name: "blame",
        summary: "Per-line last-touching commit for one file",
        args: &["path", "target"],
        mutating: false,
    },
    Op {
        name: "branch",
        summary: "Branches with tip commits; local-only by default, filterable by name_prefix",
        args: &["max_count", "local_only", "name_prefix"],
        mutating: false,
    },
    Op {
        name: "remote",
        summary: "Configured remotes and their URLs",
        args: &[],
        mutating: false,
    },
    Op {
        name: "add",
        summary: "Stage the given paths (apply via preview_token or confirm); bulk adds exclude secrets/runtime artefacts by default",
        args: &["paths", "allow_sensitive"],
        mutating: true,
    },
    Op {
        name: "commit",
        summary: "Commit (optionally exactly `paths`); returns <shorthash> <oneline> + files-changed",
        args: &["message", "paths", "trailers"],
        mutating: true,
    },
    Op {
        name: "checkout",
        summary: "Switch to a branch or commit (apply via preview_token)",
        args: &["target"],
        mutating: true,
    },
    Op {
        name: "stash",
        summary: "Stash working-tree changes (apply via preview_token)",
        args: &[],
        mutating: true,
    },
    Op {
        name: "tag",
        summary: "Create a tag (apply via preview_token)",
        args: &["target", "message"],
        mutating: true,
    },
    Op {
        name: "merge",
        summary: "Merge a branch into the current one (apply via preview_token)",
        args: &["target"],
        mutating: true,
    },
    Op {
        name: "rebase",
        summary: "Rebase the current branch onto a target (non-interactive; apply via preview_token)",
        args: &["target"],
        mutating: true,
    },
    Op {
        name: "push",
        summary: "Push the current branch (confirm:true allowed); returns the remote ref range + ahead/behind",
        args: &[],
        mutating: true,
    },
    Op {
        name: "reset",
        summary: "Move the current branch to a commit (--hard needs hard:true and a preview_token)",
        args: &["target", "hard"],
        mutating: true,
    },
    Op {
        name: "force",
        summary: "Force-push (needs force:true + confirm_text from the plan, plus its preview_token)",
        args: &["target", "force", "confirm_text"],
        mutating: true,
    },
];

impl Op {
    fn parse(raw: &str) -> Option<&'static Op> {
        OPS.iter().find(|op| op.name == raw)
    }

    /// One-line schema entry: `name [args]: summary`.
    ///
    /// The argument names alone do not tell a model what an operation *does*
    /// (`force` vs `reset`), so every entry carries its summary. Kept on one
    /// line per operation to bound the advertised schema.
    fn usage(&self) -> String {
        let args = if self.args.is_empty() {
            String::new()
        } else {
            format!(" [{}]", self.args.join(", "))
        };
        format!("{}{args}: {}", self.name, self.summary)
    }
}

impl GitHandler {
    /// Tool name advertised by the legacy surface and dispatched by the router.
    pub fn name(&self) -> &str {
        "leindex_git"
    }

    /// Human-readable display title.
    pub fn title(&self) -> &str {
        "LeIndex [Git]"
    }

    /// Tool description.
    pub fn description(&self) -> &str {
        "Live Git: status, diff, log, show, blame, branch and remote (read); mutations \
         (add, commit, checkout, stash, tag, merge, rebase, push, reset, force) return a \
         dry-run plan (argv + probe + a preview_token bound to argv/HEAD/index). Apply by \
         re-sending the call with preview_token; confirm:true applies the safe subset \
         (add, commit, push) directly. Force-push needs force:true + confirm_text; \
         reset --hard / clean -fd / branch delete need hard:true / clean:true / delete:true \
         and are token-only."
    }

    /// Argument schema: every git argument plus the `action` selector.
    pub fn argument_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": OPS.iter().map(|op| op.name).collect::<Vec<_>>(),
                    "description": format!(
                        "Git operation. Mutating operations return a plan; apply via preview_token or confirm:true (safe subset). \
                         Args per operation: {}",
                        OPS.iter().map(Op::usage).collect::<Vec<_>>().join(" | ")
                    ),
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory; omit to use the configured project"
                },
                "target": {
                    "type": "string",
                    "description": "One ref, exactly as `git branch` / `git log --oneline` writes it: a branch ('cw32/30b-admission'), a tag, or a commit oid, with optional ~N / ^N / @{N} suffixes. A '-' is allowed INSIDE a name but not as the first character. ':' is refused (it is `rev:path`, which escapes the repository). NOT a range: use action=diff with `range` for 'a..b'. Used by: log, show, blame, checkout, tag, merge, rebase, reset, force — for `tag` it is the NEW TAG NAME, not a commit."
                },
                "path": {
                    "type": "string",
                    "description": "One project-relative PATH, not a ref and not a git pathspec magic: a file for blame, a file or directory to narrow log. Must not start with '-'."
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Project-relative paths: to stage (add) or to commit exactly (commit, pathspec commit; other staged files stay staged)"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (commit) or tag message (tag)"
                },
                "trailers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Commit trailer lines, e.g. 'Co-Authored-By: Name <email>' (commit; passed as --trailer)"
                },
                "confirm": {
                    "type": "boolean",
                    "default": false,
                    "description": "Apply without a prior plan. Safe subset only: add, commit, push (without force). Everything else needs the plan's preview_token."
                },
                "preview_token": {
                    "type": "string",
                    "description": "The token from a mutation plan; binds the argv AND the repository state (HEAD, index), so a plan reviewed against one state cannot be applied against another."
                },
                "force": {
                    "type": "boolean",
                    "default": false,
                    "description": "force branch: allow rewriting the remote (additionally requires confirm_text)"
                },
                "confirm_text": {
                    "type": "string",
                    "description": "force branch: must equal the string the plan printed, e.g. 'force-push origin/branch'"
                },
                "hard": {
                    "type": "boolean",
                    "default": false,
                    "description": "reset branch: allow --hard (discards working tree; requires hard:true and a preview_token)"
                },
                "clean": {
                    "type": "boolean",
                    "default": false,
                    "description": "reset branch: allow clean -fd (untracked removal; requires clean:true and a preview_token)"
                },
                "delete": {
                    "type": "boolean",
                    "default": false,
                    "description": "reset branch (target -d <branch>): allow branch deletion (requires delete:true and a preview_token)"
                },
                "allow_sensitive": {
                    "type": "boolean",
                    "default": false,
                    "description": "add branch: stage directory-discovered secrets/runtime artefacts instead of excluding them (explicit paths always stage, with a warning)"
                },
                "max_count": {
                    "type": "integer",
                    "default": DEFAULT_COUNT,
                    "minimum": 1,
                    "maximum": MAX_COUNT,
                    "description": "Entries to return: commits for log, branches for branch. Clamped to 1..=200. `log` silently stops at the commit count; `branch` reports `total` and sets `truncated` so a cut tail is never invisible."
                },
                "local_only": {
                    "type": "boolean",
                    "default": true,
                    "description": "branch only: local branches only (default) or, with false, local plus remote-tracking branches"
                },
                "name_prefix": {
                    "type": "string",
                    "description": "branch only: keep names that START WITH this string — 'cw32/' selects cw32/30b-admission, '30b' does not"
                },
                "include_patch": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include the full patch (show defaults to stat + changed symbols; set true for the patch text)"
                },
                "max_chars": {
                    "type": "integer",
                    "default": DEFAULT_MAX_CHARS,
                    "minimum": 0,
                    "maximum": MAX_MAX_CHARS,
                    "description": "Bound on the length of returned text fields (patch, blame text)"
                }
            },
            "required": []
        })
    }

    /// Execute one git operation.
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let raw_action = args.get("action").and_then(Value::as_str);
        let op = match raw_action {
            None => {
                return crate::cli::mcp::git_status_handler::GitStatusHandler
                    .execute(registry, args)
                    .await;
            }
            Some(action) => Op::parse(action).ok_or_else(|| {
                JsonRpcError::invalid_params_with_suggestion(
                    format!("Unknown git action '{action}'"),
                    format!(
                        "Set action to one of: {}",
                        OPS.iter().map(|op| op.name).collect::<Vec<_>>().join(", ")
                    ),
                )
            })?,
        };
        let root = resolve_root(registry, &args).await?;

        if op.mutating {
            return if args.get("preview_token").and_then(Value::as_str).is_some()
                || args
                    .get("confirm")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                crate::cli::mcp::git_apply::handle(op.name, &root, &args).await
            } else {
                mutation_plan(op, &root, &args).await
            };
        }

        // `status` and `diff` are the pre-existing PDG-enriched handlers; the
        // router points those two branches straight at them, so reaching here
        // with either means a direct `leindex_git` call named them explicitly.
        if matches!(op.name, "status" | "diff") {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                format!(
                    "'{}' is served by its own handler",
                    if op.name == "status" {
                        "leindex_git_status"
                    } else {
                        "leindex_git_diff"
                    }
                ),
                "Call leindex_git without `action` (status) or use the router's status/diff branch"
                    .to_string(),
            ));
        }

        let warnings = argument_warnings(op, &args)?;
        let payload = read_operation(op, &root, &args).await?;
        Ok(with_warnings(
            wrap_live_with_meta(payload, &root),
            &warnings,
        ))
    }
}

/// Keys the router or transport legitimately adds, so an operation must accept
/// them even when it does not read them.
const SHARED_KEYS: [&str; 2] = ["project_path", "max_latency_ms"];

/// Keys hoisted to the top level of the router's advertised schema from a
/// sub-tool's schema, and therefore seen by every git operation.
const ROUTER_KEYS: [&str; 4] = ["action", "allow_partial", "max_chars", "token_budget"];

/// Keys this operation does not read but whose effect the caller can still
/// observe somewhere else, so a warning — not an error — is right.
///
/// `max_chars` truncates every returned text field regardless of operation, so
/// a caller bounding `show` output is doing something real even though the
/// operation-specific key would be `include_patch`.
const SILENTLY_HONORED: [&str; 1] = ["max_chars"];

/// Whether some git operation reads `key`.
///
/// A key outside this set is a typo the caller needs told about. A key INSIDE
/// it is a real git argument that simply belongs to a different operation — the
/// caller asked for something the router understands, so it is reported rather
/// than refused.
fn any_op_reads(key: &str) -> bool {
    OPS.iter().any(|op| op.args.contains(&key))
}

/// Refuse unknown keys, and name the known-but-ignored ones.
///
/// Nothing used to look at unrecognized arguments: `{"max_chars": 10}` on
/// `show` and `{"max_chr": 10}` (a typo) both took the same path, and the caller
/// had no way to tell a bound that applied from a typo that silently did
/// nothing.
///
/// Three outcomes, in order:
///   * honored — the key this operation reads, or a transport key the server
///     consumes before dispatch: no output at all;
///   * known-but-ignored — a key a DIFFERENT git operation reads: a warning,
///     because the caller asked for something real that will not apply here;
///   * unknown — a warning is too weak: a returned payload would look like it
///     obeyed, so this is an error naming what the operation accepts.
fn argument_warnings(op: &Op, args: &Value) -> Result<Vec<String>, JsonRpcError> {
    let Some(object) = args.as_object() else {
        return Ok(Vec::new());
    };
    // Transport/router keys are consumed by the server before the operation
    // ever runs, so they are never "ignored" — warning about them would train
    // the caller to ignore warnings.
    let is_transport = |key: &str| {
        SHARED_KEYS.contains(&key) || ROUTER_KEYS.contains(&key) || SILENTLY_HONORED.contains(&key)
    };
    let mut warnings = Vec::new();
    for key in object.keys() {
        if op.args.contains(&key.as_str()) || is_transport(key) {
            continue;
        }
        if any_op_reads(key) {
            warnings.push(format!("git {} ignores '{key}'", op.name));
            continue;
        }
        return Err(JsonRpcError::invalid_params_with_suggestion(
            format!("git {} does not take '{key}'", op.name),
            format!(
                "git {} accepts: {}{}{}. Omit '{key}'.",
                op.name,
                if op.args.is_empty() {
                    String::new()
                } else {
                    format!("{} ", op.args.join(", "))
                },
                SHARED_KEYS.join(", "),
                ROUTER_KEYS
                    .iter()
                    .map(|key| format!(", {key}"))
                    .collect::<String>()
            ),
        ));
    }
    warnings.sort();
    Ok(warnings)
}

/// Attach the warnings array to a payload, only when non-empty.
fn with_warnings(mut payload: Value, warnings: &[String]) -> Value {
    if warnings.is_empty() {
        return payload;
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert("warnings".to_string(), json!(warnings));
    }
    payload
}

/// Resolve the project root, defaulting to the registry's configured project.
async fn resolve_root(
    registry: &Arc<ProjectRegistry>,
    args: &Value,
) -> Result<PathBuf, JsonRpcError> {
    let raw_project = match args.get("project_path").and_then(Value::as_str) {
        Some(path) => PathBuf::from(path),
        None => registry.default_project_path().await?,
    };
    let live = LiveProject::resolve(&raw_project.to_string_lossy()).map_err(|error| {
        JsonRpcError::invalid_params(format!("Cannot resolve project path: {error}"))
    })?;
    Ok(live.root().to_path_buf())
}

/// Run a read operation and shape its payload.
async fn read_operation(op: &Op, root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let max_chars = extract_bounded_usize(args, "max_chars", DEFAULT_MAX_CHARS, MAX_MAX_CHARS)?;
    let root = root.to_path_buf();
    let owned_args = args.clone();
    let name = op.name;

    let result = tokio::task::spawn_blocking(move || match name {
        "log" => git_log(&root, &owned_args),
        "show" => git_show(&root, &owned_args),
        "blame" => git_blame(&root, &owned_args),
        "branch" => git_branch(&root, &owned_args),
        "remote" => git_remote(&root),
        other => Err(JsonRpcError::internal_error(format!(
            "unhandled git action '{other}'"
        ))),
    })
    .await
    .map_err(|error| JsonRpcError::internal_error(format!("git task failed: {error}")))?;

    Ok(clip_payload(result?, max_chars))
}

/// `git log` as structured entries, each with a files-changed count (§4.5).
///
/// `--name-only` appends each commit's file list after its header line; the
/// parser splits records on the header shape (40-hex oid + tab), so a file
/// list can never be mistaken for a header and a header can never be a file
/// line (subjects cannot contain the tab separator).
fn git_log(root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let count = extract_bounded_u64(args, "max_count", DEFAULT_COUNT, MAX_COUNT)?;
    let target = optional_rev(args)?;
    let path = optional_path(args)?;
    let mut argv = vec![
        "log".to_string(),
        "--no-color".to_string(),
        format!("--max-count={count}"),
        "--date=iso-strict".to_string(),
        "--format=%H%x09%ad%x09%an%x09%s".to_string(),
        "--name-only".to_string(),
        target.unwrap_or_else(|| "HEAD".to_string()),
    ];
    if let Some(path) = path {
        argv.push("--".to_string());
        argv.push(path);
    }
    let (text, truncated) = run_text(root, &argv)?;
    let mut commits: Vec<Value> = Vec::new();
    let mut files = 0usize;
    for line in text.lines() {
        let mut fields = line.splitn(4, '\t');
        let oid = fields.next().unwrap_or_default();
        if oid.len() == 40 && oid.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Some(commit) = commits.last_mut() {
                commit["files_changed"] = json!(files);
            }
            files = 0;
            commits.push(json!({
                "oid": oid,
                "date": fields.next().unwrap_or_default(),
                "author": fields.next().unwrap_or_default(),
                "subject": fields.next().unwrap_or_default(),
            }));
        } else if !line.trim().is_empty() {
            files += 1;
        }
    }
    if let Some(commit) = commits.last_mut() {
        commit["files_changed"] = json!(files);
    }
    Ok(json!({
        "action": "log",
        "count": commits.len(),
        "commits": commits,
        "truncated": truncated,
    }))
}

/// `git show` for one commit: compact by default (§4.5) — per-file stat plus
/// a files-changed count — with the full patch only on `include_patch: true`.
fn git_show(root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let target = optional_rev(args)?.unwrap_or_else(|| "HEAD".to_string());
    let include_patch = args
        .get("include_patch")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (stat, stat_truncated) = run_text(
        root,
        &[
            "show".to_string(),
            "--no-color".to_string(),
            "--format=medium".to_string(),
            "--stat".to_string(),
            target.clone(),
        ],
    )?;
    let mut payload = json!({
        "action": "show",
        "target": target,
        "include_patch": include_patch,
        "stat": stat,
        "truncated": stat_truncated,
    });
    if include_patch {
        let (patch, patch_truncated) = run_text(
            root,
            &[
                "show".to_string(),
                "--no-color".to_string(),
                "--format=fuller".to_string(),
                target.clone(),
            ],
        )?;
        payload["patch"] = Value::String(patch);
        payload["truncated"] = Value::Bool(stat_truncated || patch_truncated);
    }
    Ok(payload)
}

/// `git blame` for one file, bounded.
fn git_blame(root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let raw_path = args.get("path").and_then(Value::as_str).ok_or_else(|| {
        JsonRpcError::invalid_params_with_suggestion(
            "blame requires 'path'".to_string(),
            "Provide path: a project-relative file to blame".to_string(),
        )
    })?;
    // Resolves and rejects anything outside the project, including `..`
    // escapes and absolute paths to other trees.
    let absolute = validate_file_within_project(raw_path, root)
        .map_err(|error| JsonRpcError::invalid_params(format!("invalid blame path: {error}")))?;
    if !absolute.is_file() {
        return Err(JsonRpcError::invalid_params(format!(
            "'{raw_path}' is not a file"
        )));
    }
    let target = optional_rev(args)?;
    // `--no-color` is NOT a git-blame option: git reads it as an ambiguous
    // prefix of `--no-color-lines` / `--no-color-by-age` and exits 129 with a
    // usage page, so every blame call failed. Color is suppressed through the
    // config override instead (`-c color.ui=false`), which every command
    // honors and which cannot collide with an option name. Measured: `git
    // blame --no-color` exits 129, `git -c color.ui=false blame` exits 0.
    let mut argv = vec![
        "-c".to_string(),
        "color.ui=false".to_string(),
        "blame".to_string(),
        "-w".to_string(),
    ];
    if let Some(target) = target.clone() {
        argv.push(target);
    }
    argv.push("--".to_string());
    argv.push(raw_path.to_string());
    let (text, truncated) = run_text(root, &argv)?;
    let lines = text.lines().count();
    Ok(json!({
        "action": "blame",
        "path": raw_path,
        "target": target.unwrap_or_else(|| "HEAD".to_string()),
        "line_count": lines,
        "text": text,
        "truncated": truncated,
    }))
}

/// `git branch -vv` as structured rows, honouring `max_count`, `local_only`
/// and `name_prefix`.
///
/// Truncation is reported, never silent: `total` is how many branches existed
/// AFTER filtering and `truncated` says whether `max_count` cut the tail, so a
/// caller can neither mistake a capped list for the whole set nor miss that it
/// lost entries.
fn git_branch(root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let max_count = extract_bounded_u64(args, "max_count", DEFAULT_COUNT, MAX_COUNT)? as usize;
    let local_only = args
        .get("local_only")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let name_prefix = args
        .get("name_prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty());

    // `--all` lists remote-tracking entries and a `remotes/HEAD` symref line;
    // local-only is the default because that is the set a caller can act on.
    // `-c color.ui=false` avoids the same option collision that broke blame.
    let mut argv = vec![
        "-c".to_string(),
        "color.ui=false".to_string(),
        "branch".to_string(),
    ];
    if !local_only {
        argv.push("--all".to_string());
    }
    argv.push("-vv".to_string());

    let (text, text_truncated) = run_text(root, &argv)?;
    let all = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        // `--all` prints `remotes/origin/HEAD -> origin/master`; that is a
        // pointer, not a branch.
        .filter(|line| !line.contains(" -> "))
        .map(|line| {
            let current = line.starts_with('*');
            let body = line.trim_start_matches(['*', ' ', '+']).trim();
            let mut fields = body.split_whitespace();
            let name = fields.next().unwrap_or_default();
            let oid = fields.next().unwrap_or_default();
            let rest = fields.collect::<Vec<_>>().join(" ");
            json!({
                "name": name,
                "current": current,
                "oid": oid,
                "detail": rest,
            })
        })
        .filter(|branch| {
            name_prefix.is_none_or(|prefix| {
                branch["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(prefix))
            })
        })
        .collect::<Vec<_>>();

    let total = all.len();
    let truncated = total > max_count || text_truncated;
    let branches = all.into_iter().take(max_count).collect::<Vec<_>>();
    Ok(json!({
        "action": "branch",
        "count": branches.len(),
        "total": total,
        "branches": branches,
        "truncated": truncated,
        "local_only": local_only,
        "name_prefix": name_prefix,
    }))
}

/// `git remote -v` as structured rows. A repository without remotes is a valid
/// answer, not an error.
fn git_remote(root: &Path) -> Result<Value, JsonRpcError> {
    let argv = vec!["remote".to_string(), "-v".to_string()];
    let (text, truncated) = run_text(root, &argv)?;
    let remotes = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let url = fields.next().unwrap_or_default();
            let kind = fields.next().unwrap_or_default().trim_matches(['(', ')']);
            Some(json!({ "name": name, "url": url, "kind": kind }))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "action": "remote",
        "count": remotes.len(),
        "remotes": remotes,
        "truncated": truncated,
        "note": if remotes.is_empty() {
            Value::String("no remotes are configured for this repository".to_string())
        } else {
            Value::Null
        },
    }))
}

/// Guidance attached to every mutation plan.
///
/// A plan without a token/confirm changes nothing; the hint says what to do
/// next for each path. The `confirm` wording is deliberately included for the
/// safe subset — that path now works — while destructive shapes are steered to
/// the token plus their rails (`test_plan_hint_says_how_to_apply` pins it).
const PLAN_NEXT_HINT: &str = "Nothing was changed. To apply: re-send this call with \
     preview_token (add/commit/push may use confirm:true). Destructive shapes \
     (force-push, reset --hard, clean -fd, branch delete) additionally need their \
     explicit flags and are token-only.";

/// Build the dry-run plan for a mutating operation.
///
/// The plan is the review surface: the exact argv, what a bulk add would stage
/// (post-exclusion, so the plan lists exactly what will be staged), a
/// read-only probe, and the token the executor validates against the LIVE
/// state. Nothing here spawns a mutating git command: [`plan_argv`] only
/// *describes* the argv, and every probe it runs is read-only.
async fn mutation_plan(op: &Op, root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let (argv, staging) = if op.name == "add" {
        // Compute the staging outcome ONCE: the shared helper builds the argv
        // from it AND it is the report the plan shows, so a second call could
        // disagree with the argv the token binds.
        let outcome = super::git_apply::staging_outcome(root, args)?;
        (super::git_apply::add_argv(&outcome), Some(outcome))
    } else {
        (plan_argv(op, root, &args.clone())?, None)
    };
    // Issued through the SAME function the executor validates with: the plan's
    // token must be the apply's token, or every token-only operation is
    // unreachable (the plan reports a token the executor can never accept).
    let token = super::git_apply::token_now(root, op.name, &argv)?;
    let warnings = argument_warnings(op, args)?;
    let root_buf = root.to_path_buf();
    let probe_args = args.clone();
    let op_name = op.name;

    let probe = tokio::task::spawn_blocking(move || probe_state(op_name, &root_buf, &probe_args))
        .await
        .map_err(|error| JsonRpcError::internal_error(format!("git task failed: {error}")))?;

    let mut payload = json!({
        "action": op.name,
        "mutating": true,
        "applied": false,
        "dry_run": true,
        "argv": argv,
        "preview_token": token,
        "probe": probe,
        "next": PLAN_NEXT_HINT,
    });
    // The plan shows what an apply would attach, so the reviewer sees the
    // exclusions and warnings before committing to anything.
    if let Some(outcome) = staging {
        payload["will_stage"] = json!(outcome.staged);
        payload["excluded"] = json!(
            outcome
                .excluded
                .iter()
                .map(|(path, reason)| json!({ "path": path, "reason": reason }))
                .collect::<Vec<_>>()
        );
        payload["warnings"] = json!(outcome.warnings);
    }
    Ok(with_warnings(payload, &warnings))
}

/// Validate the optional `target` argument once, before any op-specific work.
///
/// `reset` may carry flag-shaped targets (`-d`, `-fd`, `-d <branch>`) whose
/// rails live in [`plan_argv`]; those bypass the ref gate here and are
/// accepted or refused there, so the destructive shapes stay in one place.
fn plan_target(op: &Op, args: &Value) -> Result<Option<String>, JsonRpcError> {
    match args.get("target").and_then(Value::as_str) {
        Some(target) if op.name == "reset" && target.starts_with('-') => {
            Ok(Some(target.trim().to_string()))
        }
        Some(target) => Ok(Some(super::git_ref::validate_ref_or_error(target)?)),
        None => Ok(None),
    }
}

/// Error for an op that cannot run without a target.
fn require_target(
    target: &Option<String>,
    op_name: &str,
    field: &str,
) -> Result<String, JsonRpcError> {
    target.clone().ok_or_else(|| {
        JsonRpcError::invalid_params_with_suggestion(
            format!("git {op_name} requires '{field}'"),
            format!("Provide {field}: a branch name, tag or commit oid"),
        )
    })
}

/// The argv for a force-push to `target`.
///
/// `git push` takes `[<repository> [<refspec>...]]`, so a single
/// `origin/main` argument is parsed as a REPOSITORY and git refuses it
/// ("fatal: 'origin/main' is not a valid push target", verified on git
/// 2.56.1). The `remote/branch` form the plan advertises is therefore split
/// into the two arguments git wants, pushing the local HEAD to that branch.
/// A bare remote name keeps git's own upstream resolution.
pub(crate) fn force_push_argv(target: &str) -> Vec<String> {
    let mut argv = vec!["push".to_string(), "--force-with-lease".to_string()];
    match target.split_once('/') {
        Some((remote, branch)) if !remote.is_empty() && !branch.is_empty() => {
            argv.push(remote.to_string());
            argv.push(format!("HEAD:refs/heads/{branch}"));
        }
        _ => argv.push(target.to_string()),
    }
    argv
}

/// `<subcommand> [flag] <target>` — the shape every single-target op shares.
fn target_argv(subcommand: &str, flag: Option<&str>, target: String) -> Vec<String> {
    let mut argv = vec![subcommand.to_string()];
    if let Some(flag) = flag {
        argv.push(flag.to_string());
    }
    argv.push(target);
    argv
}

/// `commit --no-gpg-sign -m <message> [--trailer <line>]... [-- <paths>]`.
///
/// Commit `paths` uses git's native **pathspec commit** semantics rather than a
/// temporary index — chosen deliberately:
/// * `git commit -- <paths>` commits the updated working-tree contents of
///   exactly those paths and *disregards contents staged for other paths*,
///   which is the brief's "don't sweep other staged changes in" — guaranteed
///   by git instead of simulated by us.
/// * One atomic git invocation; no lockfile/index juggling that could leave a
///   torn state if the process dies between the two steps.
/// * Untracked files are NOT silently added by a pathspec commit — only `add`
///   brings new files into the repository. The plan's probe reports which
///   requested paths are untracked so the caller can stage them explicitly.
///
/// Trailers ride git's `--trailer` (one flag per line), so git validates and
/// places them per its own rules. Trailer lines are single-line by contract;
/// a newline would smuggle a second message paragraph past the caller.
fn commit_argv(root: &Path, args: &Value) -> Result<Vec<String>, JsonRpcError> {
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty() && message.len() <= 10_000)
        .ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                "git commit requires a non-empty 'message' (max 10000 chars)".to_string(),
                "Provide message: the commit subject and body".to_string(),
            )
        })?;
    let mut argv = vec![
        "commit".to_string(),
        "--no-gpg-sign".to_string(),
        "-m".to_string(),
        message.to_string(),
    ];
    if let Some(trailers) = args.get("trailers").and_then(Value::as_array) {
        if trailers.len() > 20 {
            return Err(JsonRpcError::invalid_params(
                "at most 20 trailer lines are accepted".to_string(),
            ));
        }
        for trailer in trailers {
            let line = trailer.as_str().unwrap_or_default().trim();
            if line.is_empty() {
                return Err(JsonRpcError::invalid_params(
                    "trailer lines must be non-empty, e.g. 'Co-Authored-By: Name <email>'"
                        .to_string(),
                ));
            }
            if line.contains('\n') || line.contains('\r') {
                return Err(JsonRpcError::invalid_params(
                    "each trailer must be a single line".to_string(),
                ));
            }
            argv.push("--trailer".to_string());
            argv.push(line.to_string());
        }
    }
    if let Some(paths) = args.get("paths").and_then(Value::as_array) {
        if paths.is_empty() {
            return Err(JsonRpcError::invalid_params(
                "'paths' must name at least one file, or be omitted".to_string(),
            ));
        }
        // Same gate as `add`: project-relative, no escapes, no option shapes.
        let validated = staged_paths(root, args)?;
        argv.push("--".to_string());
        argv.extend(validated);
    }
    Ok(argv)
}

/// `tag <name>`, annotated only when a non-blank message is supplied.
fn tag_argv(args: &Value, name: String) -> Vec<String> {
    let mut argv = vec!["tag".to_string()];
    match args.get("message").and_then(Value::as_str) {
        Some(message) if !message.trim().is_empty() => {
            argv.push("-a".to_string());
            argv.push(name);
            argv.push("-m".to_string());
            argv.push(message.to_string());
        }
        _ => argv.push(name),
    }
    argv
}

/// The explicit `delete: true` gate for branch deletion.
fn delete_flag(args: &Value) -> bool {
    args.get("delete").and_then(Value::as_bool).unwrap_or(false)
}

/// Describe the argv a mutation would run, validating every argument first.
///
/// This is the single place a mutation's command line is decided, with one
/// documented exception: `add` is built by
/// [`super::git_apply::add_argv`] from a `StagingOutcome` the caller already
/// has, because the stage list IS the reviewer-facing report and recomputing
/// it here would let the plan and the payload disagree. Its arm below routes
/// through that same helper, so the definition still has one owner.
/// Crate callers go through [`plan_argv_for`]; [`Op`] stays module-private.
fn plan_argv(op: &Op, root: &Path, args: &Value) -> Result<Vec<String>, JsonRpcError> {
    let target = plan_target(op, args)?;
    let required = |field: &str| require_target(&target, op.name, field);

    match op.name {
        // `add` is planned over the exact post-exclusion path list the
        // guardrails produce, so the plan's staged list is the truth.
        "add" => Ok(super::git_apply::add_argv(
            &super::git_apply::staging_outcome(root, args)?,
        )),
        "commit" => commit_argv(root, args),
        "checkout" => Ok(target_argv(
            "checkout",
            Some("--no-guess"),
            required("target")?,
        )),
        "stash" => Ok(vec![
            "stash".to_string(),
            "push".to_string(),
            "--include-untracked".to_string(),
        ]),
        "tag" => Ok(tag_argv(args, required("target")?)),
        "merge" => Ok(target_argv("merge", Some("--no-edit"), required("target")?)),
        // Non-interactive only (brief §5): no editor, no interactive rebase.
        // `git rebase` has NO `--no-edit` flag (git rejects it with exit
        // 129 "unknown option"), and a non-`-i` rebase opens no editor
        // anyway — the executor's `GIT_EDITOR=true` covers the todo-list
        // case. The argv is deliberately bare: `git rebase <target>`.
        "rebase" => Ok(target_argv("rebase", None, required("target")?)),
        "push" => Ok(vec!["push".to_string(), "--porcelain".to_string()]),
        "reset" => {
            let target = required("target")?;
            let hard = args.get("hard").and_then(Value::as_bool).unwrap_or(false);
            let clean = args.get("clean").and_then(Value::as_bool).unwrap_or(false);
            // Rails: the destructive shapes are only BUILDABLE when the
            // explicit flag is set — an unset flag cannot produce a plan
            // that the executor could then token-validate into running.
            if clean {
                if !target.starts_with('-') {
                    return Err(JsonRpcError::invalid_params_with_suggestion(
                        "clean:true expects target '-d' or '-fd'".to_string(),
                        "Use target:'-fd' with clean:true (token-only)".to_string(),
                    ));
                }
                return Ok(vec!["clean".to_string(), target, "-f".to_string()]);
            }
            if target.starts_with('-') {
                // The one legal flag-shaped target is branch deletion, and
                // only with delete:true (the executor then makes it
                // token-only). Everything else flag-shaped is a refusal.
                let Some(branch) = target
                    .strip_prefix("-d ")
                    .map(str::trim)
                    .filter(|b| !b.is_empty())
                else {
                    return Err(JsonRpcError::invalid_params_with_suggestion(
                        format!("'{target}' is not a revision"),
                        "reset takes a commit/branch; branch deletion is target:'-d <branch>' with delete:true".to_string(),
                    ));
                };
                if !delete_flag(args) {
                    return Err(JsonRpcError::invalid_params_with_suggestion(
                        format!("branch deletion refused: '{branch}' needs delete:true (and is token-only)"),
                        "Pass target:'-d <branch>' with delete:true; a preview_token from its plan applies it".to_string(),
                    ));
                }
                return Ok(vec![
                    "branch".to_string(),
                    "-d".to_string(),
                    branch.to_string(),
                ]);
            }
            let flag = if hard { "--hard" } else { "--mixed" };
            Ok(target_argv("reset", Some(flag), target))
        }
        // `force` is a force-push; naming it in the plan makes the destructive
        // shape explicit instead of hiding it behind `push`.
        "force" => {
            let branch_target = required("target")?;
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            if !force {
                return Err(JsonRpcError::invalid_params_with_suggestion(
                    "force-push refused: set force:true to allow rewriting the remote".to_string(),
                    format!("Set force:true and confirm_text:\"force-push {branch_target}\""),
                ));
            }
            let printed = format!("force-push {branch_target}");
            let supplied = args
                .get("confirm_text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if supplied != printed {
                return Err(JsonRpcError::invalid_params_with_suggestion(
                    format!(
                        "confirm_text does not match the plan's confirmation string ('{supplied}')"
                    ),
                    format!("Copy it verbatim from the plan: confirm_text:\"{printed}\""),
                ));
            }
            Ok(force_push_argv(&branch_target))
        }
        other => Err(JsonRpcError::internal_error(format!(
            "git operation '{other}' has no plan"
        ))),
    }
}

/// Resolve an action NAME to its plan argv — the executor's entry point, so
/// the private [`Op`] table stays encapsulated in this module.
pub(crate) fn plan_argv_for(
    op_name: &str,
    root: &Path,
    args: &Value,
) -> Result<Vec<String>, JsonRpcError> {
    let op = Op::parse(op_name).ok_or_else(|| {
        JsonRpcError::internal_error(format!("git operation '{op_name}' has no plan"))
    })?;
    plan_argv(op, root, args)
}

/// Read-only probe run alongside a plan, so the plan describes the real state.
fn probe_state(op: &str, root: &Path, args: &Value) -> Value {
    match op {
        "commit" | "stash" => match crate::cli::git::status(root) {
            Ok(status) => json!({
                "branch": status.branch,
                "head_oid": status.head_oid,
                "staged_count": status.staged.len(),
                "modified_count": status.modified.len(),
                "untracked_count": status.untracked.len(),
            }),
            Err(error) => json!({ "error": error.to_string() }),
        },
        "add" => {
            let paths = args
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(|path| path.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            match crate::cli::git::status(root) {
                Ok(status) => {
                    let mut dirty = Vec::new();
                    for path in &paths {
                        let reported = status
                            .modified
                            .iter()
                            .chain(status.staged.iter())
                            .chain(status.untracked.iter())
                            .chain(status.deleted.iter())
                            .any(|candidate| candidate.to_string_lossy().ends_with(path.as_str()));
                        dirty.push(json!({ "path": path, "reported_dirty": reported }));
                    }
                    json!({ "paths": dirty })
                }
                Err(error) => json!({ "error": error.to_string() }),
            }
        }
        "push" => match crate::cli::git::status(root) {
            Ok(status) => json!({
                "branch": status.branch,
                "head_oid": status.head_oid,
            }),
            Err(error) => json!({ "error": error.to_string() }),
        },
        // Everything else takes a revision: prove it resolves before offering
        // to check it out, merge it, or reset to it. The argv is built by
        // `probe_rev_argv`, which validates first — building it from the
        // caller's raw string here would be the one unvalidated entry point in
        // this file, and `rev-parse` still parses a leading `-` as a flag.
        _ => match args.get("target").and_then(Value::as_str) {
            Some(raw) => match probe_rev_argv(raw) {
                Ok(argv) => match git_output_capped(root, &argv, 4096) {
                    Ok((text, _)) if !text.trim().is_empty() => {
                        json!({ "target": raw, "resolves_to": text.trim() })
                    }
                    Ok(_) => json!({ "target": raw, "resolves_to": Value::Null }),
                    Err(error) => json!({ "target": raw, "error": error.to_string() }),
                },
                Err(error) => json!({ "target": raw, "error": error.message_with_hint() }),
            },
            None => json!({ "target": Value::Null }),
        },
    }
}

/// argv for the read-only "does this revision resolve?" probe.
///
/// The argv is built by [`super::git_ref::probe_argv`], which validates first:
/// a rejected revision cannot reach the process at all, and `rev-parse` still
/// parses a leading `-` as a flag.
fn probe_rev_argv(raw: &str) -> Result<Vec<String>, JsonRpcError> {
    super::git_ref::probe_argv(raw).map_err(|error| super::git_ref::rpc_error(raw, error))
}

/// Validate and normalise the `paths` argument for `add`.
///
/// Every path must resolve inside the project, so a mutation can never be
/// aimed at a file the project does not own.
fn staged_paths(root: &Path, args: &Value) -> Result<Vec<String>, JsonRpcError> {
    let paths = args.get("paths").and_then(Value::as_array).ok_or_else(|| {
        JsonRpcError::invalid_params_with_suggestion(
            "git add requires a non-empty 'paths' array".to_string(),
            "Provide paths: project-relative files to stage".to_string(),
        )
    })?;
    let mut validated = Vec::new();
    for path in paths {
        let raw = path.as_str().unwrap_or_default();
        validate_file_within_project(raw, root).map_err(|_| {
            JsonRpcError::invalid_params_with_suggestion(
                format!("'{raw}' is not a path inside this project"),
                "Stage project-relative paths only".to_string(),
            )
        })?;
        validated.push(raw.to_string());
    }
    if validated.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "git add requires at least one path".to_string(),
        ));
    }
    Ok(validated)
}

/// Preview token identifying exactly one `(repository, operation, argv, state)`
/// tuple.
///
/// `state` is the repository binding ([`super::git_apply::state_binding`]:
/// HEAD oid + index digest). Both ends of the seam go through
/// [`super::git_apply::token_now`] — the plan issues the token with it and the
/// executor recomputes it — so a plan's token is always the one the executor
/// accepts, and a moved HEAD or a changed index still invalidates it.
pub(crate) fn preview_token(root: &Path, op: &str, argv: &[String], state: &str) -> String {
    blake3::hash(
        format!(
            "{}\u{0}{op}\u{0}{}\u{0}{state}",
            root.display(),
            argv.join("\u{0}")
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string()
}

/// Run a read-only git command and return its text.
///
/// A failure here is environmental (git missing, not a repository, a bad
/// revision): it is reported as an internal error carrying git's own message,
/// not as a malformed-argument error blaming the caller.
fn run_text(root: &Path, argv: &[String]) -> Result<(String, bool), JsonRpcError> {
    git_output_capped(root, argv, MAX_GIT_BYTES).map_err(|error| {
        let message = error.to_string();
        if message.trim().is_empty() {
            JsonRpcError::internal_error(format!(
                "git {} failed without reporting a reason",
                argv.join(" ")
            ))
        } else {
            JsonRpcError::internal_error(message)
        }
    })
}

/// Bound the strings in a payload so one call cannot flood the transport.
fn clip_payload(payload: Value, max_chars: usize) -> Value {
    let Value::Object(object) = payload else {
        return payload;
    };
    let mut clipped = serde_json::Map::new();
    for (key, value) in object {
        let value = match value {
            Value::String(text) => {
                Value::String(crate::cli::mcp::output::truncate_chars(&text, max_chars))
            }
            other => other,
        };
        clipped.insert(key, value);
    }
    Value::Object(clipped)
}

/// Optional revision argument, validated by the shared ref gate.
fn optional_rev(args: &Value) -> Result<Option<String>, JsonRpcError> {
    match args.get("target").and_then(Value::as_str) {
        Some(target) => Ok(Some(super::git_ref::validate_ref_or_error(target)?)),
        None => Ok(None),
    }
}

/// Optional pathspec argument, rejected if it could be read as an option.
fn optional_path(args: &Value) -> Result<Option<String>, JsonRpcError> {
    match args.get("path").and_then(Value::as_str) {
        Some(path) => {
            if path.is_empty() || path.len() > 4096 || path.starts_with('-') || path.contains('\0')
            {
                Err(JsonRpcError::invalid_params_with_suggestion(
                    format!("'{path}' is not a usable pathspec"),
                    "Use a project-relative file or directory path that does not start with '-'"
                        .to_string(),
                ))
            } else {
                Ok(Some(path.to_string()))
            }
        }
        None => Ok(None),
    }
}

/// Read a bounded integer argument. JSON numbers arrive as `f64` (the schema
/// advertises `integer`, which serde_json does not model separately), so a
/// non-negative finite float is accepted and truncated.
fn bounded_integer(args: &Value, key: &str) -> Result<Option<u64>, JsonRpcError> {
    let Some(value) = args.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let number = match value {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|v| v as u64)
        }),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    };
    number.map(Some).ok_or_else(|| {
        JsonRpcError::invalid_params_with_suggestion(
            format!("'{key}' must be a non-negative integer"),
            format!("Provide {key} as a whole number"),
        )
    })
}

fn extract_bounded_u64(
    args: &Value,
    key: &str,
    default: u64,
    max: u64,
) -> Result<u64, JsonRpcError> {
    Ok(bounded_integer(args, key)?.map_or(default, |value| value.clamp(1, max)))
}

fn extract_bounded_usize(
    args: &Value,
    key: &str,
    default: usize,
    max: usize,
) -> Result<usize, JsonRpcError> {
    Ok(bounded_integer(args, key)?.map_or(default, |value| (value as usize).min(max)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_every_op_is_selectable_and_documented() {
        for op in &OPS {
            assert!(!op.summary.is_empty(), "{} has no summary", op.name);
            assert!(Op::parse(op.name).is_some(), "{} does not parse", op.name);
        }
        assert!(Op::parse("nope").is_none());
    }

    #[test]
    fn test_mutations_are_marked_mutating_and_reads_are_not() {
        for op in &OPS {
            let expected = !matches!(
                op.name,
                "status" | "diff" | "log" | "show" | "blame" | "branch" | "remote"
            );
            assert_eq!(op.mutating, expected, "{} mutation flag", op.name);
        }
    }

    /// The plan's advertised token MUST be the token the executor accepts.
    ///
    /// Regression (integration review, F1): `mutation_plan` issued the token
    /// against an EMPTY state while the executor validates against
    /// `{head_oid}\0{index_digest}`, so every plan-issued token was rejected.
    /// That made every TOKEN_ONLY operation (checkout/stash/tag/merge/rebase)
    /// and every destructive shape unreachable — only `confirm:true` worked,
    /// and the negative "token rejected after HEAD moves" test kept passing
    /// because it hand-derived the token with the real state instead of
    /// round-tripping a plan. This test round-trips: bare stash → plan →
    /// re-send with the PLAN'S token → applied.
    #[tokio::test]
    async fn test_plan_token_round_trips_into_a_successful_apply() {
        let repo = scratch_repo();
        let root = repo.path();
        // Something to stash: an untracked file is enough (`stash -u`).
        std::fs::write(root.join("dirty.txt"), "dirty\n").unwrap();

        let plan = mutation_plan(Op::parse("stash").unwrap(), root, &json!({}))
            .await
            .expect("plan");
        assert_eq!(plan["applied"], false, "a plan applies nothing: {plan}");
        let token = plan["preview_token"]
            .as_str()
            .expect("the plan advertises a token")
            .to_string();

        let applied =
            super::super::git_apply::handle("stash", root, &json!({ "preview_token": token }))
                .await
                .unwrap_or_else(|error| panic!("the plan's own token must apply: {error}"));
        assert_eq!(
            applied["applied"], true,
            "token path must reach git: {applied}"
        );
        assert!(
            !root.join("dirty.txt").exists(),
            "the stash actually ran — the untracked file is gone from the tree"
        );
    }

    /// Regression (Codex round 4, C4-2): the rebase plan carried
    /// `--no-edit`, which `git rebase` does NOT accept (`error: unknown
    /// option 'no-edit'`, exit 129) — so every token-authorized rebase failed
    /// at the executor while the plan looked fine. No argv assertion could
    /// catch it; the planned argv has to be RUN. A non-`-i` rebase opens no
    /// editor, and the executor sets `GIT_EDITOR=true`, so a bare
    /// `git rebase <target>` is already non-interactive.
    #[tokio::test]
    async fn test_rebase_plan_token_applies_without_a_bogus_no_edit_flag() {
        let repo = scratch_repo();
        let root = repo.path();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .expect("git available");
            assert!(status.success(), "git {args:?}");
        };
        // A ref, resolved through the same runner the handler uses.
        let rev = |rev: &str| -> String {
            git_output_capped(
                root,
                &["rev-parse".to_string(), rev.to_string()],
                MAX_GIT_BYTES,
            )
            .map(|(out, _)| out.trim().to_string())
            .unwrap_or_else(|error| panic!("rev-parse {rev}: {error}"))
        };
        // A topic commit on top of master, then a master commit the topic
        // must be replayed onto.
        git(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(root.join("topic.txt"), "topic\n").unwrap();
        git(&["add", "topic.txt"]);
        git(&["commit", "-q", "-m", "topic work"]);
        git(&["checkout", "-q", "master"]);
        std::fs::write(root.join("master.txt"), "master\n").unwrap();
        git(&["add", "master.txt"]);
        git(&["commit", "-q", "-m", "master moved on"]);
        git(&["checkout", "-q", "topic"]);
        let master_tip = rev("master");
        assert_ne!(
            rev("master~1"),
            master_tip,
            "precondition: master must have moved on by exactly one commit"
        );
        assert_ne!(
            rev("topic"),
            rev("topic~1"),
            "precondition: topic must have exactly one commit of its own"
        );
        // The topic commit sits on master's PARENT — the rebase moves it.
        assert_eq!(
            rev("topic^"),
            rev("master~1"),
            "precondition: topic forked before master moved"
        );

        let plan = mutation_plan(
            Op::parse("rebase").unwrap(),
            root,
            &json!({ "target": "master" }),
        )
        .await
        .expect("plan");
        assert_eq!(plan["applied"], false, "a plan applies nothing: {plan}");
        // The advertised argv is what the token binds and the executor runs.
        assert_eq!(
            plan["argv"],
            json!(["rebase", "master"]),
            "the rebase argv must carry no `--no-edit` (git rejects it): {plan}"
        );
        let token = plan["preview_token"]
            .as_str()
            .expect("the plan advertises a token")
            .to_string();

        let applied = super::super::git_apply::handle(
            "rebase",
            root,
            &json!({ "target": "master", "preview_token": token }),
        )
        .await
        .unwrap_or_else(|error| panic!("the plan's own token must apply: {error}"));
        assert_eq!(
            applied["applied"], true,
            "token path must reach git: {applied}"
        );

        // The rebase actually ran: topic's parent is now master's tip.
        assert_eq!(
            rev("topic^"),
            master_tip,
            "topic must sit directly on master's tip after the rebase"
        );
    }

    /// The whole mutation policy in one assertion: a bare mutating call is a
    /// plan, and the token binds to the argv (a different message must not
    /// share it).
    #[test]
    fn test_mutation_default_is_a_plan_not_an_execution() {
        let root = Path::new("/tmp/project");
        let op = Op::parse("commit").unwrap();
        let argv = plan_argv(op, root, &json!({ "message": "hi" })).unwrap();
        assert_eq!(argv, ["commit", "--no-gpg-sign", "-m", "hi"]);
        let token = preview_token(root, op.name, &argv, "");
        assert!(!token.is_empty());
        // A different message must not share the token.
        let other = plan_argv(op, root, &json!({ "message": "bye" })).unwrap();
        assert_ne!(token, preview_token(root, op.name, &other, ""));
        // State participates in the binding: the same argv under a different
        // repository state hashes differently (the executor's reject-on-move).
        assert_ne!(
            token,
            preview_token(root, op.name, &argv, "head-moved-or-index-changed")
        );
    }

    /// The plan's `next` field is the instruction a caller actually follows.
    /// The apply side exists now, so the hint must SAY how to apply — via the
    /// token, with confirm reserved to the safe subset — and must still state
    /// that a bare plan changed nothing. The old text ("this build cannot
    /// apply git mutations") is stale: the brief (§5) explicitly requires
    /// real apply, and the old tests pinned preview-only behaviour.
    #[test]
    fn test_plan_hint_says_how_to_apply() {
        assert!(
            PLAN_NEXT_HINT.contains("preview_token"),
            "the hint must point at the token path: {PLAN_NEXT_HINT}"
        );
        assert!(
            PLAN_NEXT_HINT.contains("confirm:true"),
            "the hint must name the safe-subset shortcut: {PLAN_NEXT_HINT}"
        );
        assert!(
            PLAN_NEXT_HINT.contains("Nothing was changed"),
            "the hint must state that no state changed: {PLAN_NEXT_HINT}"
        );
        assert!(
            PLAN_NEXT_HINT.contains("token-only"),
            "destructive shapes must be marked token-only: {PLAN_NEXT_HINT}"
        );
    }

    #[test]
    fn test_mutation_requiring_a_target_refuses_to_default() {
        let root = Path::new("/tmp/project");
        for name in ["checkout", "merge", "rebase", "reset", "force", "tag"] {
            let op = Op::parse(name).unwrap();
            let error = plan_argv(op, root, &json!({})).unwrap_err();
            assert!(
                error.message_with_hint().contains("requires 'target'"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn test_revisions_cannot_smuggle_options_or_paths() {
        for bad in ["-u", "--upload-pack=x", "HEAD:../../etc/passwd", "", "a b"] {
            assert!(
                super::super::git_ref::validate_ref(bad).is_err(),
                "{bad} must be rejected"
            );
        }
        for good in [
            "HEAD",
            "main",
            "origin/main",
            "v1.2.3",
            "HEAD~3",
            "a1b2c3d^",
            "cw32/30b-admission",
        ] {
            assert!(
                super::super::git_ref::validate_ref(good).is_ok(),
                "{good} must be accepted"
            );
        }
    }

    /// The plan validates its revision; the probe must use the SAME gate rather
    /// than rebuilding argv from the caller's raw string.
    ///
    /// This asserts on `probe_rev_argv` directly. An earlier version asserted on
    /// `probe_state` against a nonexistent repository and passed even with the
    /// guard removed — git failed for an unrelated reason — so it proved
    /// nothing.
    #[test]
    fn test_probe_validates_the_revision_before_building_argv() {
        let error = probe_rev_argv("-u").expect_err("a leading '-' must be rejected");
        assert!(
            error.message_with_hint().contains("not valid ref syntax"),
            "{error}"
        );
        assert!(
            probe_rev_argv("HEAD:../../etc/passwd").is_err(),
            "rev:path syntax must be rejected"
        );
        assert_eq!(
            probe_rev_argv("HEAD").unwrap(),
            ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]
        );
    }

    /// The schema must advertise exactly the apply path the executor
    /// implements. `confirm` and `preview_token` were removed when the apply
    /// side was unwired; the brief (§5) requires real apply, so they are back
    /// — and the destructive rails must be advertised too, because a model
    /// that cannot see `confirm_text` cannot pass the force-push gate.
    #[test]
    fn test_schema_advertises_the_apply_path() {
        let schema = GitHandler.argument_schema();
        let properties = schema["properties"].as_object().expect("properties");
        for key in [
            "confirm",
            "preview_token",
            "force",
            "confirm_text",
            "hard",
            "clean",
            "delete",
            "allow_sensitive",
            "trailers",
        ] {
            assert!(properties.contains_key(key), "{key} must be advertised");
        }
        assert_eq!(schema["required"].as_array().map(Vec::len), Some(0));
    }

    /// Rails at the argv layer: destructive shapes are only buildable with
    /// their explicit flag, and every one of them must be token-only at the
    /// executor (SAFE_TO_CONFIRM must not contain the ops that carry them).
    #[test]
    fn test_destructive_shapes_need_their_explicit_flags() {
        let root = Path::new("/tmp/project");

        // reset --hard without hard:true stays --mixed.
        let soft = plan_argv(
            Op::parse("reset").unwrap(),
            root,
            &json!({ "target": "HEAD~1", "hard": false }),
        )
        .unwrap();
        assert_eq!(soft, ["reset", "--mixed", "HEAD~1"]);
        let hard = plan_argv(
            Op::parse("reset").unwrap(),
            root,
            &json!({ "target": "HEAD~1", "hard": true }),
        )
        .unwrap();
        assert_eq!(hard, ["reset", "--hard", "HEAD~1"]);

        // Branch deletion: only via the flag shape, and only with delete:true.
        let no_flag = plan_argv(
            Op::parse("reset").unwrap(),
            root,
            &json!({ "target": "-d feature" }),
        )
        .unwrap_err();
        assert!(
            no_flag.message_with_hint().contains("delete:true"),
            "{no_flag}"
        );
        let delete = plan_argv(
            Op::parse("reset").unwrap(),
            root,
            &json!({ "target": "-d feature", "delete": true }),
        )
        .unwrap();
        assert_eq!(delete, ["branch", "-d", "feature"]);

        // clean without clean:true is refused; with it, only -d/-fd pass.
        let no_clean = plan_argv(
            Op::parse("reset").unwrap(),
            root,
            &json!({ "target": "HEAD", "clean": true }),
        )
        .unwrap_err();
        assert!(no_clean.message_with_hint().contains("-fd"), "{no_clean}");
        let clean = plan_argv(
            Op::parse("reset").unwrap(),
            root,
            &json!({ "target": "-fd", "clean": true }),
        )
        .unwrap();
        assert_eq!(clean, ["clean", "-fd", "-f"]);

        // Force-push without force:true never builds.
        let no_force = plan_argv(
            Op::parse("force").unwrap(),
            root,
            &json!({ "target": "origin/main" }),
        )
        .unwrap_err();
        assert!(
            no_force.message_with_hint().contains("force:true"),
            "{no_force}"
        );

        // The executor's confirm subset must exclude every op that can carry
        // one of the destructive shapes.
        let safe: &[&str] = &super::super::git_apply::SAFE_TO_CONFIRM;
        for op in ["reset", "force"] {
            assert!(
                !safe.contains(&op),
                "{op} can be destructive and must not be confirmable without a token"
            );
        }
    }

    /// commit paths rides git's native pathspec commit (no temp index): the
    /// argv is `commit … -- <paths>`, trailers ride `--trailer` one per line,
    /// and a pathspec commit must not sweep other staged changes in — that is
    /// git's documented `git commit -- <paths>` behaviour, so the exact-args
    /// test plus the integration test (commit paths doesn't include other
    /// staged files) pin both halves.
    #[test]
    fn test_commit_paths_use_pathspec_semantics_and_trailers_validate() {
        let repo = scratch_repo();
        let root = repo.path();
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        let args = json!({
            "message": "subject",
            "paths": ["a.txt"],
            "trailers": ["Co-Authored-By: T <t@t>"],
        });
        let argv = commit_argv(root, &args).unwrap();
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--".to_string(), "a.txt".to_string()].as_slice()),
            "paths must arrive after -- as a pathspec: {argv:?}"
        );
        assert_eq!(
            argv.iter().filter(|a| a.as_str() == "--trailer").count(),
            1,
            "one --trailer flag per line: {argv:?}"
        );
        assert!(
            !argv.contains(&"b.txt".to_string()),
            "commit paths must not include unrequested files: {argv:?}"
        );
        // Trailers are single-line only; a newline would smuggle a paragraph.
        let bad =
            commit_argv(root, &json!({ "message": "m", "trailers": ["two\nlines"] })).unwrap_err();
        assert!(bad.message_with_hint().contains("single line"), "{bad}");
        // Empty paths array is a caller error, not a whole-tree commit.
        let empty = commit_argv(root, &json!({ "message": "m", "paths": [] })).unwrap_err();
        assert!(
            empty.message_with_hint().contains("at least one"),
            "{empty}"
        );
    }

    /// Unknown keys are refused; known-but-ignored ones are reported.
    ///
    /// Nothing used to look at unrecognized arguments, so a typo and a bound
    /// that applied were indistinguishable.
    #[test]
    fn test_argument_hygiene_refuses_unknown_and_warns_on_ignored() {
        let branch = Op::parse("branch").unwrap();
        // A typo must not be silently dropped.
        let error = argument_warnings(branch, &json!({ "max_cnt": 5 })).unwrap_err();
        assert!(error.message_with_hint().contains("max_cnt"), "{error}");
        assert!(
            error.message_with_hint().contains("name_prefix"),
            "the hint must list what branch accepts: {error}"
        );

        // A key another operation reads is reported, not obeyed silently.
        let warnings = argument_warnings(branch, &json!({ "target": "HEAD" })).unwrap();
        assert_eq!(warnings, vec!["git branch ignores 'target'".to_string()]);

        // Router/transport keys are legitimate and produce nothing.
        assert!(
            argument_warnings(
                branch,
                &json!({ "project_path": "/x", "max_latency_ms": 5, "token_budget": 10 })
            )
            .unwrap()
            .is_empty()
        );
        // `max_chars` truncates every response, so it is honored everywhere.
        assert!(
            argument_warnings(Op::parse("show").unwrap(), &json!({ "max_chars": 10 }))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_bounds_are_clamped() {
        assert_eq!(
            extract_bounded_u64(&json!({ "max_count": 10_000 }), "max_count", 20, MAX_COUNT)
                .unwrap(),
            MAX_COUNT
        );
        assert_eq!(
            extract_bounded_usize(&json!({}), "max_chars", DEFAULT_MAX_CHARS, MAX_MAX_CHARS)
                .unwrap(),
            DEFAULT_MAX_CHARS
        );
        assert!(
            extract_bounded_u64(&json!({ "max_count": "x" }), "max_count", 20, MAX_COUNT).is_err()
        );
    }

    #[test]
    fn test_remote_parsing_tolerates_an_empty_repository() {
        // A repo with no remotes prints nothing; that is an answer, not a failure.
        let parsed = "".lines().filter(|line| !line.trim().is_empty()).count();
        assert_eq!(parsed, 0);
    }

    /// §4.5: `show` is compact by default — stat, no patch. The patch is
    /// opt-in via `include_patch: true`.
    #[test]
    fn test_show_defaults_to_stat_and_omits_the_patch() {
        let repo = scratch_repo();
        let compact = git_show(repo.path(), &json!({})).unwrap();
        assert_eq!(compact["include_patch"], false);
        assert!(
            compact.get("patch").is_none(),
            "compact show must not carry a patch: {compact}"
        );
        assert!(
            compact["stat"]
                .as_str()
                .is_some_and(|stat| stat.contains("f.txt")),
            "stat names the changed file: {compact}"
        );

        let with_patch = git_show(repo.path(), &json!({ "include_patch": true })).unwrap();
        assert_eq!(with_patch["include_patch"], true);
        assert!(
            with_patch["patch"].is_string(),
            "include_patch:true carries the patch: {with_patch}"
        );
    }

    /// Helper: a scratch repository with one commit on branch `master`.
    fn scratch_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .expect("git available");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "master"]);
        std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        dir
    }

    /// Regression: blame passed `--no-color`, which git-blame reads as an
    /// ambiguous option prefix and rejects with exit 129 — so every blame call
    /// failed. Color is now suppressed with `-c color.ui=false`.
    ///
    /// Both shapes are exercised: with a `target` (argv carries the rev before
    /// `--`) and without (no rev at all).
    #[test]
    fn test_blame_works_with_and_without_a_target() {
        let repo = scratch_repo();
        for args in [
            json!({ "path": "f.txt" }),
            json!({ "path": "f.txt", "target": "HEAD" }),
        ] {
            let value =
                git_blame(repo.path(), &args).unwrap_or_else(|error| panic!("{args}: {error}"));
            assert_eq!(value["line_count"], 2, "{args}: {value}");
            // Each blamed line carries the abbreviated commit oid and the
            // blamed source text. `git blame` prints no commit SUBJECT, so
            // asserting on one tests a feature blame does not have. A boundary
            // commit (the root, which every line here is) is prefixed with
            // `^`, so that marker is part of valid output.
            let text = value["text"].as_str().unwrap_or_default();
            for line in text.lines() {
                let oid = line
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_start_matches('^');
                assert!(
                    oid.len() >= 7 && oid.chars().all(|c| c.is_ascii_hexdigit()),
                    "each line must name its last-touching commit: {line}"
                );
            }
            assert!(text.contains(") a"), "line 1 content missing: {value}");
            assert!(text.contains(") b"), "line 2 content missing: {value}");
            // The reported target defaults to HEAD when none was supplied.
            assert_eq!(value["target"], "HEAD");
        }
    }

    /// Regression: `branch` returned every entry it found, ignoring `max_count`,
    /// with a `truncated` flag that only tracked the byte cap — so a capped list
    /// looked complete.
    #[test]
    fn test_branch_honours_max_count_and_reports_the_dropped_tail() {
        let repo = scratch_repo();
        for name in ["cw32/30b-admission", "cw32/31c-other", "feature/x"] {
            let status = std::process::Command::new("git")
                .args(["branch", name])
                .current_dir(repo.path())
                .status()
                .unwrap();
            assert!(status.success());
        }

        let value = git_branch(repo.path(), &json!({ "max_count": 2 })).unwrap();
        assert_eq!(value["count"], 2, "{value}");
        assert_eq!(value["total"], 4, "master + three branches: {value}");
        assert_eq!(value["truncated"], true);
        assert_eq!(
            value["branches"].as_array().map(Vec::len),
            Some(2),
            "max_count is a real bound: {value}"
        );

        // A generous bound is not truncated.
        let full = git_branch(repo.path(), &json!({ "max_count": 20 })).unwrap();
        assert_eq!(full["total"], 4);
        assert_eq!(full["truncated"], false);

        // The prefix filter is a prefix, not a substring.
        let filtered = git_branch(
            repo.path(),
            &json!({ "name_prefix": "cw32/", "max_count": 20 }),
        )
        .unwrap();
        let names: Vec<&str> = filtered["branches"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|branch| branch["name"].as_str())
            .collect();
        assert_eq!(names.len(), 2, "{filtered}");
        assert!(names.iter().all(|name| name.starts_with("cw32/")));
        assert_eq!(filtered["total"], 2);

        let substring = git_branch(
            repo.path(),
            &json!({ "name_prefix": "30b", "max_count": 20 }),
        )
        .unwrap();
        assert_eq!(
            substring["count"], 0,
            "'30b' is not a prefix of cw32/30b-admission: {substring}"
        );

        // Local-only is the default; `local_only: false` adds remote-tracking
        // refs, so the default must NOT be `--all`.
        let local = git_branch(repo.path(), &json!({ "max_count": 20 })).unwrap();
        assert_eq!(local["local_only"], true);
        assert!(local["branches"].as_array().unwrap().iter().all(|branch| {
            !branch["name"]
                .as_str()
                .unwrap_or_default()
                .starts_with("remotes/")
        }));
    }
}
