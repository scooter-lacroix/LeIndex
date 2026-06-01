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
    match normalized.as_str() {
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
    }
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
                .or_else(|| r.get("content"))
                .or_else(|| r.get("signature"))
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
                .cloned()
                .or_else(|| r.get("score").cloned())
                .unwrap_or(Value::Null);
            serde_json::json!({
                "file_path": r.get("file_path").cloned().unwrap_or(Value::Null),
                "symbol": r.get("symbol_name").cloned().unwrap_or(Value::Null),
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
    serde_json::json!({
        "node_id": data.get("node_id"),
        "symbol": data.get("symbol"),
        "file_path": data.get("file_path"),
        "line": data.get("line"),
        "symbol_type": data.get("symbol_type"),
        "content": data.get("content"),
        "callers": data.get("callers"),
        "callees": data.get("callees"),
    })
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
    serde_json::json!({
        "symbol": data.get("symbol"),
        "file_path": data.get("file_path"),
        "line": data.get("line"),
        "symbol_type": data.get("symbol_type"),
        "signature": data.get("signature"),
        "callers": data.get("callers"),
        "callees": data.get("callees"),
    })
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
    ] {
        if let Some(v) = data.get(k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    // The source body is the dominant token cost. Truncate to 2k chars
    // and expose a flag so the LLM knows to call again with a wider
    // budget if it really needs the full body.
    if let Some(src) = data.get("source").and_then(|v| v.as_str()) {
        let (head, truncated) = if src.len() > 2000 {
            (truncate_chars(&src[..2000], 2000), true)
        } else {
            (src.to_string(), false)
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
    // Keep the structured diff (file_path, additions, deletions, hunks)
    // — the LLM uses this to reason about the change. Drop the
    // `diff_text` echo (the LLM doesn't need a second copy as a string)
    // and the validation subtree (it's an internal report).
    serde_json::json!({
        "preview_token": data.get("preview_token"),
        "diff": data.get("diff"),
        "affected_symbols": data.get("affected_symbols"),
        "affected_files": data.get("affected_files"),
        "breaking_changes": data.get("breaking_changes"),
        "risk_level": data.get("risk_level"),
        "change_count": data.get("change_count"),
    })
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
        // Source truncated to 2000 chars
        let src = t["source"].as_str().unwrap();
        assert!(src.len() <= 2000, "got {}", src.len());
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
}
