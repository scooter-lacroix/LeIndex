use super::helpers::{extract_bool, extract_usize};
use super::protocol::JsonRpcError;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::search::textsearch::{
    CaseMode, Compiled, FileFilter, Query, RootSpec, SearchOptions, SearchOutput, search,
    search_symbols,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_LIMIT: usize = 50;
const DEFAULT_PER_FILE: usize = 20;
const DEFAULT_TIMEOUT_MS: usize = 20_000;
/// Characters a token_budget entry may cost before the page is full.
const CHARS_PER_TOKEN: usize = 4;
/// Hard ceiling on a page. `limit` has no schema maximum today, and the
/// engine buffers `offset + limit` hits per file to keep pagination
/// complete — an unbounded `limit: 100_000_000` let one request park
/// hundreds of MB of hits in the shared daemon. Values above the ceiling
/// are clamped, not rejected.
const MAX_LIMIT: usize = 10_000;

/// Handler for LeIndex \[Find\] — index-accelerated, unbounded text and symbol
/// search across the project and any other path on the machine.
#[derive(Clone)]
pub struct FindHandler;

fn strings(args: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        match args.get(*key) {
            Some(Value::String(one)) if !one.is_empty() => return vec![one.clone()],
            Some(Value::Array(many)) => {
                return many
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
            }
            _ => {}
        }
    }
    Vec::new()
}

fn first<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| args.get(*key))
}

fn case_mode(args: &Value) -> CaseMode {
    if let Some(text) = args.get("case").and_then(Value::as_str) {
        return match text.to_ascii_lowercase().as_str() {
            "sensitive" | "exact" | "match" => CaseMode::Sensitive,
            "insensitive" | "ignore" | "i" => CaseMode::Insensitive,
            _ => CaseMode::Smart,
        };
    }
    match args.get("case_sensitive") {
        Some(_) if extract_bool(args, "case_sensitive", false) => CaseMode::Sensitive,
        Some(_) => CaseMode::Insensitive,
        None => CaseMode::Smart,
    }
}

fn expand_home(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(raw), |home| home.join(rest)),
        None => PathBuf::from(raw),
    }
}

/// The project a call belongs to, resolved without hydrating anything.
struct ProjectRef {
    root: PathBuf,
    storage: PathBuf,
    active_storage: PathBuf,
}

fn project_ref(raw: &Path) -> Option<ProjectRef> {
    let live = LiveProject::resolve(&raw.to_string_lossy()).ok()?;
    Some(ProjectRef {
        root: live.root().to_path_buf(),
        storage: live.storage().to_path_buf(),
        active_storage: live.active_storage(),
    })
}

fn project_index(project: &ProjectRef) -> Option<Arc<crate::search::textsearch::TextIndex>> {
    crate::cli::textindex::ensure(&project.root, &project.storage)
}

#[allow(missing_docs)]
impl FindHandler {
    pub fn name(&self) -> &str {
        "leindex_find"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Find]"
    }

    pub fn description(&self) -> &str {
        "Search text or symbol definitions across the project and any other path. Index-accelerated (milliseconds), always reads live files, unbounded via limit/offset paging."
    }

    pub fn argument_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Text to find (regex when regex=true)" },
                "target": { "type": "string", "enum": ["text", "symbols", "auto"], "default": "text", "description": "text: matching lines. symbols: definitions by name. auto: symbols, else text" },
                "regex": { "type": "boolean", "default": false, "description": "Treat pattern as a regular expression" },
                "case": { "type": "string", "enum": ["smart", "sensitive", "insensitive"], "default": "smart", "description": "smart: ignore case unless pattern has uppercase" },
                "word": { "type": "boolean", "default": false, "description": "Whole-word matches only" },
                "paths": { "type": "array", "items": { "type": "string" }, "description": "Extra files/dirs anywhere on disk (no index needed)" },
                "scope": { "type": "string", "description": "Restrict to a project-relative directory or file" },
                "include_globs": { "type": "array", "items": { "type": "string" }, "description": "Only these, e.g. [\"*.rs\"]" },
                "exclude_globs": { "type": "array", "items": { "type": "string" }, "description": "Skip these, e.g. [\"vendor/\"]" },
                "output": { "type": "string", "enum": ["matches", "files", "count", "symbols", "definitions", "callsites", "exists"], "default": "matches", "description": "matches (default), files, count, symbols (enclosing symbols by hits), definitions (where it is defined), callsites (call lines, definition excluded), exists (boolean + first location)" },
                "kind": { "type": "string", "description": "target=symbols: function, class, struct, ..." },
                "token_budget": { "type": "integer", "minimum": 0, "description": "Cap the page at roughly this many tokens (1 token = 4 chars); files fill greedily in rank order and the page ends with +N more — next: <token>" },
                "include_tests": { "type": "boolean", "default": false, "description": "Show test-file hits in full instead of collapsing them to one line after the source hits" },
                "next": { "type": "string", "description": "Continuation token from a previous page's `next`; pass it (plus project_path) alone to continue without re-sending the query" },
                "context_lines": { "type": "integer", "default": 0, "minimum": 0, "maximum": 10, "description": "Context lines per match (adjacent windows are merged, no line repeats)" },
                "limit": { "type": "integer", "default": DEFAULT_LIMIT, "minimum": 0, "maximum": MAX_LIMIT, "description": "Hits per page; 0 = ceiling (10000)" },
                "offset": { "type": "integer", "default": 0, "minimum": 0, "maximum": MAX_LIMIT, "description": "Hits to skip; clamped to 10000; stream ends there" },
                "per_file_cap": { "type": "integer", "default": DEFAULT_PER_FILE, "minimum": 0, "description": "Shown per file; matches past the cap stay reachable on the next page (offset/next) and the file announces +K more; 0 = no cap" },
                "max_line_chars": { "type": "integer", "default": 200, "minimum": 20, "maximum": 2000, "description": "Longest line shown" },
                "timeout_ms": { "type": "integer", "default": DEFAULT_TIMEOUT_MS, "minimum": 0, "description": "Time budget; partial results + has_more. 0 = none" },
                "project_path": { "type": "string", "description": "Project directory; omit to use the current project" }
            },
            "required": ["pattern"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        // §6.8: `next` alone (with project_path) continues a previous call
        // without re-sending the query. The stored arguments win; the
        // caller's overrides (project_path) merge over them; the position
        // comes from the token.
        if let Some(token) = args
            .get("next")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            let resumed =
                crate::cli::mcp::find_paging::resume("leindex_find", token).map_err(|reason| {
                    JsonRpcError::invalid_params_with_suggestion(reason, "Re-send the full query")
                })?;
            let merged = crate::cli::mcp::find_paging::merge(
                &resumed.args,
                &args,
                resumed.offset,
                &resumed.cursors,
            );
            return Box::pin(self.run(registry, merged)).await;
        }
        // `auto`: definitions named like the pattern first; when there are
        // none (or no index to ask), fall back to the text itself.
        if args.get("target").and_then(Value::as_str) == Some("auto") {
            let mut symbols = args.clone();
            symbols["target"] = json!("symbols");
            let found = self.run(registry, symbols).await?;
            if found["total_symbols"].as_u64().unwrap_or(0) > 0 {
                return Ok(found);
            }
            let mut text = args;
            text["target"] = json!("text");
            let mut result = self.run(registry, text).await?;
            result["fallback"] = json!("no symbol matched; searched text");
            return Ok(result);
        }
        self.run(registry, args).await
    }

    async fn run(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        // Regex compilation (regex-automata's meta builder) is stack-hungry,
        // and this handler sits on a deep async poll chain: compiling on the
        // tokio worker's ~2 MiB stack overflowed in debug builds and stays
        // borderline in release. It is pure CPU work — run it on the
        // blocking pool, where the search below already runs. The offload
        // stays unconditional (the meta builder runs for literals too);
        // only the small extracted `Query` is moved in, so the common case
        // pays one handoff and no argument deep-clone.
        let query = find_query(&args)?;
        let found = tokio::task::spawn_blocking(move || compile_query(query))
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("find failed: {e}")))??;
        let output_mode = output_mode_arg(&args)?;
        let window = find_window(&args)?;

        let target_symbols = args.get("target").and_then(Value::as_str) == Some("symbols");

        // ── Roots ────────────────────────────────────────────────────────
        let (specs, primary) = find_roots(registry, &args).await?;

        // ── Symbol definitions ───────────────────────────────────────────
        // `target=symbols` and `output=definitions` share the same
        // machinery; §4.2f names the question differently ("where is X
        // defined") but the answer is identical.
        if target_symbols || output_mode == "definitions" {
            let mut value =
                symbols_result(&args, found, specs, &primary, window.offset, window.limit).await?;
            if output_mode == "definitions" {
                value["output"] = json!("definitions");
            }
            return Ok(value);
        }

        // ── Answer-shaped modes: exists stops at the first hit ──────────
        if output_mode == "exists" {
            return exists_result(found, specs, &primary, window).await;
        }

        // ── Text ─────────────────────────────────────────────────────────
        let windowed = output_mode == "matches" || output_mode == "callsites";
        let (engine_offset, engine_limit) =
            paging_for_window(windowed, window.offset, window.limit);
        // §4.2f: callsites fetch a slack window — definitions are dropped
        // AFTER the fetch (see `filter_callsites_rows`), so a definition that
        // would consume the single fetched slot must not crowd the call out.
        // The response still shows at most `limit` calls.
        let engine_limit = if output_mode == "callsites" {
            engine_limit.map(|limit| limit.saturating_mul(2).min(MAX_LIMIT))
        } else {
            engine_limit
        };
        let options = SearchOptions {
            offset: engine_offset,
            limit: engine_limit,
            per_file_cap: window.per_file,
            skip_in_file: parse_resume_cursors(&args),
            context: window.context,
            max_line_chars: window.max_line_chars,
            deadline: search_deadline(window.timeout_ms),
            collect_hits: windowed,
            want_symbols: extract_bool(&args, "symbols", true),
        };
        // §3.8: a literal search with regex metacharacters that returns
        // nothing gets ONE retry as a regex, inside the same blocking task —
        // the double read is paid only on the zero path, and the note says
        // what happened either way. §6.7: a case-sensitive zero also gets a
        // case-insensitive recount so the reason can say "0 sensitive,
        // N insensitive".
        let case_sensitive = !found.compiled.case_insensitive;
        let compiled = found.compiled;
        let literal = found.text.clone();
        let want_retry = !extract_bool(&args, "regex", false) && looks_like_regex(&literal);
        let retry_args = args.clone();
        let (result, regex_note_owned, case_insensitive_hits) = tokio::task::spawn_blocking(
            move || -> Result<(SearchOutput, Option<String>, usize), JsonRpcError> {
                let mut result = search(&specs, &compiled, &options);
                let mut note = None;
                if want_retry && result.stats.match_lines == 0 {
                    let mut query = find_query(&retry_args)?;
                    query.regex = true;
                    query.pattern = literal;
                    if let Ok(regex_compiled) = query.compile() {
                        let retried = search(&specs, &regex_compiled, &options);
                        note = retry_note(&retried).map(str::to_string);
                        result = retried;
                    }
                }
                let mut insensitive = 0usize;
                if result.stats.match_lines == 0 && case_sensitive {
                    let mut query = find_query(&retry_args)?;
                    query.case = CaseMode::Insensitive;
                    if let Ok(ci_compiled) = query.compile() {
                        insensitive = search(&specs, &ci_compiled, &options).stats.match_lines;
                    }
                }
                Ok((result, note, insensitive))
            },
        )
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("find failed: {e}")))??;
        let regex_note = regex_note_owned.as_deref();

        let mut value = shape_text_result(
            &found.text,
            &output_mode,
            &primary,
            result,
            &window,
            regex_note,
            case_insensitive_hits,
            &args,
        );
        if output_mode == "callsites" {
            filter_callsites(&mut value, &found.text);
        }
        Ok(value)
    }
}

