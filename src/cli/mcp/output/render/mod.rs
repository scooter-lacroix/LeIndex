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
    normalize_tool_name, truncate_chars, BOLD, DIM, LIGHT_BLUE, LIGHT_CYAN, LIGHT_GREEN,
    LIGHT_GREY, LIGHT_MAGENTA, LIGHT_RED, LIGHT_YELLOW, RESET, WHITE,
};
mod git_status;
use git_status::render_git_status;

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
        format!(
            "  {}[{}→{}←{}]{}",
            count_color, outgoing, incoming, reset, reset,
        )
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
        out.push_str(&field("Index size", &format!("{:.2} MB", v), color));
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
    out.push_str(&render_diagnostics_health(data, color));
    out.push_str(&render_diagnostics_issues(data, color));
    out
}

fn render_project_map(data: &Value, color: bool) -> String {
    let mut out = header("Project Structure", color);
    out.push('\n');
    if let Some(tree) = data.get("tree").and_then(|v| v.as_array()) {
        out.push_str(&render_tree(tree, color));
    } else if let Some(roots) = data.get("root").map(|v| vec![v.clone()]) {
        out.push_str(&render_tree(&roots, color));
    } else if let Some(files) = data.get("files").and_then(|v| v.as_array()) {
        let tree = build_tree_from_files(files);
        if tree.is_empty() {
            // No directory info is available in the file entries (the
            // handler ships basenames, not full paths), so render a flat
            // ranked list rather than fabricating fake directories.
            out.push_str(&render_flat_files(files, color));
        } else {
            out.push_str(&render_tree(&tree, color));
        }
    }
    if let Some(stats) = data.get("stats") {
        out.push('\n');
        if let Some(v) = stats.get("total_files").and_then(|v| v.as_u64()) {
            out.push_str(&field("Files", &v.to_string(), color));
        }
        if let Some(v) = stats.get("total_symbols").and_then(|v| v.as_u64()) {
            out.push_str(&field("Symbols", &v.to_string(), color));
        }
        if let Some(v) = stats.get("avg_complexity").and_then(|v| v.as_f64()) {
            out.push_str(&field("Avg complexity", &format!("{:.1}", v), color));
        }
        if let Some(v) = stats.get("total_loc").and_then(|v| v.as_u64()) {
            out.push_str(&field("Lines of code", &v.to_string(), color));
        }
    }
    // Also show total_files_in_scope from the handler output
    // (the handler puts this at top level, not under "stats")
    if let Some(v) = data.get("total_files_in_scope").and_then(|v| v.as_u64()) {
        if data.get("stats").is_none() {
            out.push('\n');
        }
        out.push_str(&field("Files in scope", &v.to_string(), color));
    }
    out
}

fn render_flat_files(files: &[Value], color: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {}(flat list — files ordered as returned){}\n",
        if color { DIM } else { "" },
        if color { RESET } else { "" },
    ));
    for (i, f) in files.iter().enumerate() {
        let path = f
            .get("path")
            .and_then(|v| v.as_str())
            .or_else(|| f.get("relative_path").and_then(|v| v.as_str()))
            .unwrap_or("?");
        let syms = f.get("symbol_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let cx = f
            .get("total_complexity")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let deps = f
            .get("incoming_dependencies")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + f.get("outgoing_dependencies")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        // The complexity value is shown as a colour-coded integer
        // (`cx:{N}`); the human-readable label ("low" / "med" /
        // "high") was previously computed alongside the colour but
        // never rendered, so simplify the conditional to just the
        // colour string. When colour is disabled the colour string
        // is the empty literal.
        let cx_color = if color {
            match cx {
                0..=20 => LIGHT_GREEN,
                21..=60 => LIGHT_YELLOW,
                _ => LIGHT_RED,
            }
        } else {
            ""
        };
        out.push_str(&format!(
            "  {}{:>3}.{} {}{}{}  {}{} sym  cx:{}{}{}  deps:{}\n",
            if color { BOLD } else { "" },
            i + 1,
            if color { RESET } else { "" },
            if color { LIGHT_YELLOW } else { "" },
            path,
            if color { RESET } else { "" },
            if color { DIM } else { "" },
            syms,
            cx_color,
            cx,
            if color { RESET } else { "" },
            deps,
        ));
    }
    out
}

