//! Per-tool CLI render functions and the central dispatcher.
//!
//! Each `render_*` function reads a JSON `Value` (the same data the
//! LLM sees) and produces a human-readable, colored string for the
//! CLI. They use shared helpers (`header`, `field`,
//! `suffix`) defined at the top of this file so the visual style stays
//! consistent across tools.
//!
//! `render_tool_output` is the single entry point used by
//! `leindex tools run` and any other CLI surface; it dispatches on the
//! normalized tool name to the right `render_*` function.

use serde_json::Value;

use super::diff::{render_default, render_diff_value};
use super::{
    BOLD, DIM, LIGHT_BLUE, LIGHT_CYAN, LIGHT_GREEN, LIGHT_GREY, LIGHT_MAGENTA, LIGHT_RED,
    LIGHT_YELLOW, RESET, WHITE, normalize_tool_name, truncate_chars,
};
mod git_status;
use git_status::render_git_status;

mod symbol_phase;
use symbol_phase::*;

mod map_impact;
use map_impact::*;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

// =============================================================================
// Shared formatters — small helpers used by multiple render_* fns
// =============================================================================

pub(super) fn header(title: &str, color: bool) -> String {
    if color {
        format!("{}── {} ──{}", LIGHT_CYAN, title, RESET)
    } else {
        format!("── {} ──", title)
    }
}

fn field(name: &str, value: &str, color: bool) -> String {
    if color {
        format!(
            "  {}{}:{} {}{}{}\n",
            BOLD, name, RESET, LIGHT_CYAN, value, RESET
        )
    } else {
        format!("  {}: {}\n", name, value)
    }
}

fn suffix(symbol_count: u64, color: &str, reset: &str) -> String {
    if symbol_count == 0 {
        String::new()
    } else {
        format!("  {}[{} symbols]{}", color, symbol_count, reset)
    }
}

fn line_for(data: &Value) -> u64 {
    // Top-level `line` is the legacy flat shape (a real source line
    // number). The canonical `AnalysisResult` only carries the
    // anchor node's byte range in `results[0].byte_range` — and
    // `byte_range[0]` is a *byte offset* in the source file, not a
    // line number. Showing a byte offset as a line number is
    // misleading (e.g. an anchor at byte 15342 in a long file would
    // render as "Line: 15342" for a symbol that is actually on a
    // much earlier source line). The renderer does not have file
    // contents available to convert bytes to lines, so for the
    // canonical shape we return 0 — the caller interprets 0 as
    // "no real line available, start the gutter at 1" and omits
    // the `Line` field. See the
    // `byte_range_to_line_range` helper in `helpers.rs` for the
    // proper conversion when file contents are available.
    if let Some(n) = data.get("line").and_then(|v| v.as_u64()) {
        return n;
    }
    0
}

