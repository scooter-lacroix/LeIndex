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
        format!(
            "  {}{}:{} {}{}{}\n",
            BOLD, name, RESET, LIGHT_CYAN, value, RESET
        )
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
        let line_number = r.get("line_number").and_then(|v| v.as_u64());

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
            out.push_str(&format!(
                " :: {}{}{}",
                if color { LIGHT_CYAN } else { "" },
                sym,
                if color { RESET } else { "" },
            ));
        }

        if let Some(typ) = symbol_type {
            out.push_str(&format!(
                " {}[{}]{}",
                if color { DIM } else { "" },
                typ,
                if color { RESET } else { "" },
            ));
        }

        if let Some(ln) = line_number {
            out.push_str(&format!(
                " {}:{}{}",
                if color { DIM } else { "" },
                ln,
                if color { RESET } else { "" },
            ));
        }

        if let Some(sc) = score {
            let pct = (sc * 100.0).round() as usize;
            out.push_str(&format!(
                "  {}{}%{}",
                if color { DIM } else { "" },
                pct,
                if color { RESET } else { "" }
            ));
        }
        out.push('\n');

        // Show the signature or first context line, whichever the handler
        // populated. Trim to keep the CLI output compact. The fallback
        // chain is signature → context → snippet, but a populated-but-
        // empty (whitespace-only) `signature` must not block the
        // fallback to `context` or `snippet`. We track a `printed`
        // flag so an empty signature falls through, and so the byte-
        // range hint only appears when none of the three text sources
        // produced output.
        let mut printed = false;
        if let Some(sig) = signature {
            let trimmed = sig.trim();
            if !trimmed.is_empty() {
                out.push_str(&format!(
                    "      {}{}{}\n",
                    if color { DIM } else { "" },
                    truncate_chars(trimmed, 160),
                    if color { RESET } else { "" },
                ));
                printed = true;
            }
        }
        if !printed {
            if let Some(ctx) = context {
                let first = ctx.lines().next().unwrap_or("").trim();
                if !first.is_empty() {
                    out.push_str(&format!(
                        "      {}{}{}\n",
                        if color { DIM } else { "" },
                        truncate_chars(first, 160),
                        if color { RESET } else { "" },
                    ));
                    printed = true;
                }
            }
        }
        if !printed {
            if let Some(snip) = snippet {
                // `snippet` is already a single short line; trim()
                // once more to be defensive against leading/trailing
                // whitespace.
                let trimmed = snip.trim();
                if !trimmed.is_empty() {
                    out.push_str(&format!(
                        "      {}{}{}\n",
                        if color { DIM } else { "" },
                        truncate_chars(trimmed, 160),
                        if color { RESET } else { "" },
                    ));
                    printed = true;
                }
            }
        }
        // Surface byte ranges when no signature/context/snippet is
        // available — helps the user locate the hit in a very large
        // file. The check is "did we print anything?" rather than
        // "are all three fields None?" so an empty `signature` falls
        // through to a real `context` or `snippet`, and only emits
        // the byte-range hint when there is genuinely no text.
        if !printed {
            if let Some(br) = byte_range {
                if br.len() == 2 {
                    let start = br[0].as_u64().unwrap_or(0);
                    let end = br[1].as_u64().unwrap_or(0);
                    if end > start {
                        out.push_str(&format!(
                            "      {}(bytes {}-{}){}\n",
                            if color { DIM } else { "" },
                            start,
                            end,
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
    // Pull node metadata from `results[0]` (the canonical anchor
    // node) when present. Fall back to top-level fields for the
    // legacy flat shape.
    let anchor = data
        .get("results")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());
    if let Some(sym) = anchor
        .and_then(|r| r.get("symbol_name"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("symbol").and_then(|v| v.as_str()))
    {
        out.push_str(&field("Symbol", sym, color));
    }
    if let Some(file) = anchor
        .and_then(|r| r.get("file_path"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("file_path").and_then(|v| v.as_str()))
    {
        out.push_str(&field("File", file, color));
    }
    if let Some(typ) = anchor
        .and_then(|r| r.get("symbol_type"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("symbol_type").and_then(|v| v.as_str()))
    {
        out.push_str(&field("Type", typ, color));
    }
    // `Line` field: only emit when we have a real source-line
    // number. The top-level `line` field (legacy flat shape) is
    // authoritative. The anchor's `byte_range[0]` is a byte offset
    // (not a line) so we must NOT surface it as a line number here
    // — see `line_for`. When the canonical shape is in play and no
    // real line is available, drop the field; the gutter will
    // number the snippet starting at 1.
    if let Some(line) = data.get("line").and_then(|v| v.as_u64()) {
        out.push_str(&field("Line", &line.to_string(), color));
    } else if let Some(br) = anchor
        .and_then(|r| r.get("byte_range"))
        .and_then(|v| v.as_array())
    {
        // Show the byte range as a "Range: bytes X-Y" hint so the
        // user still has a concrete pointer to the location, but
        // don't mislabel a byte offset as a line number.
        if br.len() == 2 {
            let start = br[0].as_u64().unwrap_or(0);
            let end = br[1].as_u64().unwrap_or(0);
            if end > start {
                out.push_str(&field("Range", &format!("bytes {}-{}", start, end), color));
            }
        }
    }
    // The expanded PDG body is in `context` (canonical) or
    // `content` (legacy flat shape). Either way it is a multi-line
    // string; we render it with a line-number gutter. When a real
    // source-line number is available the gutter starts at that
    // line; otherwise it starts at 1 (relative to the snippet) so
    // the user does not see a byte offset mislabelled as a line.
    let body = data
        .get("context")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("content").and_then(|v| v.as_str()));
    if let Some(snippet) = body {
        out.push('\n');
        let base = line_for(data);
        let gutter_base = if base == 0 { 1 } else { base };
        for (i, l) in snippet.lines().enumerate() {
            let n = gutter_base.saturating_add(i as u64);
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
            let symbol = r.get("symbol_name").and_then(|v| v.as_str()).unwrap_or("?");
            let file = r.get("file_path").and_then(|v| v.as_str()).unwrap_or("?");
            out.push_str(&format!(
                "  → {}{}{} {}{}{}\n",
                if color { LIGHT_CYAN } else { "" },
                symbol,
                if color { RESET } else { "" },
                if color { DIM } else { "" },
                file,
                if color { RESET } else { "" }
            ));
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
    // System health section
    if let Some(sh) = data.get("system_health") {
        out.push('\n');
        out.push_str("  System Health:\n");
        if let Some(v) = sh.get("index_health").and_then(|v| v.as_str()) {
            out.push_str(&field("  Index health", v, color));
        }
        if let Some(v) = sh.get("pdg_loaded").and_then(|v| v.as_bool()) {
            out.push_str(&field("  PDG loaded", &v.to_string(), color));
        }
        if let Some(v) = sh.get("pdg_nodes").and_then(|v| v.as_u64()) {
            out.push_str(&field("  PDG nodes", &v.to_string(), color));
        }
        if let Some(v) = sh.get("pdg_edges").and_then(|v| v.as_u64()) {
            out.push_str(&field("  PDG edges", &v.to_string(), color));
        }
        if let Some(v) = sh.get("search_index_nodes").and_then(|v| v.as_u64()) {
            out.push_str(&field("  Search nodes", &v.to_string(), color));
        }
        if let Some(v) = sh.get("embedding_model").and_then(|v| v.as_str()) {
            out.push_str(&field("  Embedding model", v, color));
        }
        if let Some(v) = sh.get("total_signatures").and_then(|v| v.as_u64()) {
            out.push_str(&field("  Total signatures", &v.to_string(), color));
        }
        if let Some(v) = sh.get("failed_parses").and_then(|v| v.as_u64()) {
            out.push_str(&field("  Failed parses", &v.to_string(), color));
        }
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
                let msg = issue.get("message").and_then(|v| v.as_str()).unwrap_or("?");
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
                    sev_color,
                    sev,
                    if color { RESET } else { "" },
                    msg,
                    "",
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

    // Direct callers
    if let Some(arr) = data.get("direct_callers").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str(&format!(
                "  {}Direct callers ({}):{}\n",
                if color { BOLD } else { "" },
                arr.len(),
                if color { RESET } else { "" },
            ));
            for item in arr.iter().take(20) {
                let name = item
                    .as_str()
                    .unwrap_or_else(|| item.get("name").and_then(|v| v.as_str()).unwrap_or("?"));
                out.push_str(&format!(
                    "    {}← {}{}\n",
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                ));
            }
        }
    }

    // Transitive affected symbols
    if let Some(arr) = data
        .get("transitive_affected_symbols")
        .and_then(|v| v.as_array())
    {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str(&format!(
                "  {}Transitive affected symbols ({}):{}\n",
                if color { BOLD } else { "" },
                arr.len(),
                if color { RESET } else { "" },
            ));
            for item in arr.iter().take(30) {
                let name = item
                    .as_str()
                    .unwrap_or_else(|| item.get("name").and_then(|v| v.as_str()).unwrap_or("?"));
                out.push_str(&format!(
                    "    {}→ {}{}\n",
                    if color { LIGHT_YELLOW } else { "" },
                    name,
                    if color { RESET } else { "" },
                ));
            }
            if arr.len() > 30 {
                out.push_str(&format!(
                    "    {}… {} more{}\n",
                    if color { DIM } else { "" },
                    arr.len() - 30,
                    if color { RESET } else { "" },
                ));
            }
        }
    }

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

    // Numeric counts from handler fields
    let affected_files = data
        .get("transitive_affected_files")
        .and_then(|v| v.as_u64());
    let transitive_callers = data.get("transitive_callers").and_then(|v| v.as_u64());
    if affected_files.is_some() || transitive_callers.is_some() {
        out.push('\n');
        if let Some(af) = affected_files {
            out.push_str(&field("Affected files", &af.to_string(), color));
        }
        if let Some(tc) = transitive_callers {
            out.push_str(&field("Transitive callers", &tc.to_string(), color));
        }
    }

    out
}

#[allow(dead_code)]
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
    if let Some(src) = data.get("source").and_then(|v| v.as_str()) {
        // `lookup_single_symbol` already truncates to `char_budget/2`
        // chars; render the first 12 non-empty lines as a preview.
        out.push('\n');
        let mut shown = 0usize;
        for l in src.lines() {
            if l.trim().is_empty() {
                continue;
            }
            out.push_str(&format!(
                "      {}{}{}\n",
                if color { DIM } else { "" },
                truncate_chars(l, 160),
                if color { RESET } else { "" },
            ));
            shown += 1;
            if shown >= 12 {
                break;
            }
        }
    }

    if let Some(arr) = data.get("callers").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str("  Callers:\n");
            for c in arr.iter().take(50) {
                let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let file = c.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let typ = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
                // 7 placeholders, 7 args. The previous format string
                // had 8 `{}`s with 8 args, where the trailing empty
                // string was silently emitted as empty and could
                // mask a future placeholder/argument mismatch.
                out.push_str(&format!(
                    "    → {}{}{} {}{}{}{}\n",
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                    if color { DIM } else { "" },
                    file,
                    if !typ.is_empty() {
                        format!(" [{}]", typ)
                    } else {
                        String::new()
                    },
                    if color { RESET } else { "" },
                ));
            }
            if data
                .get("callers_truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                out.push_str(&format!(
                    "    {}... (showing 50 of more){}\n",
                    if color { DIM } else { "" },
                    if color { RESET } else { "" },
                ));
            }
        }
    }
    if let Some(arr) = data.get("callees").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str("  Callees:\n");
            for c in arr.iter().take(50) {
                let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let file = c.get("file").and_then(|v| v.as_str()).unwrap_or("");
                let typ = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!(
                    "    ← {}{}{} {}{}{}{}\n",
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                    if color { DIM } else { "" },
                    file,
                    if !typ.is_empty() {
                        format!(" [{}]", typ)
                    } else {
                        String::new()
                    },
                    if color { RESET } else { "" },
                ));
            }
            if data
                .get("callees_truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                out.push_str(&format!(
                    "    {}... (showing 50 of more){}\n",
                    if color { DIM } else { "" },
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

    // Summary counts
    if let Some(s) = data.get("summary").and_then(|v| v.as_object()) {
        let modified = s.get("modified").and_then(|v| v.as_u64()).unwrap_or(0);
        let staged = s.get("staged").and_then(|v| v.as_u64()).unwrap_or(0);
        let untracked = s.get("untracked").and_then(|v| v.as_u64()).unwrap_or(0);
        if modified > 0 || staged > 0 || untracked > 0 {
            out.push_str(&format!(
                "  {}{} modified, {} staged, {} untracked{}\n",
                if color { DIM } else { "" },
                modified,
                staged,
                untracked,
                if color { RESET } else { "" },
            ));
        }
    }

    // File lists — handler returns string arrays under
    // modified_files / staged_files / untracked_files
    git_status_file_list(
        data,
        "modified_files",
        "Modified",
        "~",
        LIGHT_YELLOW,
        color,
        &mut out,
    );
    git_status_file_list(
        data,
        "staged_files",
        "Staged",
        "+",
        LIGHT_GREEN,
        color,
        &mut out,
    );
    git_status_file_list(
        data,
        "untracked_files",
        "Untracked",
        "?",
        LIGHT_GREY,
        color,
        &mut out,
    );

    // Changed symbols with structural impact
    if let Some(arr) = data.get("changed_symbols").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str(&format!(
                "  {}Changed Symbols:{}\n",
                if color { BOLD } else { "" },
                if color { RESET } else { "" },
            ));
            for entry in arr.iter().take(20) {
                let file = entry.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                let status = entry
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("modified");
                let symbols = entry
                    .get("symbols")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                out.push_str(&format!(
                    "    {}{}{} {}({}){}\n",
                    if color { LIGHT_YELLOW } else { "" },
                    file,
                    if color { RESET } else { "" },
                    if color { DIM } else { "" },
                    status,
                    if color { RESET } else { "" },
                ));
                for sym in symbols.iter().take(10) {
                    let name = sym.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let typ = sym.get("type").and_then(|v| v.as_str()).unwrap_or("symbol");
                    let caller_count = sym
                        .get("caller_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let impact = sym
                        .get("forward_impact_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let (icon, ic) = if color {
                        match typ {
                            "function" | "fn" => ("ƒ", LIGHT_GREEN),
                            "method" => ("m", LIGHT_CYAN),
                            "struct" => ("S", LIGHT_MAGENTA),
                            _ => ("•", WHITE),
                        }
                    } else {
                        ("•", "")
                    };
                    out.push_str(&format!(
                        "      {}{}{} {}{}{} {}{} callers, {} impact{}\n",
                        ic,
                        icon,
                        if color { RESET } else { "" },
                        if color { LIGHT_CYAN } else { "" },
                        name,
                        if color { RESET } else { "" },
                        if color { DIM } else { "" },
                        caller_count,
                        impact,
                        if color { RESET } else { "" },
                    ));
                }
                if symbols.is_empty() {
                    out.push_str(&format!(
                        "      {}No indexed symbols{}\n",
                        if color { DIM } else { "" },
                        if color { RESET } else { "" },
                    ));
                }
            }
        }
    }

    // Impact summary
    if let Some(imp) = data.get("impact_summary").and_then(|v| v.as_object()) {
        let total = imp
            .get("total_affected_symbols")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let affected = imp
            .get("affected_files")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if total > 0 {
            out.push('\n');
            out.push_str(&format!(
                "  {}Impact:{} {} affected symbols across {} files\n",
                if color { BOLD } else { "" },
                if color { RESET } else { "" },
                total,
                affected,
            ));
        }
    }

    // Show PDG enrichment status so the LLM knows whether structural
    // analysis was available (VAL-TRANSPORT-008).
    if let Some(pdg) = data.get("pdg_enrichment") {
        let available = pdg
            .get("available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (icon, icon_color) = if available {
            ("✓", LIGHT_GREEN)
        } else {
            ("⚠", LIGHT_YELLOW)
        };
        let status_text = if available {
            "available"
        } else {
            "unavailable"
        };
        out.push('\n');
        out.push_str(&format!(
            "  {}PDG Enrichment:{} {}{}{} {}\n",
            if color { BOLD } else { "" },
            if color { RESET } else { "" },
            if color { icon_color } else { "" },
            icon,
            if color { RESET } else { "" },
            status_text,
        ));
        if !available {
            if let Some(reason) = pdg.get("reason").and_then(|v| v.as_str()) {
                out.push_str(&format!(
                    "    {}Reason:{} {}\n",
                    if color { DIM } else { "" },
                    if color { RESET } else { "" },
                    reason,
                ));
            }
        }
    }

    out
}

