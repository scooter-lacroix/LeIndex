//! Per-tool CLI render functions and the central dispatcher.
//!
//! Each `render_*` function reads a JSON `Value` (the same data the
//! LLM sees) and produces a human-readable, colored string for the
//! CLI. They use shared helpers (`header`, `field`, `status_icon`,
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
        format!("  {}{}:{} {}{}{}\n", BOLD, name, RESET, LIGHT_CYAN, value, RESET)
    } else {
        format!("  {}: {}\n", name, value)
    }
}

fn status_icon(status: &str) -> (&'static str, &'static str) {
    match status {
        "completed" | "success" | "clean" => ("✓", LIGHT_GREEN),
        "failed" | "error" | "dirty" | "high" => ("✗", LIGHT_RED),
        "warning" | "medium" => ("⚠", LIGHT_YELLOW),
        "skipped" => ("○", DIM),
        "low" => ("·", LIGHT_GREEN),
        _ => ("•", WHITE),
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
    data.get("line").and_then(|v| v.as_u64()).unwrap_or(0)
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
        out.push_str(&render_tree_node(node, "", i == nodes.len() - 1, color, true));
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

    if is_root {
        out.push_str(&format!(
            "{}{}{}{}\n",
            name_color, name, reset, suffix(symbol_count, count_color, reset),
        ));
    } else {
        out.push_str(&format!(
            "{}{}{}{}{}{}{}{}\n",
            prefix,
            connector,
            name_color,
            name,
            reset,
            count_color,
            suffix(symbol_count, count_color, reset),
            reset,
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

fn render_search(data: &Value, query: &str, color: bool) -> String {
    let arr = extract_array(data, &["results", "items"]);
    if arr.is_empty() {
        return format!(
            "{}\n  No results for: {}\n",
            header(&format!("Search: \"{}\"", query), color),
            query,
        );
    }
    let mut out = header(&format!("Search: \"{}\" ({} results)", query, arr.len()), color);
    out.push('\n');
    for (idx, r) in arr.iter().enumerate() {
        let file = r.get("file_path").and_then(|v| v.as_str()).unwrap_or("?");
        let symbol = r
            .get("symbol")
            .or_else(|| r.get("symbol_name"))
            .and_then(|v| v.as_str());
        let symbol_type = r.get("symbol_type").and_then(|v| v.as_str());
        // The score is a `Score` struct with `overall`/`neural`/`text`/
        // `structural`; we want a single 0..1 composite for the CLI.
        let score = r
            .get("score")
            .and_then(|v| v.get("overall"))
            .and_then(|v| v.as_f64())
            .or_else(|| r.get("score").and_then(|v| v.as_f64()));
        let signature = r.get("signature").and_then(|v| v.as_str());
        let context = r.get("context").and_then(|v| v.as_str());
        // `trim_search` emits `snippet` (a one-line preview built from
        // `context` / `content` / `signature`). When the LLM is
        // looking at a trimmed payload we may not have `signature`
        // or `context` to fall back on, so read `snippet` directly.
        let snippet = r.get("snippet").and_then(|v| v.as_str());
        let byte_range = r.get("byte_range").and_then(|v| v.as_array());

        out.push_str(&format!(
            "  {}{}.{} {}",
            if color { BOLD } else { "" },
            idx + 1,
            if color { RESET } else { "" },
            if color { LIGHT_YELLOW } else { "" },
        ));
        out.push_str(file);
        out.push_str(if color { RESET } else { "" });

        if let Some(sym) = symbol {
            out.push_str(&format!(" :: {}{}{}",
                if color { LIGHT_CYAN } else { "" },
                sym,
                if color { RESET } else { "" },
            ));
        }

        if let Some(typ) = symbol_type {
            out.push_str(&format!(" {}[{}]{}",
                if color { DIM } else { "" },
                typ,
                if color { RESET } else { "" },
            ));
        }

        if let Some(sc) = score {
            let pct = (sc * 100.0).round() as usize;
            out.push_str(&format!("  {}{}%{}",
                if color { DIM } else { "" },
                pct,
                if color { RESET } else { "" }
            ));
        }
        out.push('\n');

        // Show the signature or first context line, whichever the handler
        // populated. Trim to keep the CLI output compact.
        if let Some(sig) = signature {
            let trimmed = sig.trim();
            if !trimmed.is_empty() {
                out.push_str(&format!("      {}{}{}\n",
                    if color { DIM } else { "" },
                    truncate_chars(trimmed, 160),
                    if color { RESET } else { "" },
                ));
            }
        } else if let Some(ctx) = context {
            let first = ctx.lines().next().unwrap_or("").trim();
            if !first.is_empty() {
                out.push_str(&format!("      {}{}{}\n",
                    if color { DIM } else { "" },
                    truncate_chars(first, 160),
                    if color { RESET } else { "" },
                ));
            }
        } else if let Some(snip) = snippet {
            // `snippet` is already a single short line; trim() once more
            // to be defensive against leading/trailing whitespace.
            let trimmed = snip.trim();
            if !trimmed.is_empty() {
                out.push_str(&format!("      {}{}{}\n",
                    if color { DIM } else { "" },
                    truncate_chars(trimmed, 160),
                    if color { RESET } else { "" },
                ));
            }
        }
        // Surface byte ranges when no signature/context/snippet is
        // available — helps the user locate the hit in a very large file.
        if signature.is_none() && context.is_none() && snippet.is_none() {
            if let Some(br) = byte_range {
                if br.len() == 2 {
                    let start = br[0].as_u64().unwrap_or(0);
                    let end = br[1].as_u64().unwrap_or(0);
                    if end > start {
                        out.push_str(&format!("      {}(bytes {}-{}){}\n",
                            if color { DIM } else { "" },
                            start, end,
                            if color { RESET } else { "" },
                        ));
                    }
                }
            }
        }
    }
    out
}

fn render_context(data: &Value, node_id: &str, color: bool) -> String {
    // Context output is structurally similar to a search hit: show the
    // node and the surrounding code with line numbers.
    let mut out = header(&format!("Context: {}", node_id), color);
    out.push('\n');
    if let Some(symbol) = data.get("symbol").and_then(|v| v.as_str()) {
        out.push_str(&field("Symbol", symbol, color));
    }
    if let Some(file) = data.get("file_path").and_then(|v| v.as_str()) {
        out.push_str(&field("File", file, color));
    }
    if let Some(typ) = data.get("symbol_type").and_then(|v| v.as_str()) {
        out.push_str(&field("Type", typ, color));
    }
    if let Some(line) = data.get("line").and_then(|v| v.as_u64()) {
        out.push_str(&field("Line", &line.to_string(), color));
    }
    if let Some(snippet) = data.get("content").and_then(|v| v.as_str()) {
        out.push('\n');
        for (i, l) in snippet.lines().enumerate() {
            let n = line_for(data).saturating_add(i as u64);
            let gutter = format!("{:>4}", n);
            out.push_str(&format!(
                "  {}{}{}│ {}\n",
                if color { DIM } else { "" },
                gutter,
                if color { RESET } else { "" },
                l,
            ));
        }
    } else if let Some(results) = data.get("results").and_then(|v| v.as_array()) {
        for r in results {
            let symbol = r.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
            let file = r.get("file_path").and_then(|v| v.as_str()).unwrap_or("?");
            out.push_str(&format!("  → {}{}{} {}{}{}\n",
                if color { LIGHT_CYAN } else { "" }, symbol, if color { RESET } else { "" },
                if color { DIM } else { "" }, file, if color { RESET } else { "" }));
        }
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
    if let Some(v) = data.get("stale").and_then(|v| v.as_bool()) {
        out.push_str(&field("Stale", &v.to_string(), color));
    }
    if let Some(v) = data.get("last_indexed_secs_ago").and_then(|v| v.as_u64()) {
        out.push_str(&field("Last indexed", &format!("{}s ago", v), color));
    }
    if let Some(arr) = data.get("issues").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str("  Issues:\n");
            for issue in arr.iter().take(10) {
                let sev = issue
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info");
                let msg = issue
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let sev_color = if color {
                    match sev {
                        "error" => LIGHT_RED,
                        "warning" => LIGHT_YELLOW,
                        _ => LIGHT_BLUE,
                    }
                } else {
                    ""
                };
                out.push_str(&format!(
                    "    {}{}{} {}{}{}\n",
                    sev_color, sev, if color { RESET } else { "" }, msg, "",
                    "",
                ));
            }
        }
    }
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
        let cx = f.get("total_complexity").and_then(|v| v.as_u64()).unwrap_or(0);
        let deps = f.get("incoming_dependencies").and_then(|v| v.as_u64()).unwrap_or(0)
            + f.get("outgoing_dependencies").and_then(|v| v.as_u64()).unwrap_or(0);
        let cx_label = if color {
            match cx {
                0..=20 => (LIGHT_GREEN, "low"),
                21..=60 => (LIGHT_YELLOW, "med"),
                _ => (LIGHT_RED, "high"),
            }
        } else {
            ("", "low")
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
            cx_label.0,
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

fn render_impact(data: &Value, color: bool) -> String {
    let mut out = header("Impact Analysis", color);
    out.push('\n');
    if let Some(sym) = data.get("symbol").and_then(|v| v.as_str()) {
        out.push_str(&field("Symbol", sym, color));
    }
    if let Some(risk) = data.get("risk_level").and_then(|v| v.as_str()) {
        let (icon, c) = if color {
            match risk.to_lowercase().as_str() {
                "high" => ("●", LIGHT_RED),
                "medium" => ("●", LIGHT_YELLOW),
                "low" => ("●", LIGHT_GREEN),
                _ => ("○", WHITE),
            }
        } else {
            ("●", "")
        };
        out.push_str(&format!(
            "  {}{}:{} {}{} {}{}\n",
            if color { BOLD } else { "" },
            "Risk",
            if color { RESET } else { "" },
            c,
            icon,
            risk,
            if color { RESET } else { "" },
        ));
    }
    render_impact_side(data, "forward_impact", "Forward (callers of callees)", "→", color, &mut out);
    render_impact_side(data, "backward_impact", "Backward (what calls this)", "←", color, &mut out);
    out
}

fn render_impact_side(
    data: &Value,
    key: &str,
    label: &str,
    arrow: &str,
    color: bool,
    out: &mut String,
) {
    if let Some(arr) = data.get(key).and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str(&format!("  {}:\n", label));
            for item in arr.iter().take(20) {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let file = item.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let line = item.get("line").and_then(|v| v.as_u64());
                out.push_str(&format!(
                    "    {}{}{} {}{}{}{}{}\n",
                    if color { LIGHT_CYAN } else { "" },
                    arrow,
                    name,
                    if color { RESET } else { "" },
                    if color { DIM } else { "" },
                    file,
                    line.map(|l| format!(":{}", l)).unwrap_or_default(),
                    if color { RESET } else { "" },
                ));
            }
        }
    }
}

fn render_symbol_lookup(data: &Value, color: bool) -> String {
    let mut out = header("Symbol Lookup", color);
    out.push('\n');
    if let Some(sym) = data.get("symbol").and_then(|v| v.as_str()) {
        out.push_str(&field("Symbol", sym, color));
    }
    if let Some(file) = data.get("file_path").and_then(|v| v.as_str()) {
        out.push_str(&field("File", file, color));
    }
    if let Some(line) = data.get("line").and_then(|v| v.as_u64()) {
        out.push_str(&field("Line", &line.to_string(), color));
    }
    if let Some(typ) = data.get("symbol_type").and_then(|v| v.as_str()) {
        out.push_str(&field("Type", typ, color));
    }
    if let Some(sig) = data.get("signature").and_then(|v| v.as_str()) {
        out.push_str(&field("Signature", sig, color));
    }

    if let Some(arr) = data.get("callers").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str("  Callers:\n");
            for c in arr.iter().take(15) {
                let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let file = c.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let line = c.get("line").and_then(|v| v.as_u64());
                out.push_str(&format!(
                    "    → {}{}{} {}{}{}{}\n",
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                    if color { DIM } else { "" },
                    file,
                    line.map(|l| format!(":{}", l)).unwrap_or_default(),
                    if color { RESET } else { "" },
                ));
            }
        }
    }
    if let Some(arr) = data.get("callees").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str("  Callees:\n");
            for c in arr.iter().take(15) {
                let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let file = c.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let line = c.get("line").and_then(|v| v.as_u64());
                out.push_str(&format!(
                    "    ← {}{}{} {}{}{}{}\n",
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                    if color { DIM } else { "" },
                    file,
                    line.map(|l| format!(":{}", l)).unwrap_or_default(),
                    if color { RESET } else { "" },
                ));
            }
        }
    }
    out
}

