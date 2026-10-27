use super::helpers::{extract_bool, extract_usize, get_direct_callers, node_type_str};
use super::protocol::JsonRpcError;
use super::request_meta::{WorkBudget, record_git_ms, record_pdg_ms};
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::graph::pdg::{NodeId, ProgramDependenceGraph, TraversalConfig};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

/// Upper bound on diff bytes read from git (the patch is bounded again for output).
const MAX_DIFF_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_PATCH_CHARS: usize = 20_000;

/// Handler for PDG-enriched `git diff`.
#[derive(Clone)]
pub struct GitDiffHandler;

/// What to diff.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffSpec {
    /// Working tree (staged + unstaged) against `HEAD`.
    WorkingTree,
    /// Index against `HEAD`.
    Staged,
    /// One commit against its parent.
    Ref(String),
    /// `a..b` / `a...b` (or any single rev-range git accepts).
    Range(String),
}

impl DiffSpec {
    fn kind(&self) -> &'static str {
        match self {
            DiffSpec::WorkingTree => "working_tree",
            DiffSpec::Staged => "staged",
            DiffSpec::Ref(_) => "ref",
            DiffSpec::Range(_) => "range",
        }
    }

    /// Revision arguments placed between the fixed flags and `--`.
    fn rev_args(&self, has_head: bool) -> Vec<String> {
        match self {
            DiffSpec::WorkingTree if has_head => vec!["HEAD".to_string()],
            DiffSpec::WorkingTree => vec!["--cached".to_string()],
            DiffSpec::Staged if has_head => vec!["--cached".to_string(), "HEAD".to_string()],
            DiffSpec::Staged => vec!["--cached".to_string()],
            DiffSpec::Ref(rev) => vec![format!("{rev}^!")],
            DiffSpec::Range(range) => vec![range.clone()],
        }
    }

    /// The new side of the diff is the file on disk, so hunk line numbers can
    /// be mapped onto the resident PDG's byte ranges.
    fn new_side_is_worktree(&self) -> bool {
        matches!(self, DiffSpec::WorkingTree)
    }
}

/// Parsed `--name-status` letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileState {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

impl FileState {
    fn as_str(self) -> &'static str {
        match self {
            FileState::Added => "added",
            FileState::Modified => "modified",
            FileState::Deleted => "deleted",
            FileState::Renamed => "renamed",
            FileState::Copied => "copied",
            FileState::TypeChanged => "type_changed",
        }
    }
}

#[derive(Debug, Clone)]
struct ChangedFile {
    path: String,
    old_path: Option<String>,
    state: FileState,
    additions: Option<u64>,
    deletions: Option<u64>,
}

/// Parse `git diff --name-status -z -M` output.
fn parse_name_status(bytes: &[u8]) -> Vec<ChangedFile> {
    let mut tokens = bytes
        .split(|b| *b == 0)
        .map(|t| String::from_utf8_lossy(t).into_owned());
    let mut files = Vec::new();
    while let Some(status) = tokens.next() {
        let Some(letter) = status.chars().next() else {
            continue;
        };
        let state = match letter {
            'A' => FileState::Added,
            'M' => FileState::Modified,
            'D' => FileState::Deleted,
            'R' => FileState::Renamed,
            'C' => FileState::Copied,
            'T' => FileState::TypeChanged,
            _ => FileState::Modified,
        };
        if matches!(state, FileState::Renamed | FileState::Copied) {
            let (Some(old), Some(new)) = (tokens.next(), tokens.next()) else {
                break;
            };
            files.push(ChangedFile {
                path: new,
                old_path: Some(old),
                state,
                additions: None,
                deletions: None,
            });
        } else {
            let Some(path) = tokens.next() else { break };
            files.push(ChangedFile {
                path,
                old_path: None,
                state,
                additions: None,
                deletions: None,
            });
        }
    }
    files
}

/// Parse `git diff --numstat -z -M` into `path -> (additions, deletions)`.
/// Binary files report `-` and are recorded as `None`.
fn parse_numstat(bytes: &[u8]) -> BTreeMap<String, (Option<u64>, Option<u64>)> {
    let mut tokens = bytes
        .split(|b| *b == 0)
        .map(|t| String::from_utf8_lossy(t).into_owned());
    let mut stats = BTreeMap::new();
    while let Some(token) = tokens.next() {
        let mut parts = token.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let counts = (added.parse().ok(), deleted.parse().ok());
        if path.is_empty() {
            // Rename/copy: the next two NUL-separated fields are old, new.
            let (Some(_old), Some(new)) = (tokens.next(), tokens.next()) else {
                break;
            };
            stats.insert(new, counts);
        } else {
            stats.insert(path.to_string(), counts);
        }
    }
    stats
}

