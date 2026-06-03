//! Beautiful output formatting for LeIndex tools
//!
//! This module provides visually appealing, human-readable output formatting
//! for various LeIndex tools including search results, diffs, project maps, and diagnostics.
//!
//! The architecture is layered:
//!
//! 1. **Structured types** — `DiffResult`, `DiffHunk`, `DiffLine` capture the
//!    data shape that the LLM receives (clean JSON, no ANSI).
//! 2. **Compute** — `compute_diff` and the trim functions in [`trim`] turn
//!    raw handler output into these structured types.
//! 3. **Render** — per-tool render functions in [`render`] take a `Value`
//!    and produce a human-readable colored string for the CLI. They share
//!    the same structured core so MCP and CLI stay in lock-step.
//! 4. **Dispatch** — `render_tool_output(name, value, args)` (in [`render`])
//!    is the single entry point used by `leindex tools run` and any other
//!    CLI surface.

use serde_json::Value;

pub mod diff;
pub mod render;
pub mod trim;

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
// Cross-submodule utilities
// =============================================================================
//
// These are used by both `diff`, `render`, and `trim` submodules, so they
// live here at the parent. They are `pub(crate)` / `pub(super)` rather than
// fully public because they are implementation details.

/// `truncate` keeps `max - 1` characters and appends the single-char `…`
/// (Unicode horizontal ellipsis) so the result is at most `max` characters
/// wide. Used by the split-view diff layout to keep each side equal width.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let keep = max.saturating_sub(1);
    truncate_with_ellipsis(s, keep, "…")
}

/// `truncate_chars` keeps the first `max_chars` characters and appends
/// `...` so the result is `max_chars + 3` characters wide. Used by
/// render and trim code paths to cap context snippets.
pub(crate) fn truncate_chars(input: &str, max_chars: usize) -> String {
    truncate_with_ellipsis(input, max_chars, "...")
}

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
    for (char_count, (i, ch)) in input.char_indices().enumerate() {
        if char_count == max {
            cut = Some(i);
            break;
        }
        // Advance `cut` to just past this character so if the loop
        // exits naturally (we ran out of input) we still know the
        // total byte length. `ch` is the character at byte offset
        // `i` so `ch.len_utf8()` is the exact byte length of that
        // one character — the previous `input[i..].chars().next()
        // .unwrap().len_utf8()` recomputed the same value through
        // a fresh UTF-8 decode.
        cut = Some(i + ch.len_utf8());
    }
    match cut {
        Some(end) if end < input.len() => {
            let mut out = String::with_capacity(end + ellipsis.len());
            out.push_str(&input[..end]);
            out.push_str(ellipsis);
            out
        }
        _ => input.to_string(),
    }
}

/// Normalize a tool name so dispatch can accept both `leindex.search` /
/// `leindex.search` / `leindex-search` / `leindex search` and the bare
/// `search` short form.
pub(super) fn normalize_tool_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .replace(['-', '.', ' '], "_")
}

/// Returns the args needed to render a tool's CLI output.
pub fn lookup_args<'a>(_name: &str, args: Option<&'a Value>) -> &'a Value {
    static EMPTY: Value = Value::Null;
    args.unwrap_or(&EMPTY)
}

// =============================================================================
// Public re-exports — preserve the existing API so external callers
// (`crate::cli::mcp::output::DiffResult`, etc.) keep working.
// =============================================================================

pub use diff::{
    compute_diff, render_split_diff, render_unified_diff, DiffFormatter, DiffHunk, DiffLine,
    DiffOp, DiffResult,
};
pub use render::{
    render_tree, render_tool_output, DiagnosticsFormatter, FileSummaryFormatter, GitStatusFormatter,
    ImpactFormatter, PhaseFormatter, ProjectMapFormatter, SearchFormatter, SymbolLookupFormatter,
};
pub use trim::trim_llm_payload;

#[cfg(test)]
mod tests {
    use super::*;

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
