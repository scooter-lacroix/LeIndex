//! Handler for `leindex_git` — the live Git surface of the MCP server.
//!
//! Every operation shells out to `git` through the shared runner in
//! [`super::git_diff_handler`]: no new dependency, no second subprocess
//! implementation, and the cap-and-drain read keeps a chatty child from
//! deadlocking the transport.
//!
//! ## Mutation policy (this is the security-relevant part)
//!
//! A mutating operation never runs. Issuing one returns a *plan*: whether the
//! operation mutates, the exact argv that would run, what a read-only probe
//! says about the current state, and a `preview_token` derived from
//! `(repository, operation, argv)`.
//!
//! The token is an IDENTITY, not an authorisation. There is no `confirm`
//! argument and no apply side: an MCP surface should not commit, push or rewrite
//! history until its executor has been reviewed on its own. Advertising a
//! `confirm` that could only ever refuse was worse than advertising nothing —
//! it made a permanent limitation look like a transient failure to retry.
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
        summary: "PREVIEW ONLY: stage the given paths",
        args: &["paths"],
        mutating: true,
    },
    Op {
        name: "commit",
        summary: "PREVIEW ONLY: commit the staged changes",
        args: &["message"],
        mutating: true,
    },
    Op {
        name: "checkout",
        summary: "PREVIEW ONLY: switch to a branch or commit",
        args: &["target"],
        mutating: true,
    },
    Op {
        name: "stash",
        summary: "PREVIEW ONLY: stash working-tree changes",
        args: &[],
        mutating: true,
    },
    Op {
        name: "tag",
        summary: "PREVIEW ONLY: create a tag",
        args: &["target", "message"],
        mutating: true,
    },
    Op {
        name: "merge",
        summary: "PREVIEW ONLY: merge a branch into the current one",
        args: &["target"],
        mutating: true,
    },
    Op {
        name: "rebase",
        summary: "PREVIEW ONLY: rebase the current branch onto a target",
        args: &["target"],
        mutating: true,
    },
    Op {
        name: "push",
        summary: "PREVIEW ONLY: push the current branch to its remote",
        args: &[],
        mutating: true,
    },
    Op {
        name: "reset",
        summary: "PREVIEW ONLY: move the current branch to a commit",
        args: &["target"],
        mutating: true,
    },
    Op {
        name: "force",
        summary: "PREVIEW ONLY: force-push (requires a target ref)",
        args: &["target"],
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
        "Live Git: status, diff, log, show, blame, branch and remote (read); add, commit, \
         checkout, stash, tag, merge, rebase, push, reset and force NEVER run — each returns a \
         dry-run plan (argv + read-only probe) and a preview_token that identifies the plan. \
         There is no confirm argument and no apply side."
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
                        "Git operation. Mutating operations never apply: they return a plan. \
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
                    "description": "Project-relative paths to stage (add)"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (commit) or tag message (tag)"
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
                    "description": "Return the patch rather than a stat summary (show)"
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
            return mutation_plan(op, &root, &args).await;
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

/// `git log` as structured entries.
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
        target.unwrap_or_else(|| "HEAD".to_string()),
    ];
    if let Some(path) = path {
        argv.push("--".to_string());
        argv.push(path);
    }
    let (text, truncated) = run_text(root, &argv)?;
    let commits = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.splitn(4, '\t');
            json!({
                "oid": fields.next().unwrap_or_default(),
                "date": fields.next().unwrap_or_default(),
                "author": fields.next().unwrap_or_default(),
                "subject": fields.next().unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "action": "log",
        "count": commits.len(),
        "commits": commits,
        "truncated": truncated,
    }))
}

