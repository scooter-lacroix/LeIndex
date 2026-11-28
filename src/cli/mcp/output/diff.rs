//! Structured diff types, computation, and rendering.
//!
//! `DiffResult` / `DiffHunk` / `DiffLine` are the data shape the LLM
//! sees (clean JSON, no ANSI). `compute_diff` is the single source of
//! truth — both MCP (via `to_json()`) and CLI (via `render_unified_diff`)
//! read from the same `DiffResult` so the two surfaces can never drift.
//!
//! §3.5: the CLI renders a COMPACT unified diff — changed lines in full
//! (never truncated), ±1 context line, long unchanged runs collapsed to
//! a counted line. The old side-by-side split renderer is gone.

use serde_json::Value;

use super::{LIGHT_CYAN, LIGHT_GREEN, LIGHT_GREY, LIGHT_RED, RESET};

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

/// Layout parameters for the unified hunk renderer.
#[derive(Clone, Copy)]
struct RowLayout {
    /// Context lines kept on each side of a collapsed unchanged run.
    max_context: usize,
    /// Whether to emit ANSI color escapes.
    color: bool,
}

/// Render a `DiffResult` as a compact unified diff (closest to
/// `git diff -U1`): changed lines in FULL — never truncated with `…` —
/// ±1 context line around changes, and long unchanged runs collapsed
/// to one counted line (§3.5).
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
    let layout = RowLayout {
        max_context: 1,
        color,
    };
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
        out.push_str(&render_hunk_split(hunk, &layout));
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

