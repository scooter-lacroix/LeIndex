//! Beautiful output formatting for LeIndex tools
//!
//! This module provides visually appealing, human-readable output formatting
//! for various LeIndex tools including search results, diffs, project maps, and diagnostics.
//!
//! The architecture is layered:
//!
//! 1. **Structured types** — `DiffResult`, `DiffHunk`, `DiffLine` capture the
//!    data shape that the LLM receives (clean JSON, no ANSI).
//! 2. **Compute** — `compute_diff`, `extract_search_results`, `extract_tree`
//!    turn raw handler output into these structured types.
//! 3. **Render** — per-tool render functions take a `Value` and produce a
//!    human-readable colored string for the CLI. They share the same
//!    structured core so MCP and CLI stay in lock-step.
//! 4. **Dispatch** — `render_tool_output(name, value, args)` is the single
//!    entry point used by `leindex tools run` and any other CLI surface.

use serde_json::Value;

// =============================================================================
// ANSI constants
// =============================================================================

/// Resets all ANSI formatting attributes
pub const RESET: &str = "\x1b[0m";
/// Bold text formatting
pub const BOLD: &str = "\x1b[1m";
/// Dimmed text formatting
pub const DIM: &str = "\x1b[2m";

/// Black foreground color
pub const BLACK: &str = "\x1b[30m";
/// Red foreground color
pub const RED: &str = "\x1b[31m";
/// Green foreground color
pub const GREEN: &str = "\x1b[32m";
/// Yellow foreground color
pub const YELLOW: &str = "\x1b[33m";
/// Blue foreground color
pub const BLUE: &str = "\x1b[34m";
/// Magenta foreground color
pub const MAGENTA: &str = "\x1b[35m";
/// Cyan foreground color
pub const CYAN: &str = "\x1b[36m";
/// White foreground color
pub const WHITE: &str = "\x1b[37m";

/// Light grey foreground color
pub const LIGHT_GREY: &str = "\x1b[90m";
/// Light red foreground color
pub const LIGHT_RED: &str = "\x1b[91m";
/// Light green foreground color
pub const LIGHT_GREEN: &str = "\x1b[92m";
/// Light yellow foreground color
pub const LIGHT_YELLOW: &str = "\x1b[93m";
/// Light blue foreground color
pub const LIGHT_BLUE: &str = "\x1b[94m";
/// Light magenta foreground color
pub const LIGHT_MAGENTA: &str = "\x1b[95m";
/// Light cyan foreground color
pub const LIGHT_CYAN: &str = "\x1b[96m";

/// Background highlight for added lines (split view)
pub const BG_GREEN: &str = "\x1b[48;5;22m";
/// Background highlight for removed lines (split view)
pub const BG_RED: &str = "\x1b[48;5;52m";

// =============================================================================
// Structured diff types — what the LLM sees
// =============================================================================

/// Operation kind for a single diff line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOp {
    /// Unchanged context line.
    Context,
    /// Line present in original, removed in modified.
    Remove,
    /// Line added in modified, not in original.
    Add,
}

impl DiffOp {
    /// Stable lowercase string used in serialized JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            DiffOp::Context => "context",
            DiffOp::Remove => "remove",
            DiffOp::Add => "add",
        }
    }
}

/// A single line inside a diff hunk.
///
/// `old_line` and `new_line` are 1-based and `None` for the side that
/// does not have the line (e.g. a removed line has no `new_line`).
#[derive(Debug, Clone)]
pub struct DiffLine {
    /// What happened to this line.
    pub op: DiffOp,
    /// 1-based line number in the original file, or `None` if added.
    pub old_line: Option<usize>,
    /// 1-based line number in the modified file, or `None` if removed.
    pub new_line: Option<usize>,
    /// The text of the line without its trailing newline.
    pub content: String,
}

/// A contiguous hunk of changes with 3 lines of surrounding context
/// (matching `diffy`'s default behaviour).
#[derive(Debug, Clone)]
pub struct DiffHunk {
    /// 1-based line number of the first line of the original file in this hunk.
    pub old_start: usize,
    /// 1-based line number of the first line of the modified file in this hunk.
    pub new_start: usize,
    /// The lines that make up the hunk, in original file order.
    pub lines: Vec<DiffLine>,
}

