//! Applied-line echo math (§4.3): translate ORIGINAL-file byte offsets into
//! FINAL-file coordinates without slicing the `modified` text at original
//! offsets.
//!
//! `EditChange::ReplaceText { start, end }` offsets are byte indices into the
//! ORIGINAL file (see `helpers::parse_replace_change`). Earlier
//! length-changing replacements can move a later change's `start`/`end`
//! inside a multi-byte UTF-8 character of the modified text; slicing the
//! modified text at those stale offsets panics and reports wrong line ranges
//! for the `Applied: path:Lx-Ly` echo. The cumulative-shift translation in
//! this module maps every original offset to its final position before any
//! slicing happens, using the same total-delta accounting the edit engine
//! produces (replacements are applied in descending start order, but the net
//! shift at any boundary is order-independent).

use super::edit_batch::BatchFileEdit;
use crate::edit::EditChange;

/// One replacement, as its `(original_start, original_end, new_len)` span.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReplaceSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) new_len: usize,
}

impl ReplaceSpan {
    /// Net byte delta this replacement applies to everything after it.
    fn delta(&self) -> i64 {
        self.new_len as i64 - (self.end - self.start) as i64
    }
}

/// Extract the `ReplaceText` spans from a change list, preserving caller order.
pub(crate) fn replace_spans(changes: &[EditChange]) -> Vec<ReplaceSpan> {
    changes
        .iter()
        .filter_map(|c| match c {
            EditChange::ReplaceText {
                start,
                end,
                new_text,
                ..
            } => Some(ReplaceSpan {
                start: *start,
                end: *end,
                new_len: new_text.len(),
            }),
            _ => None,
        })
        .collect()
}

/// Translate an ORIGINAL-file byte offset into its FINAL-file byte offset
/// after every replacement has been applied. Never panics, and always
/// returns a position that is a UTF-8 char boundary in the modified text:
///
/// * offsets at or after a replaced range's end accrue that range's net
///   delta (the edit engine keeps later-start replacements disjoint from
///   earlier ones, so boundaries stay boundaries);
/// * offsets INSIDE a replaced range collapse to the start of that range's
///   replacement text;
/// * offsets before any replacement resolve unchanged.
pub(crate) fn translate_original_offset(offset: usize, spans: &[ReplaceSpan]) -> usize {
    let mut shift: i64 = 0;
    for span in spans {
        if span.end <= offset {
            shift += span.delta();
        } else if span.start <= offset && offset < span.end {
            // Inside a replaced region: map to the start of the replacement.
            return (span.start as i64 + shift).max(0) as usize;
        }
    }
    (offset as i64 + shift).max(0) as usize
}

/// 1-based line number of a byte offset within a completed file string. The
/// offset must be a char boundary (as produced by [`translate_original_offset`]
/// or clamped to the string length, which is always a boundary).
fn line_of(content: &str, byte_offset: usize) -> usize {
    let byte_offset = byte_offset.min(content.len());
    content[..byte_offset].matches('\n').count() + 1
}