/// Convert a flat list of `{path, relative_path, symbol_count, ...}`
/// entries into a nested directory tree suitable for `render_tree`.
/// Returns an empty Vec if the file entries don't carry directory
/// information (caller falls back to flat rendering).
fn build_tree_from_files(files: &[Value]) -> Vec<Value> {
    use std::collections::BTreeMap;

    // Bail out unless at least one entry has a path with a directory
    // separator — otherwise we'd fabricate a meaningless single-level
    // tree from basenames.
    let any_with_dir = files.iter().any(|f| {
        f.get("relative_path")
            .and_then(|v| v.as_str())
            .or_else(|| f.get("path").and_then(|v| v.as_str()))
            .map(|p| p.contains('/') || p.contains('\\'))
            .unwrap_or(false)
    });
    if !any_with_dir {
        return Vec::new();
    }

    // A `Node` here is a tiny tree of name -> (entry, children).
    struct Node {
        entry: Option<Value>,
        children: BTreeMap<String, Node>,
    }

    impl Node {
        fn new() -> Self {
            Self {
                entry: None,
                children: BTreeMap::new(),
            }
        }
        /// Convert a directory node to a `{name, type, children}` JSON
        /// value. File nodes pass through their entry. Each child is
        /// converted using its own key as the name so nested directory
        /// labels stay distinct instead of inheriting the parent's
        /// segment.
        fn into_value(self, name: &str) -> Value {
            let children: Vec<Value> = self
                .children
                .into_iter()
                .map(|(child_name, child)| child.into_value(&child_name))
                .collect();
            if let Some(mut entry) = self.entry {
                if let Some(obj) = entry.as_object_mut() {
                    if !children.is_empty() {
                        obj.insert("children".to_string(), Value::Array(children));
                    }
                }
                entry
            } else {
                serde_json::json!({
                    "name": name,
                    "type": "directory",
                    "children": children,
                })
            }
        }
    }

    let mut root = Node::new();
    for file in files {
        let rel = file
            .get("relative_path")
            .and_then(|v| v.as_str())
            .or_else(|| file.get("path").and_then(|v| v.as_str()))
            .unwrap_or("?");
        // Normalize to forward slashes for stable tree building.
        let rel = rel.replace('\\', "/");
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        let mut node = &mut root;
        for (i, part) in parts.iter().enumerate() {
            let key = part.to_string();
            let is_file = i + 1 == parts.len();
            let child = node.children.entry(key.clone()).or_insert_with(Node::new);
            if is_file {
                let mut entry = file.clone();
                if let Some(obj) = entry.as_object_mut() {
                    obj.entry("name".to_string())
                        .or_insert(Value::String((*part).to_string()));
                    obj.entry("type".to_string())
                        .or_insert(Value::String("file".to_string()));
                }
                child.entry = Some(entry);
            }
            node = child;
        }
    }

    // The root is a virtual container with no name of its own — return
    // its top-level children directly so `render_tree` shows them as
    // siblings rather than nested under a fabricated "root" node.
    let mut top: Vec<Value> = Vec::new();
    for (name, child) in root.children.into_iter() {
        top.push(child.into_value(&name));
    }
    top
}

fn render_impact_risk(data: &Value, color: bool) -> String {
    let Some(risk) = data.get("risk_level").and_then(|v| v.as_str()) else {
        return String::new();
    };
    let (icon, risk_color) = if color {
        match risk.to_lowercase().as_str() {
            "high" => ("●", LIGHT_RED),
            "medium" => ("●", LIGHT_YELLOW),
            "low" => ("●", LIGHT_GREEN),
            _ => ("○", WHITE),
        }
    } else {
        ("●", "")
    };
    format!(
        "  {}Risk:{} {risk_color}{icon} {risk}{}\n",
        if color { BOLD } else { "" },
        if color { RESET } else { "" },
        if color { RESET } else { "" },
    )
}