/// A complete diff between two pieces of text, plus the file the diff
/// applies to (used for headers in the rendered view).
#[derive(Debug, Clone)]
pub struct DiffResult {
    /// Path of the file the diff was computed against.
    pub file_path: String,
    /// Number of added lines across all hunks.
    pub additions: usize,
    /// Number of removed lines across all hunks.
    pub deletions: usize,
    /// The change hunks, ordered by position in the original file.
    pub hunks: Vec<DiffHunk>,
}

impl DiffResult {
    /// Returns `true` if the diff contains at least one hunk.
    pub fn has_changes(&self) -> bool {
        !self.hunks.is_empty()
    }

    /// Serialize to the JSON shape the LLM sees: structured, no ANSI.
    pub fn to_json(&self) -> Value {
        let hunk_json: Vec<Value> = self
            .hunks
            .iter()
            .map(|h| {
                let line_json: Vec<Value> = h
                    .lines
                    .iter()
                    .map(|l| {
                        serde_json::json!({
                            "op": l.op.as_str(),
                            "old_line": l.old_line,
                            "new_line": l.new_line,
                            "content": l.content,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "old_start": h.old_start,
                    "new_start": h.new_start,
                    "lines": line_json,
                })
            })
            .collect();
        serde_json::json!({
            "file_path": self.file_path,
            "additions": self.additions,
            "deletions": self.deletions,
            "hunks": hunk_json,
        })
    }
}

/// Compute a structured diff between two pieces of text.
///
/// This is the source of truth for what the LLM receives and what the
/// CLI renders — both go through the same `DiffResult`.
pub fn compute_diff(original: &str, modified: &str, file_path: &str) -> DiffResult {
    let mut result = DiffResult {
        file_path: file_path.to_string(),
        additions: 0,
        deletions: 0,
        hunks: Vec::new(),
    };

    if original == modified {
        return result;
    }

    let patch = diffy::create_patch(original, modified);
    for hunk in patch.hunks() {
        let mut lines = Vec::with_capacity(hunk.lines().len());
        let mut old_line = hunk.old_range().start();
        let mut new_line = hunk.new_range().start();
        for line in hunk.lines() {
            // diffy includes the trailing newline in each line; strip it
            // so the diff renderer can pad each side cleanly without
            // breaking the row layout.
            let strip = |s: &str| s.trim_end_matches('\n').trim_end_matches('\r').to_string();
            match line {
                diffy::Line::Context(s) => {
                    lines.push(DiffLine {
                        op: DiffOp::Context,
                        old_line: Some(old_line),
                        new_line: Some(new_line),
                        content: strip(s),
                    });
                    old_line += 1;
                    new_line += 1;
                }
                diffy::Line::Insert(s) => {
                    lines.push(DiffLine {
                        op: DiffOp::Add,
                        old_line: None,
                        new_line: Some(new_line),
                        content: strip(s),
                    });
                    result.additions += 1;
                    new_line += 1;
                }
                diffy::Line::Delete(s) => {
                    lines.push(DiffLine {
                        op: DiffOp::Remove,
                        old_line: Some(old_line),
                        new_line: None,
                        content: strip(s),
                    });
                    result.deletions += 1;
                    old_line += 1;
                }
            }
        }
        result.hunks.push(DiffHunk {
            old_start: hunk.old_range().start(),
            new_start: hunk.new_range().start(),
            lines,
        });
    }
    result
}

// =============================================================================
// Diff rendering
// =============================================================================

/// Render a `DiffResult` as a unified diff (closest to `git diff`).
pub fn render_unified_diff(diff: &DiffResult, color: bool) -> String {
    let mut out = String::new();
    if !diff.has_changes() {
        out.push_str(&format!(
            "{}--- a/{}\n+++ b/{}\n(no changes)\n",
            if color { LIGHT_GREY } else { "" },
            diff.file_path,
            diff.file_path,
        ));
        return out;
    }
    out.push_str(&format!(
        "{}--- a/{}\n+++ b/{}\n",
        if color { LIGHT_GREY } else { "" },
        diff.file_path,
        diff.file_path,
    ));
    for hunk in &diff.hunks {
        out.push_str(&format!(
            "{}@@ -{},{} +{},{} @@\n",
            if color { LIGHT_CYAN } else { "" },
            hunk.old_start,
            hunk.lines.iter().filter(|l| l.op != DiffOp::Add).count(),
            hunk.new_start,
            hunk.lines.iter().filter(|l| l.op != DiffOp::Remove).count(),
        ));
        for line in &hunk.lines {
            out.push_str(&unified_line(line, color));
        }
    }
    out
}

fn unified_line(line: &DiffLine, color: bool) -> String {
    let content = &line.content;
    match line.op {
        DiffOp::Context => format!(
            " {}{}{}\n",
            if color { LIGHT_GREY } else { "" },
            content,
            if color { RESET } else { "" }
        ),
        DiffOp::Add => format!(
            "+{}{}{}\n",
            if color { LIGHT_GREEN } else { "" },
            content,
            if color { RESET } else { "" }
        ),
        DiffOp::Remove => format!(
            "-{}{}{}\n",
            if color { LIGHT_RED } else { "" },
            content,
            if color { RESET } else { "" }
        ),
    }
}

/// Render a `DiffResult` as a split-view diff with line numbers and
/// +/- markers (like opencode's Edit). Falls back to unified if the
/// terminal is too narrow.
pub fn render_split_diff(diff: &DiffResult, color: bool, width: Option<usize>) -> String {
    let mut out = String::new();
    if !diff.has_changes() {
        out.push_str(&format!(
            "{}── {} (no changes) ──{}\n",
            if color { LIGHT_CYAN } else { "" },
            diff.file_path,
            if color { RESET } else { "" },
        ));
        return out;
    }

    // Summary header
    let summary = format!(
        "{}  +{}  -{}",
        diff.file_path, diff.additions, diff.deletions
    );
    out.push_str(&format!(
        "{}── {} ──{}\n",
        if color { LIGHT_CYAN } else { "" },
        summary,
        if color { RESET } else { "" },
    ));

    let gutter = 4usize; // 4-digit line number
    let half = match width {
        Some(w) => w.saturating_sub(gutter * 2 + 5) / 2,
        None => 60,
    };

    for (hi, hunk) in diff.hunks.iter().enumerate() {
        if hi > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "{}@@ -{},{} +{},{} @@{}\n",
            if color { LIGHT_CYAN } else { "" },
            hunk.old_start,
            hunk.lines.iter().filter(|l| l.op != DiffOp::Add).count(),
            hunk.new_start,
            hunk.lines.iter().filter(|l| l.op != DiffOp::Remove).count(),
            if color { RESET } else { "" },
        ));
        out.push_str(&render_hunk_split(hunk, &RowLayout { gutter, half, color }));
    }
    out
}