fn render_hunk_split(hunk: &DiffHunk, layout: &RowLayout) -> String {
    let mut out = String::new();
    // Walk the hunk in runs. Context gaps longer than `layout.max_context`
    // collapse to one counted line (§3.5); changed lines always render in
    // full — no truncation, ever.
    let mut i = 0;
    while i < hunk.lines.len() {
        let line = &hunk.lines[i];
        if line.op == DiffOp::Context {
            // Measure the whole context run.
            let mut run = 0usize;
            while i + run < hunk.lines.len() && hunk.lines[i + run].op == DiffOp::Context {
                run += 1;
            }
            if run > layout.max_context * 2 + 2 {
                // Keep `max_context` on each side, collapse the middle.
                // The +2 headroom: collapsing must always HIDE lines — a
                // run that would render as 1 kept + 1 counted + 1 kept is
                // longer than just printing the three lines.
                for line in &hunk.lines[i..i + layout.max_context] {
                    out.push_str(&unified_line(line, layout.color));
                }
                let hidden = run - layout.max_context * 2;
                out.push_str(&format!(
                    "{}⋯ {} unchanged line{} ⋯{}\n",
                    if layout.color { LIGHT_GREY } else { "" },
                    hidden,
                    if hidden == 1 { "" } else { "s" },
                    if layout.color { RESET } else { "" },
                ));
                for line in &hunk.lines[i + run - layout.max_context..i + run] {
                    out.push_str(&unified_line(line, layout.color));
                }
            } else {
                for line in &hunk.lines[i..i + run] {
                    out.push_str(&unified_line(line, layout.color));
                }
            }
            i += run;
            continue;
        }
        out.push_str(&unified_line(line, layout.color));
        i += 1;
    }
    out
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
    if let Some(arr) = data.get("diffs").and_then(|v| v.as_array())
        && !arr.is_empty()
    {
        return Some(DiffPayload::List { entries: arr });
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
    render_unified_diff(&diff, color)
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
        render_unified_diff(&diff, self.color)
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
    fn test_render_unified_diff_shows_changed_lines_in_full() {
        // §3.5: a changed line LONGER than 200 chars must appear in FULL,
        // never truncated with `…`.
        let long_old = format!("let old = \"{}\";", "x".repeat(220));
        let long_new = format!("let new = \"{}\";", "y".repeat(220));
        let d = compute_diff(&long_old, &long_new, "long.rs");
        let s = render_unified_diff(&d, false);
        assert!(
            s.contains(&long_new),
            "added line must be rendered in full, got: {:?}",
            s
        );
        assert!(
            s.contains(&long_old),
            "removed line must be rendered in full, got: {:?}",
            s
        );
        assert!(!s.contains('…'), "no truncation glyph allowed: {s}");
    }

    #[test]
    fn test_render_unified_diff_compact_context_collapses_runs() {
        // ±1 context: two changes 5 unchanged lines apart land in ONE
        // diffy hunk (diffy merges gaps <= 2*3); the middle run collapses
        // to a counted line while each change keeps one context line.
        // (Gaps >= 7 become separate hunks — collapsing by construction.)
        let original = format!(
            "fn one() {{}}\n{}\nfn two() {{}}\n{}\nfn three() {{}}\n",
            (1..=5)
                .map(|i| format!("ctx_a{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            (1..=5)
                .map(|i| format!("ctx_b{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let modified = original
            .replacen("ctx_a1", "CTX_A1", 1)
            .replacen("ctx_b1", "CTX_B1", 1);
        let d = compute_diff(&original, &modified, "ctx.rs");
        assert_eq!(d.hunks.len(), 1, "fixture must yield one merged hunk");
        let s = render_unified_diff(&d, false);
        assert!(
            s.contains("⋯ 3 unchanged lines ⋯"),
            "5-line middle run must collapse keeping 2, counting 3: {s}"
        );
        // ±1 context: exactly one context line before each change.
        let body_lines: Vec<&str> = s.lines().collect();
        let change_idx = body_lines
            .iter()
            .position(|l| l.starts_with("-ctx_a1"))
            .expect("removed line present");
        assert!(
            body_lines[change_idx - 1].starts_with(' '),
            "context before change: {}",
            body_lines[change_idx - 1]
        );
        assert_eq!(
            body_lines[change_idx - 1],
            " fn one() {}",
            "exactly ONE context line before the first change: {s}"
        );
        let change2_idx = body_lines
            .iter()
            .position(|l| l.starts_with("-ctx_b1"))
            .expect("second change present");
        assert_eq!(
            body_lines[change2_idx - 1],
            " fn two() {}",
            "exactly ONE context line before the second change: {s}"
        );
    }

    #[test]
    fn test_render_unified_diff_keeps_short_context_runs_intact() {
        let original = "a\nb\nc\n";
        let modified = "a\nB\nc\n";
        let d = compute_diff(original, modified, "small.rs");
        let s = render_unified_diff(&d, false);
        assert!(
            !s.contains("unchanged line"),
            "short run must not collapse: {s}"
        );
        assert!(s.contains(" a\n"), "context line a must be shown: {s}");
        assert!(s.contains(" c\n"), "context line c must be shown: {s}");
        assert!(s.contains("-b\n"), "removed line must be shown: {s}");
        assert!(s.contains("+B\n"), "added line must be shown: {s}");
    }

    #[test]
    fn test_render_one_diff_uses_unified_for_handlers() {
        // §3.5: the handler dispatch path (edit_preview / rename / write)
        // must render unified — no side-by-side `│` separator remains.
        let data = serde_json::json!({
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
        });
        let s = render_diff_value(&data, false);
        assert!(!s.contains('│'), "side-by-side separator must be gone: {s}");
        assert!(s.contains("-  let x = 1;"), "removed line: {s}");
        assert!(s.contains("+  let x = 2;"), "added line: {s}");
        assert!(s.contains("@@"), "hunk header: {s}");
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
    fn test_render_diff_value_handles_rename_symbol_diffs_list() {
        // Regression test for the bug where the `diffs[]` branch in
        // `render_diff_value` was unreachable for `rename_symbol`-shaped
        // data. The rename handler returns a top-level `diffs` array with
        // NO top-level `diff` field; the previous nested-`if-let`
        // implementation short-circuited and rendered an empty string.
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

    // =====================================================================
    // §3.5: the side-by-side renderer is GONE — the tests that locked its
    // internals (row width, split markers, red/green sides) died with it.
    // The unified contract is locked by test_render_unified_diff_* above.
    // =====================================================================

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
