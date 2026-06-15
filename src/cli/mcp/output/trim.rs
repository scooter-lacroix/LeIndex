//! LLM payload trimming — strip verbose fields before the MCP
//! transport hands a tool's `Value` to the model.
//!
//! Each `trim_*` function knows the schema of one tool's output and
//! drops the parts an LLM does not need while keeping the structure
//! the CLI renderers depend on (so MCP and CLI stay in lock-step).
//!
//! `trim_llm_payload(name, data)` is the single entry point used by
//! the MCP transport; it dispatches on the normalized tool name.

use serde_json::Value;

use super::{normalize_tool_name, truncate_chars};

// =============================================================================
// Public entry point
// =============================================================================

/// Strip verbose fields that an LLM does not need while keeping the
/// structure that the CLI renderers depend on. The MCP transport runs
/// the value through this function before returning it to the model.
pub fn trim_llm_payload(name: &str, data: &Value) -> Value {
    let normalized = normalize_tool_name(name);
    let trimmed = match normalized.as_str() {
        "leindex_search" | "search" => trim_search(data),
        "leindex_context" | "context" => trim_context(data),
        "leindex_diagnostics" | "diagnostics" => trim_diagnostics(data),
        "leindex_impact_analysis" | "impact_analysis" => trim_impact(data),
        "leindex_symbol_lookup" | "symbol_lookup" => trim_symbol_lookup(data),
        "leindex_file_summary" | "file_summary" => trim_file_summary(data),
        "leindex_phase_analysis" | "phase_analysis" => trim_phase(data),
        "leindex_git_status" | "git_status" => trim_git_status(data),
        "leindex_read_file" | "read_file" => trim_read_file(data),
        "leindex_read_symbol" | "read_symbol" => trim_read_symbol(data),
        "leindex_grep_symbols" | "grep_symbols" => trim_grep_symbols(data),
        "leindex_text_search" | "text_search" => trim_text_search(data),
        "leindex_deep_analyze" | "deep_analyze" => trim_deep_analyze(data),
        "leindex_write" | "write" => trim_write(data),
        "leindex_index" | "index" => trim_index(data),
        "leindex_edit_preview" | "edit_preview" => trim_edit(data),
        "leindex_edit_apply" | "edit_apply" => trim_edit(data),
        "leindex_rename_symbol" | "rename_symbol" => trim_rename_symbol(data),
        _ => data.clone(),
    };
    // Preserve any `_warning` (or other top-level meta field prefixed
    // with `_`) added by `wrap_with_meta` when the project is stale.
    // Most `trim_*` functions build a fresh object, so the warning
    // would otherwise be silently dropped — leaving the LLM to use
    // stale results without knowing it should reindex.
    merge_meta(data, &trimmed)
}

/// Copy top-level fields whose name starts with `_` from `original`
/// into `trimmed` when they are not already present. This keeps
/// out-of-band metadata (currently `_warning` from stale-index
/// detection) visible to the model after the LLM-payload trim step.
fn merge_meta(original: &Value, trimmed: &Value) -> Value {
    let Some(orig_obj) = original.as_object() else {
        return trimmed.clone();
    };
    let mut out = trimmed.clone();
    let Some(out_obj) = out.as_object_mut() else {
        return trimmed.clone();
    };
    for (k, v) in orig_obj {
        if k.starts_with('_') && !out_obj.contains_key(k) {
            out_obj.insert(k.clone(), v.clone());
        }
    }
    out
}

// =============================================================================
// Per-tool trimmers
// =============================================================================

pub(crate) fn trim_search(data: &Value) -> Value {
    // Borrow the source array (do NOT clone the whole `results`
    // array — each entry's `context` / `content` strings can be
    // tens of kilobytes, and cloning the whole array upfront only
    // to throw most of it away during the per-entry mapping
    // defeats the purpose of payload trimming). The previous
    // `as_array().cloned().or_else(...).cloned()` form was O(N *
    // total_payload_size) extra allocations on every search call.
    let arr: Option<&Vec<Value>> = data
        .as_array()
        .or_else(|| data.get("results").and_then(|v| v.as_array()));
    let trimmed: Vec<Value> = match arr {
        Some(a) => a
            .iter()
            .map(|r| {
                // Use whichever content the handler populated — `context`
                // (full expansion) or `content` (raw node body) — and
                // collapse the multi-dimensional `score` struct down to
                // the single composite that callers actually use.
                let snippet = r
                    .get("context")
                    .filter(|v| !v.is_null())
                    .or_else(|| r.get("content").filter(|v| !v.is_null()))
                    .or_else(|| r.get("signature").filter(|v| !v.is_null()))
                    .cloned()
                    .map(|v| match v {
                        Value::String(s) => {
                            let first = s.lines().next().unwrap_or("").trim();
                            Value::String(truncate_chars(first, 240))
                        }
                        other => other,
                    })
                    .unwrap_or(Value::Null);
                let score = r
                    .get("score")
                    .and_then(|v| v.get("overall"))
                    .filter(|v| !v.is_null())
                    .cloned()
                    .or_else(|| r.get("score").filter(|v| !v.is_null()).cloned())
                    .unwrap_or(Value::Null);
                serde_json::json!({
                    "file_path": r.get("file_path").cloned().unwrap_or(Value::Null),
                    // `render_search` in `render.rs` already supports
                    // either `symbol` or `symbol_name` on the source
                    // record; the trim path previously only looked at
                    // `symbol_name`, so a result that arrived with
                    // `symbol` populated and `symbol_name` absent lost
                    // its name to `Null`. Walk both keys so the LLM-
                    // visible payload always carries the symbol. The
                    // `.filter(|v| !v.is_null())` guards make sure
                    // that an explicit `null` in `symbol_name` falls
                    // through to `symbol` (and vice-versa) rather
                    // than blocking the chain.
                    "symbol": r
                        .get("symbol_name")
                        .filter(|v| !v.is_null())
                        .or_else(|| r.get("symbol").filter(|v| !v.is_null()))
                        .cloned()
                        .unwrap_or(Value::Null),
                    "symbol_type": r.get("symbol_type").cloned().unwrap_or(Value::Null),
                    "line_number": r.get("line_number").cloned().unwrap_or(Value::Null),
                    "score": score,
                    "snippet": snippet,
                })
            })
            .collect(),
        None => Vec::new(),
    };
    // The model already knows the query it sent; we just hand back the
    // count and the per-hit fields it needs to make decisions.
    // Pagination metadata must round-trip too — the model's follow-up
    // `leindex.search` call with `offset: N` depends on seeing
    // `offset` / `has_more` to know how many more pages are
    // available, and `suggestion` (only populated on zero-result
    // queries) is the model's hint for query reformulation.
    let mut out = serde_json::Map::new();
    out.insert("count".to_string(), serde_json::json!(trimmed.len()));
    out.insert("results".to_string(), serde_json::json!(trimmed));
    if let Some(v) = data.get("offset") {
        out.insert("offset".to_string(), v.clone());
    }
    if let Some(v) = data.get("has_more") {
        out.insert("has_more".to_string(), v.clone());
    }
    if let Some(v) = data.get("suggestion") {
        out.insert("suggestion".to_string(), v.clone());
    }
    Value::Object(out)
}