fn render_hunk_split(hunk: &DiffHunk, layout: &RowLayout) -> String {
    let mut out = String::new();
    // Walk the hunk in runs. A run of `Remove` is always followed by
    // (optional) `Add` lines; pair them row-by-row so a true side-by-side
    // layout is produced (otherwise we'd show a remove on the left and
    // a blank on the right for a single replacement).
    let mut old_line = hunk.old_start;
    let mut new_line = hunk.new_start;
    let mut i = 0;
    while i < hunk.lines.len() {
        let line = &hunk.lines[i];
        match line.op {
            DiffOp::Context => {
                out.push_str(&split_row(
                    &SplitRow {
                        old_line: Some(old_line),
                        old_content: &line.content,
                        new_line: Some(new_line),
                        new_content: &line.content,
                        marker: " ",
                    },
                    layout,
                ));
                old_line += 1;
                new_line += 1;
                i += 1;
            }
            DiffOp::Remove => {
                // Collect this run of removes.
                let mut removes: Vec<&DiffLine> = vec![line];
                let mut j = i + 1;
                while j < hunk.lines.len() && hunk.lines[j].op == DiffOp::Remove {
                    removes.push(&hunk.lines[j]);
                    j += 1;
                }
                // Collect the immediately-following run of adds.
                let mut adds: Vec<&DiffLine> = Vec::new();
                while j < hunk.lines.len() && hunk.lines[j].op == DiffOp::Add {
                    adds.push(&hunk.lines[j]);
                    j += 1;
                }
                let consumed = removes.len() + adds.len();
                let max = removes.len().max(adds.len());
                for k in 0..max {
                    let rem = removes.get(k);
                    let add = adds.get(k);
                    let row = match (rem, add) {
                        (Some(r), Some(a)) => SplitRow {
                            old_line: Some(old_line + k),
                            old_content: r.content.as_str(),
                            new_line: Some(new_line + k),
                            new_content: a.content.as_str(),
                            marker: " ",
                        },
                        (Some(r), None) => SplitRow {
                            old_line: Some(old_line + k),
                            old_content: r.content.as_str(),
                            new_line: None,
                            new_content: "",
                            marker: " ",
                        },
                        (None, Some(a)) => SplitRow {
                            old_line: None,
                            old_content: "",
                            new_line: Some(new_line + k),
                            new_content: a.content.as_str(),
                            marker: " ",
                        },
                        (None, None) => unreachable!(),
                    };
                    out.push_str(&split_row(&row, layout));
                }
                old_line += removes.len();
                new_line += adds.len();
                i += consumed;
            }
            DiffOp::Add => {
                // Standalone add with no preceding remove — happens when
                // new lines are inserted (e.g. a brand-new function body).
                out.push_str(&split_row(
                    &SplitRow {
                        old_line: None,
                        old_content: "",
                        new_line: Some(new_line),
                        new_content: &line.content,
                        marker: " ",
                    },
                    layout,
                ));
                new_line += 1;
                i += 1;
            }
        }
    }
    out
}

