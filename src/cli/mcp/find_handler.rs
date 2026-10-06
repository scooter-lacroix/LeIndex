use super::helpers::{extract_bool, extract_usize};
use super::protocol::JsonRpcError;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::search::textsearch::{
    CaseMode, Compiled, FileFilter, Query, RootSpec, SearchOptions, SearchOutput, search,
    search_symbols,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_LIMIT: usize = 50;
const DEFAULT_PER_FILE: usize = 20;
const DEFAULT_TIMEOUT_MS: usize = 20_000;
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
                "output": { "type": "string", "enum": ["matches", "files", "count", "symbols"], "default": "matches", "description": "matches (default), files, count, or symbols (enclosing symbols by hits)" },
                "kind": { "type": "string", "description": "target=symbols: function, class, struct, ..." },
                "context_lines": { "type": "integer", "default": 0, "minimum": 0, "maximum": 10, "description": "Context lines per match" },
                "limit": { "type": "integer", "default": DEFAULT_LIMIT, "minimum": 0, "maximum": MAX_LIMIT, "description": "Hits per page; 0 = ceiling (10000)" },
                "offset": { "type": "integer", "default": 0, "minimum": 0, "maximum": MAX_LIMIT, "description": "Hits to skip; clamped to 10000; stream ends there" },
                "per_file_cap": { "type": "integer", "default": DEFAULT_PER_FILE, "minimum": 0, "description": "Shown per file; past-cap matches appear on no page; 0 = no cap" },
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
        if target_symbols {
            return symbols_result(&args, found, specs, &primary, window.offset, window.limit)
                .await;
        }

        // ── Text ─────────────────────────────────────────────────────────
        let windowed = output_mode == "matches";
        let (engine_offset, engine_limit) =
            paging_for_window(windowed, window.offset, window.limit);
        let options = SearchOptions {
            offset: engine_offset,
            limit: engine_limit,
            per_file_cap: window.per_file,
            context: window.context,
            max_line_chars: window.max_line_chars,
            deadline: search_deadline(window.timeout_ms),
            collect_hits: windowed,
            want_symbols: extract_bool(&args, "symbols", true),
        };
        let compiled = found.compiled;
        let result: SearchOutput =
            tokio::task::spawn_blocking(move || search(&specs, &compiled, &options))
                .await
                .map_err(|e| JsonRpcError::internal_error(format!("find failed: {e}")))?;

        Ok(shape_text_result(
            &found.text,
            &output_mode,
            &primary,
            result,
            window.offset,
            window.limit,
        ))
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
    if ["matches", "files", "count", "symbols"].contains(&mode.as_str()) {
        return Ok(mode);
    }
    Err(JsonRpcError::invalid_params_with_suggestion(
        format!("Unknown output '{mode}'"),
        "Use output: matches | files | count | symbols",
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
        "note": if primary.iter().all(|r| !r.indexed) {
            Some("Symbol search needs an index: run leindex_manage action=index")
        } else { None },
        "source_freshness": "live",
    });
    // Same wire shape as the text paths: the key appears only when true.
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
}