fn render_phase(data: &Value, color: bool) -> String {
    let mut out = header("Phase Analysis", color);
    out.push('\n');
    if let Some(mode) = data.get("mode").and_then(|v| v.as_str()) {
        out.push_str(&field("Mode", mode, color));
    }
    if let Some(arr) = data.get("phases").and_then(|v| v.as_array()) {
        out.push('\n');
        for p in arr {
            let num = p.get("phase").and_then(|v| v.as_u64()).unwrap_or(0);
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let findings = p.get("findings").and_then(|v| v.as_u64()).unwrap_or(0);
            let (icon, c) = if color {
                status_icon(status)
            } else {
                (status_icon(status).0, "")
            };
            out.push_str(&format!(
                "  {}{}{} {}{}Phase {}{} {} {}{}({} findings){}\n",
                c,
                icon,
                if color { RESET } else { "" },
                if color { BOLD } else { "" },
                if color { RESET } else { "" },
                num,
                if color { RESET } else { "" },
                name,
                if color { DIM } else { "" },
                "",
                findings,
                if color { RESET } else { "" },
            ));
        }
    }
    if let Some(summary) = data.get("summary").and_then(|v| v.as_str()) {
        out.push('\n');
        let truncated = truncate_chars(summary, 300);
        out.push_str(&format!(
            "  {}{}:{} {}{}{}\n",
            if color { BOLD } else { "" },
            "Summary",
            if color { RESET } else { "" },
            if color { DIM } else { "" },
            truncated,
            if color { RESET } else { "" },
        ));
    }
    out
}