/// Whether a line met while inside a hunk body must resync the parser to
/// header state instead of being consumed as body.
///
/// `@@ ` and `diff --git ` open headers that can never be body content
/// (valid body lines begin with ' ', '-', '+' or '\\'), so they resync
/// unconditionally — otherwise a crafted count could swallow the rest of
/// the patch, silently dropping every remaining hunk and file. A `+++ ` or
/// `--- ` line is ambiguous (an added line whose content itself starts
/// with "++ " renders as `+++ …`): those stay body unless the counters are
/// implausible — claiming more remaining body than the patch has lines
/// left — which is exactly the crafted-count case.
fn is_body_resync(line: &str, body_old: usize, body_new: usize, remaining: usize) -> bool {
    if line.starts_with("@@ ") || line.starts_with("diff --git ") {
        return true;
    }
    let header_like = line.starts_with("+++ ") || line.starts_with("--- ");
    let implausible = body_old > remaining || body_new > remaining;
    header_like && implausible
}

/// New-side line ranges (1-based, inclusive) touched per file, from a
/// `-U0` unified diff.
///
/// Hunk bodies are tracked: an added line whose *content* begins with `++`
/// (`+++ …`) or `--` must not be mistaken for a file header while the body
/// counters are plausible. Without the body state, such a line cleared the
/// current file and silently dropped every remaining hunk of that file;
/// see [`is_body_resync`] for how a crafted count is prevented from
/// swinging to the opposite failure mode.
fn parse_hunks(patch: &str) -> BTreeMap<String, Vec<(usize, usize)>> {
    let mut hunks: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut current: Option<String> = None;
    // Lines still expected inside the current hunk body (old-side and
    // new-side counters; both zero = between hunks, where `+++`/`---`
    // headers are meaningful).
    let mut body_old = 0usize;
    let mut body_new = 0usize;
    let lines: Vec<&str> = patch.lines().collect();
    for (idx, &line) in lines.iter().enumerate() {
        let remaining = lines.len() - idx - 1;
        if body_old > 0 || body_new > 0 {
            if is_body_resync(line, body_old, body_new, remaining) {
                body_old = 0;
                body_new = 0;
            } else {
                match line.as_bytes().first() {
                    Some(b'-') => body_old = body_old.saturating_sub(1),
                    Some(b'+') => body_new = body_new.saturating_sub(1),
                    // "\ No newline at end of file" belongs to the previous line.
                    Some(b'\\') => {}
                    // Context line (or anything mangled): consumes both sides.
                    _ => {
                        body_old = body_old.saturating_sub(1);
                        body_new = body_new.saturating_sub(1);
                    }
                }
                continue;
            }
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            current = path
                .strip_prefix("b/")
                .map(str::to_string)
                .filter(|p| p != "/dev/null");
        } else if line.starts_with("--- ") {
            continue;
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            let Some(file) = current.as_ref() else {
                continue;
            };
            let side = |prefix: char| -> usize {
                rest.split_whitespace()
                    .find(|part| part.starts_with(prefix))
                    .and_then(|part| part[1..].split_once(','))
                    .and_then(|(_, count)| count.parse::<usize>().ok())
                    .unwrap_or(1)
            };
            let count = side('-');
            let Some(new_side) = rest.split_whitespace().find(|part| part.starts_with('+')) else {
                continue;
            };
            let spec = new_side.trim_start_matches('+');
            let (start, hit_count) = match spec.split_once(',') {
                Some((start, count)) => (start.parse::<usize>(), count.parse::<usize>()),
                None => (spec.parse::<usize>(), Ok(1)),
            };
            if let (Ok(start), Ok(hit_count)) = (start, hit_count) {
                // A pure deletion (count 0) sits between lines: attribute it to
                // the line where the removal happened. Saturating arithmetic:
                // the counts come straight from (possibly crafted) patch text
                // via a custom diff driver or textconv, and `start + count - 1`
                // on usize::MAX overflowed — a panic in debug builds and a
                // whole-file range in release.
                let start = start.max(1);
                let end = if hit_count == 0 {
                    start
                } else {
                    start.saturating_add(hit_count.saturating_sub(1))
                };
                hunks.entry(file.clone()).or_default().push((start, end));
                // Enter the hunk body with the remaining (non-context) side
                // counts so body lines cannot be parsed as headers. The -U0
                // form has no context lines; `count` lines are `-`, `hit_count`
                // are `+`. The counts come from possibly crafted text and can
                // be absurd; the header-resync at the top of the loop is what
                // keeps a crafted count from swallowing the rest of the patch.
                body_old = count.min(u32::MAX as usize);
                body_new = hit_count.min(u32::MAX as usize);
            }
        }
    }
    hunks
}