/// Render a file list section for git status. The handler returns
/// arrays of strings (file paths), not objects with a `path` field.
fn git_status_file_list(
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
            out.push_str(&format!("  {} ({}):\n", label, arr.len(),));
            for f in arr.iter().take(30) {
                // Files are plain strings in the handler output
                let path = f
                    .as_str()
                    .unwrap_or_else(|| f.get("path").and_then(|v| v.as_str()).unwrap_or("?"));
                out.push_str(&format!(
                    "    {}{}{} {}\n",
                    if color { marker_color } else { "" },
                    marker,
                    if color { RESET } else { "" },
                    path,
                ));
            }
            if arr.len() > 30 {
                out.push_str(&format!(
                    "    {}… {} more{}\n",
                    if color { DIM } else { "" },
                    arr.len() - 30,
                    if color { RESET } else { "" },
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
    if let Some(lc) = data.get("line_count").and_then(|v| v.as_u64()) {
        out.push_str(&field("Lines", &lc.to_string(), color));
    }
    if let Some(sc) = data.get("symbol_count").and_then(|v| v.as_u64()) {
        out.push_str(&field("Symbols", &sc.to_string(), color));
    }
    if let Some(role) = data.get("module_role").and_then(|v| v.as_str()) {
        out.push_str(&field("Role", role, color));
    }
    if let Some(arr) = data.get("symbols").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str("  Symbols:\n");
            for sym in arr.iter().take(50) {
                let name = sym.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let typ = sym.get("type").and_then(|v| v.as_str()).unwrap_or("symbol");
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
                    "    {}{}{} {}{}{}\n",
                    c,
                    icon,
                    if color { RESET } else { "" },
                    if color { LIGHT_CYAN } else { "" },
                    name,
                    if color { RESET } else { "" },
                ));
            }
            // Truncation indicator
            let truncated = data
                .get("symbols_truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let total = data
                .get("symbol_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let shown = arr.len().min(50);
            if truncated || shown < total {
                let hidden = total.saturating_sub(shown);
                out.push_str(&format!(
                    "    {}… {} more symbols (truncated){}\n",
                    if color { DIM } else { "" },
                    hidden,
                    if color { RESET } else { "" },
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

fn render_write(data: &Value, color: bool) -> String {
    // `WriteHandler` returns a confirmation payload with shape
    // (see `src/cli/mcp/write_handler.rs::WriteHandler::execute`):
    //   { success, file_path, language, symbols: [{name, type, range}] }
    // Routing it through `render_diff_value` produced an empty
    // string because the handler does not emit `diff` / `diffs` /
    // `diff_text`. Surface the success status, the file path, the
    // language, and the parsed symbol table so the CLI user sees
    // the confirmation and the structural context the handler
    // actually returned.
    let mut out = String::new();
    let success = data
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (status_label, status_color) = if success {
        ("Wrote", if color { LIGHT_GREEN } else { "" })
    } else {
        ("Write failed", if color { LIGHT_RED } else { "" })
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
    if let Some(lang) = data.get("language").and_then(|v| v.as_str()) {
        out.push_str(&field("Language", lang, color));
    }
    if let Some(arr) = data.get("symbols").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            out.push('\n');
            out.push_str(&format!(
                "  {}Symbols ({}):{}\n",
                if color { DIM } else { "" },
                arr.len(),
                if color { RESET } else { "" },
            ));
            for s in arr.iter().take(20) {
                let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let typ = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
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
            if arr.len() > 20 {
                out.push_str(&format!(
                    "    {}…and {} more{}\n",
                    if color { DIM } else { "" },
                    arr.len() - 20,
                    if color { RESET } else { "" },
                ));
            }
        }
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
    if let Some(region_value) = data.get("edit_region") {
        // `edit_region` may be a string (surrounding code excerpt)
        // or an object (e.g. `{"start": 10, "end": 25}`) carrying
        // byte ranges from `trim_edit`. The string form is the
        // human-readable preview; the object form is what the
        // trimmer preserves for apply-shaped payloads where the
        // surrounding text was not retained. Render the object
        // form as a compact `{start,end}` so the region context
        // is never dropped from the CLI output.
        let region_text: Option<String> = if let Some(s) = region_value.as_str() {
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
        if let Some(text) = region_text {
            out.push('\n');
            if region_value.is_string() {
                // String form: the surrounding text is a
                // multi-line excerpt. The header is a
                // marker line; the lines themselves are
                // emitted below as a per-line colorized
                // expansion. Putting the raw multi-line
                // `text` on the header would duplicate the
                // expansion and produce messy output.
                out.push_str(&format!(
                    "  {}Surrounding region:{}\n",
                    if color { DIM } else { "" },
                    if color { RESET } else { "" },
                ));
                for l in text.lines() {
                    out.push_str(&format!(
                        "      {}{}{}\n",
                        if color { DIM } else { "" },
                        truncate_chars(l, 160),
                        if color { RESET } else { "" },
                    ));
                }
            } else {
                // Object form (`{start, end}` etc.):
                // the `text` is already a compact single
                // line like `bytes 10..25`, so we put it
                // on the same line as the header and do
                // not expand it per-line.
                out.push_str(&format!(
                    "  {}Surrounding region:{} {}\n",
                    if color { DIM } else { "" },
                    if color { RESET } else { "" },
                    text,
                ));
            }
        }
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
            .map(|n| {
                n.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(names, vec!["src", "tests"]);
        // Inside `src`, the child directory must be `cli` (not `src`).
        let src = tree.iter().find(|n| n["name"] == "src").unwrap();
        let src_children = src.get("children").and_then(|v| v.as_array()).unwrap();
        let src_child_names: Vec<String> = src_children
            .iter()
            .map(|n| {
                n.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        assert_eq!(src_child_names, vec!["cli"]);
        // Inside `cli`, the grandchild directory must be `sub`.
        let cli = src_children.iter().find(|n| n["name"] == "cli").unwrap();
        let cli_children = cli.get("children").and_then(|v| v.as_array()).unwrap();
        let cli_child_names: Vec<String> = cli_children
            .iter()
            .map(|n| {
                n.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })
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
        assert!(
            a_line.contains("│   └──"),
            "a.rs should be under 'src' with continuation: {:?}",
            a_line
        );
        // The second file line should sit under `tests` with a space
        // prefix (no continuation since tests is the last root child).
        let b_line = lines.iter().find(|l| l.contains("b.rs")).unwrap();
        assert!(
            b_line.starts_with("    └──"),
            "b.rs should be under 'tests' with space indent: {:?}",
            b_line
        );
    }

    #[test]
    fn test_render_tool_output_dispatches_by_name() {
        let args = v(r#"{"query": "foo", "top_k": 1}"#);
        // Search payload (using trimmed form so the renderer sees what
        // the LLM would see).
        let search_data = trim_search(&v(
            r#"{"results": [{"file_path": "/p.rs", "symbol_name": "f", "score": {"overall": 0.5}}]}"#,
        ));
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

    #[test]
    fn test_render_search_signature_empty_falls_through_to_snippet() {
        // Regression: a populated-but-empty `signature` must not
        // block the fallback chain — a real `snippet` is still
        // printed instead of leaving the result blank.
        let args = v(r#"{"query": "foo", "top_k": 1}"#);
        let payload = v(r#"{
            "count": 1,
            "results": [{
                "file_path": "/p.rs",
                "symbol": "main",
                "symbol_type": "function",
                "score": 0.9,
                "signature": "   ",
                "snippet": "fn main() { return 0; }"
            }]
        }"#);
        let s = render_tool_output("leindex.search", &payload, &args);
        assert!(
            s.contains("fn main()"),
            "snippet must print when signature is empty: {}",
            s
        );
    }

    #[test]
    fn test_render_context_reads_from_results_anchor() {
        // Regression: `ContextHandler` returns an `AnalysisResult`
        // whose expanded PDG text is in `context` and whose anchor
        // node lives in `results[0]`. The CLI must surface both.
        let args = v(r#"{"node_id": "main"}"#);
        let payload = v(r#"{
            "query": "Context for main",
            "results": [{
                "rank": 1,
                "node_id": "src/main.rs:main",
                "file_path": "src/main.rs",
                "symbol_name": "main",
                "symbol_type": "function",
                "byte_range": [10, 50]
            }],
            "context": "fn main() { return 0; }\nfn helper() {}",
            "tokens_used": 12,
            "processing_time_ms": 1
        }"#);
        let s = render_tool_output("leindex.context", &payload, &args);
        assert!(s.contains("Symbol"), "missing Symbol field: {}", s);
        assert!(s.contains("main"), "missing symbol name: {}", s);
        assert!(s.contains("src/main.rs"), "missing file path: {}", s);
        assert!(s.contains("fn main()"), "missing expanded body: {}", s);
    }

    #[test]
    fn test_render_symbol_lookup_uses_real_field_names() {
        // Regression: `lookup_single_symbol` returns `file` / `type` /
        // `byte_range` / `complexity` / `language` / `impact_radius`
        // / optional `source` — NOT the legacy `file_path` / `line` /
        // `symbol_type` / `signature` aliases. Verify every real
        // field is surfaced and the legacy aliases are not.
        let args = v(r#"{"symbol": "main", "include_source": true}"#);
        let payload = v(r#"{
            "symbol": "main",
            "type": "function",
            "file": "src/main.rs",
            "byte_range": [10, 60],
            "complexity": 3,
            "language": "rust",
            "callers": [{"name": "caller_a", "file": "src/lib.rs", "type": "function"}],
            "callees": [],
            "impact_radius": {"affected_symbols": 5, "affected_files": 2},
            "source": "fn main() { return 0; }"
        }"#);
        let s = render_tool_output("leindex.symbol-lookup", &payload, &args);
        assert!(s.contains("Symbol"));
        assert!(s.contains("main"));
        assert!(s.contains("File"), "missing File field: {}", s);
        assert!(s.contains("src/main.rs"), "missing real file: {}", s);
        assert!(s.contains("Type"), "missing Type field: {}", s);
        assert!(s.contains("function"), "missing real type: {}", s);
        assert!(s.contains("Language"), "missing Language field: {}", s);
        assert!(s.contains("rust"), "missing language: {}", s);
        assert!(s.contains("Range"), "missing Range field: {}", s);
        assert!(s.contains("bytes 10-60"), "missing byte range: {}", s);
        assert!(s.contains("Complexity"), "missing Complexity field: {}", s);
        assert!(s.contains("Impact"), "missing Impact field: {}", s);
        assert!(
            s.contains("5 symbols / 2 files"),
            "missing impact counts: {}",
            s
        );
        assert!(s.contains("caller_a"), "missing caller: {}", s);
        assert!(s.contains("Callers"));
        // Source preview (first non-empty line).
        assert!(s.contains("fn main()"), "missing source preview: {}", s);
        // Legacy aliases must NOT appear in the rendered output.
        assert!(
            !s.contains("file_path"),
            "renderer still emits file_path alias: {}",
            s
        );
        assert!(
            !s.contains("symbol_type"),
            "renderer still emits symbol_type alias: {}",
            s
        );
        assert!(
            !s.contains("Signature"),
            "renderer still emits Signature (legacy alias): {}",
            s
        );
    }

    #[test]
    fn test_render_write_shows_confirmation_not_diff() {
        // Regression: `WriteHandler` returns
        // `{success, file_path, language, symbols}` and must NOT be
        // routed through `render_diff_value` (which expects
        // `diff` / `diffs` / `diff_text` and returns empty for the
        // write payload).
        let args = v(r#"{"file_path": "src/lib.rs", "content": "// hello"}"#);
        let payload = v(r#"{
            "success": true,
            "file_path": "src/lib.rs",
            "language": "rust",
            "symbols": [
                {"name": "alpha", "type": "fn() -> ()", "range": [0, 9]},
                {"name": "beta",  "type": "fn() -> ()", "range": [10, 19]}
            ]
        }"#);
        let s = render_tool_output("leindex.write", &payload, &args);
        assert!(s.contains("Wrote"), "missing confirmation header: {}", s);
        assert!(s.contains("src/lib.rs"), "missing file path: {}", s);
        assert!(s.contains("Language"), "missing language field: {}", s);
        assert!(s.contains("rust"), "missing language value: {}", s);
        assert!(s.contains("alpha"), "missing symbol name: {}", s);
        assert!(s.contains("beta"), "missing symbol name: {}", s);
        // Diff-style gutters (line numbers) must NOT appear.
        assert!(!s.contains("│"), "write must not render diff gutter: {}", s);
    }

    #[test]
    fn test_render_edit_apply_shows_confirmation_not_diff() {
        // Regression: `EditApplyHandler` returns
        // `{success, changes_applied, file_path, edit_region,
        // affected_symbols, affected_files, breaking_changes}` and
        // must NOT be routed through `render_diff_value` (which
        // expects `diff` / `diffs` / `diff_text` and returns empty
        // for the apply payload).
        let args = v(r#"{"file_path": "src/lib.rs"}"#);
        let payload = v(r#"{
            "success": true,
            "changes_applied": 2,
            "file_path": "src/lib.rs",
            "edit_region": "1: // hello\n2: fn alpha() {}",
            "affected_symbols": ["alpha", "beta"],
            "affected_files": ["src/lib.rs"],
            "breaking_changes": []
        }"#);
        let s = render_tool_output("leindex.edit-apply", &payload, &args);
        assert!(s.contains("Applied"), "missing applied header: {}", s);
        assert!(s.contains("src/lib.rs"), "missing file path: {}", s);
        assert!(
            s.contains("Affected symbols"),
            "missing affected symbols: {}",
            s
        );
        assert!(
            s.contains("Affected files"),
            "missing affected files: {}",
            s
        );
        // The surrounding region must be shown.
        assert!(s.contains("// hello"), "missing surrounding region: {}", s);
        // Diff-style gutters must NOT appear.
        assert!(
            !s.contains("│"),
            "edit-apply must not render diff gutter: {}",
            s
        );
    }

    #[test]
    fn test_render_edit_apply_noop_shows_message() {
        // No-op path: changes_applied == 0, message describes why.
        let args = v(r#"{"file_path": "src/lib.rs"}"#);
        let payload = v(r#"{
            "success": true,
            "changes_applied": 0,
            "message": "No changes to apply (content identical)"
        }"#);
        let s = render_tool_output("leindex.edit-apply", &payload, &args);
        assert!(s.contains("No-op"), "missing no-op header: {}", s);
        assert!(
            s.contains("content identical"),
            "missing no-op message: {}",
            s
        );
    }

    #[test]
    fn test_render_edit_apply_renders_object_edit_region() {
        // Regression: `trim_edit` preserves `edit_region` as an
        // object (e.g. `{"start": 10, "end": 25}`) for apply-
        // shaped payloads. The CLI renderer used to look only for
        // the string form, which would silently drop the region
        // context. Now it renders the object form as
        // `Surrounding region: bytes 10..25` so the LLM-visible
        // payload is never truncated by a shape mismatch.
        let args = v(r#"{"file_path": "src/lib.rs"}"#);
        let payload = v(r#"{
            "success": true,
            "changes_applied": 3,
            "file_path": "src/lib.rs",
            "edit_region": {"start": 10, "end": 25},
            "message": "Applied 3 changes"
        }"#);
        let s = render_tool_output("leindex.edit-apply", &payload, &args);
        assert!(
            s.contains("Surrounding region"),
            "missing surrounding region label: {}",
            s
        );
        assert!(
            s.contains("bytes 10..25"),
            "missing structured edit_region range: {}",
            s
        );
    }

    #[test]
    fn test_render_edit_apply_string_region_no_duplicate_text() {
        // Regression: when `edit_region` is a string (the
        // multi-line surrounding excerpt form), the renderer
        // must NOT print the raw multi-line text on the
        // `Surrounding region:` header line — that would
        // duplicate the per-line colorized expansion emitted
        // below the header. The header is a marker; the lines
        // themselves go on the indented block.
        let args = v(r#"{"file_path": "src/lib.rs"}"#);
        let payload = v(r#"{
            "success": true,
            "changes_applied": 1,
            "file_path": "src/lib.rs",
            "edit_region": "1: // hello\n2: fn alpha() {}\n3: fn beta() {}\n",
            "message": "Applied 1 change"
        }"#);
        let s = render_tool_output("leindex.edit-apply", &payload, &args);
        let stripped = strip_ansi(&s);
        // The string body must appear in the per-line
        // expansion (the `      // hello` indented line), so
        // the comment marker IS in the output.
        assert!(
            stripped.contains("// hello"),
            "per-line expansion missing: {}",
            stripped
        );
        assert!(stripped.contains("fn alpha() {}"));
        assert!(stripped.contains("fn beta() {}"));
        // The header line itself must be a single marker
        // line — NOT the raw multi-line text concatenated.
        // We assert that the `Surrounding region:` line, when
        // stripped, does not also include the function
        // bodies (which would be the duplication symptom).
        let header_line = stripped
            .lines()
            .find(|l| l.contains("Surrounding region"))
            .unwrap_or("");
        assert!(
            !header_line.contains("fn alpha()"),
            "string edit_region body leaked into header line: {:?}",
            header_line
        );
        assert!(
            !header_line.contains("fn beta()"),
            "string edit_region body leaked into header line: {:?}",
            header_line
        );
    }

    #[test]
    fn test_render_edit_apply_renders_partial_object_edit_region() {
        // When the trimmer preserves a half-shape object (only
        // `start`, or only `end`, or neither), the renderer must
        // still surface what is present rather than dropping the
        // region entirely.
        let args = v(r#"{"file_path": "src/lib.rs"}"#);
        let payload = v(r#"{
            "success": true,
            "changes_applied": 1,
            "file_path": "src/lib.rs",
            "edit_region": {"start": 7},
            "message": "Applied 1 change"
        }"#);
        let s = render_tool_output("leindex.edit-apply", &payload, &args);
        assert!(
            s.contains("bytes 7.."),
            "missing open-ended start range: {}",
            s
        );
    }

    #[test]
    fn test_render_context_does_not_mislabel_byte_offset_as_line() {
        // Regression: `results[0].byte_range[0]` is a byte offset,
        // not a line number. The CLI must NOT show the byte offset
        // in the `Line` field or as the gutter base. The byte
        // range is still surfaced as a `Range: bytes X-Y` hint.
        let args = v(r#"{"node_id": "main"}"#);
        let payload = v(r#"{
            "query": "Context for main",
            "results": [{
                "rank": 1,
                "node_id": "src/main.rs:main",
                "file_path": "src/main.rs",
                "symbol_name": "main",
                "symbol_type": "function",
                "byte_range": [15342, 15400]
            }],
            "context": "fn main() {\n    return 0;\n}"
        }"#);
        let s = render_tool_output("leindex.context", &payload, &args);
        // The byte offset (15342) must NOT appear as a line value.
        assert!(
            !s.contains("Line: 15342"),
            "renderer mislabelled byte offset as line number: {}",
            s
        );
        // The byte range is still surfaced as a Range hint.
        assert!(s.contains("Range"), "missing range hint: {}", s);
        assert!(s.contains("bytes 15342-15400"), "missing byte range: {}", s);
        // The snippet gutter must start at 1 (relative to the
        // snippet), not at the byte offset. The gutter value
        // (right-padded to 4 chars) is followed by an ANSI reset
        // and the `│` separator; the gutter text itself sits
        // between the leading "  " (two-space indent) and the `│`
        // separator, so we strip ANSI escapes and look for the
        // gutter lines.
        let stripped = strip_ansi(&s);
        // The first gutter line should be "   1│", not "15342│".
        let first_gutter = stripped.lines().find(|l| l.contains('│')).unwrap_or("");
        assert!(
            first_gutter.contains("   1│"),
            "first gutter line must start at 1, got {:?}",
            first_gutter
        );
        // No gutter line should start with "15342" (the byte
        // offset) — every gutter should be a small relative line
        // number, since the snippet has at most a handful of
        // lines.
        for l in stripped.lines() {
            if l.contains('│') {
                assert!(
                    !l.contains("15342│"),
                    "gutter line must not use byte offset: {:?}",
                    l
                );
            }
        }
    }

    #[test]
    fn test_render_context_uses_legacy_line_field() {
        // Regression: legacy flat-shape payloads carry a real
        // `line` field; the renderer must use that and the gutter
        // must start at that line.
        let args = v(r#"{"node_id": "main"}"#);
        let payload = v(r#"{
            "symbol": "main",
            "file_path": "src/main.rs",
            "symbol_type": "function",
            "line": 42,
            "content": "fn main() { return 0; }"
        }"#);
        let s = render_tool_output("leindex.context", &payload, &args);
        let stripped = strip_ansi(&s);
        assert!(
            stripped.contains("Line: 42"),
            "missing line field: {}",
            stripped
        );
        // The gutter must start at 42 (right-padded to width 4).
        let first_gutter = stripped.lines().find(|l| l.contains('│')).unwrap_or("");
        assert!(
            first_gutter.contains("  42│"),
            "gutter must start at 42, got {:?}",
            first_gutter
        );
    }

    #[test]
    fn test_render_flat_files_drops_unused_label_string() {
        // Regression: `cx_label.1` ("low"/"med"/"high") was
        // computed but never used. Confirm the colour-coded
        // integer is still rendered and no `low`/`med`/`high`
        // text leaks into the output.
        let files = v(r#"[
            {"path": "src/main.rs", "symbol_count": 3, "total_complexity": 5, "incoming_dependencies": 0, "outgoing_dependencies": 0},
            {"path": "src/lib.rs",  "symbol_count": 12, "total_complexity": 25, "incoming_dependencies": 1, "outgoing_dependencies": 0}
        ]"#);
        let s = render_flat_files(files.as_array().unwrap(), false);
        assert!(s.contains("src/main.rs"));
        assert!(s.contains("src/lib.rs"));
        assert!(s.contains("cx:5"), "missing complexity value: {}", s);
        assert!(s.contains("cx:25"), "missing complexity value: {}", s);
        // The unused label strings must NOT appear in the output.
        assert!(!s.contains("low"), "unused label leaked: {}", s);
        assert!(!s.contains("med"), "unused label leaked: {}", s);
        assert!(!s.contains("high"), "unused label leaked: {}", s);
    }

    /// Regression for P2 #3342365976: `lookup_symbols_batch` returns
    /// a wrapper {batch:true, count, results:[ ... ]}. The previous
    /// renderer emitted the header plus a Count field but no per-entry
    /// output, because `symbol` / `file` / `type` were at the entry
    /// level, not the top level. The fix branches on `batch:true` and
    /// recurses into each result.
    #[test]
    fn test_render_symbol_lookup_batch_renders_each_entry() {
        let args = v(r#"{"symbols": ["main", "lib_init"]}"#);
        let payload = v(r#"{
            "batch": true,
            "count": 2,
            "results": [
                {
                    "symbol": "main",
                    "type": "function",
                    "file": "src/main.rs",
                    "byte_range": [10, 60],
                    "complexity": 3,
                    "language": "rust",
                    "callers": [],
                    "callees": [],
                    "impact_radius": {"affected_symbols": 5, "affected_files": 2}
                },
                {
                    "symbol": "lib_init",
                    "type": "function",
                    "file": "src/lib.rs",
                    "byte_range": [100, 200],
                    "complexity": 7,
                    "language": "rust",
                    "callers": [],
                    "callees": [],
                    "impact_radius": {"affected_symbols": 0, "affected_files": 0}
                }
            ]
        }"#);
        let s = render_tool_output("leindex.symbol-lookup", &payload, &args);
        // Batch header and count.
        assert!(
            s.contains("Symbol Lookup (batch)"),
            "missing batch header: {}",
            s
        );
        assert!(s.contains("Count"), "missing Count field: {}", s);
        // Each entry must appear with its own Symbol / File / Type.
        assert!(s.contains("main"), "missing first entry symbol: {}", s);
        assert!(s.contains("lib_init"), "missing second entry symbol: {}", s);
        assert!(s.contains("src/main.rs"), "missing first entry file: {}", s);
        assert!(s.contains("src/lib.rs"), "missing second entry file: {}", s);
        // The byte range from each entry must be present.
        assert!(
            s.contains("bytes 10-60"),
            "missing first entry range: {}",
            s
        );
        assert!(
            s.contains("bytes 100-200"),
            "missing second entry range: {}",
            s
        );
    }

    /// Empty batch returns the wrapper with a "(no results)" marker
    /// rather than emitting only a header + Count line.
    #[test]
    fn test_render_symbol_lookup_batch_empty() {
        let args = v(r#"{"symbols": []}"#);
        let payload = v(r#"{"batch": true, "count": 0, "results": []}"#);
        let s = render_tool_output("leindex.symbol-lookup", &payload, &args);
        assert!(
            s.contains("Symbol Lookup (batch)"),
            "missing batch header: {}",
            s
        );
        assert!(s.contains("(no results)"), "missing empty marker: {}", s);
    }
}
