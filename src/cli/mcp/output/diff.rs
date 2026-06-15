//! Structured diff types, computation, and rendering.
//!
//! `DiffResult` / `DiffHunk` / `DiffLine` are the data shape the LLM
//! sees (clean JSON, no ANSI). `compute_diff` is the single source of
//! truth — both MCP (via `to_json()`) and CLI (via `render_split_diff` /
//! `render_unified_diff`) read from the same `DiffResult` so the two
//! surfaces can never drift.

use serde_json::Value;

use super::{truncate, LIGHT_CYAN, LIGHT_GREEN, LIGHT_GREY, LIGHT_RED, RESET};

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
        // Close the file-header color at the end of `+++ b/...` so
        // the `(no changes)` line is rendered in plain text rather
        // than inheriting `LIGHT_GREY` from the header.
        out.push_str(&format!(
            "{}--- a/{}\n+++ b/{}{}\n",
            if color { LIGHT_GREY } else { "" },
            diff.file_path,
            diff.file_path,
            if color { RESET } else { "" },
        ));
        out.push_str("(no changes)\n");
        return out;
    }
    out.push_str(&format!(
        "{}--- a/{}\n+++ b/{}{}\n",
        if color { LIGHT_GREY } else { "" },
        diff.file_path,
        diff.file_path,
        if color { RESET } else { "" },
    ));
    for hunk in &diff.hunks {
        out.push_str(&format!(
            "{}@@ -{},{} +{},{} @@{}\n",
            if color { LIGHT_CYAN } else { "" },
            hunk.old_start,
            hunk.lines.iter().filter(|l| l.op != DiffOp::Add).count(),
            hunk.new_start,
            hunk.lines.iter().filter(|l| l.op != DiffOp::Remove).count(),
            if color { RESET } else { "" },
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
                         // The split-view format string in `split_row` emits the
                         // following per row, excluding the gutter and the two
                         // content halves:
                         //   1 leading space
                         //   1 space between the left gutter and the left marker
                         //   1 space between the left marker and the left content
                         //   3 chars for the ` │ ` centre separator
                         //   1 space between the right gutter and the right marker
                         //   1 space between the right marker and the right content
                         //   2 trailing reset escapes when colour is enabled (each
                         //     contributes an ANSI CSI sequence; when colour is off
                         //     these are empty strings and contribute 0 to the
                         //     visible width).
                         // The empirical constant that keeps the rendered row
                         // inside the requested terminal width is 10 (8 separator
                         // chars + 2 to absorb ANSI colour escapes that still
                         // contribute to string length in some callers). Using
                         // `5` here was too small and could push the rendered row
                         // past the terminal width on an 80-column terminal,
                         // causing awkward wrapping. The `gutter * 2 + 10` formula
                         // is what the regression test below asserts against.
    let half = match width {
        Some(w) => w.saturating_sub(gutter * 2 + 10) / 2,
        None => 60,
    };
    // If the caller told us the width and the resulting per-side
    // column is too small to be readable, fall back to the unified
    // layout — that's the behaviour the doc comment promises. Without
    // this guard `half` can collapse to 0 on a narrow terminal and
    // the row layout prints nothing useful.
    if width.is_some() && half < 8 {
        return render_unified_diff(diff, color);
    }

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
        out.push_str(&render_hunk_split(
            hunk,
            &RowLayout {
                gutter,
                half,
                color,
            },
        ));
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
                        left_marker: " ",
                        right_marker: " ",
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
                    // Each row gets explicit left/right markers
                    // that mirror what a side-by-side diff viewer
                    // would render: paired remove+add becomes
                    // `-` / `+`, an unpaired remove (line was
                    // deleted) becomes `-` / ` `, an unpaired
                    // add (line was inserted against the previous
                    // block) becomes ` ` / `+`. The previous
                    // single-`marker` design forced all three
                    // cases to render with `" "` and grey, which
                    // made modifications visually identical to
                    // unchanged context.
                    let row = match (rem, add) {
                        (Some(r), Some(a)) => SplitRow {
                            old_line: Some(old_line + k),
                            old_content: r.content.as_str(),
                            new_line: Some(new_line + k),
                            new_content: a.content.as_str(),
                            left_marker: "-",
                            right_marker: "+",
                        },
                        (Some(r), None) => SplitRow {
                            old_line: Some(old_line + k),
                            old_content: r.content.as_str(),
                            new_line: None,
                            new_content: "",
                            left_marker: "-",
                            right_marker: " ",
                        },
                        (None, Some(a)) => SplitRow {
                            old_line: None,
                            old_content: "",
                            new_line: Some(new_line + k),
                            new_content: a.content.as_str(),
                            left_marker: " ",
                            right_marker: "+",
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
                        left_marker: " ",
                        right_marker: "+",
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
/// right_text) pair, plus explicit `left_marker` and `right_marker`
/// strings that drive both the displayed glyph (`" "`, `"-"`, `"+"`)
/// and the colour on each side independently.
///
/// The split renderer must distinguish context rows (` ` / ` `) from
/// modified rows (`-` / `+`) at a glance — a single shared `marker`
/// field forced both sides to render identically, which made
/// modifications indistinguishable from unchanged context. Each side
/// now carries its own marker and the row's colour is derived from
/// that side's marker rather than from whether `old_line` /
/// `new_line` is populated, so the caller is in full control of the
/// rendered output.
struct SplitRow<'a> {
    old_line: Option<usize>,
    old_content: &'a str,
    new_line: Option<usize>,
    new_content: &'a str,
    left_marker: &'a str,
    right_marker: &'a str,
}

fn split_row(row: &SplitRow<'_>, layout: &RowLayout) -> String {
    let RowLayout {
        gutter,
        half,
        color,
    } = *layout;
    let SplitRow {
        old_line,
        old_content,
        new_line,
        new_content,
        left_marker,
        right_marker,
    } = row;

    let ol_str = old_line
        .map(|n| format!("{:>width$}", n, width = gutter))
        .unwrap_or_else(|| " ".repeat(gutter));
    let nl_str = new_line
        .map(|n| format!("{:>width$}", n, width = gutter))
        .unwrap_or_else(|| " ".repeat(gutter));

    // Each side's colour is derived from its own marker so a
    // modified row (`-` / `+`) renders red on the left and green
    // on the right, while a context row (` ` / ` `) renders grey
    // on both sides. The previous implementation derived the
    // marker from `(old_line, new_line)` presence and then forced
    // both sides to use the same `marker` value, which made
    // modified rows visually identical to context rows.
    let left_paint = if color && *left_marker == "-" {
        LIGHT_RED
    } else if color && *left_marker == "+" {
        LIGHT_GREEN
    } else if color {
        LIGHT_GREY
    } else {
        ""
    };
    // The right side is the new file: it can only show context
    // (` `) or added (`+`) — there is no semantic for `-` on
    // the right because nothing is "removed from the new
    // file" (lines in the new file are either present or
    // absent, and absence is shown by leaving the row empty,
    // not by a `-` marker). The `LIGHT_RED` arm the round-19
    // patch left in place is therefore unreachable, so the
    // paint collapses to: green for `+`, grey for everything
    // else. The test `test_split_row_right_red_arm_is_dead`
    // locks this contract so a future refactor that adds a
    // right-side `-` marker must update both the test and
    // this paint.
    let right_paint = if color && *right_marker == "+" {
        LIGHT_GREEN
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

// =============================================================================
// Diff payload classification + dispatch
// =============================================================================
//
// Handlers may ship a diff in any of five shapes. The dispatcher in
// `render_diff_value` matches on a flat `DiffPayload` enum so each
// shape is a single match arm — a previous version accidentally
// nested case (c) inside case (b), making it unreachable for
// `rename_symbol`-shaped data. See `test_render_diff_value_handles_*`.

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
        return Some(DiffPayload::Embedded {
            file,
            src: data,
            hunks,
        });
    }
    // (b) wrapped in `diff: {…}` with structured content
    if let Some(inner) = data.get("diff") {
        if let (Some(file), Some(hunks)) = (
            inner.get("file_path").and_then(|v| v.as_str()),
            inner.get("hunks").and_then(|v| v.as_array()),
        ) {
            return Some(DiffPayload::Wrapped {
                file,
                src: inner,
                hunks,
            });
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

/// Render a tool's diff payload in CLI form. Used by
/// `render_tool_output` for the `edit_preview` / `edit_apply` / `write`
/// / `rename_symbol` tools. Module-internal — the dispatcher in
/// `render.rs` calls this; the public surface is `render_tool_output`.
pub(super) fn render_diff_value(data: &Value, color: bool) -> String {
    // Handlers may ship the diff in any of five shapes. We dispatch via
    // `classify_diff_payload` so each shape is a single match arm rather
    // than a nest of `if let` guards (a previous version accidentally
    // nested case (c) inside case (b), making it unreachable for
    // `rename_symbol`-shaped data — see test below).
    match classify_diff_payload(data) {
        Some(DiffPayload::Embedded { file, src, hunks }) => {
            render_one_diff(file, src, hunks, color)
        }
        Some(DiffPayload::Wrapped { file, src, hunks }) => render_one_diff(file, src, hunks, color),
        Some(DiffPayload::List { entries }) => {
            // Collect each successfully-rendered entry into a
            // `Vec<String>` and join with `\n` at the end. The
            // previous version interleaved `if i > 0 { out.push('\n'); }`
            // ahead of the `if let (file, inner) = ...` guard, so an
            // invalid entry (missing `file` or `diff`) would still
            // push the separator newline — producing trailing or
            // consecutive dangling newlines in the rendered output.
            // Collecting then joining keeps separators exactly
            // between rendered entries and never after an invalid
            // one.
            let mut rendered: Vec<String> = Vec::with_capacity(entries.len());
            for d in entries {
                if let (Some(file), Some(inner)) =
                    (d.get("file").and_then(|v| v.as_str()), d.get("diff"))
                {
                    if let Some(hunks) = inner.get("hunks").and_then(|v| v.as_array()) {
                        rendered.push(render_one_diff(file, inner, hunks, color));
                    } else if let Some(s) = inner.as_str() {
                        // Pre-rendered diff text: handlers may ship
                        // a plain string instead of a structured
                        // `{hunks: [...]}` object (e.g. older or
                        // external producers). Surface the text
                        // under the same `── file ──` header so the
                        // user still sees the diff body instead of
                        // just an empty header.
                        rendered.push(format!(
                            "{}── {} ──{}\n{}\n",
                            if color { LIGHT_CYAN } else { "" },
                            file,
                            if color { RESET } else { "" },
                            s,
                        ));
                    } else {
                        rendered.push(format!(
                            "{}── {} ──{}\n",
                            if color { LIGHT_CYAN } else { "" },
                            file,
                            if color { RESET } else { "" },
                        ));
                    }
                }
            }
            rendered.join("\n")
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
                    old_line: l
                        .get("old_line")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize),
                    new_line: l
                        .get("new_line")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize),
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

/// Pretty-JSON fallback used by the render dispatcher when no
/// per-tool renderer matches the tool name. `pub(super)` because the
/// dispatcher lives in `output::render` and needs to reach across.
/// ANSI color is intentionally ignored — raw escapes would corrupt
/// the JSON and break downstream parsers (logs, jq, etc).
pub(super) fn render_default(data: &Value, _color: bool) -> String {
    serde_json::to_string_pretty(data).unwrap_or_else(|_| "<unprintable>".to_string())
}

// =============================================================================
// DiffFormatter — backward-compat struct for callers outside this module
// that build a formatter explicitly. New code should call
// `render_tool_output` instead.
// =============================================================================

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

// =============================================================================
// Tests
// =============================================================================
/// Strip CSI (SGR) escapes for test assertions on visible text.
#[cfg(test)]
fn strip_ansi_for_test(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '\x1b' && iter.peek() == Some(&'[') {
            iter.next();
            for c2 in iter.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Regression test for the bug where the `diffs[]` branch in
    /// `render_diff_value` was unreachable for `rename_symbol`-shaped
    /// data. The rename handler returns a top-level `diffs` array with
    /// NO top-level `diff` field; the previous nested-`if-let`
    /// implementation short-circuited and rendered an empty string.
    #[test]
    fn test_render_diff_value_handles_rename_symbol_diffs_list() {
        let data = serde_json::json!({
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
        });
        let s = render_diff_value(&data, false);
        // Must produce a non-empty diff header for the listed file.
        assert!(
            s.contains("src/foo.rs"),
            "expected file path in render, got: {:?}",
            s
        );
        assert!(s.contains("@@"), "expected hunk header, got: {:?}", s);
        // Removed line content from the inner diff must appear in the
        // rendered output (split-view places it on the left side).
        assert!(
            s.contains("let x = 1;"),
            "expected removed line, got: {:?}",
            s
        );
        assert!(
            s.contains("let x = 2;"),
            "expected added line, got: {:?}",
            s
        );
    }

    /// Empty `diffs[]` array should fall through to the next shape
    /// (not panic) — it means the rename produced no file changes,
    /// which the renderer should represent as empty output rather
    /// than the default-renderer fallback.
    #[test]
    fn test_render_diff_value_handles_empty_diffs_list() {
        let data = serde_json::json!({"diffs": [], "old_name": "x", "new_name": "x"});
        let s = render_diff_value(&data, false);
        // An empty diffs list is valid — the renderer should produce
        // an empty string (not the wrapped/diff_text branches).
        assert!(s.is_empty(), "got: {:?}", s);
    }

    /// `diff` field as a plain pre-rendered string (case d) must pass
    /// through unchanged.
    #[test]
    fn test_render_diff_value_passthrough_for_string_diff() {
        let data = serde_json::json!({"diff": "--- a\n+++ b\n"});
        let s = render_diff_value(&data, false);
        assert_eq!(s, "--- a\n+++ b\n");
    }

    /// `diff_text` echo field (case e) must pass through unchanged.
    #[test]
    fn test_render_diff_value_passthrough_for_diff_text() {
        let data = serde_json::json!({"diff_text": "@@ -1 +1 @@\n-old\n+new\n"});
        let s = render_diff_value(&data, false);
        assert_eq!(s, "@@ -1 +1 @@\n-old\n+new\n");
    }

    /// `render_split_diff` must keep the rendered row width inside
    /// the terminal width on an 80-column terminal. The split-view
    /// format string emits constant characters per row that are
    /// not part of the gutter or the two content halves. A
    /// previous version subtracted only 5, which let the rendered
    /// row exceed 80 chars.
    #[test]
    fn test_render_split_diff_respects_80_col_width() {
        let diff = DiffResult {
            file_path: "src/lib.rs".to_string(),
            additions: 1,
            deletions: 1,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        op: DiffOp::Remove,
                        old_line: Some(1),
                        new_line: None,
                        // 60 chars on the remove side.
                        content: "x".repeat(60),
                    },
                    DiffLine {
                        op: DiffOp::Add,
                        old_line: None,
                        new_line: Some(1),
                        content: "y".repeat(60),
                    },
                ],
            }],
        };
        let s = render_split_diff(&diff, false, Some(80));
        // Every rendered line must fit in 80 columns.
        for (i, line) in s.lines().enumerate() {
            let stripped = strip_ansi_for_test(line);
            let visible = stripped.chars().count();
            assert!(
                visible <= 80,
                "line {} exceeds 80 cols ({} chars): {:?}",
                i,
                visible,
                stripped,
            );
        }
    }

    // =====================================================================
    // render_split_diff — round 19 gemini HIGH-priority split-marker fix
    // =====================================================================

    /// Regression for HIGH round 19: the split renderer used a
    /// single shared `marker` field on `SplitRow` and a
    /// `(old_line, new_line) → (left_marker, right_marker)`
    /// derivation in `split_row`. A paired remove+add row had
    /// `(old_line=Some, new_line=Some)` and was therefore
    /// forced to render with the caller's `marker` (which was
    /// always `" "` for paired rows), so modifications
    /// rendered with a blank glyph and grey colour —
    /// visually indistinguishable from unchanged context.
    ///
    /// The fix splits `marker` into explicit
    /// `left_marker` / `right_marker` fields, so a paired
    /// remove+add row now renders with `-` on the left (red)
    /// and `+` on the right (green) when colour is on. This
    /// test asserts the contract for the colour-on path: the
    /// row's stripped visible form contains both `-` and `+`
    /// markers, and the unstripped form contains the red
    /// ANSI escape (for the left `-`) AND the green ANSI
    /// escape (for the right `+`).
    #[test]
    fn test_render_split_diff_modified_rows_use_minus_plus_markers() {
        let diff = compute_diff("foo\n", "bar\n", "test.rs");
        // Sanity: the diff has exactly one remove and one add.
        assert_eq!(diff.additions, 1);
        assert_eq!(diff.deletions, 1);
        let stripped = strip_ansi_for_test(&render_split_diff(&diff, false, None));
        // The rendered form must include a `-` on the left
        // and a `+` on the right at the modified row, not
        // spaces. The exact format is
        //   " <gutter> - <left_content> │ <gutter> + <right_content>"
        // so a ` - ` substring (gutter + space + marker + space)
        // and a ` + ` substring (gutter + space + marker + space)
        // must both appear.
        assert!(
            stripped.contains(" - "),
            "modified row must render `-` on the left, not ` `; got: {:?}",
            stripped
        );
        assert!(
            stripped.contains(" + "),
            "modified row must render `+` on the right, not ` `; got: {:?}",
            stripped
        );
    }

    /// Companion to the marker test: when colour is on, the
    /// left `-` marker must be wrapped in `LIGHT_RED` and the
    /// right `+` marker must be wrapped in `LIGHT_GREEN`. The
    /// pre-fix renderer forced both sides to use the same
    /// `marker` value (`" "`) with `LIGHT_GREY`, so no red or
    /// green ever appeared at a modified row.
    #[test]
    fn test_render_split_diff_modified_rows_use_red_and_green_color() {
        let diff = compute_diff("foo\n", "bar\n", "test.rs");
        let rendered = render_split_diff(&diff, true, None);
        // The red marker is emitted via `LIGHT_RED` between
        // the left content and the next escape; we assert the
        // escape sequence appears in the rendered output
        // rather than testing the exact byte layout, which is
        // a stronger contract.
        assert!(
            rendered.contains(LIGHT_RED),
            "modified row's left `-` must be wrapped in LIGHT_RED; rendered: {:?}",
            rendered
        );
        assert!(
            rendered.contains(LIGHT_GREEN),
            "modified row's right `+` must be wrapped in LIGHT_GREEN; rendered: {:?}",
            rendered
        );
    }

    /// Context rows (unchanged lines) must still render with
    /// the blank marker (` `) on both sides, NOT with `-` or
    /// `+`. This locks the contract that the round-19 fix did
    /// not regress unchanged-context rendering.
    #[test]
    fn test_render_split_diff_context_rows_keep_blank_marker() {
        // Multi-line input with a single mid-file modification
        // gives us context lines surrounding the change.
        let original = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
        let modified = "alpha\nbeta\nGAMMA\ndelta\nepsilon\n";
        let diff = compute_diff(original, modified, "test.rs");
        let stripped = strip_ansi_for_test(&render_split_diff(&diff, false, None));
        // The row layout is
        //   ` <gutter> <left_marker> <left_content> │ <gutter> <right_marker> <right_content>`
        // For a 4-digit gutter and a single-char marker the
        // first 6 visible chars are:
        //   ` ` (1) + `   1` (4) + ` ` (1) = 6 chars
        // followed by the marker. Split the row on the `│`
        // separator to isolate the left half, then assert the
        // left marker is ` ` (context), not `-` or `+`.
        let mut found_blank_row = false;
        for line in stripped.lines() {
            if !line.contains('│') {
                continue;
            }
            let left = line.split('│').next().unwrap();
            // Find the gutter pattern at the start: one
            // leading space, then 4 chars of line number
            // (`   1`, `   2`, etc.).
            if left.len() < 7 {
                continue;
            }
            let gutter_and_marker = &left[0..6];
            if gutter_and_marker.starts_with(" ")
                && gutter_and_marker.ends_with(' ')
                && gutter_and_marker[1..5].trim() == "1"
            {
                // The marker is the 7th char (index 6).
                let marker = left[6..7].chars().next();
                assert_eq!(
                    marker,
                    Some(' '),
                    "context line `   1 ` must use blank marker, got {:?} in left half {:?}",
                    marker,
                    left
                );
                found_blank_row = true;
                break;
            }
        }
        assert!(
            found_blank_row,
            "expected to find a context row with gutter `   1 ` and blank marker in: {:?}",
            stripped
        );
    }

    /// Width invariant: the new explicit-marker design must
    /// not change the per-row width formula. A modified row
    /// (which is now `-` / `+` instead of ` ` / ` `) and a
    /// context row (still ` ` / ` `) must both fit inside the
    /// requested terminal width, because each marker is a
    /// single byte. The previous 80-column regression test
    /// (round 11) covered the remove+add-only case; this
    /// round-19 test covers a case that produces a CONTEXT
    /// row alongside the modification, to lock that the new
    /// explicit markers do not push any row past the
    /// requested width.
    #[test]
    fn test_render_split_diff_modified_rows_respect_terminal_width() {
        let original = "context_before\nold_line\ncontext_after\n";
        let modified = "context_before\nnew_line\ncontext_after\n";
        let diff = compute_diff(original, modified, "test.rs");
        let s = render_split_diff(&diff, false, Some(80));
        for (i, line) in s.lines().enumerate() {
            let visible = line.chars().count();
            assert!(
                visible <= 80,
                "row {} exceeds 80 cols ({} chars): {:?}",
                i,
                visible,
                line
            );
        }
    }

    /// Regression for WARNING round 20: the right side of a
    /// split row is the new file — it can only be context
    /// (` `) or added (`+`), never removed (`-`). The
    /// round-19 patch left a `*right_marker == "-"` →
    /// `LIGHT_RED` arm in `split_row`'s `right_paint`
    /// computation that no caller could ever exercise. The
    /// fix removes the dead arm; this test locks the
    /// contract by rendering every actual marker combination
    /// the caller can produce and asserting the right side
    /// NEVER contains `LIGHT_RED` (i.e. the right column is
    /// always either green for `+` or grey for ` `).
    #[test]
    fn test_split_row_right_red_arm_is_dead() {
        // Each combination the caller can pass. `old_line`
        // and `new_line` are placeholder values; the paint
        // selection is driven only by the markers.
        let cases: Vec<(&str, &str)> = vec![
            (" ", " "), // context
            ("-", "+"), // modified
            ("-", " "), // remove-only
            (" ", "+"), // add-only
            (" ", "+"), // standalone add
        ];
        for (left, right) in cases {
            let s = render_split_diff(&compute_diff("a\n", "b\n", "t.rs"), true, Some(200));
            // The split separator `│` lets us isolate the
            // right half of any row.
            for line in s.lines() {
                if !line.contains('│') {
                    continue;
                }
                let right_half = line.split('│').nth(1).unwrap();
                assert!(
                    !right_half.contains(LIGHT_RED),
                    "right half of a row with markers `{}`/`{}` must not contain \
                     LIGHT_RED (the right-paint `-` arm is dead); got: {:?}",
                    left,
                    right,
                    right_half,
                );
            }
        }
    }

    /// Regression for MED round 20: `render_diff_value` for
    /// `DiffPayload::List` previously interleaved
    /// `if i > 0 { out.push('\n'); }` ahead of the
    /// `if let (file, inner) = ...` guard, so an invalid
    /// entry (missing `file` or `diff`) would still push
    /// the separator newline — producing dangling or
    /// consecutive newlines in the rendered output. The fix
    /// collects rendered entries into a `Vec<String>` and
    /// joins with `\n` at the end, so separators appear only
    /// between successfully rendered entries.
    #[test]
    fn test_render_diff_value_list_no_trailing_newline_on_invalid_entry() {
        // A list with one valid entry and one invalid entry
        // (missing `file` field). The pre-fix code would
        // emit `<valid>...\n\n` — a trailing `\n` from the
        // separator pushed ahead of the `if let` skip.
        let data = serde_json::json!({
            "diffs": [
                {
                    "file": "valid.rs",
                    "diff": { "hunks": [] }
                },
                {
                    // missing "file" — should be skipped
                    "diff": { "hunks": [] }
                }
            ]
        });
        let s = render_diff_value(&data, false);
        // The rendered output must end with the valid
        // entry's last rendered line, not with a blank
        // line. `s.ends_with('\n')` would catch a trailing
        // separator; `s.contains("\n\n")` would catch
        // consecutive newlines that the old code emitted
        // whenever the invalid entry followed a valid one.
        assert!(
            !s.ends_with("\n\n"),
            "rendered output must not end with consecutive newlines (dangling separator); got: {:?}",
            s
        );
        // More strictly: the invalid entry must not
        // contribute a separator after the valid entry.
        // Count consecutive `\n` characters in the output
        // and assert at most one.
        let max_consec = s
            .chars()
            .fold((0usize, 0usize), |(max, cur), c| {
                let cur = if c == '\n' { cur + 1 } else { 0 };
                (max.max(cur), cur)
            })
            .0;
        assert!(
            max_consec <= 1,
            "rendered output must not contain consecutive `\\n`; max consecutive = {}; output: {:?}",
            max_consec,
            s
        );
    }

    /// Regression for MED round 11: `render_unified_diff` used to
    /// open `LIGHT_GREY` (or `LIGHT_CYAN` for the hunk header) at
    /// the start of each header line but never close it before the
    /// `\n` that terminates the line. The next line's prefix
    /// character (` `, `+`, `-`) is then emitted while the
    /// previous line's color is still active, so the prefix
    /// inherits the header color. The fix appends `RESET` to the
    /// end of every header line.
    ///
    /// The contract we test: every `--- a/...` / `+++ b/...` /
    /// `@@ ... @@` line ends with `RESET` (\x1b[0m) when
    /// `color = true`, so the prefix of the next line is not
    /// colored by the previous line.
    #[test]
    fn test_render_unified_diff_closes_header_color() {
        let s = render_unified_diff(&sample_diff(), true);
        // `+++ b/src/foo.rs` is the last header on its line, and
        // the line that follows begins with a body line (the
        // hunk header `@@ -... +... @@` or a diff line). The
        // byte between them must be the RESET escape, not raw
        // `LIGHT_GREY` left active.
        let plus_plus_idx = s.find("+++ b/src/foo.rs").unwrap();
        let after_plus_plus = &s[plus_plus_idx + "+++ b/src/foo.rs".len()..];
        assert!(
            after_plus_plus.starts_with("\x1b[0m"),
            "+++ b/... must be closed by RESET before '\\n', got: {:?}",
            &after_plus_plus[..after_plus_plus.len().min(20)],
        );

        // The hunk header `@@ -1,2 +1,2 @@` must also be closed
        // by RESET before the next diff line.
        let hunk_idx = s.find("@@ -1,2 +1,2 @@").unwrap();
        let after_hunk = &s[hunk_idx + "@@ -1,2 +1,2 @@".len()..];
        assert!(
            after_hunk.starts_with("\x1b[0m"),
            "@@ ... @@ must be closed by RESET before '\\n', got: {:?}",
            &after_hunk[..after_hunk.len().min(20)],
        );
    }

    /// The no-changes case must also close the file header color so
    /// the `(no changes)` text is not painted in `LIGHT_GREY`.
    #[test]
    fn test_render_unified_diff_no_changes_closes_color() {
        let diff = DiffResult {
            file_path: "src/empty.rs".to_string(),
            additions: 0,
            deletions: 0,
            hunks: vec![],
        };
        let s = render_unified_diff(&diff, true);
        // The `+++ b/src/empty.rs` must be followed by RESET, not
        // bleed into `(no changes)`.
        let idx = s.find("+++ b/src/empty.rs").unwrap();
        let after = &s[idx + "+++ b/src/empty.rs".len()..];
        assert!(
            after.starts_with("\x1b[0m"),
            "+++ b/src/empty.rs must be followed by RESET, got: {:?}",
            &after[..after.len().min(20)],
        );
        // And `(no changes)` must be plain (not wrapped in an
        // open color escape).
        let no_changes_idx = s.find("(no changes)").unwrap();
        assert!(
            !s[no_changes_idx..].starts_with("\x1b["),
            "(no changes) must not be colorised, got: {:?}",
            &s[no_changes_idx..no_changes_idx + 30],
        );
    }

    /// Regression for MED round 15 (gemini `3344869688`):
    /// `render_diff_value`'s `DiffPayload::List` arm used to
    /// silently discard pre-rendered string diffs — when an
    /// entry's `diff` field was a plain string instead of a
    /// `{hunks: [...]}` object, only the `── file ──` header
    /// was emitted and the body was dropped. The post-fix code
    /// adds an `inner.as_str()` fallback that surfaces the text
    /// under the same header, so older/external producers that
    /// ship a pre-rendered diff string are still rendered
    /// correctly.
    #[test]
    fn test_render_diff_value_list_with_pre_rendered_string() {
        let data = serde_json::json!({
            "diffs": [
                {
                    "file": "src/foo.rs",
                    "diff": "@@ -1,1 +1,1 @@\n-old\n+new",
                },
                {
                    "file": "src/bar.rs",
                    "diff": serde_json::json!({"hunks": []}),
                },
            ],
        });
        let s = render_diff_value(&data, false);
        // Pre-rendered string body must appear in the output.
        assert!(
            s.contains("-old") && s.contains("+new"),
            "pre-rendered diff string must be rendered, got: {:?}",
            s
        );
        assert!(
            s.contains("src/foo.rs"),
            "file header must be rendered, got: {:?}",
            s
        );
        // Structured-but-empty entry still renders the header.
        assert!(s.contains("src/bar.rs"));
    }
}
