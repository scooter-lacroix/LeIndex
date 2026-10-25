//! Handler for `leindex_git` — the live Git surface of the MCP server.
//!
//! Every operation shells out to `git` through the shared runner in
//! [`super::git_diff_handler`]: no new dependency, no second subprocess
//! implementation, and the cap-and-drain read keeps a chatty child from
//! deadlocking the transport.
//!
//! ## Mutation policy (this is the security-relevant part)
//!
//! A mutating operation never runs from a default argument set. Issuing one
//! returns a *plan*: whether the operation mutates, the exact argv that would
//! run, what a read-only probe says about the current state, and a
//! `preview_token` derived from `(repository, operation, argv)`.
//!
//! Executing requires echoing that token back in `confirm`. A token minted for
//! one argv therefore cannot authorise a different one.
//!
//! The applying half is deliberately NOT wired in this pass: an MCP surface
//! should not commit, push or rewrite history until its executor has been
//! reviewed on its own. A `confirm` carrying the matching token is refused with
//! an explicit message naming that gap, so no mutation can fire and nothing
//! silently reports success for work it did not do.

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
        summary: "Local and remote branches with tip commits",
        args: &[],
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
         checkout, stash, tag, merge, rebase, push, reset and force NEVER run unconfirmed — \
         each returns a dry-run plan and a preview_token."
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
                        "Git operation. Mutating operations are preview-only until confirmed. \
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
                    "description": "Revision, ref or branch (log/show/blame/checkout/tag/merge/rebase/reset/force)"
                },
                "path": {
                    "type": "string",
                    "description": "Project-relative file or directory (log pathspec, blame file)"
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
                    "description": "History entries to return (log)"
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
                    "description": "Bound on returned git output"
                },
                "confirm": {
                    "type": "string",
                    "description": "The preview_token of the plan being confirmed (mutations only)"
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
        let raw_action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status");
        let op = Op::parse(raw_action).ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                format!("Unknown git action '{raw_action}'"),
                format!(
                    "Set action to one of: {}",
                    OPS.iter().map(|op| op.name).collect::<Vec<_>>().join(", ")
                ),
            )
        })?;
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

        let payload = read_operation(op, &root, &args).await?;
        Ok(wrap_live_with_meta(payload, &root))
    }
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
        "branch" => git_branch(&root),
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
    let mut argv = vec![
        "blame".to_string(),
        "--no-color".to_string(),
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

/// `git branch -a -vv` as structured rows.
fn git_branch(root: &Path) -> Result<Value, JsonRpcError> {
    let argv = vec![
        "branch".to_string(),
        "--all".to_string(),
        "--no-color".to_string(),
        "-vv".to_string(),
    ];
    let (text, truncated) = run_text(root, &argv)?;
    let branches = text
        .lines()
        .filter(|line| !line.trim().is_empty())
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
        .collect::<Vec<_>>();
    Ok(json!({
        "action": "branch",
        "count": branches.len(),
        "branches": branches,
        "truncated": truncated,
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
async fn mutation_plan(op: &Op, root: &Path, args: &Value) -> Result<Value, JsonRpcError> {
    let argv = plan_argv(op, root, args)?;
    let token = preview_token(root, op.name, &argv);
    let root = root.to_path_buf();
    let confirm = args
        .get("confirm")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let probe_args = args.clone();
    let op_name = op.name;

    if let Some(confirm) = confirm {
        // Token mode: the token must match THIS operation and target.
        if confirm != token {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                format!(
                    "preview token mismatch for git {op_name}: the token does not match this \
                     operation and target, so it does not authorise it"
                ),
                "Re-issue the operation without 'confirm' to get a fresh plan and token"
                    .to_string(),
            ));
        }
        return Err(JsonRpcError::invalid_params_with_suggestion(
            format!(
                "NOTHING WAS CHANGED: git {op_name} is preview-only in this build because the \
                 confirmed apply side is not wired. This is not a permissions or plumbing \
                 problem — the capability does not exist yet, and retrying will not help"
            ),
            "Review the plan (read-only) and run the equivalent git command yourself if you \
             want it performed; this server will not perform it"
                .to_string(),
        ));
    }

    let probe = tokio::task::spawn_blocking(move || probe_state(op_name, &root, &probe_args))
        .await
        .map_err(|error| JsonRpcError::internal_error(format!("git task failed: {error}")))?;

    Ok(json!({
        "action": op.name,
        "mutating": true,
        "applied": false,
        "dry_run": true,
        "argv": argv,
        "preview_token": token,
        "probe": probe,
        "next": PLAN_NEXT_HINT,
    }))
}

/// Describe the argv a mutation would run, validating every argument first.
///
/// This is the only place a mutation's command line is decided; the plan, the
/// token and the (future) executor all consume this one definition.
fn plan_argv(op: &Op, root: &Path, args: &Value) -> Result<Vec<String>, JsonRpcError> {
    let target = args.get("target").and_then(Value::as_str);
    let target = match target {
        Some(target) => Some(validated_rev(target)?),
        None => None,
    };
    let require_target = |field: &str| -> Result<String, JsonRpcError> {
        target.clone().ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                format!("git {} requires '{field}'", op.name),
                format!("Provide {field}: a branch name, tag or commit oid"),
            )
        })
    };

    let argv = match op.name {
        "add" => {
            let paths = staged_paths(root, args)?;
            let mut argv = vec!["add".to_string(), "--".to_string()];
            argv.extend(paths);
            argv
        }
        "commit" => {
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
            vec![
                "commit".to_string(),
                "--no-gpg-sign".to_string(),
                "-m".to_string(),
                message.to_string(),
            ]
        }
        "checkout" => vec![
            "checkout".to_string(),
            "--no-guess".to_string(),
            require_target("target")?,
        ],
        "stash" => vec![
            "stash".to_string(),
            "push".to_string(),
            "--include-untracked".to_string(),
        ],
        "tag" => {
            let name = require_target("target")?;
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
        "merge" => vec![
            "merge".to_string(),
            "--no-edit".to_string(),
            require_target("target")?,
        ],
        "rebase" => vec!["rebase".to_string(), require_target("target")?],
        "push" => vec!["push".to_string(), "--porcelain".to_string()],
        "reset" => vec![
            "reset".to_string(),
            "--mixed".to_string(),
            require_target("target")?,
        ],
        // `force` is a force-push; naming it in the plan makes the destructive
        // shape explicit instead of hiding it behind `push`.
        "force" => vec![
            "push".to_string(),
            "--force-with-lease".to_string(),
            require_target("target")?,
        ],
        other => {
            return Err(JsonRpcError::internal_error(format!(
                "git operation '{other}' has no plan"
            )));
        }
    };
    Ok(argv)
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
/// Validation lives here rather than at the call site so a rejected revision
/// cannot reach the process at all, and so the rejection is directly testable
/// (a probe against a missing repository fails for an unrelated reason and
/// would hide a missing guard).
fn probe_rev_argv(raw: &str) -> Result<Vec<String>, JsonRpcError> {
    let target = validated_rev(raw)?;
    Ok(vec![
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        format!("{target}^{{commit}}"),
    ])
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

/// Token that authorises exactly one `(repository, operation, argv)` triple.
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

/// Optional revision argument, rejected if it could be read as an option.
fn optional_rev(args: &Value) -> Result<Option<String>, JsonRpcError> {
    match args.get("target").and_then(Value::as_str) {
        Some(target) => Ok(Some(validated_rev(target)?)),
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

/// Reject a revision that could be parsed as a git option or escape the repo.
///
/// Revisions are passed as a single argv entry (never through a shell), so the
/// remaining exposure is a leading `-` being read as a flag and `rev:path`
/// syntax reaching outside the repository. `:` is therefore NOT allowed: no
/// revision syntax needs it (ranges use `..`, the reflog uses `@{0}`), and
/// allowing it is what let `HEAD:../../etc/passwd` through to `git show`.
fn validated_rev(raw: &str) -> Result<String, JsonRpcError> {
    let rev = raw.trim();
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._/~^@{}+=,%".contains(c);
    if rev.is_empty() || rev.len() > 200 || rev.starts_with('-') || !rev.chars().all(allowed) {
        return Err(JsonRpcError::invalid_params_with_suggestion(
            format!("'{raw}' is not a usable revision"),
            "Use a branch name, tag, or commit oid (letters, digits, . _ / ~ ^ @ { } , + = %)"
                .to_string(),
        ));
    }
    Ok(rev.to_string())
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
            assert!(validated_rev(bad).is_err(), "{bad} must be rejected");
        }
        for good in [
            "HEAD",
            "main",
            "origin/main",
            "v1.2.3",
            "HEAD~3",
            "a1b2c3d^",
        ] {
            assert!(validated_rev(good).is_ok(), "{good} must be accepted");
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
            error.message_with_hint().contains("not a usable revision"),
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
}