/// Layout parameters shared by every row of a split-view diff render.
#[derive(Clone, Copy)]
struct RowLayout {
    /// Width of the line-number gutter on each side (e.g. 4 for "0001").
    gutter: usize,
    /// Maximum number of content characters displayed per side.
    half: usize,
    /// Whether to emit ANSI color escapes.
    color: bool,
}

/// One row of a split-view diff: a (left_line, left_text) + (right_line,
/// right_text) pair, plus an optional `marker` override used for the
/// default "context" rows. When only one side is populated the
/// `marker` argument is ignored and `+` / `-` is used on the populated
/// side.
struct SplitRow<'a> {
    old_line: Option<usize>,
    old_content: &'a str,
    new_line: Option<usize>,
    new_content: &'a str,
    marker: &'a str,
}

fn split_row(row: &SplitRow<'_>, layout: &RowLayout) -> String {
    let RowLayout { gutter, half, color } = *layout;
    let SplitRow {
        old_line,
        old_content,
        new_line,
        new_content,
        marker,
    } = row;

    let ol_str = old_line
        .map(|n| format!("{:>width$}", n, width = gutter))
        .unwrap_or_else(|| " ".repeat(gutter));
    let nl_str = new_line
        .map(|n| format!("{:>width$}", n, width = gutter))
        .unwrap_or_else(|| " ".repeat(gutter));

    let (left_marker, right_marker) = match (old_line, new_line) {
        (Some(_), Some(_)) => (*marker, *marker),
        (Some(_), None) => ("-", " "),
        (None, Some(_)) => (" ", "+"),
        (None, None) => (*marker, *marker),
    };

    let left_paint = if color && left_marker == "-" {
        LIGHT_RED
    } else if color && left_marker == "+" {
        LIGHT_GREEN
    } else if color {
        LIGHT_GREY
    } else {
        ""
    };
    let right_paint = if color && right_marker == "+" {
        LIGHT_GREEN
    } else if color && right_marker == "-" {
        LIGHT_RED
    } else if color {
        LIGHT_GREY
    } else {
        ""
    };
    let reset = if color { RESET } else { "" };

    let left = truncate(old_content, half);
    let right = truncate(new_content, half);

    format!(
        " {ol_str} {left_marker} {left_paint}{left:half$}{reset} │ {nl_str} {right_marker} {right_paint}{right:half$}{reset}\n",
        ol_str = ol_str,
        left_marker = left_marker,
        left_paint = left_paint,
        left = left,
        reset = reset,
        nl_str = nl_str,
        right_marker = right_marker,
        right_paint = right_paint,
        right = right,
        half = half,
    )
}

fn truncate(s: &str, max: usize) -> String {
    // `truncate` keeps `max - 1` characters and appends the single-char
    // `…` (Unicode horizontal ellipsis) so the result is at most `max`
    // characters wide.
    if max == 0 {
        return String::new();
    }
    let keep = max.saturating_sub(1);
    truncate_with_ellipsis(s, keep, "…")
}