/// Byte offset of the start of each 1-based line in `content`.
fn line_starts(content: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (index, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// Byte range covered by 1-based inclusive lines `start..=end`.
fn lines_to_bytes(starts: &[usize], total: usize, start: usize, end: usize) -> (usize, usize) {
    let from = starts.get(start - 1).copied().unwrap_or(total);
    let to = starts.get(end).copied().unwrap_or(total);
    (from, to.max(from))
}

/// Run git, reading at most `cap` bytes of stdout. Returns `(stdout, truncated)`.
///
/// Shared by every live-git handler: both pipes are drained concurrently (see
/// [`drain_git_output`]), so a child that fills stderr cannot deadlock the read,
/// and a runaway command is killed instead of buffered without bound.
/// `GIT_TERMINAL_PROMPT=0` keeps a command that would ask for credentials
/// (`push`, a `fetch` triggered by `pull`) from blocking the server forever.
pub(crate) fn run_git_capped(
    root: &Path,
    args: &[String],
    cap: u64,
) -> Result<(Vec<u8>, bool), std::io::Error> {
    let child = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    drain_git_output(child, cap)
}

/// Run git and decode stdout as text. `cap` bounds the bytes kept; the flag
/// reports whether the cap (and therefore the text) was cut short.
///
/// Empty output is NOT an error: `git remote -v` on a repository without
/// remotes legitimately prints nothing, and the caller decides what that means.
pub(crate) fn git_output_capped(
    root: &Path,
    args: &[String],
    cap: u64,
) -> Result<(String, bool), std::io::Error> {
    let (stdout, truncated) = run_git_capped(root, args, cap)?;
    Ok((String::from_utf8_lossy(&stdout).into_owned(), truncated))
}

/// Read a spawned child's piped stdout (capped at `cap` bytes) while draining
/// stderr on a separate thread, then reap it.
///
/// Both pipes must be drained concurrently: the OS pipe buffers hold only 64 KiB
/// each, so a child that fills stderr blocks before finishing stdout — and we
/// would block on stdout forever (`std::process::Command::output` spawns exactly
/// these two reader threads for the same reason). The byte cap alone does not
/// save us: we only notice overflow after seeing it on OUR side, which never
/// happens while the child is stuck on a full stderr pipe.
fn drain_git_output(
    mut child: std::process::Child,
    cap: u64,
) -> Result<(Vec<u8>, bool), std::io::Error> {
    let stderr_thread = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut stderr = Vec::new();
            let _ = pipe.read_to_end(&mut stderr);
            stderr
        })
    });
    let mut stdout = Vec::new();
    let mut truncated = false;
    if let Some(pipe) = child.stdout.take() {
        let mut limited = pipe.take(cap + 1);
        limited.read_to_end(&mut stdout)?;
        if stdout.len() as u64 > cap {
            stdout.truncate(cap as usize);
            truncated = true;
            let _ = child.kill();
        }
    }
    let status = child.wait()?;
    if truncated {
        // The child was killed; its stderr pipe never hit EOF, so the reader
        // thread may still be blocked. Abandon it (the kill path hands the
        // dead child to the OS reaper — no join).
        return Ok((stdout, true));
    }
    let stderr = match stderr_thread {
        Some(handle) => handle.join().unwrap_or_default(),
        None => Vec::new(),
    };
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(std::io::Error::other(message));
    }
    Ok((stdout, false))
}

fn git_args(spec: &DiffSpec, has_head: bool, flags: &[&str]) -> Vec<String> {
    let mut args = vec![
        "diff".to_string(),
        "--no-color".to_string(),
        "--no-ext-diff".to_string(),
    ];
    args.extend(flags.iter().map(|flag| (*flag).to_string()));
    args.extend(spec.rev_args(has_head));
    args.push("--".to_string());
    args
}

struct RawDiff {
    files: Vec<ChangedFile>,
    patch: Option<(String, bool)>,
}

fn collect_diff(root: &Path, spec: &DiffSpec, want_patch: bool) -> Result<RawDiff, std::io::Error> {
    let has_head = run_git_capped(
        root,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            "HEAD".into(),
        ],
        4096,
    )
    .is_ok();
    let (names, _) = run_git_capped(
        root,
        &git_args(spec, has_head, &["--name-status", "-z", "-M"]),
        MAX_DIFF_BYTES,
    )?;
    let (numstat, _) = run_git_capped(
        root,
        &git_args(spec, has_head, &["--numstat", "-z", "-M"]),
        MAX_DIFF_BYTES,
    )?;
    let stats = parse_numstat(&numstat);
    let mut files = parse_name_status(&names);
    for file in &mut files {
        if let Some((added, deleted)) = stats.get(&file.path) {
            file.additions = *added;
            file.deletions = *deleted;
        }
    }
    let patch = if want_patch {
        let (bytes, truncated) = run_git_capped(
            root,
            &git_args(spec, has_head, &["-U0", "-M"]),
            MAX_DIFF_BYTES,
        )?;
        Some((String::from_utf8_lossy(&bytes).into_owned(), truncated))
    } else {
        None
    };
    Ok(RawDiff { files, patch })
}