/// The pattern a LeIndex \[Find\] call searches for, with its matcher compiled.
struct FindPattern {
    /// Pattern text, echoed back in the response.
    text: String,
    /// Compiled matcher handed to the search engine.
    compiled: Compiled,
}

/// The paging and windowing knobs of a LeIndex \[Find\] call.
struct FindWindow {
    /// Hits per page; `None` means all of them.
    limit: Option<usize>,
    /// Hits to skip.
    offset: usize,
    /// Context lines shown per match, capped at 10.
    context: usize,
    /// Shown matches per file; 0 disables the cap.
    per_file: usize,
    /// Longest line shown, clamped to 20..=2000.
    max_line_chars: usize,
    /// Scan budget; 0 disables the deadline.
    timeout_ms: usize,
    /// Output budget: fill the page greedily by rank and stop before
    /// blowing it. `None` = unbounded page (the `limit` ceiling still
    /// applies).
    token_budget: Option<usize>,
    /// Test-file hits listed in full after source hits instead of being
    /// collapsed to a summary line.
    include_tests: bool,
}

/// The primary argument name when the caller supplied it, else the legacy
/// spelling kept for compatibility with the pre-\[Find\] text search.
fn preferred_key(args: &Value, primary: &'static str, legacy: &'static str) -> &'static str {
    if args.get(primary).is_some() {
        primary
    } else {
        legacy
    }
}

/// Pull the pattern and its modifiers out of the arguments (cheap, sync).
fn find_query(args: &Value) -> Result<Query, JsonRpcError> {
    let text = first(args, &["pattern", "query"])
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                "Missing required argument: pattern",
                "Add \"pattern\": \"<text or regex>\"; set regex=true for a regular expression",
            )
        })?
        .to_string();
    Ok(Query {
        pattern: text,
        regex: extract_bool(args, "regex", extract_bool(args, "is_regex", false)),
        case: case_mode(args),
        word: extract_bool(args, "word", false),
    })
}

/// Compile the matcher. Only ever called from `spawn_blocking` (see `run`):
/// the meta builder is stack-hungry even for literals.
fn compile_query(query: Query) -> Result<FindPattern, JsonRpcError> {
    let text = query.pattern.clone();
    let compiled = query.compile().map_err(JsonRpcError::invalid_params)?;
    Ok(FindPattern { text, compiled })
}

/// The output mode, validated against the shapes a \[Find\] call renders.
fn output_mode_arg(args: &Value) -> Result<String, JsonRpcError> {
    let mode = args
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or("matches")
        .to_ascii_lowercase();
    if [
        "matches",
        "files",
        "count",
        "symbols",
        "definitions",
        "callsites",
        "exists",
    ]
    .contains(&mode.as_str())
    {
        return Ok(mode);
    }
    Err(JsonRpcError::invalid_params_with_suggestion(
        format!("Unknown output '{mode}'"),
        "Use output: matches | files | count | symbols | definitions | callsites | exists",
    ))
}

/// Paging and windowing arguments for a \[Find\] call.
fn find_window(args: &Value) -> Result<FindWindow, JsonRpcError> {
    let raw_limit = extract_usize(
        args,
        preferred_key(args, "limit", "max_results"),
        DEFAULT_LIMIT,
    )?;
    let offset = extract_usize(args, "offset", 0)?;
    let timeout_ms = extract_usize(args, "timeout_ms", DEFAULT_TIMEOUT_MS)?;
    let context = extract_usize(
        args,
        preferred_key(args, "context_lines", "include_context_lines"),
        0,
    )?
    .min(10);
    let per_file = extract_usize(
        args,
        preferred_key(args, "per_file_cap", "max_per_file"),
        DEFAULT_PER_FILE,
    )?;
    let max_line_chars = extract_usize(args, "max_line_chars", 200)?.clamp(20, 2000);
    let token_budget = extract_usize(args, "token_budget", 0)?;
    Ok(FindWindow {
        // `limit: 0` means "as much as the ceiling allows", NOT unbounded:
        // a None limit reaches the engine as `collect_bound = 0`, which
        // collects every match of every file — exactly the daemon-parking
        // buffering MAX_LIMIT exists to bound. The offset window keeps the
        // rest reachable. `offset` is clamped for the same reason: the
        // engine buffers `offset + limit` hits per file, so an unclamped
        // deep offset re-creates the blow-up (and re-scans from position 0
        // on every page).
        limit: Some(match raw_limit {
            0 => MAX_LIMIT,
            n => n.min(MAX_LIMIT),
        }),
        offset: offset.min(MAX_LIMIT),
        context,
        per_file,
        max_line_chars,
        timeout_ms,
        token_budget: (token_budget > 0).then_some(token_budget),
        include_tests: extract_bool(args, "include_tests", false),
    })
}