fn render_impact_list(
    data: &Value,
    key: &str,
    title: &str,
    arrow: &str,
    item_color: &str,
    limit: usize,
    show_more: bool,
    color: bool,
) -> String {
    let Some(items) = data.get(key).and_then(|v| v.as_array()) else {
        return String::new();
    };
    if items.is_empty() {
        return String::new();
    }
    let (bold, dim, reset, line_color) = if color {
        (BOLD, DIM, RESET, item_color)
    } else {
        ("", "", "", "")
    };
    let mut out = format!("\n  {bold}{title} ({}):{reset}\n", items.len());
    for item in items.iter().take(limit) {
        let name = item
            .as_str()
            .or_else(|| item.get("name").and_then(|v| v.as_str()))
            .unwrap_or("?");
        out.push_str(&format!("    {line_color}{arrow} {name}{reset}\n"));
    }
    if show_more && items.len() > limit {
        out.push_str(&format!("    {dim}… {} more{reset}\n", items.len() - limit));
    }
    out
}

fn render_impact_counts(data: &Value, color: bool) -> String {
    let affected_files = data
        .get("transitive_affected_files")
        .and_then(|v| v.as_u64());
    let transitive_callers = data.get("transitive_callers").and_then(|v| v.as_u64());
    if affected_files.is_none() && transitive_callers.is_none() {
        return String::new();
    }

    let mut out = String::from("\n");
    if let Some(count) = affected_files {
        out.push_str(&field("Affected files", &count.to_string(), color));
    }
    if let Some(count) = transitive_callers {
        out.push_str(&field("Transitive callers", &count.to_string(), color));
    }
    out
}

fn render_impact(data: &Value, color: bool) -> String {
    let mut out = header("Impact Analysis", color);
    out.push('\n');
    if let Some(sym) = data.get("symbol").and_then(|v| v.as_str()) {
        out.push_str(&field("Symbol", sym, color));
    }
    if let Some(file) = data.get("file").and_then(|v| v.as_str()) {
        out.push_str(&field("File", file, color));
    }
    if let Some(ct) = data.get("change_type").and_then(|v| v.as_str()) {
        out.push_str(&field("Change type", ct, color));
    }
    out.push_str(&render_impact_risk(data, color));

    out.push_str(&render_impact_list(
        data,
        "direct_callers",
        "Direct callers",
        "←",
        LIGHT_CYAN,
        20,
        false,
        color,
    ));

    out.push_str(&render_impact_list(
        data,
        "transitive_affected_symbols",
        "Transitive affected symbols",
        "→",
        LIGHT_YELLOW,
        30,
        true,
        color,
    ));

    // Summary with numeric counts
    if let Some(s) = data.get("summary").and_then(|v| v.as_str()) {
        out.push('\n');
        out.push_str(&format!(
            "  {}Summary:{} {}\n",
            if color { BOLD } else { "" },
            if color { RESET } else { "" },
            s,
        ));
    }

    out.push_str(&render_impact_counts(data, color));

    out
}

fn render_symbol_lookup(data: &Value, color: bool) -> String {
    // `lookup_single_symbol` returns the shape (see
    // `src/cli/mcp/symbol_lookup_handler.rs::lookup_single_symbol`):
    //   { symbol, type, file, byte_range, complexity, language,
    //     callers, callees, impact_radius, [source] }
    // where each caller/callee entry is { name, file, type } (no
    // `line` field). Older renderers read `file_path` / `line` /
    // `symbol_type` / `signature` and end up emitting mostly blanks.
    //
    // Batch mode (`lookup_symbols_batch`) returns the wrapper
    //   { batch: true, count, results: [ ...singleSymbolEntries ] }
    // The previous renderer silently dropped the wrapper and
    // emitted a header followed by nothing when `symbol` /
    // `file` / `type` were absent at the top level. Branch on
    // `batch:true` and recurse into each entry.
    if data.get("batch").and_then(|v| v.as_bool()) == Some(true) {
        let count = data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut out = header("Symbol Lookup (batch)", color);
        out.push('\n');
        out.push_str(&field("Count", &count.to_string(), color));
        if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
            if arr.is_empty() {
                out.push_str("  (no results)\n");
                return out;
            }
            for (idx, entry) in arr.iter().enumerate() {
                out.push('\n');
                out.push_str(&format!(
                    "  {}{}#{}{} {}\n",
                    if color { BOLD } else { "" },
                    if color { DIM } else { "" },
                    idx + 1,
                    if color { RESET } else { "" },
                    entry.get("symbol").and_then(|v| v.as_str()).unwrap_or("?"),
                ));
                out.push_str(&render_symbol_lookup_single(entry, color));
            }
        }
        return out;
    }
    let mut out = header("Symbol Lookup", color);
    out.push('\n');
    out.push_str(&render_symbol_lookup_single(data, color));
    out
}