#[allow(missing_docs)]
impl GitDiffHandler {
    pub fn name(&self) -> &str {
        "leindex_git_diff"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Git Diff]"
    }

    pub fn description(&self) -> &str {
        "PDG-enriched git diff: working tree, staged, a commit (ref) or a range, with changed symbols and impact."
    }

    pub fn argument_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_path": { "type": "string", "description": "Project directory; omit to use the configured project" },
                "ref": { "type": "string", "description": "Diff one commit against its parent (e.g. HEAD, HEAD~1, a1b2c3d)" },
                "range": { "type": "string", "description": "Revision range, e.g. main..feature or HEAD~3..HEAD" },
                "staged": { "type": "boolean", "default": false, "description": "Diff the index against HEAD instead of the working tree" },
                "stat_only": { "type": "boolean", "default": false, "description": "Per-file statistics only; no patch, no hunk-level symbol mapping" },
                "include_patch": { "type": "boolean", "default": true, "description": "Include the (bounded) unified patch" },
                "max_patch_chars": { "type": "integer", "default": DEFAULT_PATCH_CHARS, "minimum": 0, "maximum": 200000, "description": "Patch size bound" },
                "enrich_pdg": { "type": "boolean", "default": true, "description": "Map changed hunks to PDG symbols and compute impact" },
                "scope": { "type": "string", "description": "Only report files under this project-relative path" },
                "max_latency_ms": { "type": "integer", "default": 250, "minimum": 0, "maximum": 60000, "description": "PDG enrichment budget" },
                "allow_partial": { "type": "boolean", "default": true }
            },
            "required": []
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let raw_project = match args.get("project_path").and_then(Value::as_str) {
            Some(path) => PathBuf::from(path),
            None => registry.default_project_path().await?,
        };
        let live = LiveProject::resolve(&raw_project.to_string_lossy()).map_err(|error| {
            JsonRpcError::invalid_params(format!("Cannot resolve project path: {error}"))
        })?;
        let root = live.root().to_path_buf();

        // Parsing and the two early-exit shapes live in free functions below:
        // this method was the orchestration AND every detail of it, which is
        // how it crossed the CCN 15 gate without any single branch being
        // obviously to blame.
        let parsed = parse_args(&args)?;

        // Outside a repository `git diff` silently degrades to `--no-index`
        // mode and fails with a page of usage text; answer plainly instead.
        let probe_root = root.clone();
        let is_repo =
            tokio::task::spawn_blocking(move || crate::cli::git::is_worktree(&probe_root))
                .await
                .unwrap_or(false);
        if !is_repo {
            return Ok(not_a_repo());
        }

        let git_started = Instant::now();
        let git_root = root.clone();
        let git_spec = parsed.spec.clone();
        let want_patch = parsed.include_patch || (parsed.enrich && !parsed.stat_only);
        let raw =
            tokio::task::spawn_blocking(move || collect_diff(&git_root, &git_spec, want_patch))
                .await
                .map_err(|error| {
                    JsonRpcError::internal_error(format!("git diff task failed: {error}"))
                })?;
        record_git_ms(git_started.elapsed().as_millis().min(u64::MAX as u128) as u64);

        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => return git_error_response(error),
        };

        let files: Vec<ChangedFile> = raw
            .files
            .into_iter()
            .filter(|file| {
                parsed
                    .scope
                    .as_ref()
                    .is_none_or(|s| file.path == *s || file.path.starts_with(&format!("{s}/")))
            })
            .collect();
        let hunks = raw
            .patch
            .as_ref()
            .map(|(p, _)| parse_hunks(p))
            .unwrap_or_default();
        let patch = raw.patch;

        Ok(attach_patch_and_enrich(parsed, patch, files, hunks, &root, registry).await)
    }
}

/// Everything `execute` reads out of the tool arguments before any work.
struct DiffArgs {
    spec: DiffSpec,
    stat_only: bool,
    include_patch: bool,
    enrich: bool,
    max_patch_chars: usize,
    budget: WorkBudget,
    scope: Option<String>,
}

fn parse_args(args: &Value) -> Result<DiffArgs, JsonRpcError> {
    let spec = parse_spec(args)?;
    let stat_only = extract_bool(args, "stat_only", false);
    Ok(DiffArgs {
        spec,
        stat_only,
        include_patch: extract_bool(args, "include_patch", true) && !stat_only,
        enrich: extract_bool(args, "enrich_pdg", true),
        max_patch_chars: extract_usize(args, "max_patch_chars", DEFAULT_PATCH_CHARS)?.min(200_000),
        budget: WorkBudget {
            max_latency_ms: extract_usize(args, "max_latency_ms", 250)?.min(60_000) as u64,
            allow_partial: extract_bool(args, "allow_partial", true),
        },
        scope: args
            .get("scope")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_start_matches("./").trim_end_matches('/').to_string()),
    })
}

/// The answer every not-a-repository path returns, so all of them agree.
fn not_a_repo() -> Value {
    json!({
        "is_git_repo": false,
        "message": "Not a git repository",
        "pdg_status": "not_loaded",
    })
}