fn display_path(roots: &[PrimaryRoot], root: usize, rel: &str) -> String {
    // The project alone: short project-relative paths. Anything else — an
    // outside path, or several roots — absolute, so a hit is unambiguous and
    // matches the path the caller asked about.
    match roots.get(root) {
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
                    filter: FileFilter| {
        let external = project_root.as_ref() != Some(&root);
        primary.push(PrimaryRoot {
            root: root.clone(),
            indexed: index.is_some(),
            external,
        });
        specs.push(RootSpec {
            root,
            index,
            filter,
        });
    };

    if paths.is_empty() {
        let project =
            project.ok_or_else(|| JsonRpcError::invalid_params("No project to search"))?;
        let scope = scope.map(|s| relative_to(&project.root, s));
        let filter = FileFilter::new(include, exclude, scope.as_deref());
        push(project.root.clone(), project_index(project), filter);
        return Ok((specs, primary));
    }

    let mut project_scopes: Vec<String> = Vec::new();
    for raw in paths {
        let candidate = expand_home(raw);
        let candidate = match (&project, candidate.is_relative()) {
            (Some(project), true) => project.root.join(&candidate),
            _ => candidate,
        };
        let canonical = candidate
            .canonicalize()
            .map_err(|e| JsonRpcError::invalid_params(format!("Cannot search '{raw}': {e}")))?;
        // Inside the workspace: same index, narrowed to that subpath.
        if let Some(project) = project.filter(|p| canonical.starts_with(&p.root)) {
            let rel = canonical
                .strip_prefix(&project.root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            project_scopes.push(rel);
            continue;
        }
        // Another project that already has an index: use it.
        let other_index = project_ref(&canonical)
            .filter(|_| canonical.is_dir())
            .and_then(|other| crate::cli::textindex::load(&other.storage));
        push(
            canonical,
            other_index,
            FileFilter::new(include, exclude, None),
        );
    }
    if let Some(project) = project.filter(|_| !project_scopes.is_empty()) {
        // One RootSpec per in-workspace path so each keeps its own scope.
        for scope in project_scopes {
            let filter = FileFilter::new(include, exclude, Some(&scope));
            push(project.root.clone(), project_index(project), filter);
        }
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

fn shape_text_result(
    pattern: &str,
    output_mode: &str,
    primary: &[PrimaryRoot],
    result: SearchOutput,
    offset: usize,
    limit: Option<usize>,
) -> Value {
    let stats = &result.stats;
    let (files_json, symbol_totals, file_rows) = collect_file_rows(&result, primary, output_mode);

    let mut value = json!({
        "pattern": pattern,
        "target": "text",
        "output": output_mode,
        "roots": primary.iter().map(|r| json!({
            "path": r.root.to_string_lossy(),
            "indexed": r.indexed,
        })).collect::<Vec<_>>(),
        "total_files": stats.files_matched,
        "total_matches": stats.match_lines,
        "complete": result.complete,
        "source_freshness": "live",
        "stats": {
            "candidates": stats.candidates,
            "scanned": stats.scanned,
            "changed_since_index": stats.dirty,
            "millis": stats.millis as u64,
        },
    });
    // Whether the engine's has_more was suppressed because the continuation
    // cannot advance (matches mode only; the summary modes compute their own
    // in `shape_rows_page`). A zero-hit page ends the stream too — its
    // next_offset would re-serve the identical page — but only the ceiling
    // case is a truncation; the note explains the rest.
    let ceiling_ended = output_mode == "matches"
        && result.has_more
        && continuation_past_ceiling(offset + result.returned);
    let zero_hit_ended =
        output_mode == "matches" && result.has_more && result.returned == 0 && !ceiling_ended;
    match output_mode {
        "matches" => shape_matches_page(
            &mut value,
            &result,
            offset,
            ceiling_ended,
            zero_hit_ended,
            files_json,
        ),
        "files" => shape_rows_page(
            &mut value,
            file_rows,
            offset,
            limit,
            "files",
            |(file, matches)| json!({ "file": file, "matches": matches }),
        ),
        "symbols" => {
            let mut rows: Vec<_> = symbol_totals.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            shape_rows_page(&mut value, rows, offset, limit, "symbols", |entry| {
                let ((name, kind, file), matches) = entry;
                json!({ "name": name, "kind": kind, "file": file, "matches": matches })
            });
        }
        _ => {}
    }
    let value_notes_ceiling = ceiling_ended || value.get("truncated_by_ceiling").is_some();
    // `count` carries no has_more at all — it is not pageable — so a
    // missing key means false, not true.
    let response_has_more = value
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(note) = result_note(&result, value_notes_ceiling, response_has_more) {
        value["note"] = json!(note);
    }
    value
}

/// Gather the per-root file results into the shape each output mode needs:
/// rendered match pages, per-file symbol totals, or plain (file, count) rows.
fn collect_file_rows(
    result: &SearchOutput,
    primary: &[PrimaryRoot],
    output_mode: &str,
) -> (
    Vec<Value>,
    BTreeMap<(String, &'static str, String), usize>,
    Vec<(String, usize)>,
) {
    let mut files_json = Vec::new();
    let mut symbol_totals: BTreeMap<(String, &'static str, String), usize> = BTreeMap::new();
    let mut file_rows: Vec<(String, usize)> = Vec::new();
    for (root_id, root) in result.roots.iter().enumerate() {
        for file in &root.files {
            let shown = display_path(primary, root_id, &file.rel);
            match output_mode {
                "matches" => {
                    files_json.push(json!({
                        "file": shown,
                        "matches": file.match_lines,
                        "symbols_stale": file.symbols_stale,
                        "hits": file.hits.iter().map(|hit| {
                            let mut value = json!({ "line": hit.line, "text": hit.text });
                            if hit.col > 1 { value["col"] = json!(hit.col); }
                            if let Some((name, kind)) = &hit.symbol {
                                value["symbol"] = json!(name);
                                value["kind"] = json!(kind);
                            }
                            if !hit.before.is_empty() { value["before"] = json!(hit.before); }
                            if !hit.after.is_empty() { value["after"] = json!(hit.after); }
                            value
                        }).collect::<Vec<_>>(),
                    }));
                }
                "symbols" => {
                    for (name, kind, count) in &file.symbols {
                        *symbol_totals
                            .entry((name.clone(), kind, shown.clone()))
                            .or_default() += count;
                    }
                }
                _ => file_rows.push((shown, file.match_lines)),
            }
        }
    }
    (files_json, symbol_totals, file_rows)
}

/// The `matches` response page: hit lists plus the paging/truncation signals.
fn shape_matches_page(
    value: &mut Value,
    result: &SearchOutput,
    offset: usize,
    ceiling_ended: bool,
    zero_hit_ended: bool,
    files: Vec<Value>,
) {
    let stream_ended = ceiling_ended || zero_hit_ended;
    value["returned"] = json!(result.returned);
    value["offset"] = json!(offset);
    value["has_more"] = json!(result.has_more && !stream_ended);
    if result.has_more && !stream_ended {
        value["next_offset"] = json!(offset + result.returned);
    }
    if ceiling_ended {
        value["truncated_by_ceiling"] = json!(true);
    }
    if result.cap_withheld > 0 {
        value["withheld_by_cap"] = json!(result.cap_withheld);
    }
    value["files"] = Value::Array(files);
}

/// A summarised (`files`/`symbols`) response page: window the sorted rows and
/// attach the same honest paging/truncation signals the hit pages carry.
fn shape_rows_page<T>(
    value: &mut Value,
    rows: Vec<T>,
    offset: usize,
    limit: Option<usize>,
    key: &str,
    render: impl Fn(&T) -> Value,
) {
    let cap = limit.unwrap_or(usize::MAX);
    let total = rows.len();
    let page: Vec<_> = rows.into_iter().skip(offset).take(cap).collect();
    let proposed_next = offset + page.len();
    let ceiling_ended = proposed_next < total && continuation_past_ceiling(proposed_next);
    value["has_more"] = json!(proposed_next < total && !ceiling_ended);
    if ceiling_ended {
        value["truncated_by_ceiling"] = json!(true);
    }
    value[key] = json!(page.iter().map(render).collect::<Vec<_>>());
}

/// Trailing hint for any text payload: why a complete-looking response may
/// still be incomplete. `None` when no signal applies. `ceiling_ended` is
/// the caller-computed truncation flag (it needs the request offset); the
/// ceiling sentence fires only for a COMPLETE scan — a deadline stop is the
/// time budget's cause to report, not the ceiling's.
fn result_note(
    result: &SearchOutput,
    ceiling_ended: bool,
    response_has_more: bool,
) -> Option<String> {
    let mut note = String::new();
    // Keyed on the RESPONSE's has_more, not the engine's: a page whose
    // continuation was refused reads as final to the client, so a
    // deadline-stopped scan must still say its totals are provisional.
    if !result.complete && !response_has_more {
        note.push_str("Stopped at the time budget; raise timeout_ms or narrow the search");
    }
    if ceiling_ended && result.complete {
        // The page was served in full, but a continuation past the offset
        // ceiling is unservable — say so instead of advertising a next_offset
        // the next request would clamp into a repeat.
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(&format!(
            "Result stream ends at the {}-hit window ceiling (offset + page); \
             narrow the search with scope/include_globs to reach further matches",
            MAX_LIMIT
        ));
    }
    if result.cap_withheld > 0 {
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(&format!(
            "{} buffered matches sit past per_file_cap and appear on no page; \
             raise per_file_cap (0 = no cap) to page through every match",
            result.cap_withheld
        ));
    }
    (!note.is_empty()).then_some(note)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let more = || SearchOutput {
            returned: 5,
            has_more: true,
            complete: true,
            ..SearchOutput::default()
        };

        // Control: a normal page keeps paging.
        let value = shape_text_result("p", "matches", &[], more(), 10, Some(50));
        assert_eq!(value["has_more"], true);
        assert_eq!(value["next_offset"], 15);
        assert!(value.get("truncated_by_ceiling").is_none());

        // The page starting AT the ceiling: returned hits are served in
        // full, but the continuation is refused with an explanation.
        let value = shape_text_result("p", "matches", &[], more(), MAX_LIMIT, Some(50));
        assert_eq!(value["has_more"], false);
        assert!(value["next_offset"].is_null());
        assert_eq!(value["truncated_by_ceiling"], true);
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("window ceiling"), "note: {note}");

        // The straddling page (offset + returned crosses the ceiling): same
        // honest end, no clamped overlap.
        let value = shape_text_result("p", "matches", &[], more(), MAX_LIMIT - 2, Some(50));
        assert_eq!(value["has_more"], false);
        assert!(value["next_offset"].is_null());
        assert_eq!(value["truncated_by_ceiling"], true);
    }

    /// A zero-hit page must end the stream whatever its offset: its
    /// next_offset equals its own offset, so a `while has_more` client would
    /// re-serve the identical page forever (the engine forces has_more=true
    /// on a deadline stop, making this reachable at offset == MAX_LIMIT).
    #[test]
    fn test_zero_hit_page_ends_the_stream() {
        let value = shape_text_result(
            "p",
            "matches",
            &[],
            SearchOutput {
                returned: 0,
                has_more: true,
                complete: false,
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

    /// A deadline-stopped page near the ceiling reports the TIME BUDGET as
    /// the cause, not the ceiling: `has_more` is forced true by the deadline
    /// fallback, so the ceiling sentence must not contradict it.
    #[test]
    fn test_ceiling_note_yields_to_the_time_budget_cause() {
        let value = shape_text_result(
            "p",
            "matches",
            &[],
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
            note.contains("time budget") && !note.contains("window ceiling"),
            "deadline is the binding constraint, note: {note}"
        );
    }

    /// The time-budget sentence applies to every output mode: a truncated
    /// `count` response carries provisional totals and must say so.
    #[test]
    fn test_budget_note_applies_to_summary_modes() {
        let value = shape_text_result(
            "p",
            "count",
            &[],
            SearchOutput {
                has_more: false,
                complete: false,
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
}