pub(crate) fn truncate_chars(input: &str, max_chars: usize) -> String {
    // `truncate_chars` keeps the first `max_chars` characters and
    // appends `...` so the result is `max_chars + 3` characters wide.
    truncate_with_ellipsis(input, max_chars, "...")
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

fn suffix(symbol_count: u64, color: &str, reset: &str) -> String {
    if symbol_count == 0 {
        String::new()
    } else {
        format!("  {}[{} symbols]{}", color, symbol_count, reset)
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Single-pass truncation that walks the string at most once via
/// `char_indices` to find the cut byte offset, then appends the
/// supplied ellipsis. Replaces the previous implementation that
/// `chars().count()`'d the full string and then `chars().take()`'d it
/// a second time on the truncation path — O(2n) on every diff line.
///
/// Contract (preserved from prior behavior):
/// * `truncate(s, max)` — result is at most `max` characters wide; the
///   last character is `…` (single Unicode ellipsis).
/// * `truncate_chars(s, max)` — result keeps the first `max` characters
///   and appends `...` (three ASCII dots), so total length is `max + 3`.
fn truncate_with_ellipsis(input: &str, max: usize, ellipsis: &str) -> String {
    if max == 0 {
        return String::new();
    }
    // Fast path: byte length is always <= char count, so a string whose
    // byte length fits in the budget is guaranteed to fit in `max`
    // characters when it is pure ASCII.
    if input.len() <= max && input.is_ascii() {
        return input.to_string();
    }
    // Walk the string once, counting characters and tracking the byte
    // offset. We stop as soon as we have counted `max` characters;
    // `cut` is then the byte offset of the (max+1)-th character
    // (i.e. the prefix we keep ends right before that boundary).
    let mut cut: Option<usize> = None;
    for (char_count, (i, _ch)) in input.char_indices().enumerate() {
        if char_count == max {
            cut = Some(i);
            break;
        }
        // Advance `cut` to just past this character so if the loop
        // exits naturally (we ran out of input) we still know the
        // total byte length.
        cut = Some(i + input[i..].chars().next().unwrap().len_utf8());
    }
    match cut {
        Some(end) if end < input.len() => {
            // The loop broke out before consuming the full string.
            let mut out = String::with_capacity(end + ellipsis.len());
            out.push_str(&input[..end]);
            out.push_str(ellipsis);
            out
        }
        _ => input.to_string(),
    }
}

fn normalize_tool_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .replace(['-', '.', ' '], "_")
}

/// Returns the args needed to render a tool's CLI output.
pub fn lookup_args<'a>(_name: &str, args: Option<&'a Value>) -> &'a Value {
    static EMPTY: Value = Value::Null;
    args.unwrap_or(&EMPTY)
}

fn header(title: &str, color: bool) -> String {
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
        }
        // Surface byte ranges when no signature/context is available —
        // helps the user locate the hit in a very large file.
        if signature.is_none() && context.is_none() {
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

fn line_for(data: &Value) -> u64 {
    data.get("line").and_then(|v| v.as_u64()).unwrap_or(0)
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
        /// value. File nodes pass through their entry.
        fn into_value(self, name: &str) -> Value {
            let children: Vec<Value> = self
                .children
                .into_values()
                .map(|child| child.into_value(name))
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
                out.push_str(""); // keep the formatter above tidy
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

/// Shape of a diff payload that a tool handler may return. The dispatch
/// in `render_diff_value` matches on this so the control flow is flat and
/// the four supported payload shapes are easy to read.
#[derive(Debug)]
enum DiffPayload<'a> {
    /// (a) Diff fields are at the top level (`{file_path, hunks, …}`).
    Embedded {
        file: &'a str,
        src: &'a Value,
        hunks: &'a [Value],
    },
    /// (b) Diff is wrapped in a `diff: {…}` object (e.g. `edit_preview`).
    Wrapped {
        file: &'a str,
        src: &'a Value,
        hunks: &'a [Value],
    },
    /// (c) A list of per-file diffs in `diffs: [{file, diff}, …]`
    ///     (e.g. `rename_symbol`).
    List { entries: &'a [Value] },
    /// (d) `diff: "…"` is already a pre-rendered string.
    PreRendered(String),
    /// (e) `diff_text: "…"` is already a pre-rendered string.
    EchoText(String),
}

fn classify_diff_payload(data: &Value) -> Option<DiffPayload<'_>> {
    // (a) top-level embedded diff
    if let (Some(file), Some(hunks)) = (
        data.get("file_path").and_then(|v| v.as_str()),
        data.get("hunks").and_then(|v| v.as_array()),
    ) {
        return Some(DiffPayload::Embedded { file, src: data, hunks });
    }
    // (b) wrapped in `diff: {…}` with structured content
    if let Some(inner) = data.get("diff") {
        if let (Some(file), Some(hunks)) = (
            inner.get("file_path").and_then(|v| v.as_str()),
            inner.get("hunks").and_then(|v| v.as_array()),
        ) {
            return Some(DiffPayload::Wrapped { file, src: inner, hunks });
        }
        // (d) `diff` is a pre-rendered string
        if let Some(s) = inner.as_str() {
            return Some(DiffPayload::PreRendered(s.to_string()));
        }
    }
    // (c) top-level list of per-file diffs
    if let Some(arr) = data.get("diffs").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            return Some(DiffPayload::List { entries: arr });
        }
    }
    // (e) `diff_text` echo
    if let Some(s) = data.get("diff_text").and_then(|v| v.as_str()) {
        return Some(DiffPayload::EchoText(s.to_string()));
    }
    None
}