/// Map a failed `git diff` invocation to its JSON-RPC response.
///
/// Keep the first lines only: git prints its whole usage page on a bad flag.
/// The one special case is `--no-index` degradation — outside a repository git
/// no longer fails with usage text but with a diagnostic saying so, and that
/// is an input problem, not a tool fault, so it gets the same plain answer as
/// the pre-flight probe instead of an `invalid_params` error.
fn git_error_response(error: std::io::Error) -> Result<Value, JsonRpcError> {
    let message = error
        .to_string()
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ");
    if message.contains("not a git repository") {
        return Ok(not_a_repo());
    }
    Err(JsonRpcError::invalid_params(format!(
        "git diff failed: {message}"
    )))
}

/// Build the success payload: summary and per-file records, the reader-facing
/// patch, and — when the project's PDG is resident — hunk-mapped symbol
/// enrichment. Extracted from `execute` for the same reason as `parse_args`:
/// the method was carrying every branch of payload assembly on top of
/// orchestrating the git subprocess and the registry.
async fn attach_patch_and_enrich(
    args: DiffArgs,
    patch: Option<(String, bool)>,
    files: Vec<ChangedFile>,
    hunks: BTreeMap<String, Vec<(usize, usize)>>,
    root: &Path,
    registry: &Arc<ProjectRegistry>,
) -> Value {
    let DiffArgs {
        spec,
        stat_only,
        include_patch,
        enrich,
        max_patch_chars,
        budget,
        ..
    } = args;
    let mut result = json!({
        "is_git_repo": true,
        "spec": { "kind": spec.kind(), "value": spec_value(&spec) },
        "summary": {
            "files": files.len(),
            "additions": files.iter().filter_map(|f| f.additions).sum::<u64>(),
            "deletions": files.iter().filter_map(|f| f.deletions).sum::<u64>(),
        },
        "files": files.iter().map(file_json).collect::<Vec<_>>(),
        "pdg_status": if enrich && !stat_only { "not_loaded" } else { "skipped" },
        "changed_symbols": [],
        "impact_summary": { "total_affected_symbols": 0, "affected_symbols_capped": false, "affected_files": [], "pdg_enriched": false },
    });
    if include_patch {
        if let Some((raw_patch, truncated)) = &patch {
            // Re-run with context for readers: the -U0 patch drives symbol
            // mapping, but a patch a human reads wants 3 lines of context.
            let (readable, readable_truncated) =
                readable_patch(root, &spec).unwrap_or_else(|| (raw_patch.clone(), false));
            let (text, cut) = bound_chars(&readable, max_patch_chars);
            result["patch"] = Value::String(text);
            result["patch_truncated"] = Value::Bool(cut || *truncated || readable_truncated);
        }
    }
    if !enrich || stat_only || files.is_empty() {
        return result;
    }

    let Some(handle) = registry.try_get_loaded(root).await else {
        return result;
    };
    {
        let mut guard = handle.write().await;
        if guard.pdg().is_none() {
            let _ = guard.ensure_pdg_loaded_graph_only();
        }
    }
    let guard = handle.read().await;
    let Some(pdg) = guard.pdg() else {
        return result;
    };

    let started = Instant::now();
    let pdg_started = Instant::now();
    let enrichment = enrich_files(
        pdg,
        root,
        &files,
        &hunks,
        spec.new_side_is_worktree(),
        budget,
        started,
    );
    record_pdg_ms(pdg_started.elapsed().as_millis().min(u64::MAX as u128) as u64);
    result["pdg_status"] = Value::String(enrichment.status.to_string());
    result["changed_symbols"] = Value::Array(enrichment.changed_symbols);
    result["impact_summary"] = json!({
        "total_affected_symbols": enrichment.affected_symbols,
        "affected_symbols_capped": enrichment.affected_symbols_capped,
        "affected_files": enrichment.affected_files,
        "pdg_enriched": true,
        "symbol_mapping": if spec.new_side_is_worktree() { "hunk" } else { "file" },
    });
    result
}

fn spec_value(spec: &DiffSpec) -> Value {
    match spec {
        DiffSpec::Ref(value) | DiffSpec::Range(value) => Value::String(value.clone()),
        _ => Value::Null,
    }
}

fn parse_spec(args: &Value) -> Result<DiffSpec, JsonRpcError> {
    let rev = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let (reference, range) = (rev("ref"), rev("range"));
    if reference.is_some() && range.is_some() {
        return Err(JsonRpcError::invalid_params(
            "Pass either 'ref' or 'range', not both".to_string(),
        ));
    }
    for (key, value) in [("ref", reference), ("range", range)] {
        if let Some(value) = value {
            // The SAME gate every other git operation uses. Diff is where a
            // range is legal, and `validate_ref` accepts `a..b` / `a...b` while
            // validating each side, so a range cannot smuggle what a single ref
            // could not.
            super::git_ref::validate_ref_or_error(value).map_err(|error| {
                JsonRpcError::invalid_params_with_suggestion(
                    format!("'{key}': {}", error.message),
                    super::git_ref::REF_HINT,
                )
            })?;
        }
    }
    Ok(match (reference, range) {
        (Some(reference), _) => DiffSpec::Ref(reference.to_string()),
        (_, Some(range)) => DiffSpec::Range(range.to_string()),
        _ if extract_bool(args, "staged", false) => DiffSpec::Staged,
        _ => DiffSpec::WorkingTree,
    })
}

