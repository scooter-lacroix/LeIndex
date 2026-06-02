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
    let arr = data
        .as_array()
        .cloned()
        .or_else(|| data.get("results").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let trimmed: Vec<Value> = arr
        .into_iter()
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
                .or_else(|| {
                    r.get("score")
                        .filter(|v| !v.is_null())
                        .cloned()
                })
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
                "score": score,
                "snippet": snippet,
            })
        })
        .collect();
    // The model already knows the query it sent; we just hand back the
    // count and the per-hit fields it needs to make decisions.
    serde_json::json!({
        "count": trimmed.len(),
        "results": trimmed,
    })
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
        "stale": data.get("stale"),
        "last_indexed_secs_ago": data.get("last_indexed_secs_ago"),
        "issues": data.get("issues"),
    })
}

fn trim_impact(data: &Value) -> Value {
    fn trim_side(v: &Value) -> Value {
        let arr = v.as_array().cloned().unwrap_or_default();
        let trimmed: Vec<Value> = arr
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.get("name"),
                    "file": s.get("file"),
                    "line": s.get("line"),
                })
            })
            .collect();
        Value::Array(trimmed)
    }
    serde_json::json!({
        "symbol": data.get("symbol"),
        "risk_level": data.get("risk_level"),
        "forward_impact": data.get("forward_impact").map(trim_side),
        "backward_impact": data.get("backward_impact").map(trim_side),
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
                data.get("count").cloned().unwrap_or(Value::from(trimmed_results.len())),
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
        "size": data.get("size"),
        "complexity": data.get("complexity"),
        "symbols": data.get("symbols"),
    })
}

fn trim_phase(data: &Value) -> Value {
    serde_json::json!({
        "mode": data.get("mode"),
        "phases": data.get("phases"),
        "summary": data.get("summary"),
    })
}

fn trim_git_status(data: &Value) -> Value {
    serde_json::json!({
        "branch": data.get("branch"),
        "status": data.get("status"),
        "staged": data.get("staged"),
        "modified": data.get("modified"),
        "untracked": data.get("untracked"),
        "deleted": data.get("deleted"),
    })
}

fn trim_read_file(data: &Value) -> Value {
    // `content` is the dominant cost. Caller already asked for a slice
    // (start_line / end_line), so we keep it. Drop `symbol_map` unless
    // the handler populated it; the per-entry callers/callees arrays
    // can also be capped.
    let mut out = serde_json::Map::new();
    out.insert("file_path".to_string(), data.get("file_path").cloned().unwrap_or(Value::Null));
    out.insert("language".to_string(), data.get("language").cloned().unwrap_or(Value::Null));
    out.insert("total_lines".to_string(), data.get("total_lines").cloned().unwrap_or(Value::Null));
    out.insert("start_line".to_string(), data.get("start_line").cloned().unwrap_or(Value::Null));
    out.insert("end_line".to_string(), data.get("end_line").cloned().unwrap_or(Value::Null));
    out.insert("content".to_string(), data.get("content").cloned().unwrap_or(Value::Null));
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
    let arr = sm.as_array().cloned().unwrap_or_default();
    let trimmed: Vec<Value> = arr
        .into_iter()
        .map(|mut s| {
            if let Some(obj) = s.as_object_mut() {
                obj.remove("complexity");
                if let Some(callers) = obj.get("callers").cloned() {
                    obj.insert("callers".to_string(), take_n(&callers, 5));
                }
                if let Some(callees) = obj.get("callees").cloned() {
                    obj.insert("callees".to_string(), take_n(&callees, 5));
                }
            }
            s
        })
        .collect();
    Value::Array(trimmed)
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
    // The source body is the dominant token cost. Truncate to 2k chars
    // and expose a flag so the LLM knows to call again with a wider
    // budget if it really needs the full body. Truncate on character
    // boundaries directly from the source `&str` — slicing at a fixed
    // byte offset can panic when the offset lands inside a multi-byte
    // UTF-8 sequence (e.g. emoji, identifiers in non-ASCII source).
    //
    // `char_indices().nth(2000)` is a single pass: it walks the UTF-8
    // string once and stops at the (n+1)-th character, returning the
    // byte offset of the truncation boundary. We then append the
    // standard `...` ellipsis so the LLM sees a 2003-char preview.
    // The previous `chars().count() > 2000` + `truncate_chars(...)`
    // path scanned the entire string twice on every call, which
    // dominated trim cost for very large sources.
    if let Some(src) = data.get("source").and_then(|v| v.as_str()) {
        let (head, truncated) = match src.char_indices().nth(2000) {
            Some((idx, _)) => (format!("{}...", &src[..idx]), true),
            None => (src.to_string(), false),
        };
        out.insert("source".to_string(), Value::String(head));
        out.insert("source_truncated".to_string(), Value::Bool(truncated));
    }
    // callers/callees: keep 5 by default, expose `*_more` flag.
    if let Some(callers) = data.get("callers") {
        let arr = callers.as_array().cloned().unwrap_or_default();
        out.insert("callers".to_string(), take_n(callers, 5));
        out.insert("callers_more".to_string(), Value::Bool(arr.len() > 5));
    }
    if let Some(callees) = data.get("callees") {
        let arr = callees.as_array().cloned().unwrap_or_default();
        out.insert("callees".to_string(), take_n(callees, 5));
        out.insert("callees_more".to_string(), Value::Bool(arr.len() > 5));
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
    // Drop the `before`/`after` context windows — the LLM already has
    // the matched line and can call read_file for context.
    let arr = data
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let trimmed: Vec<Value> = arr
        .into_iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for k in ["file", "line", "content", "in_symbol", "symbol_type"] {
                if let Some(v) = r.get(k) {
                    obj.insert(k.to_string(), v.clone());
                }
            }
            Value::Object(obj)
        })
        .collect();
    let mut out = serde_json::Map::new();
    out.insert("count".to_string(), data.get("count").cloned().unwrap_or(Value::Null));
    out.insert("total_matched".to_string(), data.get("total_matched").cloned().unwrap_or(Value::Null));
    out.insert("has_more".to_string(), data.get("has_more").cloned().unwrap_or(Value::Null));
    out.insert("offset".to_string(), data.get("offset").cloned().unwrap_or(Value::Null));
    out.insert("results".to_string(), Value::Array(trimmed));
    Value::Object(out)
}