fn render_diff_value(data: &Value, color: bool) -> String {
    // Handlers may ship the diff in any of four shapes. We dispatch via
    // `classify_diff_payload` so each shape is a single match arm rather
    // than a nest of `if let` guards (a previous version accidentally
    // nested case (c) inside case (b), making it unreachable for
    // `rename_symbol`-shaped data — see test below).
    match classify_diff_payload(data) {
        Some(DiffPayload::Embedded { file, src, hunks }) => {
            render_one_diff(file, src, hunks, color)
        }
        Some(DiffPayload::Wrapped { file, src, hunks }) => {
            render_one_diff(file, src, hunks, color)
        }
        Some(DiffPayload::List { entries }) => {
            let mut out = String::new();
            for (i, d) in entries.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                if let (Some(file), Some(inner)) = (
                    d.get("file").and_then(|v| v.as_str()),
                    d.get("diff"),
                ) {
                    if let Some(hunks) = inner.get("hunks").and_then(|v| v.as_array()) {
                        out.push_str(&render_one_diff(file, inner, hunks, color));
                    } else {
                        out.push_str(&format!(
                            "{}── {} ──{}\n",
                            if color { LIGHT_CYAN } else { "" },
                            file,
                            if color { RESET } else { "" },
                        ));
                    }
                }
            }
            out
        }
        Some(DiffPayload::PreRendered(s)) | Some(DiffPayload::EchoText(s)) => s,
        None => String::new(),
    }
}

fn render_one_diff(file: &str, src: &Value, hunks: &[Value], color: bool) -> String {
    let additions = src.get("additions").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let deletions = src.get("deletions").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut diff = DiffResult {
        file_path: file.to_string(),
        additions,
        deletions,
        hunks: Vec::new(),
    };
    for h in hunks {
        let old_start = h.get("old_start").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let new_start = h.get("new_start").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let mut lines = Vec::new();
        if let Some(ls) = h.get("lines").and_then(|v| v.as_array()) {
            for l in ls {
                let op = match l.get("op").and_then(|v| v.as_str()).unwrap_or("context") {
                    "add" => DiffOp::Add,
                    "remove" => DiffOp::Remove,
                    _ => DiffOp::Context,
                };
                lines.push(DiffLine {
                    op,
                    old_line: l.get("old_line").and_then(|v| v.as_u64()).map(|n| n as usize),
                    new_line: l.get("new_line").and_then(|v| v.as_u64()).map(|n| n as usize),
                    content: l
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                });
            }
        }
        diff.hunks.push(DiffHunk {
            old_start,
            new_start,
            lines,
        });
    }
    render_split_diff(&diff, color, None)
}

