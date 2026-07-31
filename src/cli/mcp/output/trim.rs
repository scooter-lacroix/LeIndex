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

type JsonObject = serde_json::Map<String, Value>;

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

/// Copy top-level fields whose name starts with `_` from `original`
/// into `trimmed` when they are not already present. This keeps
/// out-of-band metadata (currently `_warning` from stale-index
/// detection) visible to the model after the LLM-payload trim step.
fn merge_meta(original: &Value, trimmed: &Value) -> Value {
    let Some(original) = original.as_object() else {
        return trimmed.clone();
    };
    let mut out = trimmed.clone();
    let Some(output) = out.as_object_mut() else {
        return trimmed.clone();
    };
    copy_missing_meta_fields(original, output);
    merge_missing_meta_object(original, output);
    out
}

fn copy_missing_meta_fields(original: &JsonObject, output: &mut JsonObject) {
    for (key, value) in original {
        if key.starts_with('_') && !output.contains_key(key) {
            output.insert(key.clone(), value.clone());
        }
    }
}

fn merge_missing_meta_object(original: &JsonObject, output: &mut JsonObject) {
    let Some(source) = original.get("_meta").and_then(Value::as_object) else {
        return;
    };
    let Some(target) = output.get_mut("_meta").and_then(Value::as_object_mut) else {
        return;
    };
    for (key, value) in source {
        if !target.contains_key(key) {
            target.insert(key.clone(), value.clone());
        }
    }
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
        "embedding_model": data.get("embedding_model"),
        // VAL-CROSS-015 / VAL-ORT-022: ORT info is part of the diagnostics
        // contract surfaced by the diagnostics command. Keep these fields in
        // the LLM-facing payload so MCP tools/call sees the same shape as
        // `leindex diagnostics`.
        "ort_version": data.get("ort_version"),
        "ort_path": data.get("ort_path"),
        "execution_provider": data.get("execution_provider"),
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
    copy_present_fields(
        data,
        &mut out,
        &[
            "generation",
            "executed_phases",
            "cache_hit",
            "changed_files",
            "deleted_files",
            "mode",
            "phases",
            "summary",
            "file_symbols",
            "phase_explanations",
        ],
    );

    for phase in 1u8..=5 {
        let key = format!("phase{}", phase);
        if let Some(value) = data.get(&key) {
            out.insert(key, value.clone());
        }
    }

    if let Some(value) = data.get("formatted_output") {
        out.insert(
            "formatted_output".to_string(),
            trimmed_formatted_output(value),
        );
    }
    Value::Object(out)
}

fn copy_present_fields(data: &Value, out: &mut serde_json::Map<String, Value>, keys: &[&str]) {
    for key in keys {
        if let Some(value) = data.get(key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
}

fn trimmed_formatted_output(value: &Value) -> Value {
    let Some(text) = value.as_str() else {
        return value.clone();
    };
    let capped = match text.char_indices().nth(4000) {
        Some((index, _)) => &text[..index],
        None => text,
    };
    Value::String(capped.to_string())
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
#[path = "trim_test.rs"]
mod tests;