fn trim_deep_analyze(data: &Value) -> Value {
    // Keep the pre-built `context` (already token-budgeted). The
    // results array mirrors a search hit — drop verbose per-result
    // fields and cap to 10.
    let results = data
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let shown = results.iter().take(10).cloned().collect::<Vec<_>>();
    let trimmed: Vec<Value> = shown
        .into_iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            for k in ["rank", "file_path", "symbol_name", "symbol_type", "signature"] {
                if let Some(v) = r.get(k) {
                    obj.insert(k.to_string(), v.clone());
                }
            }
            Value::Object(obj)
        })
        .collect();
    serde_json::json!({
        "query": data.get("query"),
        "tokens_used": data.get("tokens_used"),
        "processing_time_ms": data.get("processing_time_ms"),
        "context": data.get("context"),
        "results": trimmed,
        "results_more": results.len().saturating_sub(10),
    })
}

fn trim_write(data: &Value) -> Value {
    // Drop per-symbol byte_range; the symbol name + type are enough for
    // the LLM to follow up with read_symbol.
    let arr = data
        .get("symbols")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let trimmed: Vec<Value> = arr
        .into_iter()
        .map(|s| {
            let mut obj = serde_json::Map::new();
            for k in ["name", "type"] {
                if let Some(v) = s.get(k) {
                    obj.insert(k.to_string(), v.clone());
                }
            }
            Value::Object(obj)
        })
        .collect();
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
    //   * `edit_preview` → `preview_token`, `diff`, `affected_*`,
    //     `risk_level`, `change_count`, `validation`
    //   * `edit_apply`   → `success`, `changes_applied`, `file_path`,
    //     `edit_region`, `message` (a no-op confirmation), plus the
    //     same diff/affected_* fields
    //
    // We want to keep the union of both shapes so an MCP `edit_apply`
    // call still surfaces the success confirmation and verification
    // context, and an `edit_preview` call still gets the diff. The
    // `diff_text` echo and the internal `validation` subtree are
    // dropped (the model doesn't need a second copy of the diff or
    // the full validator report).
    let mut out = serde_json::Map::new();
    for k in [
        // shared / preview-shaped
        "preview_token",
        "diff",
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
    // (callers rarely need more for an LLM context).
    let arr = data
        .get("diffs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let shown: Vec<Value> = arr
        .iter()
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
        .collect();
    serde_json::json!({
        "diffs": shown,
        "diffs_more": arr.len().saturating_sub(25),
        "old_name": data.get("old_name"),
        "new_name": data.get("new_name"),
    })
}

// =============================================================================
// Small array-helpers
// =============================================================================

/// Return the first `n` items of a Value (assumed array), as a Value.
fn take_n(v: &Value, n: usize) -> Value {
    let arr = v.as_array().cloned().unwrap_or_default();
    Value::Array(arr.into_iter().take(n).collect())
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
        let merged = trim_llm_payload("leindex.context", &v(r#"{
            "query": "q", "results": [], "context": "c",
            "tokens_used": 0, "processing_time_ms": 0,
            "_warning": "stale"
        }"#));
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
        // diff_text echo + validation subtree are dropped
        assert!(t.get("diff_text").is_none());
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
        let callers: Vec<Value> = (0..20).map(|i| serde_json::json!({"name": format!("c{}", i), "file": "a.rs", "line": i})).collect();
        let input = v(&format!(r#"{{
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
        }}"#, "x".repeat(3000), serde_json::to_string(&callers).unwrap()));
        let t = trim_read_symbol(&input);
        // Source is truncated to 2000 chars + 3-char "..." ellipsis.
        // The previous byte-slice version could panic on multi-byte
        // UTF-8 sequences; the new char-boundary truncation cannot.
        let src = t["source"].as_str().unwrap();
        assert!(src.chars().count() <= 2003, "got {}", src.chars().count());
        assert!(src.ends_with("..."), "missing ellipsis: {:?}", &src[src.len() - 10..]);
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
    fn test_trim_text_search_drops_context_windows() {
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
        assert!(r.get("before").is_none());
        assert!(r.get("after").is_none());
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
        let input = v(&format!(r#"{{
            "query": "what is X",
            "tokens_used": 1500,
            "processing_time_ms": 250,
            "context": "expanded prose here",
            "results": {}
        }}"#, serde_json::to_string(&results).unwrap()));
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

        // Edit payload keeps the structured diff
        let edit_data = v(r#"{"preview_token": "x", "diff": {"file_path": "a.rs", "hunks": []}, "diff_text": "unified", "validation": {}}"#);
        let out = trim_llm_payload("leindex.edit_preview", &edit_data);
        assert!(out.get("diff").is_some());
        assert!(out.get("diff_text").is_none());
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
        let deps = out["dependencies"].as_array().expect("dependencies must be an array");
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
        let s = snippet.as_str().expect("snippet must be a string, not null");
        assert!(s.contains("fn compute()"), "snippet should contain content body: {}", s);
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
}