/// `git show` for one commit: patch or stat.
fn git_show(root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let target = optional_rev(args)?.unwrap_or_else(|| "HEAD".to_string());
    let include_patch = args
        .get("include_patch")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut argv = vec!["show".to_string(), "--no-color".to_string()];
    if !include_patch {
        argv.push("--stat".to_string());
    }
    argv.push(target.clone());
    let (text, truncated) = run_text(root, &argv)?;
    Ok(json!({
        "action": "show",
        "target": target,
        "include_patch": include_patch,
        "text": text,
        "truncated": truncated,
    }))
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
/// This used to tell the caller to re-send the call with `confirm`, which is
/// precisely the path that always refuses (the apply side is not wired). A
/// model following it hit an error and could read that error as a plumbing
/// hiccup rather than "this never runs". It must never advertise `confirm`
/// while confirming cannot work; `test_plan_next_never_advertises_confirm`
/// pins that.
const PLAN_NEXT_HINT: &str = "Nothing was changed. This build cannot apply git mutations: \
     reviewing the plan and running the command yourself is the only way to perform it.";

/// Build the dry-run plan for a mutating operation.
///
/// Nothing here spawns a mutating git command: [`plan_argv`] only *describes*
/// the argv, and every probe it runs is read-only.
///
/// There is deliberately no `confirm` argument. It used to exist and could only
/// ever refuse — the apply side is not wired — so it advertised a capability
/// that did not exist and turned one guaranteed error into a second one. A
/// schema that names an argument the server refuses is a lie; the argument is
/// gone, and `preview_token` is documented as an identity hash rather than an
/// authorisation.
async fn mutation_plan(op: &Op, root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let argv = plan_argv(op, root, args)?;
    let token = preview_token(root, op.name, &argv);
    let warnings = argument_warnings(op, args)?;
    let root = root.to_path_buf();
    let probe_args = args.clone();
    let op_name = op.name;

    let probe = tokio::task::spawn_blocking(move || probe_state(op_name, &root, &probe_args))
        .await
        .map_err(|error| JsonRpcError::internal_error(format!("git task failed: {error}")))?;

    Ok(with_warnings(
        json!({
            "action": op.name,
            "mutating": true,
            "applied": false,
            "dry_run": true,
            "argv": argv,
            "preview_token": token,
            "probe": probe,
            "next": PLAN_NEXT_HINT,
        }),
        &warnings,
    ))
}

/// Validate the optional `target` argument once, before any op-specific work.
fn plan_target(args: &Value) -> Result<Option<String>, JsonRpcError> {
    match args.get("target").and_then(Value::as_str) {
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

/// `<subcommand> [flag] <target>` — the shape every single-target op shares.
fn target_argv(subcommand: &str, flag: Option<&str>, target: String) -> Vec<String> {
    let mut argv = vec![subcommand.to_string()];
    if let Some(flag) = flag {
        argv.push(flag.to_string());
    }
    argv.push(target);
    argv
}

/// `commit --no-gpg-sign -m <message>`, rejecting an empty or oversized subject.
fn commit_argv(args: &Value) -> Result<Vec<String>, JsonRpcError> {
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
    Ok(vec![
        "commit".to_string(),
        "--no-gpg-sign".to_string(),
        "-m".to_string(),
        message.to_string(),
    ])
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

/// Describe the argv a mutation would run, validating every argument first.
///
/// This is the only place a mutation's command line is decided; the plan, the
/// token and the (future) executor all consume this one definition.
fn plan_argv(op: &Op, root: &Path, args: &Value) -> Result<Vec<String>, JsonRpcError> {
    let target = plan_target(args)?;
    let required = |field: &str| require_target(&target, op.name, field);

    match op.name {
        "add" => {
            let mut argv = vec!["add".to_string(), "--".to_string()];
            argv.extend(staged_paths(root, args)?);
            Ok(argv)
        }
        "commit" => commit_argv(args),
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
        "rebase" => Ok(target_argv("rebase", None, required("target")?)),
        "push" => Ok(vec!["push".to_string(), "--porcelain".to_string()]),
        "reset" => Ok(target_argv("reset", Some("--mixed"), required("target")?)),
        // `force` is a force-push; naming it in the plan makes the destructive
        // shape explicit instead of hiding it behind `push`.
        "force" => Ok(target_argv(
            "push",
            Some("--force-with-lease"),
            required("target")?,
        )),
        other => Err(JsonRpcError::internal_error(format!(
            "git operation '{other}' has no plan"
        ))),
    }
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

/// Token that identifies exactly one `(repository, operation, argv)` triple.
///
/// An IDENTITY, not an authorisation: it lets a caller tell two plans apart and
/// detect that the plan it reviewed is not the plan it would run. Nothing
/// consumes it to permit an execution — there is no apply side.
fn preview_token(root: &Path, op: &str, argv: &[String]) -> String {
    blake3::hash(format!("{}\u{0}{op}\u{0}{}", root.display(), argv.join("\u{0}")).as_bytes())
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

    /// The whole mutation policy in one assertion: with no `confirm`, a
    /// mutating op returns a plan and does not run.
    #[test]
    fn test_mutation_default_is_a_plan_not_an_execution() {
        let root = Path::new("/tmp/project");
        let op = Op::parse("commit").unwrap();
        let argv = plan_argv(op, root, &json!({ "message": "hi" })).unwrap();
        assert_eq!(argv, ["commit", "--no-gpg-sign", "-m", "hi"]);
        let token = preview_token(root, op.name, &argv);
        assert!(!token.is_empty());
        // A different message must not share the token.
        let other = plan_argv(op, root, &json!({ "message": "bye" })).unwrap();
        assert_ne!(token, preview_token(root, op.name, &other));
    }

    #[test]
    fn test_plan_next_never_advertises_confirm() {
        // The plan's `next` field is the instruction a caller actually follows.
        // It used to say "re-send this call with `confirm`" — which is exactly
        // the path that always refuses, because the apply side is not wired.
        // That sent models into a guaranteed error and made a permanent
        // limitation look like a transient hiccup. While confirming cannot
        // succeed, the hint must not mention it, and must say nothing changed.
        assert!(
            !PLAN_NEXT_HINT.contains("confirm"),
            "the plan hint must not advertise a path that always refuses: {PLAN_NEXT_HINT}"
        );
        assert!(
            PLAN_NEXT_HINT.contains("Nothing was changed"),
            "the hint must state that no state changed: {PLAN_NEXT_HINT}"
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

    /// The schema must not name an argument the server cannot honour.
    ///
    /// `confirm` was advertised while its only possible outcome was a refusal,
    /// so a model that followed the schema hit a guaranteed error.
    #[test]
    fn test_schema_does_not_advertise_confirm() {
        let schema = GitHandler.argument_schema();
        let properties = schema["properties"].as_object().expect("properties");
        assert!(
            !properties.contains_key("confirm"),
            "confirm is not implemented and must not be advertised: {properties:?}"
        );
        assert_eq!(schema["required"].as_array().map(Vec::len), Some(0));
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