fn render_git_status(data: &Value, color: bool) -> String {
    let mut out = header("Git Status", color);
    out.push('\n');
    if let Some(b) = data.get("branch").and_then(|v| v.as_str()) {
        out.push_str(&field("Branch", b, color));
    }
    if let Some(s) = data.get("status").and_then(|v| v.as_str()) {
        let c = if color {
            match s {
                "clean" => LIGHT_GREEN,
                "dirty" => LIGHT_YELLOW,
                _ => WHITE,
            }
        } else {
            ""
        };
        out.push_str(&format!(
            "  {}{}:{} {}{}{}{}\n",
            if color { BOLD } else { "" },
            "Status",
            if color { RESET } else { "" },
            c,
            s,
            if color { RESET } else { "" },
            "",
        ));
    }
    git_status_section(data, "staged", "Staged", "+", LIGHT_GREEN, color, &mut out);
    git_status_section(data, "modified", "Modified", "~", LIGHT_YELLOW, color, &mut out);
    git_status_section(data, "untracked", "Untracked", "?", LIGHT_GREY, color, &mut out);
    git_status_section(data, "deleted", "Deleted", "✗", LIGHT_RED, color, &mut out);
    out
}

fn git_status_section(
    data: &Value,
    key: &str,
    label: &str,
    marker: &str,
    marker_color: &str,
    color: bool,
    out: &mut String,
) {
    if let Some(arr) = data.get(key).and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str(&format!("  {}:\n", label));
            for f in arr.iter().take(20) {
                let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                out.push_str(&format!(
                    "    {}{}{} {}\n",
                    if color { marker_color } else { "" },
                    marker,
                    if color { RESET } else { "" },
                    path,
                ));
            }
        }
    }
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
    if let Some(size) = data.get("size").and_then(|v| v.as_u64()) {
        out.push_str(&field("Size", &format!("{} bytes", size), color));
    }
    if let Some(complexity) = data.get("complexity").and_then(|v| v.as_u64()) {
        let (label, c) = if color {
            match complexity {
                0..=5 => ("Low", LIGHT_GREEN),
                6..=15 => ("Medium", LIGHT_YELLOW),
                _ => ("High", LIGHT_RED),
            }
        } else {
            ("", "")
        };
        out.push_str(&format!(
            "  {}{}:{} {}{} {}{}{}\n",
            if color { BOLD } else { "" },
            "Complexity",
            if color { RESET } else { "" },
            c,
            complexity,
            if color { RESET } else { "" },
            label,
            if color { RESET } else { "" },
        ));
    }
    if let Some(arr) = data.get("symbols").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str("  Symbols:\n");
            for sym in arr.iter().take(20) {
                let name = sym.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let typ = sym.get("type").and_then(|v| v.as_str()).unwrap_or("symbol");
                let line = sym.get("line").and_then(|v| v.as_u64());
                let (icon, c) = if color {
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
                    "    {}{}{} {}{}{}{}{}\n",
                    c,
                    icon,
                    if color { RESET } else { "" },
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                    line.map(|l| format!(" :{}", l)).unwrap_or_default(),
                    "",
                ));
            }
        }
    }
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