fn render_default(data: &Value, _color: bool) -> String {
    // Pretty-printed JSON; ANSI color is intentionally ignored because
    // raw escape sequences in the output would corrupt the JSON and
    // break downstream parsers that consume it (logs, jq, etc).
    serde_json::to_string_pretty(data).unwrap_or_else(|_| "<unprintable>".to_string())
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
// Central dispatch
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
// LLM payload trimmer
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

fn trim_search(data: &Value) -> Value {
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
// Existing formatter structs — thin wrappers around the new core
// =============================================================================
//
// Kept for backward compatibility with any callers outside this module
// that build them explicitly. New code should call `render_tool_output`.

/// Formatter for diff output with color-coded additions/removals
pub struct DiffFormatter {
    color: bool,
}

impl DiffFormatter {
    /// Create a new DiffFormatter with default settings
    pub fn new() -> Self {
        Self { color: true }
    }

    /// Enable or disable color output
    pub fn with_color(mut self, color: bool) -> Self {
        self.color = color;
        self
    }

    /// Format a diff between original and modified content
    pub fn format(&self, original: &str, modified: &str, file_path: &str) -> String {
        let diff = compute_diff(original, modified, file_path);
        render_split_diff(&diff, self.color, None)
    }
}

impl Default for DiffFormatter {
    fn default() -> Self {
        Self::new()
    }
}

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
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn sample_diff() -> DiffResult {
        DiffResult {
            file_path: "src/foo.rs".to_string(),
            additions: 1,
            deletions: 1,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        op: DiffOp::Context,
                        old_line: Some(1),
                        new_line: Some(1),
                        content: "fn main() {".to_string(),
                    },
                    DiffLine {
                        op: DiffOp::Remove,
                        old_line: Some(2),
                        new_line: None,
                        content: "  let x = 1;".to_string(),
                    },
                    DiffLine {
                        op: DiffOp::Add,
                        old_line: None,
                        new_line: Some(2),
                        content: "  let x = 2;".to_string(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn test_compute_diff_basic() {
        let d = compute_diff("a\nb\nc\n", "a\nB\nc\n", "test.rs");
        assert_eq!(d.file_path, "test.rs");
        assert_eq!(d.additions, 1);
        assert_eq!(d.deletions, 1);
        assert!(d.has_changes());
    }

    #[test]
    fn test_compute_diff_no_changes() {
        let d = compute_diff("a\nb\n", "a\nb\n", "test.rs");
        assert!(!d.has_changes());
        assert_eq!(d.additions, 0);
        assert_eq!(d.deletions, 0);
    }

    #[test]
    fn test_render_split_diff_has_line_numbers() {
        let d = compute_diff("a\nb\nc\n", "a\nB\nc\n", "test.rs");
        let s = render_split_diff(&d, false, None);
        // Split view should have the hunk header, line numbers, and the
        // separator between left and right.
        assert!(s.contains("@@ -1,3 +1,3 @@"), "got: {}", s);
        assert!(s.contains("│"), "expected split separator");
        assert!(s.contains("  1 "), "expected old line 1");
        assert!(s.contains("  2 "), "expected old line 2");
    }

    #[test]
    fn test_render_split_diff_strips_trailing_newlines() {
        // diffy includes the trailing newline in the line content;
        // verify the renderer strips it so the row layout doesn't break.
        let d = compute_diff("a\nb\n", "a\nB\n", "test.rs");
        let s = render_split_diff(&d, false, None);
        // No raw \n should appear in the middle of a row's content.
        for line in s.lines() {
            assert!(
                !line.contains("│\n"),
                "row contained a mid-line newline: {:?}",
                line
            );
        }
    }

    #[test]
    fn test_diff_result_to_json() {
        let d = sample_diff();
        let j = d.to_json();
        assert_eq!(j["file_path"], "src/foo.rs");
        assert_eq!(j["additions"], 1);
        assert_eq!(j["deletions"], 1);
        assert_eq!(j["hunks"].as_array().unwrap().len(), 1);
        let first_line = &j["hunks"][0]["lines"][0];
        assert_eq!(first_line["op"], "context");
        assert_eq!(first_line["old_line"], 1);
        assert_eq!(first_line["new_line"], 1);
        assert_eq!(first_line["content"], "fn main() {");
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
    fn test_render_tool_output_falls_back_to_pretty_json() {
        let data = v(r#"{"custom_field": 42, "items": [1, 2, 3]}"#);
        let s = render_tool_output("leindex.unknown_tool", &data, &Value::Null);
        // Default renderer emits pretty JSON
        assert!(s.contains("\"custom_field\""));
        assert!(s.contains("42"));
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

    /// Regression test for the bug where the `diffs[]` branch in
    /// `render_diff_value` was unreachable for `rename_symbol`-shaped
    /// data. The rename handler returns a top-level `diffs` array with
    /// NO top-level `diff` field; the previous nested-`if-let`
    /// implementation short-circuited and rendered an empty string.
    #[test]
    fn test_render_diff_value_handles_rename_symbol_diffs_list() {
        let data = v(r#"{
            "diffs": [
                {
                    "file": "src/foo.rs",
                    "diff": {
                        "file_path": "src/foo.rs",
                        "additions": 1,
                        "deletions": 1,
                        "hunks": [{
                            "old_start": 1,
                            "new_start": 1,
                            "lines": [
                                {"op": "context", "old_line": 1, "new_line": 1, "content": "fn main() {"},
                                {"op": "remove",   "old_line": 2, "new_line": null, "content": "  let x = 1;"},
                                {"op": "add",      "old_line": null, "new_line": 2, "content": "  let x = 2;"},
                                {"op": "context",  "old_line": 3, "new_line": 3, "content": "}"}
                            ]
                        }]
                    }
                }
            ],
            "old_name": "x",
            "new_name": "y"
        }"#);
        let s = render_diff_value(&data, false);
        // Must produce a non-empty diff header for the listed file.
        assert!(s.contains("src/foo.rs"), "expected file path in render, got: {:?}", s);
        assert!(s.contains("@@"), "expected hunk header, got: {:?}", s);
        // Removed line content from the inner diff must appear in the
        // rendered output (split-view places it on the left side).
        assert!(s.contains("let x = 1;"), "expected removed line, got: {:?}", s);
        assert!(s.contains("let x = 2;"), "expected added line, got: {:?}", s);
    }

    /// Empty `diffs[]` array should fall through to the next shape
    /// (not panic) — it means the rename produced no file changes,
    /// which the renderer should represent as empty output rather
    /// than the default-renderer fallback.
    #[test]
    fn test_render_diff_value_handles_empty_diffs_list() {
        let data = v(r#"{"diffs": [], "old_name": "x", "new_name": "x"}"#);
        let s = render_diff_value(&data, false);
        // An empty diffs list is valid — the renderer should produce
        // an empty string (not the wrapped/diff_text branches).
        assert!(s.is_empty(), "got: {:?}", s);
    }

    /// `diff` field as a plain pre-rendered string (case d) must pass
    /// through unchanged.
    #[test]
    fn test_render_diff_value_passthrough_for_string_diff() {
        let data = v(r#"{"diff": "--- a\n+++ b\n"}"#);
        let s = render_diff_value(&data, false);
        assert_eq!(s, "--- a\n+++ b\n");
    }

    /// `diff_text` echo field (case e) must pass through unchanged.
    #[test]
    fn test_render_diff_value_passthrough_for_diff_text() {
        let data = v(r#"{"diff_text": "@@ -1 +1 @@\n-old\n+new\n"}"#);
        let s = render_diff_value(&data, false);
        assert_eq!(s, "@@ -1 +1 @@\n-old\n+new\n");
    }

    /// Regression test for the truncated `truncate_chars` /
    /// `truncate_with_ellipsis` implementations: both used to walk the
    /// input string twice (`chars().count()` then `chars().take()`).
    /// Verify the single-pass version still preserves the prior
    /// contracts: short inputs return as-is, long inputs are cut at
    /// the right character boundary and the right ellipsis is appended.
    #[test]
    fn test_truncate_with_ellipsis_contracts() {
        // ASCII short: returns as-is
        assert_eq!(truncate_with_ellipsis("hello", 10, "..."), "hello");
        // ASCII exactly at budget: returns as-is
        assert_eq!(truncate_with_ellipsis("hello", 5, "..."), "hello");
        // ASCII long: cuts at budget, appends ellipsis
        assert_eq!(truncate_with_ellipsis("helloworld", 5, "..."), "hello...");
        // Max 0: returns empty
        assert_eq!(truncate_with_ellipsis("hello", 0, "..."), "");
        // Multibyte at boundary: cuts at char boundary, not byte boundary
        assert_eq!(truncate_with_ellipsis("héllo", 3, "…"), "hél…");
        // Single-char ellipsis for `truncate` (vs three for `truncate_chars`)
        assert_eq!(truncate("hello world", 6), "hello…");
        // truncate_chars keeps max chars and appends three dots
        assert_eq!(truncate_chars("hello world", 5), "hello...");
    }

    /// The public `truncate` and `truncate_chars` must produce the
    /// same output for the same input shape as the previous
    /// double-walk implementation, to confirm the single-pass refactor
    /// is behaviorally identical.
    #[test]
    fn test_truncate_matches_previous_contract() {
        // These exact values are taken from the test cases that
        // exercised the old implementation; they must still pass.
        // (a) input shorter than max
        assert_eq!(truncate("short", 60), "short");
        // (b) input longer than max
        assert_eq!(truncate(&"a".repeat(100), 5), "aaaa…");
        // (c) `truncate_chars` short
        assert_eq!(truncate_chars("foo", 10), "foo");
        // (d) `truncate_chars` long — keeps max chars + 3 dots
        assert_eq!(truncate_chars(&"a".repeat(300), 240).len(), 243);
        assert!(truncate_chars(&"a".repeat(300), 240).ends_with("..."));
    }
}