/// The §4.3 one-line echo: `Applied: path:L802-828 (+27/-0) in fn name`.
/// Ranges are computed in FINAL-file coordinates (the modified text), and
/// the enclosing-function suffix appears only when the PDG can name one.
pub(crate) fn applied_lines(
    files: &[BatchFileEdit],
    pdg: Option<&crate::graph::pdg::ProgramDependenceGraph>,
) -> Vec<String> {
    files
        .iter()
        .filter(|f| f.modified != f.original)
        .map(|file| {
            let spans = replace_spans(&file.changes);
            let mut first_line = usize::MAX;
            let mut last_line = 0usize;
            for change in &file.changes {
                let EditChange::ReplaceText {
                    start, new_text, ..
                } = change
                else {
                    continue;
                };
                // Translated start of the replacement in the FINAL text: a
                // char boundary by construction. Never slice `modified` at
                // the original `start` — earlier length-changing edits can
                // move it inside a multi-byte character (and even out of
                // bounds), which panics or reports a wrong range.
                let seg_start = translate_original_offset(*start, &spans).min(file.modified.len());
                let seg_end = (seg_start + new_text.len()).min(file.modified.len());
                let start_line = line_of(&file.modified, seg_start);
                let end_line = line_of(&file.modified, seg_end);
                first_line = first_line.min(start_line);
                last_line = last_line.max(end_line);
            }
            if first_line == usize::MAX {
                first_line = 1;
                last_line = 1;
            }
            let diff = crate::cli::mcp::output::compute_diff(
                &file.original,
                &file.modified,
                &file.display_path,
            );
            let enclosing = pdg.and_then(|graph| {
                let first_byte = file
                    .changes
                    .iter()
                    .filter_map(|c| match c {
                        EditChange::ReplaceText { start, .. } => Some(*start),
                        _ => None,
                    })
                    .min()?;
                graph
                    .nodes_in_file(&file.canonical_path.to_string_lossy())
                    .into_iter()
                    .filter_map(|nid| graph.get_node(nid))
                    .find(|node| node.byte_range.0 <= first_byte && first_byte < node.byte_range.1)
                    .map(|node| node.name.clone())
            });
            match enclosing {
                Some(name) => format!(
                    "Applied: {}:L{}-{} (+{}/-{}) in fn {name}",
                    file.display_path, first_line, last_line, diff.additions, diff.deletions
                ),
                None => format!(
                    "Applied: {}:L{}-{} (+{}/-{})",
                    file.display_path, first_line, last_line, diff.additions, diff.deletions
                ),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn span(start: usize, end: usize, new_text: &str) -> ReplaceSpan {
        ReplaceSpan {
            start,
            end,
            new_len: new_text.len(),
        }
    }

    #[test]
    fn test_translate_original_offset_accrues_earlier_delta() {
        // Original "a b\n": delete 'a' (0..1), then replace 'b' (2..3) with
        // two-byte "é". Applying in descending order yields " é\n".
        let spans = vec![span(0, 1, ""), span(2, 3, "é")];
        assert_eq!(translate_original_offset(0, &spans), 0);
        // Offset 1 (' ') was shifted left by the removed 'a'.
        assert_eq!(translate_original_offset(1, &spans), 0);
        // Offset 2 ('b') collapses to the start of the replacement (still a
        // char boundary) — the naive `modified[..2]` slice would land inside
        // the multi-byte "é" and panic.
        assert_eq!(translate_original_offset(2, &spans), 1);
        // Offset 3 (the newline) lands after BOTH edits: the deletion shifts
        // it left by 1, the "é" insertion shifts it right by 1 — net 0, so
        // it stays at final byte 3 (" é\n": space=1, é=2, \n=3).
        assert_eq!(translate_original_offset(3, &spans), 3);
    }

    #[test]
    fn test_translate_original_offset_after_growth() {
        // Original "ab cd": replace 'a' (0..1) with two-byte "é", then
        // replace "cd" (3..5) with "Z". Modified: "éb Z".
        let spans = vec![span(0, 1, "é"), span(3, 5, "Z")];
        // Unchanged region between the replacements shifted right by +1.
        assert_eq!(translate_original_offset(2, &spans), 3);
        // Start of the second replacement lands after the inserted "é".
        assert_eq!(translate_original_offset(3, &spans), 4);
        assert_eq!(translate_original_offset(5, &spans), 5);
    }

    #[test]
    fn test_translate_original_offset_inside_replaced_region_collapses() {
        let spans = vec![span(2, 5, "x")];
        assert_eq!(translate_original_offset(3, &spans), 2);
        assert_eq!(translate_original_offset(4, &spans), 2);
    }

    /// THE regression: a multi-byte UTF-8 character introduced by an earlier
    /// replacement must not corrupt the `Applied:` echo or panic when a later
    /// change's ORIGINAL offset lands inside that character.
    #[test]
    fn test_applied_lines_multibyte_offset_no_panic() {
        let original = "a b\n";
        let changes = crate::cli::mcp::helpers::parse_edit_changes(
            &serde_json::json!([
                {
                    "type": "replace_text",
                    "old_text": "a",
                    "new_text": ""
                },
                {
                    "type": "replace_text",
                    "old_text": "b",
                    "new_text": "é"
                }
            ]),
            Some(original),
        )
        .expect("changes must parse");

        let modified = crate::cli::mcp::helpers::apply_changes_in_memory(original, &changes)
            .expect("in-memory apply must succeed");
        assert_eq!(modified, " é\n");

        let file = BatchFileEdit {
            canonical_path: PathBuf::from("/tmp/x.rs"),
            display_path: "x.rs".to_string(),
            original: original.to_string(),
            modified,
            changes,
        };
        let out = applied_lines(&[file], None);
        assert_eq!(out.len(), 1, "one changed file must produce one echo line");
        // Earlier deletion shifted the replacement left; both edits live on
        // line 1 of the final file with +1/-1.
        assert!(
            out[0].contains("Applied: x.rs:L1-1 (+1/-1)"),
            "echo must report the final-file range, got: {}",
            out[0]
        );
    }

    #[test]
    fn test_applied_lines_later_change_unaffected_by_earlier_growth() {
        let original = "ab cd\n";
        let changes = crate::cli::mcp::helpers::parse_edit_changes(
            &serde_json::json!([
                {
                    "type": "replace_text",
                    "old_text": "a",
                    "new_text": "é"
                },
                {
                    "type": "replace_text",
                    "old_text": "cd",
                    "new_text": "Z"
                }
            ]),
            Some(original),
        )
        .expect("changes must parse");

        let modified = crate::cli::mcp::helpers::apply_changes_in_memory(original, &changes)
            .expect("in-memory apply must succeed");
        assert_eq!(modified, "éb Z\n");

        let file = BatchFileEdit {
            canonical_path: PathBuf::from("/tmp/y.rs"),
            display_path: "y.rs".to_string(),
            original: original.to_string(),
            modified,
            changes,
        };
        // Must not panic and must not report the naive (wrong) line/range.
        let out = applied_lines(&[file], None);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].contains("Applied: y.rs:L1-1 (+1/-1)"),
            "echo must use final-file coordinates, got: {}",
            out[0]
        );
    }
}