/// Render a single symbol entry (the inner shape returned by
/// `lookup_single_symbol`). Lifted out of `render_symbol_lookup` so
/// the batch wrapper can recurse into each entry.
fn render_symbol_source(data: &Value, color: bool) -> String {
    let Some(source) = data.get("source").and_then(|v| v.as_str()) else {
        return String::new();
    };

    let mut out = String::from("\n");
    let mut shown = 0usize;
    for line in source.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push_str(&format!(
            "      {}{}{}\n",
            if color { DIM } else { "" },
            truncate_chars(line, 160),
            if color { RESET } else { "" },
        ));
        shown += 1;
        if shown >= 12 {
            break;
        }
    }
    out
}

fn render_symbol_relationships(
    data: &Value,
    key: &str,
    truncated_key: &str,
    title: &str,
    arrow: &str,
    color: bool,
) -> String {
    let Some(entries) = data.get(key).and_then(|v| v.as_array()) else {
        return String::new();
    };
    if entries.is_empty() {
        return String::new();
    }

    let mut out = format!("\n  {title}:\n");
    for entry in entries.iter().take(50) {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let file = entry.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let typ = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!(
            "    {arrow} {}{}{} {}{}{}{}\n",
            if color { LIGHT_CYAN } else { "" },
            name,
            if color { RESET } else { "" },
            if color { DIM } else { "" },
            file,
            if typ.is_empty() {
                String::new()
            } else {
                format!(" [{}]", typ)
            },
            if color { RESET } else { "" },
        ));
    }
    if data
        .get(truncated_key)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        out.push_str(&format!(
            "    {}... (showing 50 or more){}\n",
            if color { DIM } else { "" },
            if color { RESET } else { "" },
        ));
    }
    out
}

fn render_symbol_lookup_single(data: &Value, color: bool) -> String {
    let mut out = String::new();
    if let Some(sym) = data.get("symbol").and_then(|v| v.as_str()) {
        out.push_str(&field("Symbol", sym, color));
    }
    if let Some(file) = data.get("file").and_then(|v| v.as_str()) {
        out.push_str(&field("File", file, color));
    }
    if let Some(typ) = data.get("type").and_then(|v| v.as_str()) {
        out.push_str(&field("Type", typ, color));
    }
    if let Some(lang) = data.get("language").and_then(|v| v.as_str()) {
        out.push_str(&field("Language", lang, color));
    }
    if let Some(br) = data.get("byte_range").and_then(|v| v.as_array()) {
        if br.len() == 2 {
            let start = br[0].as_u64().unwrap_or(0);
            let end = br[1].as_u64().unwrap_or(0);
            if end > start {
                out.push_str(&field("Range", &format!("bytes {}-{}", start, end), color));
            }
        }
    }
    if let Some(cx) = data.get("complexity").and_then(|v| v.as_u64()) {
        out.push_str(&field("Complexity", &cx.to_string(), color));
    }
    if let Some(ir) = data.get("impact_radius").and_then(|v| v.as_object()) {
        let syms = ir
            .get("affected_symbols")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let files = ir
            .get("affected_files")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        out.push_str(&field(
            "Impact",
            &format!("{} symbols / {} files", syms, files),
            color,
        ));
    }
    out.push_str(&render_symbol_source(data, color));

    out.push_str(&render_symbol_relationships(
        data,
        "callers",
        "callers_truncated",
        "Callers",
        "→",
        color,
    ));
    out.push_str(&render_symbol_relationships(
        data,
        "callees",
        "callees_truncated",
        "Callees",
        "←",
        color,
    ));
    out
}