fn readable_patch(root: &Path, spec: &DiffSpec) -> Option<(String, bool)> {
    let has_head = true;
    let (bytes, truncated) =
        run_git_capped(root, &git_args(spec, has_head, &["-M"]), MAX_DIFF_BYTES).ok()?;
    Some((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

fn bound_chars(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    (text.chars().take(max).collect(), true)
}

fn file_json(file: &ChangedFile) -> Value {
    let mut value = json!({
        "path": file.path,
        "status": file.state.as_str(),
        "additions": file.additions,
        "deletions": file.deletions,
    });
    if let Some(old) = &file.old_path {
        value["old_path"] = Value::String(old.clone());
    }
    value
}

struct Enrichment {
    status: &'static str,
    changed_symbols: Vec<Value>,
    affected_symbols: usize,
    /// Whether the impact traversal hit its node cap, so `affected_symbols` is
    /// a floor rather than the real count.
    affected_symbols_capped: bool,
    affected_files: Vec<String>,
}

fn enrich_files(
    pdg: &ProgramDependenceGraph,
    root: &Path,
    files: &[ChangedFile],
    hunks: &BTreeMap<String, Vec<(usize, usize)>>,
    map_hunks: bool,
    budget: WorkBudget,
    started: Instant,
) -> Enrichment {
    let mut roots: crate::fast_hash::FastSet<NodeId> = Default::default();
    let mut changed_symbols = Vec::new();
    let mut partial = false;

    for file in files {
        if file.state == FileState::Deleted {
            continue;
        }
        let absolute = root.join(&file.path);
        let key = absolute.to_string_lossy().into_owned();
        let mut nodes = pdg.nodes_in_file(&key);
        if nodes.is_empty() {
            if let Ok(canonical) = absolute.canonicalize() {
                nodes = pdg.nodes_in_file(&canonical.to_string_lossy());
            }
        }
        let ranges = if map_hunks {
            file_byte_ranges(&absolute, hunks.get(&file.path))
        } else {
            None
        };
        let mut symbols = Vec::new();
        for node_id in nodes {
            let Some(node) = pdg.get_node(node_id) else {
                continue;
            };
            let (start, end) = node.byte_range;
            if end <= start {
                continue; // synthetic per-file summary node
            }
            let touched = match &ranges {
                Some(ranges) => ranges
                    .iter()
                    .any(|(from, to)| start < *to.max(&(from + 1)) && end > *from),
                None => true,
            };
            if !touched {
                continue;
            }
            roots.insert(node_id);
            let callers = get_direct_callers(pdg, node_id);
            symbols.push(json!({
                "name": node.name,
                "type": node_type_str(&node.node_type),
                "complexity": node.complexity,
                "caller_count": callers.len(),
                "callers": callers
                    .iter()
                    .take(20)
                    .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
                    .collect::<Vec<_>>(),
            }));
        }
        changed_symbols.push(json!({
            "file": file.path,
            "status": file.state.as_str(),
            "symbols": symbols,
        }));
        if budget.elapsed(started) {
            partial = true;
            break;
        }
    }

    let (affected, affected_symbols_capped) = if partial || roots.is_empty() {
        (Vec::new(), false)
    } else {
        pdg.forward_impact_multi_source_capped(
            &roots,
            &TraversalConfig {
                max_depth: Some(2),
                ..TraversalConfig::for_impact_analysis()
            },
        )
    };
    let mut affected_files: Vec<String> = affected
        .iter()
        .filter_map(|id| pdg.get_node(*id).map(|node| node.file_path.to_string()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    affected_files.sort();
    Enrichment {
        status: if partial { "partial" } else { "fresh" },
        changed_symbols,
        affected_symbols: affected.len(),
        affected_symbols_capped,
        affected_files,
    }
}

/// Byte ranges covered by a file's hunks, read from disk. `None` when the file
/// has no recorded hunks or cannot be read (callers then fall back to
/// file-level attribution).
fn file_byte_ranges(
    path: &Path,
    hunks: Option<&Vec<(usize, usize)>>,
) -> Option<Vec<(usize, usize)>> {
    let hunks = hunks?;
    let content = std::fs::read(path).ok()?;
    let starts = line_starts(&content);
    Some(
        hunks
            .iter()
            .map(|(start, end)| lines_to_bytes(&starts, content.len(), *start, *end))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_name_status_handles_renames_and_spaces() {
        let raw = b"M\0src/a.rs\0A\0dir with space/b.rs\0R087\0old.rs\0new.rs\0D\0gone.rs\0";
        let files = parse_name_status(raw);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].state, FileState::Modified);
        assert_eq!(files[1].path, "dir with space/b.rs");
        assert_eq!(files[2].state, FileState::Renamed);
        assert_eq!(files[2].old_path.as_deref(), Some("old.rs"));
        assert_eq!(files[2].path, "new.rs");
        assert_eq!(files[3].state, FileState::Deleted);
    }

    #[test]
    fn test_parse_numstat_handles_binary_and_renames() {
        let raw = b"3\t1\tsrc/a.rs\0-\t-\timg.png\x005\t0\t\x00old.rs\x00new.rs\x00";
        let stats = parse_numstat(raw);
        assert_eq!(stats["src/a.rs"], (Some(3), Some(1)));
        assert_eq!(stats["img.png"], (None, None));
        assert_eq!(stats["new.rs"], (Some(5), Some(0)));
    }

    #[test]
    fn test_parse_hunks_records_new_side_ranges() {
        let patch = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -3,0 +4,2 @@\n+a\n+b\n@@ -10 +12 @@\n-x\n+y\n@@ -20,2 +21,0 @@\n-p\n-q\ndiff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1,3 +0,0 @@\n";
        let hunks = parse_hunks(patch);
        assert_eq!(hunks["x.rs"], vec![(4, 5), (12, 12), (21, 21)]);
        assert!(!hunks.contains_key("gone.rs"));
    }

    #[test]
    fn test_parse_hunks_body_lines_are_never_headers() {
        // An added line whose CONTENT begins with "++ " renders as
        // "+++ doc comment" in the diff. Without hunk-body tracking that
        // line was parsed as a file header, cleared the current file, and
        // silently dropped every remaining hunk of the file.
        let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -1,0 +2,3 @@\n+first\n+++ doc comment\n+last\n@@ -10 +20 @@\n-old\n+new\n";
        let hunks = parse_hunks(patch);
        assert_eq!(
            hunks["x.rs"],
            vec![(2, 4), (20, 20)],
            "the body line must not be treated as a header and the second hunk must survive"
        );
    }

    #[test]
    fn test_parse_hunks_saturates_crafted_huge_counts() {
        // Counts parsed straight from patch text (custom diff drivers,
        // textconv, hand-edited patches): `start + count - 1` on usize::MAX
        // overflowed — a panic under debug assertions and a whole-file
        // range in release.
        let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -2 +2,18446744073709551615 @@\n+x\n";
        let hunks = parse_hunks(patch);
        assert_eq!(
            hunks["x.rs"],
            vec![(2, usize::MAX)],
            "saturating range instead of a panic"
        );
    }

    #[test]
    fn test_parse_hunks_resyncs_after_crafted_count_swallows_body() {
        // A header declaring 2^32-1 lines on both sides used to leave the
        // body counters pinned for the rest of the patch: every subsequent
        // line — the real `+++ b/` and `@@` headers included — was consumed
        // as body, silently dropping every remaining hunk and file. The
        // header-resync must let later hunks through.
        let patch = concat!(
            "--- a/craft.rs\n",
            "+++ b/craft.rs\n",
            "@@ -1 +1,4294967295 @@\n",
            "+crafted\n",
            "--- a/real.rs\n",
            "+++ b/real.rs\n",
            "@@ -10 +20 @@\n",
            "-old\n",
            "+new\n",
        );
        let hunks = parse_hunks(patch);
        assert_eq!(
            hunks["craft.rs"],
            vec![(1, u32::MAX as usize)],
            "the crafted hunk's own saturating range is kept"
        );
        assert_eq!(
            hunks["real.rs"],
            vec![(20, 20)],
            "the real hunk after the crafted count must survive"
        );
    }

    #[test]
    fn test_parse_hunks_body_line_looking_like_header_still_consumed_when_counters_allow() {
        // The inverse case: a body line that LOOKS like a header (a '+'
        // line whose content starts with "++ ") must be consumed as body
        // while the counters still have room — otherwise a patch that adds
        // lines of patch text would resync mid-body.
        let patch = concat!(
            "--- a/x.rs\n",
            "+++ b/x.rs\n",
            "@@ -1,2 +1,3 @@\n",
            "+@@ -1 +1 @@\n",
            "+added\n",
            " context\n",
        );
        let hunks = parse_hunks(patch);
        assert_eq!(hunks["x.rs"], vec![(1, 3)]);
    }

    #[test]
    fn test_lines_to_bytes_maps_line_ranges() {
        let content = b"aa\nbbb\ncc\n";
        let starts = line_starts(content);
        assert_eq!(lines_to_bytes(&starts, content.len(), 2, 2), (3, 7));
        assert_eq!(lines_to_bytes(&starts, content.len(), 1, 3), (0, 10));
        assert_eq!(lines_to_bytes(&starts, content.len(), 9, 9), (10, 10));
    }

    #[test]
    fn test_revisions_cannot_smuggle_options() {
        // Diff shares the one ref gate, so its ranges accept a slash-and-dash
        // branch name and reject option injection on either side.
        assert!(super::super::git_ref::validate_ref("HEAD~3").is_ok());
        assert!(super::super::git_ref::validate_ref("main..feature/x").is_ok());
        assert!(super::super::git_ref::validate_ref("cw32/30b-admission..HEAD").is_ok());
        assert!(super::super::git_ref::validate_ref("v1.2.3^{commit}").is_ok());
        assert!(super::super::git_ref::validate_ref("--output=/tmp/x").is_err());
        assert!(super::super::git_ref::validate_ref("HEAD; rm -rf /").is_err());
        assert!(super::super::git_ref::validate_ref("").is_err());
        assert!(super::super::git_ref::validate_ref("HEAD..-x").is_err());
        assert!(parse_spec(&json!({"ref": "-p"})).is_err());
        assert!(parse_spec(&json!({"ref": "HEAD", "range": "a..b"})).is_err());
    }

    #[test]
    fn test_spec_selection() {
        assert_eq!(parse_spec(&json!({})).unwrap(), DiffSpec::WorkingTree);
        assert_eq!(
            parse_spec(&json!({"staged": true})).unwrap(),
            DiffSpec::Staged
        );
        assert_eq!(
            parse_spec(&json!({"range": "a..b"})).unwrap(),
            DiffSpec::Range("a..b".into())
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
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

    #[test]
    fn test_collect_diff_against_a_real_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "first"]);
        std::fs::write(root.join("a.rs"), "fn one() {}\nfn two() { let _x = 1; }\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn three() {}\n").unwrap();
        git(root, &["add", "b.rs"]);

        let working = collect_diff(root, &DiffSpec::WorkingTree, true).unwrap();
        let paths: Vec<&str> = working.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.contains(&"a.rs") && paths.contains(&"b.rs"),
            "{paths:?}"
        );
        let a = working.files.iter().find(|f| f.path == "a.rs").unwrap();
        assert_eq!((a.additions, a.deletions), (Some(1), Some(1)));
        let hunks = parse_hunks(&working.patch.unwrap().0);
        assert_eq!(hunks["a.rs"], vec![(2, 2)]);

        let staged = collect_diff(root, &DiffSpec::Staged, false).unwrap();
        assert_eq!(staged.files.len(), 1);
        assert_eq!(staged.files[0].state, FileState::Added);

        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "second"]);
        let by_ref = collect_diff(root, &DiffSpec::Ref("HEAD".into()), false).unwrap();
        assert_eq!(by_ref.files.len(), 2);
        let by_range = collect_diff(root, &DiffSpec::Range("HEAD~1..HEAD".into()), false).unwrap();
        assert_eq!(by_range.files.len(), 2);
    }

    #[tokio::test]
    async fn test_execute_reports_non_repository() {
        let dir = tempfile::tempdir().unwrap();
        let registry = crate::cli::mcp::helpers::test_registry_for(dir.path());
        let value = GitDiffHandler
            .execute(
                &registry,
                json!({"project_path": dir.path().to_string_lossy()}),
            )
            .await
            .unwrap();
        assert_eq!(value["is_git_repo"], false);
    }

    #[test]
    #[cfg(unix)]
    fn test_drain_git_output_survives_large_stderr() {
        // Child writes >64 KiB to stderr (fills the OS pipe buffer) before a
        // small stdout payload. Without the concurrent stderr drain this
        // deadlocks: the child blocks on stderr, we block on stdout forever.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("noisy-git");
        std::fs::write(
            &fake,
            "#!/bin/sh\nawk 'BEGIN{for(i=0;i<70000;i++)printf \"x\"}' >&2\nprintf 'ok'\n",
        )
        .unwrap();
        let mut perm = std::fs::metadata(&fake).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&fake, perm).unwrap();
        let child = Command::new(&fake)
            .current_dir(dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (stdout, truncated) = drain_git_output(child, 1024 * 1024).unwrap();
        assert_eq!(stdout, b"ok");
        assert!(!truncated);
    }

    #[test]
    fn test_readable_patch_reports_bytecap_truncation() {
        // `readable_patch` re-runs git with -U3 context; when that output
        // exceeds the 8 MiB cap it must SAY so, or patch_truncated lies for a
        // cut patch (max_patch_chars up to 200k never cuts a 8 MiB patch).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("big.txt"), "a".repeat(9 * 1024 * 1024)).unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "first"]);
        std::fs::write(root.join("big.txt"), "b".repeat(9 * 1024 * 1024)).unwrap();
        let (_patch, truncated) = readable_patch(root, &DiffSpec::WorkingTree).expect("patch");
        assert!(truncated, "a >8 MiB readable re-run must report truncation");
    }
}