/// Resolve the roots to search, off the async runtime.
///
/// Returns the engine specs plus the per-root summaries the response still
/// needs once the specs have moved into the blocking scan.
async fn find_roots(
    registry: &Arc<ProjectRegistry>,
    args: &Value,
) -> Result<(Vec<RootSpec>, Vec<PrimaryRoot>), JsonRpcError> {
    let include = strings(args, &["include_globs", "include"]);
    let exclude = strings(args, &["exclude_globs", "exclude"]);
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .map(str::to_string);
    let paths = strings(args, &["paths", "path"]);

    let project_raw = match args.get("project_path").and_then(Value::as_str) {
        Some(path) => Some(expand_home(path)),
        None => registry.default_project_path().await.ok(),
    };
    let project = project_raw.as_deref().and_then(project_ref);
    if project.is_none() && paths.is_empty() {
        return Err(JsonRpcError::invalid_params_with_suggestion(
            "No project to search",
            "Pass project_path, or paths: [\"/any/directory\"] to search without a project",
        ));
    }

    let project_for_blocking = project
        .as_ref()
        .map(|p| (p.root.clone(), p.storage.clone(), p.active_storage.clone()));
    tokio::task::spawn_blocking(move || {
        let project = project_for_blocking.map(|(root, storage, active_storage)| ProjectRef {
            root,
            storage,
            active_storage,
        });
        build_roots(
            project.as_ref(),
            &paths,
            scope.as_deref(),
            &include,
            &exclude,
        )
    })
    .await
    .map_err(|e| JsonRpcError::internal_error(format!("find setup failed: {e}")))?
}

/// Search the index for symbol definitions instead of text.
async fn symbols_result(
    args: &Value,
    pattern: FindPattern,
    specs: Vec<RootSpec>,
    primary: &[PrimaryRoot],
    offset: usize,
    limit: Option<usize>,
) -> Result<Value, JsonRpcError> {
    let FindPattern { text, compiled } = pattern;
    let kinds = strings(args, &["kind", "type_filter"])
        .into_iter()
        .filter(|k| !k.eq_ignore_ascii_case("all"))
        .collect::<Vec<_>>();
    let lowered = text.to_ascii_lowercase();
    let (hits, total) = tokio::task::spawn_blocking(move || {
        search_symbols(&specs, &compiled, &lowered, &kinds, offset, limit)
    })
    .await
    .map_err(|e| JsonRpcError::internal_error(format!("find failed: {e}")))?;
    let returned = hits.len();
    // The stream ends honestly instead of offering a continuation that
    // cannot advance: a page whose next offset would land past the ceiling
    // (the next request would clamp back into a repeat), and a page with no
    // hits at all (next_offset == offset re-serves this page verbatim).
    let proposed_next = offset + returned;
    let ceiling_cut = offset + returned < total && continuation_past_ceiling(proposed_next);
    let more = returned > 0 && offset + returned < total && !ceiling_cut;
    // Same wire shape as the text paths: the key appears only when true, and
    // a ceiling-cut page carries the same explanation the text paths attach
    // via `result_note` — a compact renderer prints only `note`, so without
    // it a symbols page cut at the window ceiling renders exactly like a
    // complete one.
    let mut notes: Vec<String> = Vec::new();
    if primary.iter().all(|r| !r.indexed) {
        notes.push("Symbol search needs an index: run leindex_manage action=index".to_string());
    }
    if ceiling_cut {
        notes.push(ceiling_sentence());
    }
    let mut value = json!({
        "pattern": text,
        "target": "symbols",
        "total_symbols": total,
        "returned": returned,
        "offset": offset,
        "has_more": more,
        "next_offset": more.then_some(proposed_next),
        "symbols": hits.iter().map(|h| json!({
            "name": h.name,
            "kind": h.kind,
            "file": display_path(primary, h.root, &h.rel),
            "line": h.line,
            "end_line": h.end_line,
            "exact": h.rank == 0,
            "stale": h.stale,
        })).collect::<Vec<_>>(),
        "note": notes.first().map(String::as_str),
        "source_freshness": "live",
    });
    if ceiling_cut {
        value["truncated_by_ceiling"] = json!(true);
    }
    Ok(value)
}

/// Only `matches` pages the hit list in the engine; the summarising modes ask
/// for everything and window their own rows afterwards.
fn paging_for_window(
    windowed: bool,
    offset: usize,
    limit: Option<usize>,
) -> (usize, Option<usize>) {
    if windowed { (offset, limit) } else { (0, None) }
}

/// Whether a continuation past this page would be unusable: `next_offset`
/// exceeds the offset ceiling, so the next request would be silently clamped
/// back and re-serve this page forever. The ceiling therefore ENDS the
/// stream: the page is still served in full, but `has_more` goes false and
/// the response says why (same contract shape as `withheld_by_cap` — a cap
/// must either shape the stream or not exist).
fn continuation_past_ceiling(next_offset: usize) -> bool {
    next_offset > MAX_LIMIT
}

/// The scan deadline, or `None` when the caller set no time budget.
fn search_deadline(timeout_ms: usize) -> Option<Instant> {
    if timeout_ms > 0 {
        Some(Instant::now() + Duration::from_millis(timeout_ms as u64))
    } else {
        None
    }
}

/// Root summary kept after the specs move into the blocking task.
struct PrimaryRoot {
    root: PathBuf,
    indexed: bool,
    /// Outside the project (explicit `paths` entry): report absolute paths.
    external: bool,
    /// The root IS the file (an explicit `paths` entry naming one file):
    /// display the root's own path — the engine reports that file's name
    /// as `rel`, so joining again produced the doubled `g6.log/g6.log`.
    file_root: bool,
}

fn display_path(roots: &[PrimaryRoot], root: usize, rel: &str) -> String {
    // The project alone: short project-relative paths. A named file: its
    // own path, exactly as the caller asked. Anything else — an outside
    // path, or several roots — absolute, so a hit is unambiguous and
    // matches the path the caller asked about.
    match roots.get(root) {
        Some(r) if r.file_root => r.root.to_string_lossy().into_owned(),
        Some(r) if r.external || roots.len() > 1 => r.root.join(rel).to_string_lossy().into_owned(),
        _ => rel.to_string(),
    }
}

fn build_roots(
    project: Option<&ProjectRef>,
    paths: &[String],
    scope: Option<&str>,
    include: &[String],
    exclude: &[String],
) -> Result<(Vec<RootSpec>, Vec<PrimaryRoot>), JsonRpcError> {
    let mut specs: Vec<RootSpec> = Vec::new();
    let mut primary: Vec<PrimaryRoot> = Vec::new();
    let project_root = project.map(|p| p.root.clone());
    let mut push = |root: PathBuf,
                    index: Option<Arc<crate::search::textsearch::TextIndex>>,
                    filter: FileFilter,
                    bypass_ignore: bool,
                    file_root: bool| {
        let external = project_root.as_ref() != Some(&root);
        primary.push(PrimaryRoot {
            root: root.clone(),
            indexed: index.is_some(),
            external,
            file_root,
        });
        specs.push(RootSpec {
            root,
            index,
            filter,
            bypass_ignore,
        });
    };

    if paths.is_empty() {
        let project =
            project.ok_or_else(|| JsonRpcError::invalid_params("No project to search"))?;
        let scope = scope.map(|s| relative_to(&project.root, s));
        let filter = FileFilter::new(include, exclude, scope.as_deref());
        push(
            project.root.clone(),
            project_index(project),
            filter,
            false,
            false,
        );
        return Ok((specs, primary));
    }

    // §3.7/§6.1: explicit `paths=[...]` bypass ignore rules entirely —
    // including files inside git-ignored directories and explicitly named
    // files — because the raw walk never consults .gitignore, and such
    // roots carry no trigram index (an index built from `git ls-files`
    // would silently re-apply the rules being bypassed). Named paths keep
    // the caller's include/exclude globs; NO project `scope` — the caller
    // named exactly where to search.
    for raw in paths {
        let candidate = expand_home(raw);
        let candidate = match (&project, candidate.is_relative()) {
            (Some(project), true) => project.root.join(&candidate),
            _ => candidate,
        };
        let canonical = candidate
            .canonicalize()
            .map_err(|e| JsonRpcError::invalid_params(format!("Cannot search '{raw}': {e}")))?;
        let filter = FileFilter::new(include, exclude, None);
        if canonical.is_file() {
            // The engine deliberately skips the filter for explicit FILE
            // roots (a named file is admitted raw), so the caller's
            // include/exclude globs must be honored HERE — a named file
            // that the globs exclude must not be searched.
            let rel = project_root
                .as_ref()
                .and_then(|root| canonical.strip_prefix(root).ok())
                .map_or_else(
                    || canonical.to_string_lossy().into_owned(),
                    |r| r.to_string_lossy().into_owned(),
                )
                .replace('\\', "/");
            if !filter.allows(&rel) {
                continue;
            }
        }
        push(canonical.clone(), None, filter, true, canonical.is_file());
    }
    Ok((specs, primary))
}