/// Strip ANSI CSI escape sequences from a string. Used by tests
/// (and only by tests) to assert on the visible text of the
/// rendered output. Handles only the SGR (colour) subset that
/// this module emits: `\x1b[<n>m`.
#[cfg(test)]
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '\x1b' {
            // Skip `ESC[`
            if iter.peek() == Some(&'[') {
                iter.next();
                // Skip parameters (digits and semicolons) and the
                // terminating letter.
                for c2 in iter.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn extract_array(data: &Value, keys: &[&str]) -> Vec<Value> {
    if let Some(arr) = data.as_array() {
        return arr.clone();
    }
    for k in keys {
        if let Some(arr) = data.get(*k).and_then(|v| v.as_array()) {
            return arr.clone();
        }
    }
    Vec::new()
}

// =============================================================================
// Tree rendering
// =============================================================================

/// Render a project structure as an ASCII tree with branch glyphs.
pub fn render_tree(nodes: &[Value], color: bool) -> String {
    let mut out = String::new();
    for (i, node) in nodes.iter().enumerate() {
        out.push_str(&render_tree_node(
            node,
            "",
            i == nodes.len() - 1,
            color,
            true,
        ));
    }
    out
}

fn render_tree_node(
    node: &Value,
    prefix: &str,
    is_last: bool,
    color: bool,
    is_root: bool,
) -> String {
    let mut out = String::new();
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let node_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("file");
    let symbol_count = node
        .get("symbol_count")
        .or_else(|| node.get("symbols"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let children = node.get("children").and_then(|v| v.as_array());

    let connector = if is_root {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };

    let name_color = if color {
        match node_type {
            "directory" | "dir" => LIGHT_BLUE,
            "module" => LIGHT_MAGENTA,
            _ => WHITE,
        }
    } else {
        ""
    };
    let count_color = if color { DIM } else { "" };
    let reset = if color { RESET } else { "" };

    // Dependency info for file nodes
    let incoming = node
        .get("incoming_dependencies")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let outgoing = node
        .get("outgoing_dependencies")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let dep_suffix = if incoming > 0 || outgoing > 0 {
        format!("  {}[{}→{}]{}", count_color, outgoing, incoming, reset,)
    } else {
        String::new()
    };

    if is_root {
        out.push_str(&format!(
            "{}{}{}{}{}\n",
            name_color,
            name,
            reset,
            suffix(symbol_count, count_color, reset),
            dep_suffix,
        ));
    } else {
        // `suffix(symbol_count, count_color, reset)` already wraps the
        // symbol-count line in `count_color` and ends with `reset`.
        // The earlier code emitted `count_color` immediately before
        // the suffix (and a trailing `reset` after it), so the final
        // output was `…reset {color} [N symbols]{reset} reset…` —
        // printing empty ANSI escapes when `symbol_count == 0` and
        // leaving a trailing reset that does nothing useful when it
        // is non-zero. Drop both the leading `count_color` and the
        // trailing `reset`; the suffix already opens and closes
        // colour for the symbol-count segment.
        out.push_str(&format!(
            "{}{}{}{}{}{}{}\n",
            prefix,
            connector,
            name_color,
            name,
            reset,
            suffix(symbol_count, count_color, reset),
            dep_suffix,
        ));
    }

    if let Some(kids) = children {
        // The child prefix is the vertical continuation that should
        // appear to the left of a grandchild's connector — it shows
        // whether this node has a sibling (│) or is the last ( ).
        // Root nodes pass no continuation because they have no
        // connector themselves; the first level of children sits at
        // column 0.
        let child_prefix = if is_last { "    " } else { "│   " };
        let combined_prefix = format!("{}{}", prefix, child_prefix);
        for (i, child) in kids.iter().enumerate() {
            out.push_str(&render_tree_node(
                child,
                &combined_prefix,
                i == kids.len() - 1,
                color,
                false,
            ));
        }
    }
    out
}

// =============================================================================
// Per-tool render functions
// =============================================================================

fn search_result_text(result: &Value) -> Option<&str> {
    result
        .get("signature")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            result
                .get("context")
                .and_then(|v| v.as_str())
                .and_then(|text| text.lines().next())
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .or_else(|| {
            result
                .get("snippet")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
}

fn render_search_result_preview(result: &Value, color: bool) -> String {
    let (dim, reset) = if color { (DIM, RESET) } else { ("", "") };
    if let Some(text) = search_result_text(result) {
        return format!("      {dim}{}{reset}\n", truncate_chars(text, 160));
    }

    let Some(range) = result.get("byte_range").and_then(|v| v.as_array()) else {
        return String::new();
    };
    if range.len() != 2 {
        return String::new();
    }
    let start = range[0].as_u64().unwrap_or(0);
    let end = range[1].as_u64().unwrap_or(0);
    if end <= start {
        return String::new();
    }
    format!("      {dim}(bytes {start}-{end}){reset}\n")
}

fn render_search_result(result: &Value, index: usize, color: bool) -> String {
    let (bold, yellow, cyan, dim, reset) = if color {
        (BOLD, LIGHT_YELLOW, LIGHT_CYAN, DIM, RESET)
    } else {
        ("", "", "", "", "")
    };
    let file = result
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let mut out = format!("  {bold}{}.{reset} {yellow}{file}{reset}", index + 1);

    if let Some(symbol) = result
        .get("symbol")
        .or_else(|| result.get("symbol_name"))
        .and_then(|v| v.as_str())
    {
        out.push_str(&format!(" :: {cyan}{symbol}{reset}"));
    }
    if let Some(symbol_type) = result.get("symbol_type").and_then(|v| v.as_str()) {
        out.push_str(&format!(" {dim}[{symbol_type}]{reset}"));
    }
    if let Some(line) = result.get("line_number").and_then(|v| v.as_u64()) {
        out.push_str(&format!(" {dim}:{line}{reset}"));
    }
    if let Some(score) = result
        .get("score")
        .and_then(|v| v.get("overall"))
        .and_then(|v| v.as_f64())
        .or_else(|| result.get("score").and_then(|v| v.as_f64()))
    {
        out.push_str(&format!(
            "  {dim}{}%{reset}",
            (score * 100.0).round() as usize
        ));
    }
    out.push('\n');
    out.push_str(&render_search_result_preview(result, color));
    out
}

fn render_search(data: &Value, query: &str, color: bool) -> String {
    let arr = extract_array(data, &["results", "items"]);
    if arr.is_empty() {
        return format!(
            "{}\n  No results for: {}\n",
            header(&format!("Search: \"{}\"", query), color),
            query,
        );
    }
    let mut out = header(
        &format!("Search: \"{}\" ({} results)", query, arr.len()),
        color,
    );
    out.push('\n');
    for (idx, r) in arr.iter().enumerate() {
        out.push_str(&render_search_result(r, idx, color));
    }
    // Low-signal warning (F-07): when the top composite score is below the
    // confidence floor the handler flags it; make that visible in the text
    // surface too so agents do not act on coincidental token-overlap hits.
    if data.get("low_signal").and_then(Value::as_bool) == Some(true) {
        let score = data
            .get("top_score")
            .and_then(Value::as_f64)
            .map(|s| format!(" ({:.2})", s))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n  ⚠ low signal{}: results may be coincidental token overlap; rephrase or use mode=find.\n",
            score
        ));
    }
    out
}

fn context_anchor(data: &Value) -> Option<&Value> {
    data.get("results")
        .and_then(|v| v.as_array())
        .and_then(|results| results.first())
}

fn render_context_metadata(data: &Value, color: bool) -> String {
    let anchor = context_anchor(data);
    let mut out = String::new();
    if let Some(symbol) = anchor
        .and_then(|result| result.get("symbol_name"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("symbol").and_then(|v| v.as_str()))
    {
        out.push_str(&field("Symbol", symbol, color));
    }
    if let Some(file) = anchor
        .and_then(|result| result.get("file_path"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("file_path").and_then(|v| v.as_str()))
    {
        out.push_str(&field("File", file, color));
    }
    if let Some(symbol_type) = anchor
        .and_then(|result| result.get("symbol_type"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("symbol_type").and_then(|v| v.as_str()))
    {
        out.push_str(&field("Type", symbol_type, color));
    }
    if let Some(line) = data.get("line").and_then(|v| v.as_u64()) {
        out.push_str(&field("Line", &line.to_string(), color));
    } else if let Some(range) = anchor
        .and_then(|result| result.get("byte_range"))
        .and_then(|v| v.as_array())
    {
        if range.len() == 2 {
            let start = range[0].as_u64().unwrap_or(0);
            let end = range[1].as_u64().unwrap_or(0);
            if end > start {
                out.push_str(&field("Range", &format!("bytes {start}-{end}"), color));
            }
        }
    }
    out
}

fn render_context_body(data: &Value, color: bool) -> String {
    let (cyan, dim, reset) = if color {
        (LIGHT_CYAN, DIM, RESET)
    } else {
        ("", "", "")
    };
    let body = data
        .get("context")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("content").and_then(|v| v.as_str()));
    if let Some(snippet) = body {
        let base = line_for(data);
        let gutter_base = if base == 0 { 1 } else { base };
        let mut out = String::from("\n");
        for (index, line) in snippet.lines().enumerate() {
            let number = gutter_base.saturating_add(index as u64);
            out.push_str(&format!("  {dim}{number:>4}{reset}│ {line}\n"));
        }
        return out;
    }

    let mut out = String::new();
    if let Some(results) = data.get("results").and_then(|v| v.as_array()) {
        for result in results {
            let symbol = result
                .get("symbol_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let file = result
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            out.push_str(&format!("  → {cyan}{symbol}{reset} {dim}{file}{reset}\n"));
        }
    }
    out
}

fn render_context(data: &Value, node_id: &str, color: bool) -> String {
    // `ContextHandler` returns a `AnalysisResult` (see
    // `src/cli/leindex/types.rs::AnalysisResult`) with shape:
    //   { query, results: [SearchResult, ...], context: Option<String>,
    //     tokens_used, processing_time_ms }
    // The expanded PDG text lives in `context`, not `content`. Node
    // metadata (symbol, file, type, line) is not on the top level —
    // it lives in `results[0]` (a `SearchResult`). Old callers
    // sometimes still emit a flat `content` / `file_path` /
    // `symbol_type` / `line` / `symbol` shape (e.g. the dispatcher
    // pre-trim payloads), so we fall back to that path before
    // showing the `results` summary.
    let mut out = header(&format!("Context: {}", node_id), color);
    out.push('\n');
    out.push_str(&render_context_metadata(data, color));
    out.push_str(&render_context_body(data, color));
    out
}

fn render_diagnostics_health(data: &Value, color: bool) -> String {
    let Some(health) = data.get("system_health") else {
        return String::new();
    };
    let mut out = String::from("\n  System Health:\n");
    if let Some(value) = health.get("index_health").and_then(|v| v.as_str()) {
        out.push_str(&field("  Index health", value, color));
    }
    if let Some(value) = health.get("pdg_loaded").and_then(|v| v.as_bool()) {
        out.push_str(&field("  PDG loaded", &value.to_string(), color));
    }
    for (key, label) in [
        ("pdg_nodes", "  PDG nodes"),
        ("pdg_edges", "  PDG edges"),
        ("search_index_nodes", "  Search nodes"),
        ("total_signatures", "  Total signatures"),
        ("failed_parses", "  Failed parses"),
    ] {
        if let Some(value) = health.get(key).and_then(|v| v.as_u64()) {
            out.push_str(&field(label, &value.to_string(), color));
        }
    }
    // A delta-scoped signature count covers only the files parsed in the last
    // (incremental) run — annotate it so it is not read as the project total.
    if health.get("signature_scope").and_then(Value::as_str) == Some("delta") {
        out.push_str(&field(
            "  Signature scope",
            "delta — count covers only files parsed in the last incremental run",
            color,
        ));
    }
    if let Some(value) = health.get("embedding_model").and_then(|v| v.as_str()) {
        out.push_str(&field("  Embedding model", value, color));
    }
    out
}

fn render_diagnostics_issues(data: &Value, color: bool) -> String {
    let Some(issues) = data.get("issues").and_then(|v| v.as_array()) else {
        return String::new();
    };
    if issues.is_empty() {
        return String::new();
    }

    let reset = if color { RESET } else { "" };
    let mut out = String::from("\n  Issues:\n");
    for issue in issues.iter().take(10) {
        let severity = issue
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("info");
        let message = issue.get("message").and_then(|v| v.as_str()).unwrap_or("?");
        let severity_color = if color {
            match severity {
                "error" => LIGHT_RED,
                "warning" => LIGHT_YELLOW,
                _ => LIGHT_BLUE,
            }
        } else {
            ""
        };
        out.push_str(&format!(
            "    {severity_color}{severity}{reset} {message}\n"
        ));
    }
    out
}

fn render_diagnostics(data: &Value, color: bool) -> String {
    let mut out = header("Diagnostics", color);
    out.push('\n');
    if let Some(p) = data.get("project_path").and_then(|v| v.as_str()) {
        out.push_str(&field("Project", p, color));
    }
    if let Some(v) = data.get("indexed_files").and_then(|v| v.as_u64()) {
        out.push_str(&field("Indexed files", &v.to_string(), color));
    }
    if let Some(v) = data.get("symbol_count").and_then(|v| v.as_u64()) {
        out.push_str(&field("Symbols", &v.to_string(), color));
    }
    if let Some(v) = data.get("index_size_mb").and_then(|v| v.as_f64()) {
        out.push_str(&field(
            "Index size (on disk)",
            &format!("{:.2} MB", v),
            color,
        ));
    }
    if let Some(v) = data.get("index_heap_estimate_mb").and_then(|v| v.as_f64()) {
        out.push_str(&field(
            "Index heap (estimated)",
            &format!("{:.2} MB", v),
            color,
        ));
    }
    if let Some(v) = data.get("memory_rss_mb").and_then(|v| v.as_f64()) {
        out.push_str(&field("Memory RSS", &format!("{:.2} MB", v), color));
    }
    if let Some(v) = data.get("db_size_bytes").and_then(|v| v.as_u64()) {
        out.push_str(&field("DB size", &format!("{} bytes", v), color));
    }
    if let Some(v) = data.get("stale").and_then(|v| v.as_bool()) {
        out.push_str(&field("Stale", &v.to_string(), color));
    }
    if let Some(v) = data.get("last_indexed_secs_ago").and_then(|v| v.as_u64()) {
        out.push_str(&field("Last indexed", &format!("{}s ago", v), color));
    }
    // VAL-ONNX-006: Show embedding model status at top level for CLI diagnostics
    if let Some(v) = data.get("embedding_model").and_then(|v| v.as_str()) {
        out.push_str(&field("Embedding model", v, color));
    }
    if let Some(engram) = data.get("engram") {
        let enabled = engram.get("enabled").and_then(|v| v.as_bool()) == Some(true);
        let summary = if enabled {
            let count = |key: &str| engram.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
            format!(
                "on ({} hits / {} misses, {} entries)",
                count("hits"),
                count("misses"),
                count("entries")
            )
        } else {
            "off (LEINDEX_FEATURE_ENGRAM=1 to enable)".to_string()
        };
        out.push_str(&field("Engram", &summary, color));
    }
    if let Some(enabled) = data.get("precision_enabled").and_then(|v| v.as_bool()) {
        let status = if enabled { "enabled" } else { "disabled" };
        out.push_str(&field("SCIP precision", status, color));
    }
    if let Some(v) = data.get("precision_nodes").and_then(|v| v.as_u64()) {
        out.push_str(&field("Precision nodes", &v.to_string(), color));
    }
    if let Some(languages) = data.get("precision_languages").and_then(|v| v.as_array()) {
        let names = languages
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !names.is_empty() {
            out.push_str(&field("Precision languages", &names, color));
        }
    }
    // VAL-CROSS-015 / VAL-ORT-022: surface resolved ORT library info so support
    // engineers can debug any install surface identically via `leindex diagnostics`.
    if let Some(v) = data.get("ort_version").and_then(|v| v.as_str()) {
        out.push_str(&field("ORT version", v, color));
    }
    if let Some(v) = data.get("ort_path").and_then(|v| v.as_str()) {
        out.push_str(&field("ORT dylib path", v, color));
    }
    if let Some(v) = data.get("execution_provider").and_then(|v| v.as_str()) {
        out.push_str(&field("Execution provider", v, color));
    }
    // The configured provider is a request; the worker reports what actually
    // loaded. When they differ (e.g. migraphx requested, cpu active after a
    // provider-library load failure) surface the fallback explicitly instead
    // of letting the requested value masquerade as the active one.
    if let Some(active) = data
        .get("execution_provider_active")
        .and_then(|v| v.as_str())
    {
        if let Some(requested) = data.get("execution_provider").and_then(|v| v.as_str()) {
            if active != requested {
                out.push_str(&field(
                    "Provider fallback",
                    &format!("{requested} requested, {active} ACTIVE"),
                    color,
                ));
            }
        }
    }
    out.push_str(&render_diagnostics_health(data, color));
    out.push_str(&render_diagnostics_issues(data, color));
    out
}

fn render_file_summary_symbols(data: &Value, color: bool) -> String {
    let Some(symbols) = data.get("symbols").and_then(|v| v.as_array()) else {
        return String::new();
    };
    if symbols.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n  Symbols:\n");
    for symbol in symbols.iter().take(50) {
        let name = symbol.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let typ = symbol
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("symbol");
        let (icon, icon_color) = if color {
            match typ {
                "function" | "fn" => ("ƒ", LIGHT_GREEN),
                "method" => ("m", LIGHT_CYAN),
                "struct" => ("S", LIGHT_MAGENTA),
                "enum" => ("E", LIGHT_YELLOW),
                "trait" => ("T", LIGHT_BLUE),
                "impl" => ("I", LIGHT_MAGENTA),
                "const" | "static" => ("c", LIGHT_CYAN),
                "field" => ("f", LIGHT_YELLOW),
                _ => ("•", WHITE),
            }
        } else {
            ("•", "")
        };
        out.push_str(&format!(
            "    {}{}{} {}{}{}\n",
            icon_color,
            icon,
            if color { RESET } else { "" },
            if color { LIGHT_CYAN } else { "" },
            name,
            if color { RESET } else { "" },
        ));
    }

    let truncated = data
        .get("symbols_truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let total = data
        .get("symbol_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let shown = symbols.len().min(50);
    if truncated || shown < total {
        out.push_str(&format!(
            "    {}… {} more symbols (truncated){}\n",
            if color { DIM } else { "" },
            total.saturating_sub(shown),
            if color { RESET } else { "" },
        ));
    }
    out
}

fn render_file_summary(data: &Value, color: bool) -> String {
    let mut out = header("File Summary", color);
    out.push('\n');
    if let Some(file) = data.get("file_path").and_then(|v| v.as_str()) {
        out.push_str(&field("File", file, color));
    }
    if let Some(lang) = data.get("language").and_then(|v| v.as_str()) {
        out.push_str(&field("Language", lang, color));
    }
    if let Some(lc) = data.get("line_count").and_then(|v| v.as_u64()) {
        out.push_str(&field("Lines", &lc.to_string(), color));
    }
    if let Some(sc) = data.get("symbol_count").and_then(|v| v.as_u64()) {
        out.push_str(&field("Symbols", &sc.to_string(), color));
    }
    if let Some(role) = data.get("module_role").and_then(|v| v.as_str()) {
        out.push_str(&field("Role", role, color));
    }
    out.push_str(&render_file_summary_symbols(data, color));
    out
}

fn render_read_file(data: &Value, color: bool) -> String {
    let mut out = String::new();
    if let Some(path) = data.get("file_path").and_then(|v| v.as_str()) {
        out.push_str(&header(&format!("Read: {}", path), color));
        out.push('\n');
    }
    let content = data
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let start = data.get("start_line").and_then(|v| v.as_u64()).unwrap_or(1);
    for (i, line) in content.lines().enumerate() {
        let n = start + i as u64;
        let gutter = format!("{:>4}", n);
        out.push_str(&format!(
            "  {}{}{}│ {}\n",
            if color { DIM } else { "" },
            gutter,
            if color { RESET } else { "" },
            line,
        ));
    }
    // `include_symbol_map=true` requests per-symbol PDG annotations for the
    // read range; the handler builds them and the trimmer keeps them, but
    // this renderer silently dropped the field — the parameter looked like a
    // no-op (N-08). Render a compact map when present.
    if let Some(symbols) = data.get("symbol_map").and_then(|v| v.as_array()) {
        if !symbols.is_empty() {
            out.push_str(&format!(
                "\n  {}Symbols in range ({}):{}\n",
                if color { DIM } else { "" },
                symbols.len(),
                if color { RESET } else { "" },
            ));
            for symbol in symbols.iter().take(20) {
                let name = symbol.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let typ = symbol.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let line_start = symbol.get("line_start").and_then(|v| v.as_u64());
                let line_end = symbol.get("line_end").and_then(|v| v.as_u64());
                let location = match (line_start, line_end) {
                    (Some(s), Some(e)) => format!(":{s}-{e}"),
                    (Some(s), None) => format!(":{s}"),
                    _ => String::new(),
                };
                out.push_str(&format!(
                    "    {}{}{} {}{}{}{}\n",
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                    if color { DIM } else { "" },
                    if typ.is_empty() {
                        String::new()
                    } else {
                        format!("[{typ}]")
                    },
                    location,
                    if color { RESET } else { "" },
                ));
            }
        }
    }
    out
}

fn render_write_symbols(data: &Value, color: bool) -> String {
    let Some(symbols) = data.get("symbols").and_then(|v| v.as_array()) else {
        return String::new();
    };
    if symbols.is_empty() {
        return String::new();
    }

    let mut out = format!(
        "\n  {}Symbols ({}):{}\n",
        if color { DIM } else { "" },
        symbols.len(),
        if color { RESET } else { "" },
    );
    for symbol in symbols.iter().take(20) {
        let name = symbol.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let typ = symbol.get("type").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!(
            "    {}{}{} {}{}{}\n",
            if color { LIGHT_CYAN } else { "" },
            name,
            if color { RESET } else { "" },
            if color { DIM } else { "" },
            if typ.is_empty() {
                String::new()
            } else {
                format!("[{}]", typ)
            },
            if color { RESET } else { "" },
        ));
    }
    if symbols.len() > 20 {
        out.push_str(&format!(
            "    {}…and {} more{}\n",
            if color { DIM } else { "" },
            symbols.len() - 20,
            if color { RESET } else { "" },
        ));
    }
    out
}

fn render_write(data: &Value, color: bool) -> String {
    // `WriteHandler` returns `{ success, file_path, language, symbols }`, not a diff.
    let success = data
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (status_label, status_color) = if success {
        ("Wrote", if color { LIGHT_GREEN } else { "" })
    } else {
        ("Write failed", if color { LIGHT_RED } else { "" })
    };
    let mut out = format!(
        "{}{}{}\n",
        status_color,
        status_label,
        if color { RESET } else { "" },
    );
    if let Some(path) = data.get("file_path").and_then(|v| v.as_str()) {
        out.push_str(&field("File", path, color));
    }
    if let Some(lang) = data.get("language").and_then(|v| v.as_str()) {
        out.push_str(&field("Language", lang, color));
    }
    out.push_str(&render_write_symbols(data, color));
    out
}

fn render_meta_line(label: &str, value: &str, color: bool) -> String {
    format!(
        "  {}{}:{} {}",
        if color { BOLD } else { "" },
        label,
        if color { RESET } else { "" },
        value,
    )
}

fn append_meta_lines(mut out: String, meta_lines: &[String]) -> String {
    if meta_lines.is_empty() {
        return out;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&meta_lines.join("\n"));
    out.push('\n');
    out
}

/// Render edit-preview output: diff followed by metadata fields
/// (affected_symbols, affected_files, risk_level, change_count).
fn render_edit_preview(data: &Value, color: bool) -> String {
    let mut meta_lines = Vec::new();

    if let Some(symbols) = data.get("affected_symbols").and_then(|v| v.as_array()) {
        if !symbols.is_empty() {
            let names: Vec<&str> = symbols.iter().filter_map(|v| v.as_str()).collect();
            meta_lines.push(render_meta_line(
                "Affected symbols",
                &names.join(", "),
                color,
            ));
        }
    }
    if let Some(files) = data.get("affected_files").and_then(|v| v.as_array()) {
        if !files.is_empty() {
            let names: Vec<&str> = files.iter().filter_map(|v| v.as_str()).collect();
            meta_lines.push(render_meta_line("Affected files", &names.join(", "), color));
        }
    }
    if let Some(risk) = data.get("risk_level").and_then(|v| v.as_str()) {
        meta_lines.push(render_meta_line("Risk level", risk, color));
    }
    if let Some(count) = data.get("change_count").and_then(|v| v.as_u64()) {
        meta_lines.push(render_meta_line("Change count", &count.to_string(), color));
    }
    if let Some(breaks) = data.get("breaking_changes").and_then(|v| v.as_array()) {
        for description in breaks.iter().filter_map(|value| value.as_str()) {
            meta_lines.push(render_meta_line("Breaking", description, color));
        }
    }

    append_meta_lines(render_diff_value(data, color), &meta_lines)
}

/// Render rename-symbol output: multi-file diffs followed by metadata
/// (old_name, new_name, files_affected, preview_only).
fn render_rename_symbol(data: &Value, color: bool) -> String {
    let mut meta_lines = Vec::new();

    if let (Some(old), Some(new)) = (
        data.get("old_name").and_then(|v| v.as_str()),
        data.get("new_name").and_then(|v| v.as_str()),
    ) {
        meta_lines.push(render_meta_line("Rename", &format!("{old} → {new}"), color));
    }
    if let Some(count) = data.get("files_affected").and_then(|v| v.as_u64()) {
        meta_lines.push(render_meta_line(
            "Files affected",
            &count.to_string(),
            color,
        ));
    }
    if let Some(diffs_more) = data
        .get("diffs_more")
        .and_then(|v| v.as_u64())
        .filter(|count| *count > 0)
    {
        meta_lines.push(render_meta_line(
            "Additional diffs",
            &format!("{diffs_more} more (not shown)"),
            color,
        ));
    }
    if data
        .get("preview_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        meta_lines.push(render_meta_line(
            "Preview only",
            "changes not applied",
            color,
        ));
    }

    append_meta_lines(render_diff_value(data, color), &meta_lines)
}

fn render_edit_region(region_value: &Value, color: bool) -> String {
    // `edit_region` may be a source excerpt or a byte-range object retained by `trim_edit`.
    let region_text = if let Some(s) = region_value.as_str() {
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    } else if let Some(obj) = region_value.as_object() {
        let start = obj.get("start").and_then(|v| v.as_u64());
        let end = obj.get("end").and_then(|v| v.as_u64());
        match (start, end) {
            (Some(s), Some(e)) => Some(format!("bytes {s}..{e}")),
            (Some(s), None) => Some(format!("bytes {s}..")),
            (None, Some(e)) => Some(format!("bytes ..{e}")),
            (None, None) => Some("bytes ?".to_string()),
        }
    } else {
        None
    };
    let Some(text) = region_text else {
        return String::new();
    };

    let mut out = String::from("\n");
    if region_value.is_string() {
        out.push_str(&format!(
            "  {}Surrounding region:{}\n",
            if color { DIM } else { "" },
            if color { RESET } else { "" },
        ));
        for line in text.lines() {
            out.push_str(&format!(
                "      {}{}{}\n",
                if color { DIM } else { "" },
                truncate_chars(line, 160),
                if color { RESET } else { "" },
            ));
        }
    } else {
        out.push_str(&format!(
            "  {}Surrounding region:{} {}\n",
            if color { DIM } else { "" },
            if color { RESET } else { "" },
            text,
        ));
    }
    out
}

fn render_edit_apply(data: &Value, color: bool) -> String {
    // `EditApplyHandler` returns a confirmation payload with shape
    // (see `src/cli/mcp/edit_apply_handler.rs::EditApplyHandler::
    // execute`):
    //   { success, changes_applied, file_path, edit_region,
    //     affected_symbols, affected_files, breaking_changes,
    //     [validation], [message] (no-op only) }
    // The handler never emits `diff` / `diffs` / `diff_text`, so
    // `render_diff_value` returns an empty string for the apply
    // response and the CLI prints nothing for a successful (or
    // no-op) apply. Surface the success status, the change count,
    // the file path, the affected-symbol/file summary, breaking
    // changes, and the surrounding-region excerpt.
    let mut out = String::new();
    let success = data
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let changes_applied = data
        .get("changes_applied")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let (status_label, status_color) = if !success {
        ("Edit apply failed", if color { LIGHT_RED } else { "" })
    } else if data.get("dry_run").and_then(|v| v.as_bool()) == Some(true) {
        // A dry run always reports zero applied changes by design; labeling
        // it "No-op (content identical)" misdescribes a real preview.
        (
            "Dry run (no changes written)",
            if color { LIGHT_YELLOW } else { "" },
        )
    } else if changes_applied == 0 {
        (
            "No-op (content identical)",
            if color { LIGHT_YELLOW } else { "" },
        )
    } else {
        ("Applied", if color { LIGHT_GREEN } else { "" })
    };
    out.push_str(&format!(
        "{}{}{}\n",
        status_color,
        status_label,
        if color { RESET } else { "" },
    ));
    if let Some(path) = data.get("file_path").and_then(|v| v.as_str()) {
        out.push_str(&field("File", path, color));
    }
    if let Some(msg) = data.get("message").and_then(|v| v.as_str()) {
        out.push_str(&field("Message", msg, color));
    }
    if let Some(arr) = data.get("affected_symbols").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push_str(&field("Affected symbols", &arr.len().to_string(), color));
        }
    }
    if let Some(arr) = data.get("affected_files").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push_str(&field("Affected files", &arr.len().to_string(), color));
        }
    }
    if let Some(bc) = data.get("breaking_changes").and_then(|v| v.as_array()) {
        if !bc.is_empty() {
            out.push_str(&field("Breaking changes", &bc.len().to_string(), color));
        }
    }
    out.push_str(
        &data
            .get("edit_region")
            .map_or_else(String::new, |region| render_edit_region(region, color)),
    );
    out
}

// =============================================================================
// Central dispatch — single entry point for CLI tool rendering
// =============================================================================

/// Render a tool's value for the CLI surface. The MCP transport uses
/// the raw `Value` (clean JSON for the LLM); the CLI uses this function
/// to produce a human-readable, colored view of the same data.
pub fn render_tool_output(name: &str, data: &Value, args: &Value) -> String {
    render_tool_output_with_color(name, data, args, true)
}

/// Render a tool's value with the freshness footer split out.
///
/// CLI one-shot consumers (`tools run`) print the body to stdout and the
/// footer to stderr so JSON-emitting tools stay parseable with a plain
/// `json.load`; the MCP transport concatenates both via
/// [`render_tool_output`].
pub fn render_tool_output_split(
    name: &str,
    data: &Value,
    args: &Value,
) -> (String, Option<String>) {
    let (rendered, footer) = render_tool_output_inner(name, data, args, true);
    (rendered, footer)
}

/// Render a tool's value *without* ANSI color codes. Used by the MCP
/// transport to produce clean text for the LLM (the CLI uses the
/// colored `render_tool_output`).
pub fn render_tool_output_plain(name: &str, data: &Value, args: &Value) -> String {
    render_tool_output_with_color(name, data, args, false)
}

fn render_tool_output_with_color(name: &str, data: &Value, args: &Value, color: bool) -> String {
    let (mut rendered, footer) = render_tool_output_inner(name, data, args, color);
    if let Some(footer) = footer {
        rendered.push_str(&footer);
    }
    rendered
}

fn render_tool_output_inner(
    name: &str,
    data: &Value,
    args: &Value,
    color: bool,
) -> (String, Option<String>) {
    let normalized = normalize_tool_name(name);
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let node_id = args.get("node_id").and_then(|v| v.as_str()).unwrap_or("");

    let rendered = match normalized.as_str() {
        "leindex_search" | "search" => render_search(data, query, color),
        "leindex_find" | "find" => render_find(data),
        "leindex_context" | "context" => render_context(data, node_id, color),
        "leindex_diagnostics" | "diagnostics" => render_diagnostics(data, color),
        "leindex_project_map" | "project_map" => render_project_map(data, color),
        "leindex_impact_analysis" | "impact_analysis" => render_impact(data, color),
        "leindex_symbol_lookup" | "symbol_lookup" => render_symbol_lookup(data, color),
        "leindex_phase_analysis" | "phase_analysis" => render_phase(data, color),
        "leindex_git_status" | "git_status" => render_git_status(data, color),
        "leindex_file_summary" | "file_summary" => render_file_summary(data, color),
        "leindex_read_file" | "read_file" => render_read_file(data, color),
        "leindex_edit_preview" | "edit_preview" => render_edit_preview(data, color),
        // `EditApplyHandler` returns a confirmation payload
        // (`success, changes_applied, file_path, edit_region, …`)
        // not a diff, so `render_diff_value` returns an empty
        // string for the apply response. Use the dedicated
        // confirmation renderer instead.
        "leindex_edit_apply" | "edit_apply" => render_edit_apply(data, color),
        // `WriteHandler` returns a confirmation payload
        // (`{success, file_path, language, symbols}`) not a diff, so
        // `render_diff_value` would return an empty string here.
        "leindex_write" | "write" => render_write(data, color),
        "leindex_rename_symbol" | "rename_symbol" => render_rename_symbol(data, color),
        _ => render_default(data, color),
    };
    if let Some(freshness) = data
        .get("_meta")
        .and_then(|meta| meta.get("freshness"))
        .or_else(|| data.get("freshness"))
    {
        let status = freshness
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let generation = freshness
            .get("generation")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string());
        let advisory = freshness
            .get("advisory")
            .and_then(Value::as_str)
            .or_else(|| freshness.get("warning").and_then(Value::as_str));
        let mut footer = format!("\nFreshness: status={status}, generation={generation}\n");
        if let Some(advisory) = advisory {
            footer.push_str(&format!("Advisory: {advisory}\n"));
        }
        return (rendered, Some(footer));
    }
    (rendered, None)
}

// =============================================================================
// Backward-compatible Formatter structs — thin wrappers for callers
// outside this module that build a formatter explicitly. New code
// should call `render_tool_output` instead.
// =============================================================================

/// Formatter for search results with ranked listings and scores
pub struct SearchFormatter {
    color: bool,
}

impl SearchFormatter {
    /// Create a new SearchFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format search results with ranking and scoring
    pub fn format(&self, results: &Value, query: &str) -> String {
        render_search(results, query, self.color)
    }
}

impl Default for SearchFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for project structure/dependency tree visualization
pub struct ProjectMapFormatter {
    color: bool,
}

impl ProjectMapFormatter {
    /// Create a new ProjectMapFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format project structure data as a tree view
    pub fn format(&self, data: &Value) -> String {
        render_project_map(data, self.color)
    }
}

impl Default for ProjectMapFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for project diagnostics and index status
pub struct DiagnosticsFormatter {
    color: bool,
}

impl DiagnosticsFormatter {
    /// Create a new DiagnosticsFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format diagnostics data including index stats and issues
    pub fn format(&self, data: &Value) -> String {
        render_diagnostics(data, self.color)
    }
}

impl Default for DiagnosticsFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for symbol impact analysis showing forward/backward dependencies
pub struct ImpactFormatter {
    color: bool,
}

impl ImpactFormatter {
    /// Create a new ImpactFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format impact analysis data with risk levels and affected symbols
    pub fn format(&self, data: &Value) -> String {
        render_impact(data, self.color)
    }
}

impl Default for ImpactFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for symbol lookup results with callers and callees
pub struct SymbolLookupFormatter {
    color: bool,
}

impl SymbolLookupFormatter {
    /// Create a new SymbolLookupFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format symbol lookup data showing definition, callers, and callees
    pub fn format(&self, data: &Value) -> String {
        render_symbol_lookup(data, self.color)
    }
}

impl Default for SymbolLookupFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for phase analysis results
pub struct PhaseFormatter {
    color: bool,
}

impl PhaseFormatter {
    /// Create a new PhaseFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format phase analysis data with phase status and summaries
    pub fn format(&self, data: &Value) -> String {
        render_phase(data, self.color)
    }
}

impl Default for PhaseFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for git status with staged, modified, and untracked files
pub struct GitStatusFormatter {
    color: bool,
}

impl GitStatusFormatter {
    /// Create a new GitStatusFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format git status data showing branch and file changes
    pub fn format(&self, data: &Value) -> String {
        render_git_status(data, self.color)
    }
}

impl Default for GitStatusFormatter {
    fn default() -> Self {
        Self::new()
    }
}

/// Formatter for file summary with symbols and complexity metrics
pub struct FileSummaryFormatter {
    color: bool,
}

impl FileSummaryFormatter {
    /// Create a new FileSummaryFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format file summary data with file info and symbol list
    pub fn format(&self, data: &Value) -> String {
        render_file_summary(data, self.color)
    }
}

impl Default for FileSummaryFormatter {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

// =============================================================================
// leindex_find — compact, token-lean text
// =============================================================================

/// Render a `leindex_find` result. Hits are grouped by file and, inside a file,
/// by enclosing symbol, so a symbol name is paid for once rather than per line.
fn render_find(data: &Value) -> String {
    use std::fmt::Write as _;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let num = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    let mut out = String::new();

    if data.get("is_git_repo").is_some() {
        return render_default(data, false);
    }
    let pattern = text(data, "pattern");
    if data["target"] == "symbols" {
        let total = num(data, "total_symbols");
        let _ = writeln!(out, "{total} symbol(s) named like \"{pattern}\"");
        for symbol in data["symbols"].as_array().into_iter().flatten() {
            let line = num(symbol, "line");
            let _ = writeln!(
                out,
                "  {} {} — {}{}{}",
                text(symbol, "kind"),
                text(symbol, "name"),
                text(symbol, "file"),
                if line > 0 {
                    format!(":{line}")
                } else {
                    String::new()
                },
                if symbol["stale"] == true {
                    " (file changed since indexing)"
                } else {
                    ""
                },
            );
        }
        if let Some(note) = data.get("note").and_then(Value::as_str) {
            let _ = writeln!(out, "{note}");
        }
        if data["has_more"] == true {
            let _ = writeln!(out, "… more: offset={}", num(data, "next_offset"));
        }
        return out;
    }

    let stats = &data["stats"];
    let indexed = data["roots"]
        .as_array()
        .is_some_and(|roots| roots.iter().all(|r| r["indexed"] == true));
    let _ = writeln!(
        out,
        "{} match(es) in {} file(s) for \"{}\" · {}ms{}",
        num(data, "total_matches"),
        num(data, "total_files"),
        pattern,
        num(stats, "millis"),
        if indexed { " · indexed" } else { "" },
    );
    if let Some(fallback) = data.get("fallback").and_then(Value::as_str) {
        let _ = writeln!(out, "({fallback})");
    }
    match text(data, "output").as_str() {
        "count" => {}
        "files" => {
            for file in data["files"].as_array().into_iter().flatten() {
                let _ = writeln!(out, "  {} ({})", text(file, "file"), num(file, "matches"));
            }
        }
        "symbols" => {
            for symbol in data["symbols"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    out,
                    "  {} {} — {} ({})",
                    text(symbol, "kind"),
                    text(symbol, "name"),
                    text(symbol, "file"),
                    num(symbol, "matches"),
                );
            }
        }
        _ => {
            for file in data["files"].as_array().into_iter().flatten() {
                let shown = file["hits"].as_array().map_or(0, Vec::len) as u64;
                let total = num(file, "matches");
                let _ = writeln!(
                    out,
                    "{}{}",
                    text(file, "file"),
                    if total > shown {
                        format!(" ({shown} of {total} shown)")
                    } else {
                        String::new()
                    },
                );
                let mut current: Option<String> = None;
                for hit in file["hits"].as_array().into_iter().flatten() {
                    let symbol = hit
                        .get("symbol")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if symbol != current {
                        if let Some(name) = &symbol {
                            let _ = writeln!(out, " {} ({})", name, text(hit, "kind"));
                        }
                        current = symbol;
                    }
                    for line in hit["before"].as_array().into_iter().flatten() {
                        let _ = writeln!(out, "   | {}", line.as_str().unwrap_or(""));
                    }
                    let _ = writeln!(out, "  {}: {}", num(hit, "line"), text(hit, "text"));
                    for line in hit["after"].as_array().into_iter().flatten() {
                        let _ = writeln!(out, "   | {}", line.as_str().unwrap_or(""));
                    }
                }
            }
        }
    }
    if data["has_more"] == true {
        let _ = writeln!(out, "… more results: offset={}", num(data, "next_offset"));
    }
    if let Some(note) = data.get("note").and_then(Value::as_str) {
        let _ = writeln!(out, "{note}");
    }
    out
}