// =============================================================================
// Central dispatch — single entry point for CLI tool rendering
// =============================================================================

/// Render a tool's value for the CLI surface. The MCP transport uses
/// the raw `Value` (clean JSON for the LLM); the CLI uses this function
/// to produce a human-readable, colored view of the same data.
pub fn render_tool_output(name: &str, data: &Value, args: &Value) -> String {
    let normalized = normalize_tool_name(name);
    let color = true;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match normalized.as_str() {
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
        "leindex_edit_preview" | "edit_preview" => render_diff_value(data, color),
        "leindex_edit_apply" | "edit_apply" => render_diff_value(data, color),
        "leindex_write" | "write" => render_diff_value(data, color),
        "leindex_rename_symbol" | "rename_symbol" => render_diff_value(data, color),
        _ => render_default(data, color),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::output::trim::trim_search;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn test_render_tree_basic() {
        let tree = v(r#"[
            {"name": "src", "type": "directory", "children": [
                {"name": "main.rs", "type": "file", "symbol_count": 5, "children": []},
                {"name": "lib.rs", "type": "file", "symbol_count": 12, "children": []}
            ]}
        ]"#);
        let s = render_tree(tree.as_array().unwrap(), false);
        assert!(s.contains("src"), "root dir name missing: {}", s);
        assert!(s.contains("main.rs"), "child file missing: {}", s);
        assert!(s.contains("lib.rs"), "child file missing: {}", s);
        // Directory connector at root
        assert!(s.contains("├──"), "missing connector: {}", s);
    }

    #[test]
    fn test_build_tree_preserves_child_names() {
        // Regression: each nested directory's `name` field must equal
        // its own path segment, not the parent's. Otherwise the tree
        // renderer shows duplicated labels like "src → src → main.rs"
        // for any multi-level path.
        let files = v(r#"[
            {"relative_path": "src/cli/main.rs"},
            {"relative_path": "src/cli/sub/lib.rs"},
            {"relative_path": "tests/integration.rs"}
        ]"#);
        let tree = build_tree_from_files(files.as_array().unwrap());
        // The top-level must be src + tests.
        let names: Vec<String> = tree
            .iter()
            .map(|n| n.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string())
            .collect();
        assert_eq!(names, vec!["src", "tests"]);
        // Inside `src`, the child directory must be `cli` (not `src`).
        let src = tree.iter().find(|n| n["name"] == "src").unwrap();
        let src_children = src.get("children").and_then(|v| v.as_array()).unwrap();
        let src_child_names: Vec<String> = src_children
            .iter()
            .map(|n| n.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string())
            .collect();
        assert_eq!(src_child_names, vec!["cli"]);
        // Inside `cli`, the grandchild directory must be `sub`.
        let cli = src_children
            .iter()
            .find(|n| n["name"] == "cli")
            .unwrap();
        let cli_children = cli.get("children").and_then(|v| v.as_array()).unwrap();
        let cli_child_names: Vec<String> = cli_children
            .iter()
            .map(|n| n.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string())
            .collect();
        assert_eq!(cli_child_names, vec!["main.rs", "sub"]);
    }

    #[test]
    fn test_render_tree_indents_children() {
        let tree = v(r#"[
            {"name": "src", "type": "directory", "children": [
                {"name": "a.rs", "type": "file", "symbol_count": 1, "children": []}
            ]},
            {"name": "tests", "type": "directory", "children": [
                {"name": "b.rs", "type": "file", "symbol_count": 1, "children": []}
            ]}
        ]"#);
        let s = render_tree(tree.as_array().unwrap(), false);
        // Both files appear, one per root child.
        assert!(s.contains("a.rs"));
        assert!(s.contains("b.rs"));
        // `src` is the first root child so it has a `│` continuation
        // (its sibling follows), and `tests` is the last so it has a
        // trailing space indent.
        let lines: Vec<&str> = s.lines().collect();
        // The first file line should sit under `src` with a `│` prefix.
        let a_line = lines.iter().find(|l| l.contains("a.rs")).unwrap();
        assert!(a_line.contains("│   └──"), "a.rs should be under 'src' with continuation: {:?}", a_line);
        // The second file line should sit under `tests` with a space
        // prefix (no continuation since tests is the last root child).
        let b_line = lines.iter().find(|l| l.contains("b.rs")).unwrap();
        assert!(b_line.starts_with("    └──"), "b.rs should be under 'tests' with space indent: {:?}", b_line);
    }

    #[test]
    fn test_render_tool_output_dispatches_by_name() {
        let args = v(r#"{"query": "foo", "top_k": 1}"#);
        // Search payload (using trimmed form so the renderer sees what
        // the LLM would see).
        let search_data = trim_search(&v(r#"{"results": [{"file_path": "/p.rs", "symbol_name": "f", "score": {"overall": 0.5}}]}"#));
        let s = render_tool_output("leindex.search", &search_data, &args);
        assert!(s.contains("Search: \"foo\""), "got: {}", s);
        assert!(s.contains("/p.rs"), "got: {}", s);
    }

    #[test]
    fn test_render_search_uses_snippet_field() {
        // Regression: when the trimmed payload only has `snippet` (no
        // `signature` or `context`), the search renderer should still
        // surface a preview line.
        let args = v(r#"{"query": "foo", "top_k": 1}"#);
        let payload = v(r#"{
            "count": 1,
            "results": [{
                "file_path": "/p.rs",
                "symbol": "main",
                "symbol_type": "function",
                "score": 0.9,
                "snippet": "fn main() { return 0; }"
            }]
        }"#);
        let s = render_tool_output("leindex.search", &payload, &args);
        assert!(s.contains("/p.rs"), "got: {}", s);
        assert!(s.contains("fn main()"), "snippet preview missing: {}", s);
    }

    #[test]
    fn test_render_tool_output_falls_back_to_pretty_json() {
        let data = v(r#"{"custom_field": 42, "items": [1, 2, 3]}"#);
        let s = render_tool_output("leindex.unknown_tool", &data, &Value::Null);
        // Default renderer emits pretty JSON
        assert!(s.contains("\"custom_field\""));
        assert!(s.contains("42"));
    }
}