fn relative_to(root: &Path, raw: &str) -> String {
    let path = Path::new(raw);
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// Response-shaping helpers live in `find_handler_shape.rs` (line budget).
#[path = "find_handler_shape.rs"]
mod shape;
use shape::{exists_result, looks_like_regex, parse_resume_cursors, retry_note, shape_text_result};

fn filter_callsites_rows(files: &mut [Value], pattern: &str, limit: Option<usize>) -> usize {
    let name = pattern.trim_end_matches('(');
    let definition_marker = format!("{name}(");
    const DEFINITION_KEYWORDS: [&str; 7] =
        ["fn", "function", "def", "func", "class", "struct", "impl"];
    let mut callsites = 0usize;
    for file in files.iter_mut() {
        let kept = file
            .get_mut("hits")
            .and_then(Value::as_array_mut)
            .map_or(0, |hits| {
                hits.retain(|hit| {
                    let Some(text) = hit.get("text").and_then(Value::as_str) else {
                        return true;
                    };
                    let trimmed = text.trim_start();
                    let Some(marker_pos) = trimmed.find(&definition_marker) else {
                        return true;
                    };
                    !trimmed[..marker_pos]
                        .split_whitespace()
                        .any(|token| DEFINITION_KEYWORDS.contains(&token))
                });
                hits.len()
            });
        file["matches"] = json!(kept);
        callsites += kept;
    }
    if let Some(limit) = limit {
        let mut remaining = limit;
        for file in files.iter_mut() {
            let Some(hits) = file.get_mut("hits").and_then(Value::as_array_mut) else {
                continue;
            };
            if hits.len() > remaining {
                hits.truncate(remaining);
            }
            remaining -= hits.len();
            file["matches"] = json!(hits.len());
            if remaining == 0 {
                break;
            }
        }
    }
    callsites
}

/// §4.2f `output=callsites`: the engine searched `name(` as a literal
/// pattern; drop the rows that are the definition itself (the
/// `fn`/`function`/`def` line naming the pattern) so what remains is the
/// call surface. Find is registry-free by design, so this stays syntactic —
/// the PDG-backed variant belongs to symbol_lookup.
fn filter_callsites(value: &mut Value, pattern: &str) {
    // The caller may search the bare name or `name(`; both mean the same
    // call surface. Strip the paren so the marker is exactly `name(`.
    let name = pattern.trim_end_matches('(');
    let definition_marker = format!("{name}(");
    // A definition line names the function after one of these keywords
    // ("pub fn", "async def", "export function", ...). A bare modifier is
    // not enough, and a call at line start has an empty prefix, which is a
    // call — not a vacuous definition.
    const DEFINITION_KEYWORDS: [&str; 7] =
        ["fn", "function", "def", "func", "class", "struct", "impl"];
    let mut callsites = 0usize;
    if let Some(files) = value.get_mut("files").and_then(Value::as_array_mut) {
        for file in files {
            let kept = file
                .get_mut("hits")
                .and_then(Value::as_array_mut)
                .map_or(0, |hits| {
                    hits.retain(|hit| {
                        let Some(text) = hit.get("text").and_then(Value::as_str) else {
                            return true;
                        };
                        let trimmed = text.trim_start();
                        let Some(marker_pos) = trimmed.find(&definition_marker) else {
                            return true;
                        };
                        !trimmed[..marker_pos]
                            .split_whitespace()
                            .any(|token| DEFINITION_KEYWORDS.contains(&token))
                    });
                    hits.len()
                });
            file["matches"] = json!(kept);
            callsites += kept;
        }
    }
    value["callsites"] = json!(callsites);
    value["total_matches"] = json!(callsites);
}

/// The ceiling sentence shared by the text paths (`result_note`) and the
/// symbols route: the page was served in full, but a continuation past the
/// offset ceiling is unservable — say so instead of advertising a next_offset
/// the next request would clamp into a repeat.
fn ceiling_sentence() -> String {
    format!(
        "Result stream ends at the {}-hit window ceiling (offset + page); \
         narrow the search with scope/include_globs to reach further matches",
        MAX_LIMIT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape a response the way the paging tests always do: no roots, no
    /// regex note, no case recount, empty query args.
    fn shape(
        pattern: &str,
        output: &str,
        result: SearchOutput,
        offset: usize,
        limit: Option<usize>,
    ) -> Value {
        let base = find_window(&serde_json::json!({})).unwrap();
        shape_text_result(
            pattern,
            output,
            &[],
            result,
            &FindWindow {
                offset,
                limit,
                ..base
            },
            None,
            0,
            &serde_json::json!({}),
        )
    }

    #[test]
    fn test_find_window_limit_zero_maps_to_ceiling_not_unbounded() {
        // `limit: 0` must NOT reach the engine as `None` (unbounded
        // collection): the ceiling exists precisely to stop one request
        // from parking every match of every file in the shared daemon.
        let window = find_window(&serde_json::json!({ "limit": 0 })).unwrap();
        assert_eq!(window.limit, Some(MAX_LIMIT));

        // Values above the ceiling clamp; the offset clamps for the same
        // reason (the engine buffers offset + limit hits per file).
        let window =
            find_window(&serde_json::json!({ "limit": 500_000, "offset": 90_000_000 })).unwrap();
        assert_eq!(window.limit, Some(MAX_LIMIT));
        assert_eq!(window.offset, MAX_LIMIT);

        // A limit of None (absent) keeps the default page, not unbounded.
        let window = find_window(&serde_json::json!({})).unwrap();
        assert_eq!(window.limit, Some(DEFAULT_LIMIT));
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn parse_config() {}\npub fn other() {\n    parse_config();\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "Call parse_config first.\n").unwrap();
        dir
    }

    async fn run(dir: &Path, args: Value) -> Value {
        // A bare registry: `test_registry_for` would create `.leindex`, which
        // marks the project as indexed.
        let registry = Arc::new(ProjectRegistry::new(2));
        let mut args = args;
        args["project_path"] = json!(dir.to_string_lossy());
        FindHandler.execute(&registry, args).await.unwrap()
    }

    #[tokio::test]
    async fn test_find_matches_are_grouped_by_file_with_lines() {
        let dir = project();
        let value = run(dir.path(), json!({"pattern": "parse_config"})).await;
        assert_eq!(value["total_files"], 2);
        assert_eq!(value["total_matches"], 3);
        assert_eq!(value["has_more"], false);
        let files: Vec<&str> = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["file"].as_str().unwrap())
            .collect();
        assert_eq!(files, ["README.md", "src/lib.rs"]);
        assert_eq!(value["files"][1]["hits"][1]["line"], 3);
    }

    #[tokio::test]
    async fn test_find_searches_outside_the_workspace_without_an_index() {
        let workspace = project();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(
            elsewhere.path().join("notes.txt"),
            "remember: parse_config lives here\n",
        )
        .unwrap();
        let value = run(
            workspace.path(),
            json!({"pattern": "parse_config", "paths": [elsewhere.path().to_string_lossy()]}),
        )
        .await;
        assert_eq!(value["total_files"], 1, "only the extra path was searched");
        let file = value["files"][0]["file"].as_str().unwrap();
        assert!(
            file.ends_with("notes.txt") && Path::new(file).is_absolute(),
            "{file}"
        );
        assert_eq!(value["roots"][0]["indexed"], false);
        assert!(
            !elsewhere.path().join(".leindex").exists(),
            "searching must have no side effects"
        );
    }

    #[tokio::test]
    async fn test_find_works_with_no_project_at_all() {
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("a.txt"), "needle\n").unwrap();
        let registry = Arc::new(ProjectRegistry::new(2));
        let value = FindHandler
            .execute(
                &registry,
                json!({"pattern": "needle", "paths": [elsewhere.path().to_string_lossy()]}),
            )
            .await
            .unwrap();
        assert_eq!(value["total_matches"], 1);
    }

    #[tokio::test]
    async fn test_find_ignored_dir_found_via_paths_bypasses_ignore_rules() {
        // §3.7: an explicitly named directory is searched RAW. The default
        // inventory consults git ignore rules and would hide the target.
        let dir = project();
        let elsewhere = tempfile::tempdir().unwrap();
        let ignored = elsewhere.path().join("target/debug");
        std::fs::create_dir_all(&ignored).unwrap();
        std::fs::write(ignored.join("artifact.txt"), "the hidden parse_config\n").unwrap();
        std::fs::write(elsewhere.path().join(".gitignore"), "target/\n").unwrap();
        let value = run(
            dir.path(),
            json!({"pattern": "parse_config", "paths": [elsewhere.path().to_string_lossy()]}),
        )
        .await;
        assert_eq!(
            value["total_files"], 1,
            "the git-ignored directory must be searched when named in paths"
        );
        let file = value["files"][0]["file"].as_str().unwrap();
        assert!(file.contains("target/debug/artifact.txt"), "file: {file}");
    }

    #[tokio::test]
    async fn test_find_paths_file_root_is_never_doubled() {
        // §3.7: a paths entry naming one FILE displays as that path exactly —
        // the engine reports the bare file name as rel, and joining root+rel
        // again produced the doubled `g6.log/g6.log`.
        let dir = project();
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("solo.log");
        std::fs::write(&file, "parse_config appears here\n").unwrap();
        let value = run(
            dir.path(),
            json!({"pattern": "parse_config", "paths": [file.to_string_lossy()]}),
        )
        .await;
        assert_eq!(value["total_files"], 1);
        let shown = value["files"][0]["file"].as_str().unwrap();
        assert_eq!(
            shown,
            file.to_string_lossy(),
            "a named file must render as its own path, got {shown}"
        );
        assert!(
            !shown.ends_with("solo.log/solo.log"),
            "the doubled path is the defect"
        );
    }

    #[tokio::test]
    async fn test_find_literal_metachar_pattern_auto_retries_as_regex() {
        // §3.8: `a|b` as a literal matches nothing; the file holds b. The
        // zero-hit literal search gets ONE retry as a regex, the results
        // arrive, and the note says what happened. A lone `.` must NOT
        // trigger the retry.
        let dir = project();
        let value = run(dir.path(), json!({"pattern": "zzz|other"})).await;
        assert_eq!(value["total_matches"], 1, "b-side of the regex");
        assert_eq!(
            value["note"].as_str().unwrap_or_default(),
            "(interpreted as regex)"
        );
        let lone_dot = run(dir.path(), json!({"pattern": "."})).await;
        assert!(
            lone_dot["total_matches"].as_u64().unwrap() > 0,
            "a lone dot matches literally and needs no reinterpretation"
        );
        let both_dead = run(dir.path(), json!({"pattern": "zzz|qqq"})).await;
        assert_eq!(both_dead["total_matches"], 0);
        assert!(
            both_dead["note"]
                .as_str()
                .unwrap_or_default()
                .contains("both matched nothing"),
            "note: {}",
            both_dead["note"]
        );
    }

    #[tokio::test]
    async fn test_find_context_merge_never_prints_a_line_twice() {
        // §4.2b: touching windows must not print a line twice — measured per
        // FILE, because line numbers across different files legitimately
        // collide.
        let dir = project();
        let value = run(
            dir.path(),
            json!({"pattern": "parse_config", "context_lines": 2}),
        )
        .await;
        let seen = collect_printed_lines(&value);
        let unique: std::collections::BTreeSet<_> = seen.iter().cloned().collect();
        assert_eq!(
            seen.len(),
            unique.len(),
            "a line printed twice is the defect; printed {seen:?}"
        );
    }

    /// Every context line the page prints, as (file, line) pairs.
    fn collect_printed_lines(value: &Value) -> Vec<(String, u64)> {
        let mut lines = Vec::new();
        for file in value["files"].as_array().unwrap() {
            let path = file["file"].as_str().unwrap_or("").to_string();
            for hit in file["hits"].as_array().unwrap() {
                let line = hit["line"].as_u64().unwrap();
                let before_len = hit["before"].as_array().map_or(0, Vec::len) as u64;
                for offset in 0..before_len {
                    lines.push((path.clone(), line - before_len + offset));
                }
                lines.push((path.clone(), line));
                let after_len = hit["after"].as_array().map_or(0, Vec::len) as u64;
                for offset in 1..=after_len {
                    lines.push((path.clone(), line + offset));
                }
            }
        }
        lines
    }

    #[tokio::test]
    async fn test_find_tests_rank_after_source_and_collapse() {
        // §4.2c: default — test-file entries come after source entries and
        // collapse to one summary line each; include_tests=true shows them
        // in full, still ranked last.
        let dir = project();
        std::fs::write(
            dir.path().join("src/lib_test.rs"),
            "parse_config in tests\n",
        )
        .unwrap();
        let collapsed = run(dir.path(), json!({"pattern": "parse_config"})).await;
        let files = collapsed["files"].as_array().unwrap();
        let test_positions: Vec<usize> = files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.get("collapsed").is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(test_positions.len(), 1, "one collapsed test entry");
        assert_eq!(
            test_positions[0],
            files.len() - 1,
            "the collapsed entry rides at the end"
        );
        assert!(
            files[test_positions[0]]["collapsed"]
                .as_str()
                .unwrap()
                .starts_with("tests: ")
        );
        let source_entry = files
            .iter()
            .find(|f| f.get("collapsed").is_none())
            .expect("source files stay detailed");
        assert!(source_entry.get("hits").is_some());

        let expanded = run(
            dir.path(),
            json!({"pattern": "parse_config", "include_tests": true}),
        )
        .await;
        let expanded_files = expanded["files"].as_array().unwrap();
        assert!(
            expanded_files.iter().all(|f| f.get("collapsed").is_none()),
            "include_tests=true shows tests in full"
        );
        let last = expanded_files.last().unwrap()["file"].as_str().unwrap();
        assert!(
            last.ends_with("_test.rs"),
            "tests still rank after source, got {last}"
        );
    }

    #[tokio::test]
    async fn test_find_token_budget_fills_greedily_and_emits_next() {
        // §4.2d: a tiny budget keeps one row, ends the page early, and the
        // continuation is the token (with per-file cursors) — never a bare
        // offset, which cannot express the rows the budget cut away.
        let dir = project();
        let first = run(
            dir.path(),
            json!({"pattern": "parse_config", "token_budget": 12}),
        )
        .await;
        assert_eq!(first["has_more"], true);
        assert!(
            first["files"].as_array().unwrap().len() < 2,
            "the budget must cut the page short"
        );
        assert!(first["next"].as_str().is_some());
        assert!(
            first.get("next_offset").is_none(),
            "a budget-cut page must not advertise a bare offset"
        );
        assert!(first["budget_note"].is_null() || first["budget_note"].as_str().is_some());
        let token = first["next"]
            .as_str()
            .expect("budget page issues a continuation token");
        let second = run(
            dir.path(),
            json!({"pattern": "parse_config", "next": token}),
        )
        .await;
        let first_files: Vec<&str> = first["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["file"].as_str().unwrap())
            .collect();
        let second_files: Vec<&str> = second["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["file"].as_str().unwrap())
            .collect();
        for file in &first_files {
            assert!(
                !second_files.contains(file),
                "page 2 re-served {file}: pages overlap"
            );
        }
    }

    #[tokio::test]
    async fn test_find_per_file_cap_overflow_reachable_on_next_page() {
        // §4.2e: the cap bounds DISPLAY, not reach. Past-cap matches must
        // appear on the next offset page (the old text claimed they appear
        // on no page). lib.rs holds 2 matches; cap 1 shows 1 and announces
        // the withheld tail.
        let dir = project();
        let first = run(
            dir.path(),
            json!({"pattern": "parse_config", "per_file_cap": 1}),
        )
        .await;
        let lib = first["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["file"].as_str().unwrap().ends_with("lib.rs"))
            .unwrap();
        // `matches` stays the honest total (2); the DISPLAYED hits are capped.
        assert_eq!(lib["matches"], 2);
        assert_eq!(lib["hits"].as_array().unwrap().len(), 1);
        assert_eq!(lib["cap_line"], "+1 more in this file (next page)");
        assert_eq!(first["withheld_by_cap"], 1);
        let second = run(
            dir.path(),
            json!({"pattern": "parse_config", "per_file_cap": 1, "offset": first["next_offset"]}),
        )
        .await;
        assert!(
            second["total_matches"].as_u64().unwrap() >= 1,
            "past-cap matches must be reachable on the next page"
        );
    }

    #[tokio::test]
    async fn test_find_new_output_modes() {
        // §4.2f: definitions routes through the symbols machinery (with no
        // symbol index it answers honestly and says so); callsites drop the
        // definition line; exists answers and points at the hit.
        let dir = project();
        let defs = run(
            dir.path(),
            json!({"pattern": "parse_config", "output": "definitions"}),
        )
        .await;
        assert_eq!(defs["output"], "definitions");
        assert_eq!(defs["target"], "symbols");
        assert_eq!(
            defs["note"].as_str().unwrap_or_default(),
            "Symbol search needs an index: run leindex_manage action=index"
        );

        let calls = run(
            dir.path(),
            json!({"pattern": "parse_config(", "output": "callsites"}),
        )
        .await;
        assert_eq!(calls["output"], "callsites");
        assert_eq!(
            calls["total_matches"], 1,
            "the one call site; definition excluded"
        );
        let texts: Vec<&str> = calls["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| f["hits"].as_array().unwrap().iter())
            .map(|h| h["text"].as_str().unwrap())
            .collect();
        assert!(
            texts.iter().all(|t| !t.trim_start().starts_with("pub fn ")),
            "the definition line must not ride in callsites: {texts:?}"
        );

        let exists = run(
            dir.path(),
            json!({"pattern": "parse_config", "output": "exists"}),
        )
        .await;
        assert_eq!(exists["exists"], true);
        assert_eq!(exists["first"]["line"], 1);
        let missing = run(
            dir.path(),
            json!({"pattern": "zzz_nothing_zzz", "output": "exists"}),
        )
        .await;
        assert_eq!(missing["exists"], false);
    }

    #[tokio::test]
    async fn test_find_zero_result_reports_reasons() {
        // §6.7: a case-sensitive zero with insensitive hits names the case
        // mismatch; a zero with no candidates hints at ignore rules.
        let dir = project();
        let case = run(
            dir.path(),
            json!({"pattern": "PARSE_CONFIG", "case": "sensitive"}),
        )
        .await;
        assert_eq!(case["total_matches"], 0);
        let reasons: Vec<&str> = case["zero_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        assert!(
            reasons.iter().any(|r| r.contains("case-insensitive")),
            "reasons: {reasons:?}"
        );

        let no_candidates = run(
            dir.path(),
            json!({"pattern": "zzz_nothing_zzz", "exclude_globs": ["*.rs", "*.md"]}),
        )
        .await;
        assert_eq!(no_candidates["total_matches"], 0);
        let reasons: Vec<&str> = no_candidates["zero_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        assert!(
            reasons.iter().any(|r| r.contains("excluded")),
            "reasons: {reasons:?}"
        );
    }

    #[tokio::test]
    async fn test_find_next_token_pages_without_overlap() {
        // §6.8: the token alone (plus project_path) continues the stream;
        // the two pages must not share a file entry.
        let dir = project();
        let first = run(dir.path(), json!({"pattern": "parse_config", "limit": 1})).await;
        let token = first["next"].as_str().expect("page 1 must issue next");
        let registry = Arc::new(ProjectRegistry::new(2));
        let second = FindHandler
            .execute(
                &registry,
                json!({"next": token, "project_path": dir.path().to_string_lossy()}),
            )
            .await
            .unwrap();
        assert_eq!(second["pattern"], "parse_config", "the query is replayed");
        // §6.8 non-overlap is hit-level: a file with more hits than the
        // page limit legitimately appears on both pages (its NEXT hits),
        // but no (file, line) pair may repeat.
        let hit_set = |page: &Value| -> std::collections::BTreeSet<(String, u64)> {
            page["files"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|file| {
                    let path = file["file"].as_str().unwrap().to_string();
                    file["hits"]
                        .as_array()
                        .map(|hits| {
                            hits.iter()
                                .filter_map(|hit| {
                                    hit["line"].as_u64().map(|line| (path.clone(), line))
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        };
        let first_hits = hit_set(&first);
        let second_hits = hit_set(&second);
        let second_files: Vec<&str> = second["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["file"].as_str().unwrap())
            .collect();
        assert!(
            !second_files.is_empty(),
            "token page must serve the stream's remainder"
        );
        for hit in &second_hits {
            assert!(
                !first_hits.contains(hit),
                "token page re-served {hit:?}: pages overlap"
            );
        }
        assert_eq!(second["offset"], first["next_offset"]);
    }

    /// Regression (Codex round 4, C4-1): a continuation page used to drop
    /// the resume cursors of every file it did not show a row for —
    /// including files an earlier page had already delivered in full. Those
    /// files were visited again (the engine keeps their cursor entry so the
    /// next page skips them), but with no row on the page the shaper filtered
    /// their cursor out of the next token, and the page after that served
    /// their already-delivered hits a second time.
    ///
    /// Three files (b.txt holds two hits), `limit: 1` pages the stream one
    /// hit at a time: a:1, b:1 (a.txt fully skipped), b:2, c:1. Walking the
    /// tokens to exhaustion must deliver every hit exactly once.
    #[tokio::test]
    async fn test_find_budget_cut_continuation_keeps_cursors_of_fully_delivered_files() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("a.txt", "parse_config\n"),
            ("b.txt", "parse_config\nparse_config\n"),
            ("c.txt", "parse_config\n"),
        ] {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let page = |args: Value| {
            let dir = dir.path().to_path_buf();
            async move {
                let registry = Arc::new(ProjectRegistry::new(2));
                let mut args = args;
                args["project_path"] = json!(dir.to_string_lossy());
                FindHandler.execute(&registry, args).await.unwrap()
            }
        };
        let hits_of = |value: &Value| -> Vec<(String, u64)> {
            value["files"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|file| {
                    let path = file["file"].as_str().unwrap().to_string();
                    file["hits"]
                        .as_array()
                        .map(|hits| {
                            hits.iter()
                                .filter_map(|hit| {
                                    hit["line"].as_u64().map(|line| (path.clone(), line))
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        };

        let mut delivered: Vec<(String, u64)> = Vec::new();
        let mut first = page(json!({ "pattern": "parse_config", "limit": 1 })).await;
        assert_eq!(
            first["files"].as_array().unwrap().len(),
            1,
            "one hit per page: {first}"
        );
        let mut pages = 1;
        loop {
            delivered.extend(hits_of(&first));
            let Some(next) = first["next"].as_str().map(str::to_string) else {
                break;
            };
            assert!(pages < 10, "the stream never ends: {first}");
            first = page(json!({ "next": next, "limit": 1 })).await;
            pages += 1;
        }
        assert_eq!(
            first["has_more"], false,
            "the last page must close the stream: {first}"
        );
        delivered.sort();
        assert_eq!(
            delivered,
            [
                ("a.txt".to_string(), 1),
                ("b.txt".to_string(), 1),
                ("b.txt".to_string(), 2),
                ("c.txt".to_string(), 1),
            ],
            "every hit exactly once across {pages} pages"
        );
    }

    #[test]
    fn test_find_regex_like_heuristic_lone_dot_exempt() {
        // §3.8 heuristic, unit-level: the metacharacters that CHANGE meaning
        // fire; a LONE dot (e.g. a filename or version fragment) does not.
        assert!(looks_like_regex("a|b"));
        assert!(looks_like_regex("foo.*bar"));
        assert!(looks_like_regex(r"\bword\b"));
        assert!(looks_like_regex("[a-z]+"));
        assert!(looks_like_regex("(a)"));
        assert!(
            looks_like_regex("3.14"),
            "a dot next to other chars is regex-shaped"
        );
        assert!(!looks_like_regex("."), "a lone dot is literal-shaped");
        assert!(!looks_like_regex("word."));
        assert!(!looks_like_regex("plain"));
    }

    #[tokio::test]
    async fn test_find_output_modes() {
        let dir = project();
        let count = run(
            dir.path(),
            json!({"pattern": "parse_config", "output": "count"}),
        )
        .await;
        assert_eq!(count["total_matches"], 3);
        assert!(count.get("files").is_none() && count.get("hits").is_none());
        let files = run(
            dir.path(),
            json!({"pattern": "parse_config", "output": "files"}),
        )
        .await;
        assert_eq!(
            files["files"][1],
            json!({"file": "src/lib.rs", "matches": 2})
        );
    }

    #[test]
    fn test_continuation_past_ceiling_boundaries() {
        assert!(!continuation_past_ceiling(MAX_LIMIT));
        assert!(continuation_past_ceiling(MAX_LIMIT + 1));
    }

    /// A page whose continuation would land past the offset ceiling must END
    /// the stream (has_more=false, no next_offset, ceiling note) instead of
    /// advertising a next_offset the next request would clamp back to this
    /// page's start — the repeat-forever page (Codex P2, round 8).
    #[test]
    fn test_matches_page_at_ceiling_ends_stream_instead_of_repeating() {
        // Engine-shaped window stop (`has_more` pairs with `complete=false`;
        // the deadline never fired): the page starting AT the ceiling is a
        // window stop, not a time-budget stop.
        let more = || SearchOutput {
            returned: 5,
            has_more: true,
            complete: false,
            ..SearchOutput::default()
        };

        // Control: a normal page keeps paging.
        let value = shape("p", "matches", more(), 10, Some(50));
        assert_eq!(value["has_more"], true);
        assert_eq!(value["next_offset"], 15);
        assert!(value.get("truncated_by_ceiling").is_none());

        // The page starting AT the ceiling: returned hits are served in
        // full, but the continuation is refused with an explanation.
        let value = shape("p", "matches", more(), MAX_LIMIT, Some(50));
        assert_eq!(value["has_more"], false);
        assert!(value["next_offset"].is_null());
        assert_eq!(value["truncated_by_ceiling"], true);
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("window ceiling"), "note: {note}");

        // The straddling page (offset + returned crosses the ceiling): same
        // honest end, no clamped overlap.
        let value = shape("p", "matches", more(), MAX_LIMIT - 2, Some(50));
        assert_eq!(value["has_more"], false);
        assert!(value["next_offset"].is_null());
        assert_eq!(value["truncated_by_ceiling"], true);
    }

    /// The round-10 Kilo CRITICAL: a page that ended because the WINDOW
    /// filled at the ceiling (no deadline involved) must report the ceiling
    /// as the cause. The old note keyed the budget sentence on `complete`
    /// — which a window stop also sets false — so the page was mislabeled
    /// "Stopped at the time budget" and the ceiling sentence was dead code
    /// on the matches path (`has_more => !complete` made
    /// `ceiling_ended && result.complete` unreachable).
    #[test]
    fn test_window_stop_at_ceiling_reports_the_ceiling_not_the_budget() {
        let value = shape(
            "p",
            "matches",
            SearchOutput {
                returned: 50,
                has_more: true,
                complete: false,
                ..SearchOutput::default()
            },
            MAX_LIMIT,
            Some(50),
        );
        assert_eq!(value["has_more"], false);
        assert_eq!(value["truncated_by_ceiling"], true);
        let note = value["note"].as_str().unwrap();
        assert!(
            note.contains("window ceiling") && !note.contains("time budget"),
            "a window stop at the ceiling is not a time-budget stop, note: {note}"
        );
    }

    /// A zero-hit page must end the stream whatever its offset: its
    /// next_offset equals its own offset, so a `while has_more` client would
    /// re-serve the identical page forever (the engine forces has_more=true
    /// on a deadline stop, making this reachable at offset == MAX_LIMIT).
    #[test]
    fn test_zero_hit_page_ends_the_stream() {
        let value = shape(
            "p",
            "matches",
            SearchOutput {
                returned: 0,
                has_more: true,
                complete: false,
                stopped_by_deadline: true,
                ..SearchOutput::default()
            },
            MAX_LIMIT,
            Some(50),
        );
        assert_eq!(value["has_more"], false);
        assert!(value["next_offset"].is_null());
        assert!(
            value.get("truncated_by_ceiling").is_none(),
            "zero hits is not a ceiling truncation"
        );
        let note = value["note"].as_str().unwrap_or_default();
        assert!(note.contains("time budget"), "note: {note}");
    }

    /// A deadline-stopped page at the ceiling reports BOTH causes: the
    /// ceiling is the binding constraint for the stream (its continuation is
    /// unservable), and the deadline made the page's totals provisional. The
    /// old note picked one cause from `complete` alone — which a window stop
    /// also sets false — so a pure window stop was mislabeled "time budget"
    /// and a deadline stop at the ceiling lost the ceiling sentence.
    #[test]
    fn test_deadline_stopped_page_at_ceiling_reports_both_causes() {
        let value = shape(
            "p",
            "matches",
            SearchOutput {
                returned: 50,
                has_more: true,
                complete: false,
                stopped_by_deadline: true,
                ..SearchOutput::default()
            },
            MAX_LIMIT,
            Some(50),
        );
        assert_eq!(value["has_more"], false);
        assert_eq!(value["truncated_by_ceiling"], true);
        let note = value["note"].as_str().unwrap();
        assert!(
            note.contains("window ceiling") && note.contains("time budget"),
            "both the ceiling and the budget must be named, note: {note}"
        );
    }

    /// The time-budget sentence applies to every output mode: a truncated
    /// `count` response carries provisional totals and must say so. The
    /// engine end state `has_more=false, complete=false` is exactly the
    /// deadline stop (the closing fallback sets both), so the fixture
    /// carries `stopped_by_deadline`.
    #[test]
    fn test_budget_note_applies_to_summary_modes() {
        let value = shape(
            "p",
            "count",
            SearchOutput {
                has_more: false,
                complete: false,
                stopped_by_deadline: true,
                stats: crate::search::textsearch::SearchStats {
                    match_lines: 7,
                    ..Default::default()
                },
                ..SearchOutput::default()
            },
            0,
            None,
        );
        assert_eq!(value["complete"], false);
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("time budget"), "note: {note}");
    }

    #[tokio::test]
    async fn test_find_pages_with_next_offset() {
        let dir = project();
        let first = run(dir.path(), json!({"pattern": "parse_config", "limit": 2})).await;
        assert_eq!(first["returned"], 2);
        assert_eq!(first["has_more"], true);
        let next = first["next_offset"].as_u64().unwrap();
        let second = run(
            dir.path(),
            json!({"pattern": "parse_config", "limit": 2, "offset": next}),
        )
        .await;
        assert_eq!(second["returned"], 1);
        assert_eq!(second["has_more"], false);
        let all = run(dir.path(), json!({"pattern": "parse_config", "limit": 0})).await;
        assert_eq!(all["returned"], 3);
    }

    #[tokio::test]
    async fn test_find_accepts_the_legacy_text_search_spelling() {
        let dir = project();
        let value = run(
            dir.path(),
            json!({"query": "PARSE_CONFIG", "case_sensitive": false, "max_results": 5, "include_globs": ["*.rs"]}),
        )
        .await;
        assert_eq!(value["total_files"], 1);
        let sensitive = run(
            dir.path(),
            json!({"query": "PARSE_CONFIG", "case_sensitive": true}),
        )
        .await;
        assert_eq!(sensitive["total_matches"], 0);
    }

    #[tokio::test]
    async fn test_find_regex_errors_are_actionable_and_bad_args_rejected() {
        let dir = project();
        let registry = Arc::new(ProjectRegistry::new(2));
        let error = FindHandler
            .execute(&registry, json!({"pattern": "(oops", "regex": true, "project_path": dir.path().to_string_lossy()}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid pattern"), "{error}");
        let error = FindHandler.execute(&registry, json!({})).await.unwrap_err();
        assert!(error.message_with_hint().contains("pattern"), "{error}");
        let error = FindHandler
            .execute(&registry, json!({"pattern": "x", "output": "nope", "project_path": dir.path().to_string_lossy()}))
            .await
            .unwrap_err();
        assert!(
            error.message_with_hint().contains("matches | files"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn test_find_does_not_hydrate_the_project() {
        use crate::cli::mcp::request_meta::{PDG_LOADS, PROJECT_HYDRATIONS};
        use std::sync::atomic::Ordering;
        let dir = project();
        let (pdg, hydrations) = (
            PDG_LOADS.load(Ordering::Relaxed),
            PROJECT_HYDRATIONS.load(Ordering::Relaxed),
        );
        let registry = Arc::new(ProjectRegistry::new(2));
        FindHandler
            .execute(
                &registry,
                json!({"pattern": "parse_config", "project_path": dir.path().to_string_lossy()}),
            )
            .await
            .unwrap();
        assert_eq!(
            registry.len().await,
            0,
            "find must not load the project into the registry"
        );
        assert_eq!(PDG_LOADS.load(Ordering::Relaxed), pdg);
        assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), hydrations);
    }

    #[tokio::test]
    async fn test_find_uses_and_refreshes_the_index_when_the_project_is_indexed() {
        let dir = project();
        std::fs::create_dir_all(dir.path().join(".leindex")).unwrap(); // marks the project as indexed
        let first = run(dir.path(), json!({"pattern": "parse_config"})).await;
        assert_eq!(first["roots"][0]["indexed"], true);
        assert!(dir.path().join(".leindex/textindex/index.bin").is_file());

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(
            dir.path().join("src/new.rs"),
            "fn added_after_indexing() {}\n",
        )
        .unwrap();
        let fresh = run(dir.path(), json!({"pattern": "added_after_indexing"})).await;
        assert_eq!(
            fresh["total_matches"], 1,
            "files added after the index was built are still found"
        );
        assert!(fresh["stats"]["changed_since_index"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_find_auto_target_falls_back_to_text() {
        let dir = project();
        let value = run(
            dir.path(),
            json!({"pattern": "parse_config", "target": "auto"}),
        )
        .await;
        assert_eq!(value["target"], "text");
        assert_eq!(value["total_matches"], 3);
        assert!(value["fallback"].as_str().unwrap().contains("text"));
    }

    #[tokio::test]
    async fn test_find_symbols_target_without_an_index_explains_itself() {
        let dir = project();
        let value = run(dir.path(), json!({"pattern": "parse", "target": "symbols"})).await;
        assert_eq!(value["total_symbols"], 0);
        assert!(value["note"].as_str().unwrap().contains("leindex_manage"));
    }

    /// #085: exclude globs must apply to explicitly NAMED files too — the
    /// engine skips the filter for file roots, so the handler pre-checks.
    #[tokio::test]
    async fn test_find_paths_file_honors_include_exclude_globs() {
        let dir = project();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("keep.rs"), "parse_config here\n").unwrap();
        std::fs::write(elsewhere.path().join("skip.log"), "parse_config here\n").unwrap();
        let value = run(
            dir.path(),
            json!({
                "pattern": "parse_config",
                "paths": [elsewhere.path().to_string_lossy()],
                "exclude_globs": ["*.log"],
            }),
        )
        .await;
        assert_eq!(value["total_files"], 1, "the *.log file must be excluded");
        let shown = value["files"][0]["file"].as_str().unwrap();
        assert!(
            shown.ends_with("keep.rs"),
            "kept file is the .rs one, got {shown}"
        );
    }

    /// #086/#006: `output=exists` must resolve the first hit through
    /// `display_path` (absolute for an external file root) and carry a
    /// content row so the compact renderer prints the answer.
    #[tokio::test]
    async fn test_find_exists_answers_with_resolvable_path_and_content_row() {
        let dir = project();
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("solo.txt");
        std::fs::write(&file, "parse_config appears here\n").unwrap();
        let value = run(
            dir.path(),
            json!({
                "pattern": "parse_config",
                "output": "exists",
                "paths": [file.to_string_lossy()],
            }),
        )
        .await;
        assert_eq!(value["exists"], true);
        let first_file = value["first"]["file"].as_str().unwrap();
        assert_eq!(
            first_file,
            file.to_string_lossy(),
            "exists.first uses display_path"
        );
        let row = value["files"][0]["file"].as_str().unwrap();
        assert_eq!(row, file.to_string_lossy());
        assert!(
            value["files"][0]["hits"][0]["text"]
                .as_str()
                .unwrap()
                .contains("parse_config")
        );
    }

    /// #087: overlapping context windows keep the FIRST lines after the
    /// previous match — the shared lines print once, in order.
    #[tokio::test]
    async fn test_find_context_windows_keep_first_lines_and_print_once() {
        let dir = project();
        std::fs::write(
            dir.path().join("src/context.rs"),
            "parse_config one\nmiddle line\nparse_config two\n",
        )
        .unwrap();
        let value = run(
            dir.path(),
            json!({"pattern": "parse_config", "context_lines": 2}),
        )
        .await;
        let context = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["file"].as_str().unwrap().ends_with("context.rs"))
            .expect("context fixture file");
        let hits = context["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        let first_after: Vec<&str> = hits[0]["after"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect();
        assert_eq!(
            first_after,
            ["middle line"],
            "the near context line is kept"
        );
        let seen = collect_printed_lines(&value);
        let unique: std::collections::BTreeSet<_> = seen.iter().cloned().collect();
        assert_eq!(seen.len(), unique.len(), "no line prints twice: {seen:?}");
    }

    /// #088: a deadline-stopped regex retry is not a clean miss — the note
    /// must say the scan did not finish.
    #[test]
    fn test_retry_note_distinguishes_incomplete_scan_from_clean_miss() {
        let complete_zero = SearchOutput {
            complete: true,
            ..Default::default()
        };
        assert_eq!(
            retry_note(&complete_zero),
            Some("literal and regex readings both matched nothing")
        );
        let stopped = SearchOutput {
            complete: false,
            stopped_by_deadline: true,
            ..Default::default()
        };
        assert!(retry_note(&stopped).unwrap().contains("did not finish"));
    }

    /// #007: definition lines are dropped BEFORE the page limit applies, so
    /// a page of size 1 whose first hit is the definition still shows a
    /// call.
    #[tokio::test]
    async fn test_find_callsites_are_selected_before_the_limit() {
        let dir = project();
        std::fs::write(
            dir.path().join("src/calls.rs"),
            "pub fn parse_config() {}\nfn main() {\n    parse_config();\n}\n",
        )
        .unwrap();
        let value = run(
            dir.path(),
            json!({"pattern": "parse_config", "output": "callsites", "limit": 1}),
        )
        .await;
        let hits: Vec<&str> = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|f| f["hits"].as_array().into_iter().flatten())
            .filter_map(|h| h["text"].as_str())
            .collect();
        assert!(!hits.is_empty(), "a call must appear on page 1");
        assert!(hits.iter().all(|t| !t.trim_start().starts_with("pub fn")));
    }

    /// #089: the ignore regression runs against a REAL Git worktree, so the
    /// default inventory is git-aware before `paths` bypasses it.
    #[tokio::test]
    async fn test_find_ignored_file_omitted_by_inventory_found_via_paths() {
        let dir = project();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join(".gitignore"), "artifact.txt\n").unwrap();
        std::fs::write(
            elsewhere.path().join("artifact.txt"),
            "the hidden parse_config\n",
        )
        .unwrap();
        let init_ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(elsewhere.path())
            .status()
            .is_ok_and(|status| status.success());
        assert!(
            init_ok,
            "git must be available for the worktree inventory test"
        );
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(elsewhere.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(elsewhere.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(elsewhere.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-qm", "init"])
            .current_dir(elsewhere.path())
            .status()
            .unwrap();
        // The default (git-aware) inventory of the worktree omits the ignored
        // artifact — prove that before proving `paths` bypasses it.
        let omitted = run(elsewhere.path(), json!({"pattern": "parse_config"})).await;
        assert_eq!(
            omitted["total_matches"], 0,
            "git-aware inventory must omit the ignored file"
        );
        let found = run(
            dir.path(),
            json!({"pattern": "parse_config", "paths": [elsewhere.path().to_string_lossy()]}),
        )
        .await;
        assert_eq!(found["total_files"], 1);
        assert!(
            found["files"][0]["file"]
                .as_str()
                .unwrap()
                .contains("artifact.txt")
        );
    }
}