fn render_phase_section(data: &Value, phase: u8, color: bool) -> String {
    let key = format!("phase{phase}");
    let Some(value) = data.get(&key) else {
        return String::new();
    };
    let (bold, dim, reset) = if color {
        (BOLD, DIM, RESET)
    } else {
        ("", "", "")
    };
    match phase {
        1 => {
            let files = value
                .get("total_files")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let signatures = value
                .get("signatures")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let label = if value
                .get("cache_hit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "cache hit"
            } else {
                "parsed"
            };
            format!(
                "\n  {bold}Phase 1:{reset} {files} files, {signatures} signatures ({label}){reset}\n"
            )
        }
        2 => {
            let internal = value
                .get("internal_import_edges")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let external = value
                .get("external_import_edges")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let unresolved = value
                .get("unresolved_modules")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!(
                "  {bold}Phase 2:{reset} {internal} internal, {external} external, {unresolved} unresolved modules{reset}\n"
            )
        }
        3 => {
            let entries = value
                .get("entry_points")
                .and_then(|v| v.as_array())
                .map(|items| items.len())
                .unwrap_or(0);
            let impacted = value
                .get("impacted_nodes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            format!(
                "  {bold}Phase 3:{reset} {entries} entry points, {impacted} impacted nodes{reset}\n"
            )
        }
        4 => {
            let hotspots = value
                .get("hotspots")
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            let mut out = format!("  {bold}Phase 4:{reset} {hotspots} hotspots{reset}\n");
            if let Some(items) = value.get("hotspots").and_then(|v| v.as_array()) {
                for (index, hotspot) in items.iter().take(5).enumerate() {
                    let name = hotspot
                        .get("node_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let score = hotspot.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let complexity = hotspot
                        .get("complexity")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    out.push_str(&format!(
                        "    {}. {name} {dim}(score: {score:.2}, complexity: {complexity}){reset}\n",
                        index + 1,
                    ));
                }
            }
            out
        }
        5 => {
            let recommendations = value
                .get("recommendations")
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            format!("  {bold}Phase 5:{reset} {recommendations} recommendations{reset}\n")
        }
        _ => String::new(),
    }
}

fn render_phase_formatted(data: &Value, color: bool) -> String {
    let Some(formatted) = data.get("formatted_output").and_then(|v| v.as_str()) else {
        return String::new();
    };
    if formatted.is_empty() {
        return String::new();
    }
    let (dim, reset) = if color { (DIM, RESET) } else { ("", "") };
    let mut out = String::from("\n");
    for line in truncate_chars(formatted, 2000).lines() {
        out.push_str(&format!("  {dim}{line}{reset}\n"));
    }
    out
}

fn render_phase(data: &Value, color: bool) -> String {
    let mut out = header("Phase Analysis", color);
    out.push('\n');

    // Show executed phases
    if let Some(ep) = data.get("executed_phases").and_then(|v| v.as_array()) {
        let nums: Vec<String> = ep
            .iter()
            .filter_map(|v| v.as_u64().map(|n| n.to_string()))
            .collect();
        if !nums.is_empty() {
            out.push_str(&field("Executed phases", &nums.join(", "), color));
        }
    }

    // Show cache hit status
    if let Some(ch) = data.get("cache_hit").and_then(|v| v.as_bool()) {
        out.push_str(&field(
            "Cache hit",
            if ch { "true" } else { "false" },
            color,
        ));
    }

    // Show generation
    if let Some(gen) = data.get("generation").and_then(|v| v.as_str()) {
        out.push_str(&field("Generation", gen, color));
    }

    out.push_str(&render_phase_section(data, 1, color));

    out.push_str(&render_phase_section(data, 2, color));

    out.push_str(&render_phase_section(data, 3, color));

    out.push_str(&render_phase_section(data, 4, color));

    out.push_str(&render_phase_section(data, 5, color));
    out.push_str(&render_phase_formatted(data, color));

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

/// Render a tool's value *without* ANSI color codes. Used by the MCP
/// transport to produce clean text for the LLM (the CLI uses the
/// colored `render_tool_output`).
pub fn render_tool_output_plain(name: &str, data: &Value, args: &Value) -> String {
    render_tool_output_with_color(name, data, args, false)
}

fn render_tool_output_with_color(name: &str, data: &Value, args: &Value, color: bool) -> String {
    let normalized = normalize_tool_name(name);
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let node_id = args.get("node_id").and_then(|v| v.as_str()).unwrap_or("");

    let mut rendered = match normalized.as_str() {
        "leindex_search" | "search" => render_search(data, query, color),
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
        rendered.push_str(&format!(
            "\nFreshness: status={status}, generation={generation}\n"
        ));
        if let Some(advisory) = advisory {
            rendered.push_str(&format!("Advisory: {advisory}\n"));
        }
    }
    rendered
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