fn trim_context(data: &Value) -> Value {
    // `leindex.context` returns an `AnalysisResult` (see
    // `src/cli/leindex/types.rs::AnalysisResult`) with the shape:
    //   { query, results, context, tokens_used, processing_time_ms }
    // The handler populates these directly from `expand_node_context`;
    // the LLM-visible payload must keep the same fields so the model
    // can see the expanded PDG context (`context`), the timing
    // metadata, and the search-result anchor (`results`). Dropping or
    // substituting fields here would make the tool return almost
    // entirely nulls.
    let mut out = serde_json::Map::new();
    for k in [
        "query",
        "results",
        "context",
        "tokens_used",
        "processing_time_ms",
    ] {
        if let Some(v) = data.get(k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

fn trim_diagnostics(data: &Value) -> Value {
    serde_json::json!({
        "project_path": data.get("project_path"),
        "indexed_files": data.get("indexed_files"),
        "symbol_count": data.get("symbol_count"),
        "index_size_mb": data.get("index_size_mb"),
        "memory_rss_mb": data.get("memory_rss_mb"),
        "db_size_bytes": data.get("db_size_bytes"),
        "stale": data.get("stale"),
        "last_indexed_secs_ago": data.get("last_indexed_secs_ago"),
        "freshness": data.get("freshness"),
        "system_health": data.get("system_health"),
        "issues": data.get("issues"),
    })
}

fn trim_impact(data: &Value) -> Value {
    serde_json::json!({
        "symbol": data.get("symbol"),
        "file": data.get("file"),
        "change_type": data.get("change_type"),
        "direct_callers": data.get("direct_callers"),
        "transitive_affected_symbols": data.get("transitive_affected_symbols"),
        "transitive_affected_files": data.get("transitive_affected_files"),
        "transitive_callers": data.get("transitive_callers"),
        "risk_level": data.get("risk_level"),
        "summary": data.get("summary"),
    })
}

fn trim_symbol_lookup(data: &Value) -> Value {
    // Batch shape: when `leindex.symbol-lookup` is called with
    // `symbols` (plural, array of more than one entry), the
    // handler returns `{batch: true, count, results: [...]}`.
    // Each entry in `results` has the same per-symbol shape
    // as the single-symbol case below. Recurse into each
    // entry so the trimmed payload preserves the requested
    // results — without this branch, the top-level field
    // whitelist below finds nothing and the LLM-visible
    // payload becomes an empty object.
    if data.get("batch").and_then(|v| v.as_bool()).unwrap_or(false) {
        if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
            let mut trimmed_results: Vec<Value> = Vec::with_capacity(arr.len());
            for entry in arr {
                trimmed_results.push(trim_symbol_lookup_single(entry));
            }
            let mut out = serde_json::Map::new();
            out.insert("batch".to_string(), Value::Bool(true));
            out.insert(
                "count".to_string(),
                data.get("count")
                    .cloned()
                    .unwrap_or(Value::from(trimmed_results.len())),
            );
            out.insert("results".to_string(), Value::Array(trimmed_results));
            return Value::Object(out);
        }
    }
    trim_symbol_lookup_single(data)
}

/// Per-symbol trimmer used by both the single-symbol and
/// batch-result paths of `trim_symbol_lookup`.
///
/// `leindex.symbol-lookup` returns the result built by
/// `SymbolLookupHandler::lookup_single_symbol` with fields:
///   symbol, type, file, byte_range, complexity, language,
///   callers, callees, impact_radius, and optionally source
/// (when `include_source` is set). The trim must use the same
/// field names so the LLM-visible payload actually carries the
/// file path, node type, source excerpt, and impact radius —
/// not aliases that resolve to `null` from the raw handler.
fn trim_symbol_lookup_single(data: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in [
        "symbol",
        "type",
        "file",
        "byte_range",
        "complexity",
        "language",
        "callers",
        "callees",
        "impact_radius",
        "source",
    ] {
        if let Some(v) = data.get(k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

fn trim_file_summary(data: &Value) -> Value {
    serde_json::json!({
        "file_path": data.get("file_path"),
        "language": data.get("language"),
        "line_count": data.get("line_count"),
        "symbol_count": data.get("symbol_count"),
        "symbols_shown": data.get("symbols_shown"),
        "symbols_truncated": data.get("symbols_truncated"),
        "symbols": data.get("symbols"),
        "module_role": data.get("module_role"),
    })
}

fn trim_phase(data: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(gen) = data.get("generation") {
        out.insert("generation".to_string(), gen.clone());
    }
    if let Some(ep) = data.get("executed_phases") {
        out.insert("executed_phases".to_string(), ep.clone());
    }
    if let Some(ch) = data.get("cache_hit") {
        out.insert("cache_hit".to_string(), ch.clone());
    }
    if let Some(cf) = data.get("changed_files") {
        out.insert("changed_files".to_string(), cf.clone());
    }
    if let Some(df) = data.get("deleted_files") {
        out.insert("deleted_files".to_string(), df.clone());
    }
    // Pass through fields that render_phase reads for metadata display.
    // mode, phases, and summary are not currently emitted by the upstream
    // PhaseAnalysisReport, but the pass-through is defensive so the
    // renderer will show them if they ever appear.
    if let Some(v) = data.get("mode") {
        out.insert("mode".to_string(), v.clone());
    }
    if let Some(v) = data.get("phases") {
        out.insert("phases".to_string(), v.clone());
    }
    if let Some(v) = data.get("summary") {
        out.insert("summary".to_string(), v.clone());
    }
    // Keep phase summaries 1-5 (the bulk of the analysis output)
    for n in 1u8..=5 {
        let key = format!("phase{}", n);
        if let Some(v) = data.get(&key) {
            out.insert(key, v.clone());
        }
    }
    // Keep single-file symbols and reference material
    if let Some(v) = data.get("file_symbols") {
        out.insert("file_symbols".to_string(), v.clone());
    }
    if let Some(v) = data.get("phase_explanations") {
        out.insert("phase_explanations".to_string(), v.clone());
    }
    // Keep formatted_output but cap it safely at char boundaries.
    // Byte-slicing `&s[..4000]` panics when byte 4000 falls mid-UTF-8.
    if let Some(v) = data.get("formatted_output") {
        if let Some(s) = v.as_str() {
            let capped = match s.char_indices().nth(4000) {
                Some((idx, _)) => &s[..idx],
                None => s,
            };
            out.insert(
                "formatted_output".to_string(),
                Value::String(capped.to_string()),
            );
        } else {
            out.insert("formatted_output".to_string(), v.clone());
        }
    }
    Value::Object(out)
}

fn trim_git_status(data: &Value) -> Value {
    let branch = data.get("branch");
    let summary = data.get("summary");
    let modified_files = data.get("modified_files");
    let staged_files = data.get("staged_files");
    let untracked_files = data.get("untracked_files");
    let changed_symbols = data.get("changed_symbols").and_then(|v| {
        let arr = v.as_array()?;
        Some(serde_json::Value::Array(
            arr.iter()
                .map(|entry| {
                    serde_json::json!({
                        "file": entry.get("file"),
                        "status": entry.get("status"),
                        "symbols": take_n(entry.get("symbols").unwrap_or(&Value::Null), 5),
                    })
                })
                .collect(),
        ))
    });
    serde_json::json!({
        "branch": branch,
        "summary": summary,
        "modified_files": take_n(modified_files.unwrap_or(&Value::Null), 50),
        "staged_files": take_n(staged_files.unwrap_or(&Value::Null), 50),
        "untracked_files": take_n(untracked_files.unwrap_or(&Value::Null), 50),
        "changed_symbols": changed_symbols,
        "pdg_enrichment": data.get("pdg_enrichment").unwrap_or(&Value::Null),
        "impact_summary": data.get("impact_summary").unwrap_or(&Value::Null),
    })
}

fn trim_read_file(data: &Value) -> Value {
    // `content` is the dominant cost. Caller already asked for a slice
    // (start_line / end_line), so we keep it. Drop `symbol_map` unless
    // the handler populated it; the per-entry callers/callees arrays
    // can also be capped.
    let mut out = serde_json::Map::new();
    out.insert(
        "file_path".to_string(),
        data.get("file_path").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "language".to_string(),
        data.get("language").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "total_lines".to_string(),
        data.get("total_lines").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "start_line".to_string(),
        data.get("start_line").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "end_line".to_string(),
        data.get("end_line").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "content".to_string(),
        data.get("content").cloned().unwrap_or(Value::Null),
    );
    if let Some(ctx) = data.get("context") {
        if let Some(obj) = ctx.as_object() {
            let mut trimmed_ctx = serde_json::Map::new();
            if let Some(v) = obj.get("imports_from") {
                trimmed_ctx.insert("imports_from".to_string(), take_n(v, 10));
            }
            if let Some(v) = obj.get("used_by") {
                trimmed_ctx.insert("used_by".to_string(), take_n(v, 10));
            }
            if let Some(v) = obj.get("symbols_on_visible_lines") {
                trimmed_ctx.insert("symbols_on_visible_lines".to_string(), take_n(v, 20));
            }
            if !trimmed_ctx.is_empty() {
                out.insert("context".to_string(), Value::Object(trimmed_ctx));
            }
        }
    }
    if let Some(sm) = data.get("symbol_map") {
        // symbol_map is opt-in already, but each entry may carry
        // callers/callees we want to thin out.
        out.insert("symbol_map".to_string(), thin_symbol_map(sm));
    }
    Value::Object(out)
}

fn thin_symbol_map(sm: &Value) -> Value {
    if let Some(arr) = sm.as_array() {
        let trimmed: Vec<Value> = arr
            .iter()
            .map(|s| {
                let mut s = s.clone();
                if let Some(obj) = s.as_object_mut() {
                    obj.remove("complexity");
                    if let Some(callers) = obj.get("callers") {
                        obj.insert("callers".to_string(), take_n(callers, 5));
                    }
                    if let Some(callees) = obj.get("callees") {
                        obj.insert("callees".to_string(), take_n(callees, 5));
                    }
                }
                s
            })
            .collect();
        Value::Array(trimmed)
    } else {
        Value::Array(Vec::new())
    }
}

fn trim_read_symbol(data: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in [
        "symbol",
        "type",
        "file",
        "language",
        "complexity",
        "line_start",
        "line_end",
        "doc_comment",
        // `dependencies` is only present when the caller
        // explicitly passed `include_dependencies=true`. The
        // handler builds it from `dep_signatures` (see
        // `read_symbol_handler.rs` around line 248) and
        // represents full dependency signatures, not just
        // the names already exposed via `callers` /
        // `callees`. Trimming it out silently would lose
        // the extra context the caller explicitly asked
        // for, so we pass it through (caller can drop the
        // argument if they want a smaller payload).
        "dependencies",
    ] {
        if let Some(v) = data.get(k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    // The source body is the dominant token cost. The handler already
    // caps it at `token_budget * 4` chars. If the handler provided
    // `source_truncated` and `_source_char_budget`, trust those values
    // and only apply a secondary trim if the budget exceeds a safety
    // ceiling (32k chars = ~8k tokens). This ensures the LLM gets the
    // full source it requested (up to its token_budget) rather than
    // an arbitrary 2000-char truncation.
    //
    // Truncate on character boundaries directly from the source `&str`
    // — slicing at a fixed byte offset can panic when the offset lands
    // inside a multi-byte UTF-8 sequence.
    let safety_cap = 32_000usize;
    let budget = data
        .get("_source_char_budget")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(2000);
    let trim_cap = budget.min(safety_cap);

    if let Some(src) = data.get("source").and_then(|v| v.as_str()) {
        let (head, truncated) = match src.char_indices().nth(trim_cap) {
            Some((idx, _)) => (format!("{}...", &src[..idx]), true),
            None => (src.to_string(), false),
        };
        out.insert("source".to_string(), Value::String(head));
        // Use the handler's source_truncated flag when available (it's
        // accurate: it compares full source length against the actual
        // budget). Fall back to the trim-based calculation.
        let handler_truncated = data
            .get("source_truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(truncated);
        out.insert(
            "source_truncated".to_string(),
            Value::Bool(handler_truncated),
        );
    }
    // callers/callees: keep 5 by default, expose `*_more` flag.
    // The length check uses `as_array().map(|a| a.len())` rather
    // than cloning the whole array first — callers/callees graphs
    // can be hundreds of entries, and cloning the array just to
    // discard it after `len() > 5` was an O(N) heap allocation per
    // trim call.
    if let Some(callers) = data.get("callers") {
        let len = callers.as_array().map(|a| a.len()).unwrap_or(0);
        out.insert("callers".to_string(), take_n(callers, 5));
        out.insert("callers_more".to_string(), Value::Bool(len > 5));
    }
    if let Some(callees) = data.get("callees") {
        let len = callees.as_array().map(|a| a.len()).unwrap_or(0);
        out.insert("callees".to_string(), take_n(callees, 5));
        out.insert("callees_more".to_string(), Value::Bool(len > 5));
    }
    Value::Object(out)
}

fn trim_grep_symbols(data: &Value) -> Value {
    // Per-entry: drop byte_range, language; cap callers/callees at 5;
    // keep the count fields so the LLM still sees blast radius.
    let arr = data
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let trimmed: Vec<Value> = arr
        .into_iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for k in ["name", "type", "file", "complexity", "caller_count"] {
                if let Some(v) = r.get(k) {
                    obj.insert(k.to_string(), v.clone());
                }
            }
            obj.insert("callers".to_string(), take_n_for_key(&r, "callers", 5));
            obj.insert("callees".to_string(), take_n_for_key(&r, "callees", 5));
            // `source` is opt-in already; pass through if present.
            if let Some(src) = r.get("source") {
                obj.insert("source".to_string(), src.clone());
            }
            if let Some(score) = r.get("score") {
                obj.insert("score".to_string(), score.clone());
            }
            Value::Object(obj)
        })
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("results".to_string(), Value::Array(trimmed));
    if let Some(v) = data.get("total_matches") {
        out.insert("total_matches".to_string(), v.clone());
    }
    if let Some(v) = data.get("shown") {
        out.insert("shown".to_string(), v.clone());
    }
    if let Some(v) = data.get("offset") {
        out.insert("offset".to_string(), v.clone());
    }
    if let Some(v) = data.get("mode") {
        out.insert("mode".to_string(), v.clone());
    }
    if let Some(v) = data.get("truncated") {
        out.insert("truncated".to_string(), v.clone());
    }
    Value::Object(out)
}

fn trim_text_search(data: &Value) -> Value {
    // Keep before/after context windows when present — the caller
    // explicitly requested context via context_lines and the handler
    // already caps at 10 lines per side. Dropping them silently would
    // make the tool useless for the "understand match context" use case.
    let arr = data
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let trimmed: Vec<Value> = arr
        .into_iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for k in [
                "file",
                "line",
                "content",
                "before",
                "after",
                "in_symbol",
                "symbol_type",
            ] {
                if let Some(v) = r.get(k) {
                    obj.insert(k.to_string(), v.clone());
                }
            }
            Value::Object(obj)
        })
        .collect();
    let mut out = serde_json::Map::new();
    out.insert(
        "count".to_string(),
        data.get("count").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "total_matched".to_string(),
        data.get("total_matched").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "has_more".to_string(),
        data.get("has_more").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "offset".to_string(),
        data.get("offset").cloned().unwrap_or(Value::Null),
    );
    out.insert("results".to_string(), Value::Array(trimmed));
    Value::Object(out)
}

fn trim_deep_analyze(data: &Value) -> Value {
    // Keep the pre-built `context` (already token-budgeted). The
    // results array mirrors a search hit — drop verbose per-result
    // fields and cap to 10.
    //
    // Iterate by reference: the previous form cloned the whole
    // `results` array, then cloned the first 10 entries into a
    // second vector, then cloned each whitelisted field on each
    // entry. The intermediate `Vec<Value>` allocations were
    // unnecessary — the trim path only needs the projected
    // values, and the source array can be borrowed for the
    // lifetime of the call.
    let results_len = data
        .get("results")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let trimmed: Vec<Value> = data
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(10)
                .map(|r| {
                    let mut obj = serde_json::Map::new();
                    for k in [
                        "rank",
                        "file_path",
                        "symbol_name",
                        "symbol_type",
                        "signature",
                    ] {
                        if let Some(v) = r.get(k) {
                            obj.insert(k.to_string(), v.clone());
                        }
                    }
                    Value::Object(obj)
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "query": data.get("query"),
        "tokens_used": data.get("tokens_used"),
        "processing_time_ms": data.get("processing_time_ms"),
        "context": data.get("context"),
        "results": trimmed,
        "results_more": results_len.saturating_sub(10),
    })
}

fn trim_write(data: &Value) -> Value {
    // Drop per-symbol byte_range; the symbol name + type are enough for
    // the LLM to follow up with read_symbol. Borrow the array
    // rather than cloning it upfront so we only allocate the
    // whitelisted `name` / `type` values we actually keep.
    let trimmed: Vec<Value> = data
        .get("symbols")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    let mut obj = serde_json::Map::new();
                    for k in ["name", "type"] {
                        if let Some(v) = s.get(k) {
                            obj.insert(k.to_string(), v.clone());
                        }
                    }
                    Value::Object(obj)
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "success": data.get("success"),
        "file_path": data.get("file_path"),
        "language": data.get("language"),
        "symbols": trimmed,
    })
}

fn trim_index(data: &Value) -> Value {
    // IndexStats is already small. Collapse parse-success into a single
    // failure count and drop the dependency-resolution breakdown unless
    // the LLM is debugging deps.
    let failures = data.get("failed_parses").cloned().unwrap_or(Value::Null);
    serde_json::json!({
        "total_files": data.get("total_files"),
        "files_parsed": data.get("files_parsed"),
        "parse_failures": failures,
        "indexing_time_ms": data.get("indexing_time_ms"),
        "pdg_nodes": data.get("pdg_nodes"),
        "pdg_edges": data.get("pdg_edges"),
        "indexed_nodes": data.get("indexed_nodes"),
        "total_signatures": data.get("total_signatures"),
        "external_deps_unresolved": data.get("external_deps_unresolved"),
    })
}

fn trim_edit(data: &Value) -> Value {
    // The edit handlers return different shapes for `preview` vs
    // `apply`:
    //   * `edit_preview` → `preview_token`, `diff`, `diff_text`,
    //     `affected_*`, `risk_level`, `change_count`, `validation`
    //   * `edit_apply`   → `success`, `changes_applied`, `file_path`,
    //     `edit_region`, `message` (a no-op confirmation), plus the
    //     same diff/affected_* fields
    //
    // We want to keep the union of both shapes so an MCP `edit_apply`
    // call still surfaces the success confirmation and verification
    // context, and an `edit_preview` call still gets the diff. The
    // `diff_text` is kept so the LLM can see the unified diff without
    // parsing the structured `diff` object. The internal `validation`
    // subtree is dropped (the model doesn't need the full validator
    // report).
    let mut out = serde_json::Map::new();
    for k in [
        // shared / preview-shaped
        "preview_token",
        "diff",
        "diff_text",
        "affected_symbols",
        "affected_files",
        "breaking_changes",
        "risk_level",
        "change_count",
        // apply-shaped
        "success",
        "changes_applied",
        "file_path",
        "edit_region",
        "message",
    ] {
        if let Some(v) = data.get(k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

fn trim_rename_symbol(data: &Value) -> Value {
    // `diffs[]` already carries structured per-file diffs. Drop the
    // `diff_text` echoes and shrink the per-file list to the first 25
    // (callers rarely need more for an LLM context). Borrow the
    // array — only the first 25 elements need a deep clone, so the
    // clone-on-take pattern keeps peak memory at O(25) rather than
    // O(N) when N >> 25.
    let diffs = data.get("diffs").and_then(|v| v.as_array());
    let total = diffs.map(|a| a.len()).unwrap_or(0);
    let shown: Vec<Value> = diffs
        .map(|arr| {
            arr.iter()
                .take(25)
                .map(|d| {
                    let mut obj = serde_json::Map::new();
                    if let Some(v) = d.get("file") {
                        obj.insert("file".to_string(), v.clone());
                    }
                    if let Some(v) = d.get("diff") {
                        obj.insert("diff".to_string(), v.clone());
                    }
                    Value::Object(obj)
                })
                .collect()
        })
        .unwrap_or_default();
    // files_affected reflects the TOTAL number of affected files (from
    // the handler), not the trimmed diffs count. This matches the
    // assertion that files_affected equals the original diffs length.
    let files_affected = data
        .get("files_affected")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(total);
    let mut out = serde_json::Map::new();
    out.insert("diffs".to_string(), Value::Array(shown));
    out.insert(
        "diffs_more".to_string(),
        Value::from(total.saturating_sub(25)),
    );
    out.insert(
        "old_name".to_string(),
        data.get("old_name").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "new_name".to_string(),
        data.get("new_name").cloned().unwrap_or(Value::Null),
    );
    out.insert("files_affected".to_string(), Value::from(files_affected));
    out.insert(
        "preview_only".to_string(),
        data.get("preview_only")
            .cloned()
            .unwrap_or(Value::Bool(true)),
    );
    out.insert(
        "applied".to_string(),
        data.get("applied").cloned().unwrap_or(Value::Bool(false)),
    );
    Value::Object(out)
}

// =============================================================================
// Small array-helpers
// =============================================================================

/// Return the first `n` items of a Value (assumed array), as a Value.
/// Borrows the input — only the first `n` elements are cloned, so the
/// peak allocation is O(min(n, source_len)) instead of O(source_len).
fn take_n(v: &Value, n: usize) -> Value {
    match v.as_array() {
        Some(arr) => Value::Array(arr.iter().take(n).cloned().collect()),
        None => Value::Array(Vec::new()),
    }
}

/// Look up a key in an object and return the first `n` items of its
/// array value (or the original value if it's not an array).
fn take_n_for_key(obj: &Value, key: &str, n: usize) -> Value {
    match obj.get(key) {
        Some(v) if v.is_array() => take_n(v, n),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn test_trim_context_keeps_analysis_result() {
        // Regression: `leindex.context` returns an `AnalysisResult`
        // with the exact field set { query, results, context,
        // tokens_used, processing_time_ms }. The trim must keep all
        // five so the LLM can see the expanded PDG context and the
        // timing metadata.
        let input = v(r#"{
            "query": "Context for node main",
            "results": [
                {"rank": 1, "file_path": "src/main.rs", "symbol_name": "main", "language": "rust"}
            ],
            "context": "fn main() { ... }",
            "tokens_used": 120,
            "processing_time_ms": 5
        }"#);
        let t = trim_context(&input);
        assert_eq!(t["query"], "Context for node main");
        assert!(t["results"].is_array());
        assert_eq!(t["context"], "fn main() { ... }");
        assert_eq!(t["tokens_used"], 120);
        assert_eq!(t["processing_time_ms"], 5);
        // The stale-index `_warning` is preserved by `merge_meta`
        // after the trim, not by the trim function itself.
        let merged = trim_llm_payload(
            "leindex.context",
            &v(r#"{
            "query": "q", "results": [], "context": "c",
            "tokens_used": 0, "processing_time_ms": 0,
            "_warning": "stale"
        }"#),
        );
        assert_eq!(merged["_warning"], "stale");
    }

    #[test]
    fn test_trim_symbol_lookup_keeps_actual_fields() {
        // Regression: `leindex.symbol-lookup` returns the shape
        // built by `lookup_single_symbol`: { symbol, type, file,
        // byte_range, complexity, language, callers, callees,
        // impact_radius, source }. Keep the same names — the
        // previous trim used `file_path` / `line` / `symbol_type`
        // / `signature` which do not exist on the raw output and
        // resolved to `null`.
        let input = v(r#"{
            "symbol": "main",
            "type": "function",
            "file": "src/main.rs",
            "byte_range": [0, 100],
            "complexity": 3,
            "language": "rust",
            "callers": [{"name": "test_main"}],
            "callees": [],
            "impact_radius": {"affected_symbols": 5, "affected_files": 2},
            "source": "fn main() { ... }"
        }"#);
        let t = trim_symbol_lookup(&input);
        assert_eq!(t["symbol"], "main");
        assert_eq!(t["type"], "function");
        assert_eq!(t["file"], "src/main.rs");
        assert_eq!(t["byte_range"][0], 0);
        assert_eq!(t["complexity"], 3);
        assert_eq!(t["language"], "rust");
        assert!(t["callers"].is_array());
        assert!(t["callees"].is_array());
        assert_eq!(t["impact_radius"]["affected_symbols"], 5);
        assert_eq!(t["source"], "fn main() { ... }");
        // The pre-fix trim used `file_path` / `signature` — those
        // must NOT appear since the handler does not emit them.
        assert!(t.get("file_path").is_none());
        assert!(t.get("signature").is_none());
    }

    #[test]
    fn test_trim_search_drops_verbose_fields() {
        let input = v(r#"{
            "results": [
                {
                    "rank": 1,
                    "node_id": "abc123",
                    "file_path": "/p/src/foo.rs",
                    "symbol_name": "main",
                    "symbol_type": "function",
                    "language": "rust",
                    "byte_range": [0, 100],
                    "line_number": 42,
                    "complexity": 3,
                    "caller_count": 5,
                    "dependency_count": 2,
                    "context": "// first line",
                    "score": {"overall": 0.85, "neural": 0.9, "text": 0.7, "structural": 0.8}
                }
            ],
            "offset": 0,
            "count": 1,
            "has_more": false,
            "suggestion": "nope"
        }"#);
        let t = trim_search(&input);
        let r = &t["results"][0];
        assert_eq!(r["file_path"], "/p/src/foo.rs");
        assert_eq!(r["symbol"], "main");
        assert_eq!(r["symbol_type"], "function");
        assert_eq!(r["line_number"], 42);
        assert_eq!(r["score"], 0.85);
        assert_eq!(r["snippet"], "// first line");
        // Verbose fields the LLM doesn't need
        assert!(r.get("node_id").is_none() || r["node_id"].is_null());
        assert!(r.get("byte_range").is_none() || r["byte_range"].is_null());
        assert!(r.get("complexity").is_none() || r["complexity"].is_null());
        assert!(r.get("caller_count").is_none() || r["caller_count"].is_null());
        assert!(r.get("language").is_none() || r["language"].is_null());
    }

    #[test]
    fn test_trim_search_falls_back_to_symbol_key() {
        // Regression: the trim path previously only looked at
        // `symbol_name`. If a search result only had `symbol`
        // populated, the trim dropped it to `Null` and
        // `render_search` (which reads `symbol` first) got no
        // name. The trim now walks both keys.
        let input = v(r#"{
            "results": [
                {
                    "rank": 1,
                    "file_path": "/p/src/foo.rs",
                    "symbol": "main",
                    "symbol_type": "function",
                    "context": "fn main() { return 0; }"
                }
            ]
        }"#);
        let t = trim_search(&input);
        let r = &t["results"][0];
        assert_eq!(r["symbol"], "main");
        assert_eq!(r["file_path"], "/p/src/foo.rs");
    }

    #[test]
    fn test_trim_search_prefers_symbol_name_when_both_present() {
        // When both `symbol` and `symbol_name` are populated,
        // `symbol_name` wins because it is the canonical
        // `SearchResult` field and we want trim output to be
        // stable regardless of which key the caller used.
        let input = v(r#"{
            "results": [
                {
                    "file_path": "/p/src/foo.rs",
                    "symbol": "alias",
                    "symbol_name": "canonical",
                    "symbol_type": "function",
                    "context": "x"
                }
            ]
        }"#);
        let t = trim_search(&input);
        let r = &t["results"][0];
        assert_eq!(r["symbol"], "canonical");
    }

    #[test]
    fn test_trim_edit_keeps_structured_diff() {
        let input = v(r#"{
            "preview_token": "tok",
            "diff": {"file_path": "src/foo.rs", "additions": 1, "deletions": 1, "hunks": []},
            "diff_text": "--- a\n+++ b\n@@ ...",
            "affected_symbols": ["main"],
            "affected_files": ["src/foo.rs"],
            "breaking_changes": [],
            "risk_level": "low",
            "change_count": 1,
            "validation": {"x": 1}
        }"#);
        let t = trim_edit(&input);
        assert_eq!(t["preview_token"], "tok");
        assert!(t["diff"].is_object());
        assert_eq!(t["risk_level"], "low");
        assert_eq!(t["change_count"], 1);
        assert_eq!(t["affected_symbols"][0], "main");
        // diff_text is now preserved so the LLM can see the unified diff
        assert!(t.get("diff_text").is_some());
        // validation subtree is dropped (internal detail)
        assert!(t.get("validation").is_none());
    }

    #[test]
    fn test_trim_edit_keeps_apply_result_fields() {
        // Regression: `edit_apply` returns `success`, `changes_applied`,
        // `file_path`, `edit_region`, and a no-op `message`. The trim
        // must preserve these so the LLM gets confirmation context
        // for a successful or no-op apply.
        let input = v(r#"{
            "success": true,
            "changes_applied": 3,
            "file_path": "src/foo.rs",
            "edit_region": {"start": 10, "end": 25},
            "message": "Applied 3 changes",
            "diff": {"file_path": "src/foo.rs", "additions": 3, "deletions": 0, "hunks": []}
        }"#);
        let t = trim_edit(&input);
        assert_eq!(t["success"], true);
        assert_eq!(t["changes_applied"], 3);
        assert_eq!(t["file_path"], "src/foo.rs");
        assert!(t["edit_region"].is_object());
        assert_eq!(t["message"], "Applied 3 changes");
    }

    #[test]
    fn test_trim_read_symbol_caps_callers() {
        let callers: Vec<Value> = (0..20)
            .map(|i| serde_json::json!({"name": format!("c{}", i), "file": "a.rs", "line": i}))
            .collect();
        let input = v(&format!(
            r#"{{
            "symbol": "main",
            "type": "function",
            "file": "src/main.rs",
            "language": "rust",
            "complexity": 5,
            "line_start": 1,
            "line_end": 10,
            "doc_comment": "/// entry",
            "source": "{}",
            "callers": {},
            "callees": []
        }}"#,
            "x".repeat(3000),
            serde_json::to_string(&callers).unwrap()
        ));
        let t = trim_read_symbol(&input);
        // Source is truncated to 2000 chars + 3-char "..." ellipsis.
        // The previous byte-slice version could panic on multi-byte
        // UTF-8 sequences; the new char-boundary truncation cannot.
        let src = t["source"].as_str().unwrap();
        assert!(src.chars().count() <= 2003, "got {}", src.chars().count());
        assert!(
            src.ends_with("..."),
            "missing ellipsis: {:?}",
            &src[src.len() - 10..]
        );
        assert_eq!(t["source_truncated"], true);
        // Callers capped at 5
        assert_eq!(t["callers"].as_array().unwrap().len(), 5);
        assert_eq!(t["callers_more"], true);
    }

    #[test]
    fn test_trim_grep_symbols_drops_byte_range() {
        let input = v(r#"{
            "results": [
                {
                    "name": "main",
                    "type": "function",
                    "file": "src/main.rs",
                    "byte_range": [0, 200],
                    "complexity": 3,
                    "language": "rust",
                    "caller_count": 10,
                    "callers": [{"name": "a"}, {"name": "b"}, {"name": "c"}, {"name": "d"}, {"name": "e"}, {"name": "f"}, {"name": "g"}],
                    "callees": [{"name": "x"}]
                }
            ],
            "total_matches": 1,
            "shown": 1,
            "offset": 0,
            "mode": "code"
        }"#);
        let t = trim_grep_symbols(&input);
        let r = &t["results"][0];
        assert!(r.get("byte_range").is_none());
        assert!(r.get("language").is_none());
        assert_eq!(r["callers"].as_array().unwrap().len(), 5);
        assert_eq!(r["callee_count"], Value::Null); // not present in input
                                                    // caller_count (kept) reflects blast radius even when callers list is capped
        assert_eq!(r["caller_count"], 10);
    }

    #[test]
    fn test_trim_text_search_preserves_context_windows() {
        let input = v(r#"{
            "count": 1,
            "total_matched": 1,
            "has_more": false,
            "offset": 0,
            "results": [
                {
                    "file": "src/foo.rs",
                    "line": 42,
                    "content": "let x = 1;",
                    "before": ["fn main() {", "  let y = 2;"],
                    "after": ["  let z = 3;", "}"],
                    "in_symbol": "main",
                    "symbol_type": "function"
                }
            ]
        }"#);
        let t = trim_text_search(&input);
        let r = &t["results"][0];
        // before/after context windows are now preserved so the LLM
        // can understand match context without a follow-up read_file.
        assert!(r.get("before").is_some());
        assert!(r.get("after").is_some());
        assert_eq!(r["file"], "src/foo.rs");
        assert_eq!(r["line"], 42);
    }

    #[test]
    fn test_trim_deep_analyze_caps_results() {
        let results: Vec<Value> = (0..15)
            .map(|i| {
                serde_json::json!({
                    "rank": i + 1,
                    "node_id": format!("n{}", i),
                    "file_path": format!("/p/src/file{}.rs", i),
                    "symbol_name": format!("sym{}", i),
                    "symbol_type": "function",
                    "signature": "fn x()",
                    "complexity": i as u32,
                    "context": "ctx",
                    "score": {"overall": 0.5}
                })
            })
            .collect();
        let input = v(&format!(
            r#"{{
            "query": "what is X",
            "tokens_used": 1500,
            "processing_time_ms": 250,
            "context": "expanded prose here",
            "results": {}
        }}"#,
            serde_json::to_string(&results).unwrap()
        ));
        let t = trim_deep_analyze(&input);
        // Capped at 10
        assert_eq!(t["results"].as_array().unwrap().len(), 10);
        assert_eq!(t["results_more"], 5);
        // Per-result verbose fields dropped
        let r0 = &t["results"][0];
        assert!(r0.get("node_id").is_none());
        assert!(r0.get("complexity").is_none());
    }

    #[test]
    fn test_trim_write_drops_byte_range() {
        let input = v(r#"{
            "success": true,
            "file_path": "src/new.rs",
            "language": "rust",
            "symbols": [
                {"name": "main", "type": "function", "byte_range": [0, 50]},
                {"name": "helper", "type": "function", "byte_range": [50, 100]}
            ]
        }"#);
        let t = trim_write(&input);
        assert_eq!(t["success"], true);
        let syms = t["symbols"].as_array().unwrap();
        for s in syms {
            assert!(s.get("byte_range").is_none());
        }
    }

    #[test]
    fn test_trim_index_collapses_parse_failures() {
        let input = v(r#"{
            "total_files": 100,
            "files_parsed": 95,
            "successful_parses": 95,
            "failed_parses": 5,
            "total_signatures": 200,
            "pdg_nodes": 1000,
            "pdg_edges": 2000,
            "indexed_nodes": 1000,
            "indexing_time_ms": 1234,
            "external_deps_in_lockfile": 50,
            "external_deps_resolved": 45,
            "external_deps_unresolved": 5
        }"#);
        let t = trim_index(&input);
        assert_eq!(t["parse_failures"], 5);
        assert_eq!(t["total_files"], 100);
        assert!(t.get("successful_parses").is_none());
        assert_eq!(t["external_deps_unresolved"], 5);
        assert!(t.get("external_deps_in_lockfile").is_none());
        assert!(t.get("external_deps_resolved").is_none());
    }

    #[test]
    fn test_trim_llm_payload_dispatches() {
        // Test the public entry point picks the right trim function.
        let search_data = v(r#"{"results": [{"file_path": "/p.rs", "symbol_name": "f"}]}"#);
        let out = trim_llm_payload("leindex.search", &search_data);
        assert!(out.get("results").is_some());
        assert!(out.get("count").is_some());

        // Edit payload keeps the structured diff and diff_text
        let edit_data = v(
            r#"{"preview_token": "x", "diff": {"file_path": "a.rs", "hunks": []}, "diff_text": "unified", "validation": {}}"#,
        );
        let out = trim_llm_payload("leindex.edit_preview", &edit_data);
        assert!(out.get("diff").is_some());
        assert!(out.get("diff_text").is_some());
        assert!(out.get("validation").is_none());

        // Unknown tool passes through unchanged
        let raw = v(r#"{"any": "value", "x": 1}"#);
        let out = trim_llm_payload("leindex.something_new", &raw);
        assert_eq!(out, raw);
    }

    #[test]
    fn test_trim_llm_payload_preserves_meta_warning() {
        // Regression: a stale index attaches `_warning` via
        // `wrap_with_meta` so the LLM knows to reindex. The trim
        // pipeline must not silently drop that field — otherwise the
        // model serves stale results thinking they are fresh.
        let stale_search = v(r#"{
            "count": 1,
            "results": [{"file_path": "src/foo.rs", "symbol": "main"}],
            "_warning": "index is stale (last update: 60s ago) — call leindex.index to refresh"
        }"#);
        let out = trim_llm_payload("leindex.search", &stale_search);
        assert_eq!(out["count"], 1);
        assert!(out.get("results").is_some());
        assert_eq!(
            out["_warning"],
            "index is stale (last update: 60s ago) — call leindex.index to refresh"
        );

        // _warning also preserved for tools that build a fresh object
        // (trim_diagnostics, trim_edit, etc.)
        let stale_edit = v(r#"{
            "preview_token": "tok",
            "diff": {"file_path": "a.rs", "hunks": []},
            "_warning": "stale"
        }"#);
        let out = trim_llm_payload("leindex.edit_apply", &stale_edit);
        assert_eq!(out["_warning"], "stale");

        // Non-`_`-prefixed fields are NOT merged (trim is still the
        // primary filter — `_` is the explicit out-of-band prefix).
        let extra = v(r#"{
            "results": [{"file_path": "src/foo.rs"}],
            "ignored_warning": "should not appear"
        }"#);
        let out = trim_llm_payload("leindex.search", &extra);
        assert!(out.get("ignored_warning").is_none());
    }

    #[test]
    fn test_trim_symbol_lookup_preserves_batch_results() {
        // Regression: when `leindex.symbol-lookup` is called in
        // batch mode (with the `symbols` array containing more
        // than one entry), the handler returns
        // `{batch: true, count, results: [...]}`. The trim
        // must detect the batch shape, recurse into each entry,
        // and preserve the requested results — otherwise the
        // LLM-visible payload becomes an empty object.
        let data = v(r#"{
            "batch": true,
            "count": 2,
            "results": [
                {
                    "symbol": "alpha",
                    "type": "function",
                    "file": "src/a.rs",
                    "byte_range": [10, 50],
                    "complexity": 3,
                    "language": "rust",
                    "callers": [],
                    "callees": [],
                    "impact_radius": {"affected_symbols": 0, "affected_files": 0}
                },
                {
                    "symbol": "beta",
                    "type": "function",
                    "file": "src/b.rs",
                    "byte_range": [60, 90],
                    "complexity": 1,
                    "language": "rust",
                    "callers": [],
                    "callees": [],
                    "impact_radius": {"affected_symbols": 0, "affected_files": 0}
                }
            ]
        }"#);
        let out = trim_llm_payload("leindex.symbol-lookup", &data);
        assert_eq!(out["batch"], true);
        assert_eq!(out["count"], 2);
        let results = out["results"].as_array().expect("results must be an array");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["symbol"], "alpha");
        assert_eq!(results[0]["file"], "src/a.rs");
        assert_eq!(results[0]["type"], "function");
        assert_eq!(results[0]["byte_range"][0], 10);
        assert_eq!(results[1]["symbol"], "beta");
        assert_eq!(results[1]["file"], "src/b.rs");
    }

    #[test]
    fn test_trim_symbol_lookup_batch_with_lookup_error_entry() {
        // Regression: a batch entry that hit a lookup error
        // comes back as `{"symbol": "...", "error": "..."}`
        // without the per-symbol fields. The per-entry
        // recursion must not crash on the missing fields and
        // must still copy the error entry through unchanged.
        let data = v(r#"{
            "batch": true,
            "count": 2,
            "results": [
                {"symbol": "ok", "type": "function", "file": "src/ok.rs", "byte_range": [0, 1], "complexity": 1, "language": "rust", "callers": [], "callees": [], "impact_radius": {}},
                {"symbol": "missing", "error": "Symbol not found"}
            ]
        }"#);
        let out = trim_llm_payload("leindex.symbol-lookup", &data);
        let results = out["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["symbol"], "ok");
        assert_eq!(results[0]["file"], "src/ok.rs");
        // The error entry has no `file` / `type` / etc. — those
        // fields simply don't appear in the trimmed copy. The
        // `error` field is not on the per-symbol whitelist but
        // the entry still survives as a result the LLM can
        // surface to the user.
        assert_eq!(results[1]["symbol"], "missing");
    }

    #[test]
    fn test_trim_read_symbol_includes_dependencies_when_present() {
        // Regression: when the caller passes
        // `include_dependencies=true`, the handler adds a
        // `dependencies` array to the payload. The trim must
        // preserve it so the caller-requested extra context
        // is not silently dropped.
        let data = v(r#"{
            "symbol": "compute",
            "type": "function",
            "file": "src/lib.rs",
            "language": "rust",
            "complexity": 4,
            "line_start": 10,
            "line_end": 25,
            "doc_comment": "/// Compute result",
            "source": "fn compute() {}",
            "callers": [],
            "callees": [],
            "dependencies": ["fn helper_a()", "fn helper_b()"]
        }"#);
        let out = trim_llm_payload("leindex.read-symbol", &data);
        assert_eq!(out["symbol"], "compute");
        let deps = out["dependencies"]
            .as_array()
            .expect("dependencies must be an array");
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0], "fn helper_a()");
        assert_eq!(deps[1], "fn helper_b()");
    }

    #[test]
    fn test_trim_search_falls_through_explicit_null_context() {
        // Regression: when a search result row carries an
        // explicit `context: null` (e.g. the handler skipped
        // snippet expansion for an unindexed file), the
        // trim fallback chain must NOT stop at the null —
        // it has to fall through to `content` and then to
        // `signature`. The previous `r.get("context").or_else(...)`
        // chain short-circuited on `Some(&Value::Null)`, so
        // the snippet was always `null` even when a
        // non-null `content` was available.
        let data = v(r#"{
            "query": "compute",
            "results": [
                {
                    "file_path": "src/lib.rs",
                    "symbol_name": "compute",
                    "symbol_type": "function",
                    "score": 0.9,
                    "context": null,
                    "content": "fn compute() -> i32 { 42 }",
                    "signature": "fn compute() -> i32"
                }
            ]
        }"#);
        let out = trim_llm_payload("leindex.search", &data);
        let results = out["results"].as_array().unwrap();
        let snippet = &results[0]["snippet"];
        // The `content` field carries the source body — the
        // trim path's snippet transform takes the first line
        // and truncates to 240 chars. Assert the body text is
        // present (not the literal `null`).
        let s = snippet
            .as_str()
            .expect("snippet must be a string, not null");
        assert!(
            s.contains("fn compute()"),
            "snippet should contain content body: {}",
            s
        );
    }

    #[test]
    fn test_trim_search_falls_through_explicit_null_symbol_name() {
        // Regression: same null-blocking pattern for the
        // `symbol` / `symbol_name` pair — an explicit
        // `null` in `symbol_name` must not block the
        // fallback to `symbol`.
        let data = v(r#"{
            "query": "x",
            "results": [
                {
                    "file_path": "src/lib.rs",
                    "symbol_name": null,
                    "symbol": "fallback_name",
                    "symbol_type": "function",
                    "score": 0.5,
                    "content": "fn fallback_name() {}"
                }
            ]
        }"#);
        let out = trim_llm_payload("leindex.search", &data);
        let results = out["results"].as_array().unwrap();
        assert_eq!(
            results[0]["symbol"], "fallback_name",
            "explicit null in symbol_name must fall through to symbol"
        );
    }

    /// Regression for P2 #3342365969: the search payload is
    /// paginated. `SearchHandler` returns `{results, count, offset,
    /// has_more, [suggestion]}` and the renderer/trim path was
    /// dropping `offset`, `has_more`, and `suggestion`. After the
    /// fix, all three must round-trip.
    #[test]
    fn test_trim_search_preserves_pagination_fields() {
        let data = v(r#"{
            "query": "find me",
            "results": [
                {"file": "a.rs", "byte_range": [0, 10], "content": "x"}
            ],
            "count": 1,
            "offset": 5,
            "has_more": true,
            "suggestion": "try a broader query"
        }"#);
        let out = trim_llm_payload("leindex.search", &data);
        assert_eq!(out["count"], 1, "count must round-trip");
        assert_eq!(out["offset"], 5, "offset must round-trip");
        assert_eq!(out["has_more"], true, "has_more must round-trip");
        assert_eq!(
            out["suggestion"], "try a broader query",
            "suggestion must round-trip"
        );
        assert_eq!(out["results"].as_array().unwrap().len(), 1);
    }

    /// `has_more=false` and `suggestion` absent on success is also
    /// the typical shape, not just the zero-results case. Verify
    /// both fields are accepted.
    #[test]
    fn test_trim_search_preserves_pagination_fields_no_more_no_suggestion() {
        let data = v(r#"{
            "query": "find me",
            "results": [{"file": "a.rs", "byte_range": [0, 10], "content": "x"}],
            "count": 1,
            "offset": 0,
            "has_more": false
        }"#);
        let out = trim_llm_payload("leindex.search", &data);
        assert_eq!(out["count"], 1);
        assert_eq!(out["offset"], 0);
        assert_eq!(out["has_more"], false);
        assert!(
            out.get("suggestion").is_none(),
            "no suggestion key when absent"
        );
    }

    /// Regression for HIGH round 12: `trim_search` used to clone the
    /// entire `results` array (`as_array().cloned()`), then clone
    /// each entry's `context` / `content` strings during the
    /// per-entry mapping. The intermediate allocations were
    /// unnecessary because only a few whitelisted fields are kept
    /// per entry. After the fix, the function borrows the source
    /// array and clones only the whitelisted fields.
    ///
    /// The contract we test: the output shape is unchanged. Each
    /// entry's `snippet` is taken from `context` / `content` /
    /// `signature` (truncated to first line, ≤ 240 chars), `score`
    /// collapses to `score.overall` or `score`, `symbol` falls
    /// through `symbol_name` → `symbol`, and `file_path` /
    /// `symbol_type` round-trip.
    #[test]
    fn test_trim_search_borrows_input_array() {
        // The "context" string is intentionally long to make the
        // "would have been cloned" cost observable in the
        // pre-fix code path. The post-fix code only borrows it.
        let big_context = "x".repeat(50_000);
        let data = v(&format!(
            r#"{{
                "query": "find me",
                "results": [
                    {{
                        "file_path": "src/a.rs",
                        "symbol_name": "alpha",
                        "symbol_type": "function",
                        "context": "{}",
                        "score": {{"overall": 0.91, "tfidf": 0.5, "neural": 0.95}}
                    }},
                    {{
                        "file_path": "src/b.rs",
                        "symbol": "beta",
                        "symbol_type": "struct",
                        "content": "struct Beta {{ x: u32 }}",
                        "score": 0.42
                    }}
                ],
                "count": 2,
                "offset": 0,
                "has_more": false
            }}"#,
            big_context
        ));
        let out = trim_llm_payload("leindex.search", &data);
        let results = out["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);

        // First entry: context was 50_000 chars, snippet must be
        // the first line truncated to 240 chars + "..." ellipsis
        // (243 chars total — `truncate_chars(s, 240)` preserves
        // 240 input chars and appends "...").
        let snippet0 = results[0]["snippet"].as_str().unwrap();
        assert_eq!(
            snippet0.len(),
            243,
            "snippet must be 240 chars + '...' ellipsis"
        );
        assert!(snippet0.ends_with("..."));
        assert!(snippet0[..240].chars().all(|c| c == 'x'));
        assert_eq!(results[0]["symbol"], "alpha");
        assert_eq!(results[0]["file_path"], "src/a.rs");
        assert_eq!(results[0]["symbol_type"], "function");
        // score collapsed to `overall`.
        assert!((results[0]["score"].as_f64().unwrap() - 0.91).abs() < 1e-9);

        // Second entry: content used (no context), symbol_name
        // absent so falls through to `symbol`.
        let snippet1 = results[1]["snippet"].as_str().unwrap();
        assert_eq!(snippet1, "struct Beta { x: u32 }");
        assert_eq!(results[1]["symbol"], "beta");
        // score is a plain number, not a struct.
        assert!((results[1]["score"].as_f64().unwrap() - 0.42).abs() < 1e-9);
    }

    /// Regression for HIGH round 12: `trim_read_symbol` used to
    /// clone the entire `callers` and `callees` arrays just to
    /// check `len() > 5`. The post-fix code computes the length
    /// without cloning via `as_array().map(|a| a.len())`.
    ///
    /// The contract: when `callers` / `callees` have more than 5
    /// entries, `*_more` is true; otherwise false. The first 5
    /// entries are still passed through via `take_n`.
    #[test]
    fn test_trim_read_symbol_callers_more_flag_no_clone() {
        // 7 callers — `callers_more` must be true, first 5 must
        // be passed through.
        let mut callers = Vec::new();
        for i in 0..7 {
            callers
                .push(serde_json::json!({"name": format!("c{}", i), "file": "x.rs", "type": "fn"}));
        }
        let data = v(&format!(
            r#"{{
                "symbol": "main",
                "type": "function",
                "file": "src/main.rs",
                "byte_range": [0, 100],
                "language": "rust",
                "callers": {},
                "callees": []
            }}"#,
            serde_json::to_string(&callers).unwrap()
        ));
        let out = trim_llm_payload("leindex.read-symbol", &data);
        assert_eq!(
            out["callers"].as_array().unwrap().len(),
            5,
            "first 5 callers passed through"
        );
        assert_eq!(
            out["callers_more"], true,
            "more than 5 callers => callers_more true"
        );

        // 3 callers — `callers_more` must be false, all 3 pass through.
        let mut small_callers = Vec::new();
        for i in 0..3 {
            small_callers.push(serde_json::json!({"name": format!("c{}", i)}));
        }
        let data = v(&format!(
            r#"{{
                "symbol": "main",
                "callers": {},
                "callees": []
            }}"#,
            serde_json::to_string(&small_callers).unwrap()
        ));
        let out = trim_llm_payload("leindex.read-symbol", &data);
        assert_eq!(out["callers"].as_array().unwrap().len(), 3);
        assert_eq!(
            out["callers_more"], false,
            "3 callers => callers_more false"
        );
    }

    /// Regression for HIGH round 12: `trim_deep_analyze` used to
    /// clone the whole `results` array, then clone the first 10
    /// entries into a second vector, then clone each whitelisted
    /// field on each entry. The post-fix code borrows the source
    /// array, takes 10, and clones only the whitelisted fields.
    ///
    /// The contract: with N>10 results, the trimmed output has
    /// 10 entries, `results_more` is N-10, and the whitelisted
    /// fields are present.
    #[test]
    fn test_trim_deep_analyze_caps_at_10_without_clone() {
        // 15 results.
        let mut results = Vec::new();
        for i in 0..15 {
            results.push(serde_json::json!({
                "rank": i,
                "file_path": format!("src/f{}.rs", i),
                "symbol_name": format!("sym{}", i),
                "symbol_type": "function",
                "signature": format!("fn sym{}()", i),
                // Verbose fields the trim should drop.
                "byte_range": [0, 1000],
                "language": "rust",
                "complexity": 5,
            }));
        }
        let data = v(&format!(
            r#"{{
                "query": "x",
                "tokens_used": 100,
                "processing_time_ms": 5,
                "context": "some context",
                "results": {}
            }}"#,
            serde_json::to_string(&results).unwrap()
        ));
        let out = trim_llm_payload("leindex.deep-analyze", &data);
        assert_eq!(
            out["results"].as_array().unwrap().len(),
            10,
            "cap at 10 entries"
        );
        assert_eq!(out["results_more"], 5, "results_more = 15 - 10 = 5");
        // First entry: whitelisted fields present, dropped fields absent.
        let first = &out["results"].as_array().unwrap()[0];
        assert_eq!(first["rank"], 0);
        assert_eq!(first["file_path"], "src/f0.rs");
        assert_eq!(first["symbol_name"], "sym0");
        assert_eq!(first["symbol_type"], "function");
        assert_eq!(first["signature"], "fn sym0()");
        assert!(
            first.get("byte_range").is_none(),
            "byte_range must be dropped"
        );
        assert!(first.get("language").is_none(), "language must be dropped");
        assert!(
            first.get("complexity").is_none(),
            "complexity must be dropped"
        );

        // 5 results: no cap, all pass through, results_more = 0.
        let mut small = Vec::new();
        for i in 0..5 {
            small.push(serde_json::json!({"rank": i, "file_path": "a.rs"}));
        }
        let data = v(&format!(
            r#"{{"query": "x", "results": {}}}"#,
            serde_json::to_string(&small).unwrap()
        ));
        let out = trim_llm_payload("leindex.deep-analyze", &data);
        assert_eq!(out["results"].as_array().unwrap().len(), 5);
        assert_eq!(out["results_more"], 0);
    }

    /// Regression for MED round 14 (gemini `3344534855`):
    /// `take_n` used to do `v.as_array().cloned().unwrap_or_default()`,
    /// which deep-cloned the entire source array before taking `n`.
    /// The post-fix code borrows the source and only clones the
    /// first `n` elements. The contract: with N=100 source items
    /// and n=3, the output is a 3-element array of the first three
    /// source items, preserving order.
    #[test]
    fn test_take_n_borrows_source() {
        let mut arr = Vec::new();
        for i in 0..100 {
            arr.push(serde_json::json!({"rank": i, "payload": "x".repeat(64)}));
        }
        let v = Value::Array(arr);
        let out = take_n(&v, 3);
        let out = out.as_array().unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["rank"], 0);
        assert_eq!(out[1]["rank"], 1);
        assert_eq!(out[2]["rank"], 2);
    }

    /// Regression for MED round 14 (gemini `3344534868`):
    /// `thin_symbol_map` used to clone the whole source array, then
    /// clone the first 5 callers and first 5 callees for each entry
    /// (cloning the unused tail as well). The post-fix code borrows
    /// the source array and clones only the 5-item windows of
    /// callers/callees. The contract: with callers/callees arrays
    /// of length 20 each, the output caps each at 5, drops
    /// `complexity`, and preserves the entry-level fields.
    #[test]
    fn test_thin_symbol_map_caps_callers_callees_at_5() {
        let mut big_callers = Vec::new();
        let mut big_callees = Vec::new();
        for i in 0..20 {
            big_callers.push(serde_json::json!({"name": format!("c{}", i)}));
            big_callees.push(serde_json::json!({"name": format!("e{}", i)}));
        }
        let sm = serde_json::json!([
            {
                "name": "foo",
                "complexity": 42,
                "callers": big_callers,
                "callees": big_callees,
            },
            {
                "name": "bar",
                "complexity": 7,
                "callers": [{"name": "only"}],
                "callees": [],
            }
        ]);
        let out = thin_symbol_map(&sm);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let foo = &arr[0];
        assert!(
            foo.get("complexity").is_none(),
            "complexity must be dropped"
        );
        assert_eq!(foo["name"], "foo");
        assert_eq!(
            foo["callers"].as_array().unwrap().len(),
            5,
            "callers must cap at 5 even when source has 20"
        );
        assert_eq!(foo["callers"][0]["name"], "c0");
        assert_eq!(foo["callers"][4]["name"], "c4");
        assert_eq!(
            foo["callees"].as_array().unwrap().len(),
            5,
            "callees must cap at 5 even when source has 20"
        );
        let bar = &arr[1];
        assert_eq!(bar["callers"].as_array().unwrap().len(), 1);
        assert_eq!(bar["callees"].as_array().unwrap().len(), 0);
    }

    /// Regression for MED round 14 (gemini `3344534871`):
    /// `trim_rename_symbol` used to clone the whole `diffs` array,
    /// then `take(25).map(|d| ...)` cloned only the first 25
    /// entries' file/diff fields. The post-fix code borrows the
    /// source array, takes 25, and clones only the file/diff
    /// fields of those 25. The contract: with N=50 source diffs,
    /// the output has 25 entries, `diffs_more` is 25, and
    /// `diffs_total` (if present) reflects the source length.
    #[test]
    fn test_trim_rename_symbol_caps_diffs_at_25() {
        let mut diffs = Vec::new();
        for i in 0..50 {
            diffs.push(serde_json::json!({
                "file": format!("src/f{}.rs", i),
                "diff": format!("--- a/src/f{}.rs\n+++ b/src/f{}.rs\n@@ -1,1 +1,1 @@\n-x\n+y", i, i),
                "diff_text": format!("long echo of diff {}", i),
            }));
        }
        let data = serde_json::json!({
            "old_name": "foo",
            "new_name": "bar",
            "diffs": diffs,
        });
        let out = trim_rename_symbol(&data);
        let shown = out["diffs"].as_array().unwrap();
        assert_eq!(shown.len(), 25, "must cap at 25 when source has 50");
        assert_eq!(out["diffs_more"], 25, "diffs_more = total - 25");
        assert_eq!(out["old_name"], "foo");
        assert_eq!(out["new_name"], "bar");
        // First shown is the first source; the long `diff_text` echo
        // is dropped (we keep only `file` and `diff`).
        assert!(shown[0].get("diff_text").is_none());
        assert_eq!(shown[0]["file"], "src/f0.rs");
    }

    /// Regression for MED round 20: `trim_write` previously
    /// cloned the entire `symbols` array via
    /// `.cloned().unwrap_or_default()` and then mapped over
    /// the clone. The clone was a deep copy (every
    /// `serde_json::Map` node, every string, every number)
    /// for elements that mostly get reduced to a
    /// two-field `{name, type}` object. The fix borrows the
    /// input array and only allocates the small output
    /// objects. This test locks the output contract: the
    /// trimmed `symbols` array has the same whitelisted
    /// fields and the same shape as before, so a future
    /// refactor that re-introduces the deep clone (or that
    /// accidentally changes the whitelist) is caught.
    #[test]
    fn test_trim_write_borrows_symbols_array() {
        // Each symbol carries a `byte_range` (the field
        // we're dropping) plus an unrelated `metadata`
        // object to make the "deep clone would have copied
        // all of this" cost visible.
        let input = v(r#"{
            "success": true,
            "file_path": "src/new.rs",
            "language": "rust",
            "symbols": [
                {
                    "name": "main",
                    "type": "function",
                    "byte_range": [0, 50],
                    "metadata": {"scope": "crate", "visibility": "pub", "attrs": ["inline"]}
                },
                {
                    "name": "helper",
                    "type": "function",
                    "byte_range": [50, 100],
                    "metadata": {"scope": "module", "visibility": "pub(crate)", "attrs": []}
                }
            ]
        }"#);
        let t = trim_write(&input);
        // Top-level fields preserved.
        assert_eq!(t["success"], true);
        assert_eq!(t["file_path"], "src/new.rs");
        assert_eq!(t["language"], "rust");
        // Per-symbol: only `name` and `type` are kept.
        let syms = t["symbols"].as_array().unwrap();
        assert_eq!(syms.len(), 2);
        for s in syms {
            assert!(s.get("byte_range").is_none());
            assert!(s.get("metadata").is_none());
            // The whitelist keys exist and are populated.
            assert!(s.get("name").is_some());
            assert!(s.get("type").is_some());
        }
        assert_eq!(syms[0]["name"], "main");
        assert_eq!(syms[0]["type"], "function");
        assert_eq!(syms[1]["name"], "helper");
        assert_eq!(syms[1]["type"], "function");
    }

    /// Regression for MED round 20: `trim_impact` /
    /// `trim_side` previously cloned the entire impact array
    /// upfront via `v.as_array().cloned().unwrap_or_default()`.
    /// Each cloned element was a `serde_json::Map` with
    /// dozens of fields (file, line, byte_range, scope,
    /// call_graph, ...), and most of the data was discarded
    /// in the subsequent mapping. The fix borrows the input
    /// array and only allocates the small output objects.
    /// This test locks the output contract: the per-entry
    /// shape is `{name, file, line}` with no other fields,
    /// and the top-level `symbol` / `risk_level` /
    /// `forward_impact` / `backward_impact` fields are
    /// preserved.
    #[test]
    fn test_trim_impact_borrows_impact_array() {
        let input = v(r#"{
            "symbol": "Foo::bar",
            "file": "src/foo.rs",
            "change_type": "modify",
            "risk_level": "medium",
            "direct_callers": ["caller_a", "caller_b"],
            "transitive_affected_symbols": ["callee_x", "callee_y", "callee_z"],
            "transitive_affected_files": 2,
            "transitive_callers": 5,
            "summary": "Changing 'Foo::bar' directly affects 3 symbols in 2 files (risk: medium)"
        }"#);
        let t = trim_impact(&input);
        // Top-level: all handler fields are preserved.
        assert_eq!(t["symbol"], "Foo::bar");
        assert_eq!(t["file"], "src/foo.rs");
        assert_eq!(t["change_type"], "modify");
        assert_eq!(t["risk_level"], "medium");
        assert!(t.get("direct_callers").is_some());
        assert!(t.get("transitive_affected_symbols").is_some());
        assert!(t.get("transitive_affected_files").is_some());
        assert!(t.get("transitive_callers").is_some());
        assert!(t.get("summary").is_some());
        // direct_callers array preserved
        let callers = t["direct_callers"].as_array().unwrap();
        assert_eq!(callers.len(), 2);
        assert_eq!(callers[0], "caller_a");
        // transitive_affected_symbols array preserved
        let affected = t["transitive_affected_symbols"].as_array().unwrap();
        assert_eq!(affected.len(), 3);
        assert_eq!(affected[0], "callee_x");
        // summary string preserved
        assert!(t["summary"].as_str().unwrap().contains("3 symbols"));
    }

    #[test]
    fn test_trim_git_status_preserves_pdg_enrichment() {
        // Regression: trim_git_status must preserve pdg_enrichment
        // and impact_summary so they survive trimming and reach the
        // MCP response (VAL-TRANSPORT-008).
        let input = v(r#"{
            "is_git_repo": true,
            "branch": "main",
            "summary": {"modified": 1, "staged": 0, "untracked": 0},
            "modified_files": ["src/main.rs"],
            "staged_files": [],
            "untracked_files": [],
            "changed_symbols": [
                {"file": "src/main.rs", "status": "modified", "symbols": [{"name": "main"}]}
            ],
            "pdg_enrichment": {"available": true},
            "impact_summary": {
                "total_affected_symbols": 3,
                "affected_files": ["src/main.rs"],
                "pdg_enriched": true
            },
            "diff": "some diff"
        }"#);
        let t = trim_git_status(&input);
        // Core fields preserved
        assert_eq!(t["branch"], "main");
        assert!(t["modified_files"].is_array());
        assert!(t["changed_symbols"].is_array());
        // pdg_enrichment and impact_summary must survive trimming
        assert!(t.get("pdg_enrichment").is_some());
        assert_eq!(t["pdg_enrichment"]["available"], true);
        assert!(t.get("impact_summary").is_some());
        assert_eq!(t["impact_summary"]["total_affected_symbols"], 3);
    }

    #[test]
    fn test_trim_git_status_pdg_unavailable() {
        // When PDG is unavailable, pdg_enrichment should still be
        // present with available=false.
        let input = v(r#"{
            "branch": "main",
            "summary": {"modified": 0, "staged": 0, "untracked": 0},
            "modified_files": [],
            "staged_files": [],
            "untracked_files": [],
            "changed_symbols": [],
            "pdg_enrichment": {
                "available": false,
                "reason": "PDG load failed",
                "error": "database error"
            },
            "impact_summary": {
                "total_affected_symbols": 0,
                "affected_files": [],
                "pdg_enriched": false
            }
        }"#);
        let t = trim_git_status(&input);
        assert_eq!(t["pdg_enrichment"]["available"], false);
        assert_eq!(t["pdg_enrichment"]["reason"], "PDG load failed");
        assert_eq!(t["pdg_enrichment"]["error"], "database error");
        assert_eq!(t["impact_summary"]["pdg_enriched"], false);
    }
}
