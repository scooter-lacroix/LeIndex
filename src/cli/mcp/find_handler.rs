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
        let case_sensitive = !found.compiled.case_insensitive;
        let want_retry = !extract_bool(&args, "regex", false) && looks_like_regex(&found.text);
        let literal = found.text.clone();
        let retry_args = args.clone();
        let compiled = found.compiled;
        let (result, regex_note_owned, case_insensitive_hits) =
            tokio::task::spawn_blocking(move || {
                search_with_fallbacks(
                    &specs,
                    &compiled,
                    &options,
                    want_retry,
                    literal,
                    retry_args,
                    case_sensitive,
                )
            })
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

/// One text-search attempt, plus the two advisory follow-ups, run on the
/// blocking pool.
///
/// §3.8: a literal search with regex metacharacters that returns nothing gets
/// ONE retry as a regex — the double read is paid only on the zero path, and
/// the note says what happened either way. §6.7: a case-sensitive zero also
/// gets a case-insensitive recount so the reason can say "0 sensitive,
/// N insensitive".
fn search_with_fallbacks(
    specs: &[RootSpec],
    compiled: &Compiled,
    options: &SearchOptions,
    want_retry: bool,
    literal: String,
    retry_args: Value,
    case_sensitive: bool,
) -> Result<(SearchOutput, Option<String>, usize), JsonRpcError> {
    let mut result = search(specs, compiled, options);
    let mut note = None;
    if want_retry && result.stats.match_lines == 0 {
        let mut query = find_query(&retry_args)?;
        query.regex = true;
        query.pattern = literal;
        if let Ok(regex_compiled) = query.compile() {
            let retried = search(specs, &regex_compiled, options);
            note = retry_note(&retried).map(str::to_string);
            result = retried;
        }
    }
    let mut insensitive = 0usize;
    if result.stats.match_lines == 0 && case_sensitive {
        let mut query = find_query(&retry_args)?;
        query.case = CaseMode::Insensitive;
        if let Ok(ci_compiled) = query.compile() {
            insensitive = search(specs, &ci_compiled, options).stats.match_lines;
        }
    }
    Ok((result, note, insensitive))
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

/// The module's tests live in `find_handler_test.rs` so this file stays under
/// the repository's 2000-line budget (`Large File Detection`). Same seam the
/// sibling handlers use (`#[path = ...] mod test;` in `git_apply.rs`,
/// `git_handler.rs`, `git_apply_exec.rs`, `edit_apply_handler.rs`).
#[cfg(test)]
#[path = "find_handler_test.rs"]
mod tests;
