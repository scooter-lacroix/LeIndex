//! Tests for the read-path engine: handles, §3.1 numbering, elision,
//! session ledger (brief §3.1, §4.1, §6.9 regression tests).

use super::*;

fn symbol_at(content: &str, name: &str, start_marker: &str) -> CatalogSymbol {
    let start = content.find(start_marker).unwrap();
    // Span ends AT the item's own closing brace (first line starting with
    // `}` at column 0), including it but not the trailing newline — the
    // shape tree-sitter item spans actually have. The old fixture scanned
    // for the first `\n}\n` ANYWHERE below the start, so for an item whose
    // first closing brace belongs to a nested block it swallowed every line
    // down to some later item's closer and invented a line span that does
    // not exist in the file.
    let end = content[start..]
        .lines()
        .scan(0usize, |offset, line| {
            let here = *offset;
            *offset += line.len() + 1;
            Some((here, line))
        })
        .find(|(_, line)| line.starts_with('}'))
        .map(|(line_offset, line)| start + line_offset + line.len())
        .unwrap_or(content.len());
    CatalogSymbol {
        node_id: format!("fixture.rs:{name}"),
        symbol_name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: PathBuf::from("fixture.rs"),
        language: "rust".to_string(),
        node_type: "function".to_string(),
        complexity: 0,
        byte_range: (start, end),
    }
}

// =============================================================================
// §3.1: numbers exactly once
// =============================================================================

#[test]
fn test_number_lines_emits_each_number_exactly_once() {
    let out = number_lines("fn a() {}\nfn b() {}", 599);
    assert_eq!(out, "599 fn a() {}\n600 fn b() {}");
    assert!(!out.contains("│"), "no gutter bar in the body");
    assert!(!out.contains("600:"), "no colon-number duplication");
}

#[test]
fn test_number_lines_is_unpadded_for_wide_files() {
    let out = number_lines("x", 1);
    assert_eq!(out, "1 x", "line 1 is not padded to a fixed gutter width");
}

/// PR #117 finding #116: a selected range that ENDS in blank lines used to
/// lose them, because `str::lines()` drops a trailing empty line. The caller
/// then got fewer lines than it asked for and every number after the gap
/// pointed at the wrong line of the file.
///
/// The slice arrives here already line-selected and joined with `'\n'`, so a
/// trailing `""` element is a REAL line of the selection.
#[test]
fn test_number_lines_preserves_trailing_blank_lines_in_a_selected_range() {
    // Lines 2..=5 of `alpha\n\nbeta\n\n\ngamma\n`: "", "beta", "", "".
    let selected = ["", "beta", "", ""].join("\n");
    let out = number_lines(&selected, 2);
    assert_eq!(
        out, "2 \n3 beta\n4 \n5 ",
        "all four selected lines are rendered, still on their real line numbers"
    );
    assert_eq!(
        out.split('\n').count(),
        4,
        "a blank line at the end of the range is not swallowed"
    );
}

/// …and the interior-blank case, so a fix for the trailing case cannot
/// regress the middle one.
#[test]
fn test_number_lines_preserves_interior_blank_lines() {
    let out = number_lines("a\n\nb", 7);
    assert_eq!(out, "7 a\n8 \n9 b");
}

/// No lines in, no phantom line out: an empty selection must not render as
/// one numbered empty line.
#[test]
fn test_number_lines_renders_an_empty_selection_as_empty() {
    assert_eq!(number_lines("", 600), "");
}

// =============================================================================
// Handles: path#symbol, path#L600-800
// =============================================================================

#[test]
fn test_parse_read_handle_plain_path() {
    let h = parse_read_handle("/abs/path.rs", None);
    assert_eq!(h.file_path, "/abs/path.rs");
    assert_eq!(h.symbol, None);
    assert_eq!(h.line_range, None);
}

#[test]
fn test_parse_read_handle_symbol_fragment() {
    let h = parse_read_handle("src/lib.rs#build_thing", None);
    assert_eq!(h.file_path, "src/lib.rs");
    assert_eq!(h.symbol.as_deref(), Some("build_thing"));
}

#[test]
fn test_parse_read_handle_line_range_fragment() {
    let h = parse_read_handle("src/lib.rs#L600-800", None);
    assert_eq!(h.line_range, Some((600, 800)));
    assert_eq!(h.symbol, None);
}

#[test]
fn test_parse_read_handle_single_line_fragment() {
    let h = parse_read_handle("src/lib.rs#L42", None);
    assert_eq!(h.line_range, Some((42, 42)));
}

#[test]
fn test_parse_read_handle_relative_resolves_against_root() {
    let h = parse_read_handle("src/lib.rs#foo", Some(Path::new("/proj")));
    assert_eq!(h.file_path, "/proj/src/lib.rs");
}

#[test]
fn test_parse_read_handle_decodes_percent_escapes() {
    let h = parse_read_handle("/a%20b%23c.rs#foo", None);
    assert_eq!(h.file_path, "/a b#c.rs");
    assert_eq!(h.symbol.as_deref(), Some("foo"));
}

#[test]
fn test_handles_round_trip_through_parse() {
    let root = Path::new("/proj");
    let file = root.join("src/lib.rs");
    let range_handle = handle_for_range(&file, Some(root), 600, 800);
    assert_eq!(range_handle, "src/lib.rs#L600-800");
    let parsed = parse_read_handle(&range_handle, Some(root));
    assert_eq!(parsed.line_range, Some((600, 800)));

    // Symbol handles are rendered inline (`elide_around`, the read_symbol
    // handler); pin their round-trip contract through the parser here.
    let symbol_handle = format!("{}#focus", "src/lib.rs");
    let parsed = parse_read_handle(&symbol_handle, Some(root));
    assert_eq!(parsed.symbol.as_deref(), Some("focus"));
}

#[test]
fn test_handle_outside_project_stays_absolute() {
    let h = handle_for_range(Path::new("/tmp/log.txt"), Some(Path::new("/proj")), 1, 2);
    assert_eq!(h, "/tmp/log.txt#L1-2");
}

// =============================================================================
// Elided reads
// =============================================================================

const ELIDE_SRC: &str = r#"/// Doc for first.
#[derive(Debug)]
pub struct First {
    pub a: u32,
}

/// Sibling doc.
pub fn sibling(x: u32) -> u32 {
    let y = x + 1;
    let z = y * 2;
    z
}

/// Doc for focus.
pub fn focus() -> u32 {
    42
}
"#;

#[test]
fn test_elide_collapses_siblings_and_keeps_focus_verbatim() {
    let symbols = vec![
        symbol_at(ELIDE_SRC, "First", "pub struct First"),
        symbol_at(ELIDE_SRC, "sibling", "pub fn sibling"),
        symbol_at(ELIDE_SRC, "focus", "pub fn focus"),
    ];
    let out = elide_around(ELIDE_SRC, &symbols, "focus", "fixture.rs", 500).unwrap();

    // Siblings collapsed to one signature line, each with its OWN expand
    // handle (`path#item`), so any sibling can be expanded later.
    assert!(
        out.body
            .contains("3 pub struct First { … 3 lines }  [expand: fixture.rs#First]"),
        "struct collapsed: {}",
        out.body
    );
    assert!(
        out.body.contains(
            "8 pub fn sibling(x: u32) -> u32 { … 5 lines }  [expand: fixture.rs#sibling]"
        ),
        "fn collapsed: {}",
        out.body
    );
    // Focus item verbatim, including its doc comment (lines 14-17).
    assert!(out.body.contains("14 /// Doc for focus."), "{}", out.body);
    assert!(
        out.body.contains("15 pub fn focus() -> u32 {"),
        "{}",
        out.body
    );
    assert!(out.body.contains("17 }"), "{}", out.body);
    // Collapsed bodies and their doc/attr blocks are gone.
    assert!(!out.body.contains("pub a: u32"), "{}", out.body);
    assert!(!out.body.contains("let y = x + 1"), "{}", out.body);
    assert!(!out.body.contains("/// Sibling doc."), "{}", out.body);
    assert!(!out.body.contains("/// Doc for first."), "{}", out.body);
    // The focus item's own handle is reported for a follow-up read_symbol.
    assert_eq!(out.expand, "fixture.rs#focus");
    assert_eq!(out.focus_start_line, 15);
    assert_eq!(out.focus_end_line, 17);
}

#[test]
fn test_elide_truncation_is_announced_not_silent() {
    let symbols = vec![
        symbol_at(ELIDE_SRC, "First", "pub struct First"),
        symbol_at(ELIDE_SRC, "sibling", "pub fn sibling"),
        symbol_at(ELIDE_SRC, "focus", "pub fn focus"),
    ];
    let out = elide_around(ELIDE_SRC, &symbols, "focus", "fixture.rs", 4).unwrap();
    assert!(
        out.body.contains("more lines (elided view truncated"),
        "truncation must be announced: {}",
        out.body
    );
}

#[test]
fn test_elide_errors_on_missing_focus() {
    let symbols = vec![symbol_at(ELIDE_SRC, "focus", "pub fn focus")];
    let err = elide_around(ELIDE_SRC, &symbols, "nope", "fixture.rs", 500).unwrap_err();
    assert!(err.contains("nope"));
}

#[test]
fn test_top_level_spans_filter_nested_items() {
    let spans = vec![
        SymbolSpan {
            name: "outer".into(),
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            end_line: 20,
        },
        SymbolSpan {
            name: "inner".into(),
            start_byte: 10,
            end_byte: 50,
            start_line: 2,
            end_line: 10,
        },
    ];
    let top = top_level_spans(&spans);
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].name, "outer");
}

#[test]
fn test_find_span_suffix_fallback() {
    let spans = vec![SymbolSpan {
        name: "Impl::build_x".into(),
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        end_line: 5,
    }];
    assert_eq!(
        find_span(&spans, "build_x").map(|s| s.name.as_str()),
        Some("Impl::build_x")
    );
}

// =============================================================================
// Session ledger
// =============================================================================

#[test]
fn test_second_identical_read_returns_unchanged_marker() {
    let ledger = SessionLedger::new(8, 64);
    let path = Path::new("/proj/src/lib.rs");
    let first = ledger.record("s1", path, "hash-a", 600, 800);
    let verdict = ledger.consult("s1", path, "hash-a", 600, 800);
    assert_eq!(
        verdict,
        ConsultVerdict::Unchanged {
            since_ordinal: first,
            start_line: 600,
            end_line: 800
        }
    );
    let payload = unchanged_payload("/proj/src/lib.rs", first, 600, 800);
    assert!(
        payload["note"]
            .as_str()
            .unwrap()
            .contains(&format!("unchanged since read #{first} (lines 600-800)")),
        "{}",
        payload["note"].as_str().unwrap()
    );
}

#[test]
fn test_different_session_does_not_share_reads() {
    let ledger = SessionLedger::new(8, 64);
    let path = Path::new("/proj/src/lib.rs");
    ledger.record("s1", path, "hash-a", 600, 800);
    assert_eq!(
        ledger.consult("s2", path, "hash-a", 600, 800),
        ConsultVerdict::New
    );
}

#[test]
fn test_content_change_resets_reads() {
    let ledger = SessionLedger::new(8, 64);
    let path = Path::new("/proj/src/lib.rs");
    let first = ledger.record("s1", path, "hash-a", 600, 800);
    let second = ledger.record("s1", path, "hash-b", 600, 800);
    // The stale content's cached read is gone after the content change...
    assert_eq!(
        ledger.consult("s1", path, "hash-a", 600, 800),
        ConsultVerdict::New
    );
    // ...and the re-read of the NEW content was itself served (record #2),
    // so a repeat hits the unchanged marker: a content change resets the old
    // entries, it does not disable dedup for the new content.
    assert_eq!(
        ledger.consult("s1", path, "hash-b", 600, 800),
        ConsultVerdict::Unchanged {
            since_ordinal: second,
            start_line: 600,
            end_line: 800
        }
    );
    assert!(first > 0);
}

#[test]
fn test_edit_invalidates_cached_reads_until_reread() {
    let ledger = SessionLedger::new(8, 64);
    let path = Path::new("/proj/src/lib.rs");
    let ordinal = ledger.record("s1", path, "hash-a", 600, 800);
    ledger.record_edit("s1", path);
    assert_eq!(
        ledger.consult("s1", path, "hash-a", 600, 800),
        ConsultVerdict::New
    );
    // The post-edit re-read of the same bytes is served (a fresh hash).
    let after = ledger.record("s1", path, "hash-post", 600, 800);
    assert!(after > ordinal);
    assert_eq!(
        ledger.consult("s1", path, "hash-post", 600, 800),
        ConsultVerdict::Unchanged {
            since_ordinal: after,
            start_line: 600,
            end_line: 800
        }
    );
}

#[test]
fn test_partial_overlap_returns_only_unseen_ranges() {
    let ledger = SessionLedger::new(8, 64);
    let path = Path::new("/proj/src/lib.rs");
    ledger.record("s1", path, "hash-a", 600, 700);
    match ledger.consult("s1", path, "hash-a", 650, 800) {
        ConsultVerdict::PartialOverlap { unseen, .. } => {
            assert_eq!(unseen, vec![(701, 800)], "only the new tail is unseen");
        }
        other => panic!("expected partial overlap, got {other:?}"),
    }
}

#[test]
fn test_ledger_sessions_are_bounded() {
    let ledger = SessionLedger::new(2, 64);
    let path = Path::new("/proj/src/lib.rs");
    ledger.record("first", path, "h", 1, 2);
    ledger.record("second", path, "h", 1, 2);
    ledger.record("third", path, "h", 1, 2);
    // The oldest session was evicted.
    assert_eq!(
        ledger.consult("first", path, "h", 1, 2),
        ConsultVerdict::New
    );
    assert_ne!(
        ledger.consult("second", path, "h", 1, 2),
        ConsultVerdict::New
    );
}

#[test]
fn test_ledger_reads_per_file_are_bounded() {
    let ledger = SessionLedger::new(8, 4);
    let path = Path::new("/proj/src/lib.rs");
    for line in 1..=10 {
        ledger.record("s1", path, "h", line, line);
    }
    let (files, _) = ledger.summarize_session("s1");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].1.len(), 4, "at most max_reads_per_file reads kept");
    // Newest kept, oldest dropped.
    assert_eq!(files[0].1.last().map(|r| r.1), Some(10));
}

#[test]
fn test_merge_ranges_merges_adjacent_and_overlapping() {
    assert_eq!(
        merge_ranges(vec![(1, 5), (6, 9), (20, 25), (21, 30)]),
        vec![(1, 9), (20, 30)]
    );
    assert!(merge_ranges(Vec::new()).is_empty());
}

// =============================================================================
// §6.9: session reads resource
// =============================================================================

#[test]
fn test_session_reads_resource_lists_reads_and_edits_merged() {
    let ledger = SessionLedger::new(8, 64);
    let path = Path::new("/proj/src/lib.rs");
    let first = ledger.record("s1", path, "h", 100, 120);
    let second = ledger.record("s1", path, "h", 110, 150);
    ledger.record_edit("s1", Path::new("/proj/src/lib.rs"));

    let (files, edits) = ledger.summarize_session("s1");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, path);
    assert_eq!(
        files[0].1.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![first, second]
    );
    let body = render_session_reads(&files, &edits);
    assert!(body.contains("/proj/src/lib.rs"), "{body}");
    assert!(body.contains(&format!("#{first},{second}")), "{body}");
    assert!(body.contains("L100-150"), "ranges merged: {body}");
    assert!(body.contains("edited"), "{body}");
}

#[test]
fn test_session_reads_resource_empty_state() {
    let body = render_session_reads(&[], &[]);
    assert!(body.contains("no recorded reads or edits"), "{body}");
}

#[test]
fn test_consult_does_not_report_unchanged_when_a_range_remains_unserved() {
    // Regression: `covering` could be set by one recorded read while a later
    // read left part of the request in `unseen`. The verdict then took the
    // `Unchanged` branch and silently dropped the unserved tail.
    let ledger = SessionLedger::new(8, 64);
    let path = Path::new("/proj/src/lib.rs");
    // Read 1 serves only line 2; read 2 serves only line 1.
    ledger.record("s1", path, "hash-a", 2, 2);
    ledger.record("s1", path, "hash-a", 1, 1);
    // Lines 3..=4 were never served by either read.
    let verdict = ledger.consult("s1", path, "hash-a", 1, 4);
    assert_ne!(
        verdict,
        ConsultVerdict::Unchanged {
            since_ordinal: 1,
            start_line: 1,
            end_line: 4
        },
        "reported Unchanged while lines 3-4 were never served"
    );
    match verdict {
        ConsultVerdict::PartialOverlap { unseen, .. } => assert_eq!(unseen, vec![(3, 4)]),
        other => panic!("expected PartialOverlap naming the unserved range, got {other:?}"),
    }
}

#[test]
fn test_find_span_does_not_match_an_unrelated_name_with_the_same_suffix() {
    // Regression (#119): the fallback used a bare `ends_with`, so a request for
    // `build` matched a file's only `rebuild`, and `test` matched `greatest`.
    let rebuild = SymbolSpan {
        name: "rebuild".into(),
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        end_line: 5,
    };
    assert_eq!(
        find_span(std::slice::from_ref(&rebuild), "build"),
        None,
        "must not return `rebuild` for a request of `build`"
    );

    let greatest = SymbolSpan {
        name: "greatest".into(),
        start_byte: 0,
        end_byte: 10,
        start_line: 1,
        end_line: 5,
    };
    assert_eq!(
        find_span(std::slice::from_ref(&greatest), "test"),
        None,
        "must not return `greatest` for a request of `test`"
    );
}

#[test]
fn test_ledger_file_records_per_session_are_bounded() {
    // Regression (#28): `SessionState::files` had no bound, so a session that
    // read many distinct files grew the map without limit — contradicting the
    // struct doc ("No structure here grows with the number of tool calls").
    let ledger = SessionLedger::new(8, 64);
    for i in 0..10_000 {
        let path = PathBuf::from(format!("/proj/src/file_{i}.rs"));
        ledger.record("s1", &path, "h", 1, 2);
    }
    let (files, _) = ledger.summarize_session("s1");
    assert!(
        files.len() <= SessionLedger::MAX_FILES_PER_SESSION,
        "file records grew to {} — must be bounded",
        files.len()
    );
}

/// `served_*_line` is the range of FOCUS lines actually emitted. The caller
/// records it in the session ledger, so it must shrink with truncation:
/// recording the full focus range while the cap cut the body made a later
/// read of the omitted lines answer `unchanged since read`.
#[test]
fn test_elide_reports_only_the_focus_lines_actually_served() {
    let symbols = vec![
        symbol_at(ELIDE_SRC, "First", "pub struct First"),
        symbol_at(ELIDE_SRC, "sibling", "pub fn sibling"),
        symbol_at(ELIDE_SRC, "focus", "pub fn focus"),
    ];
    // Whole focus served: identical to the focus range.
    let full = elide_around(ELIDE_SRC, &symbols, "focus", "fixture.rs", 500).unwrap();
    assert_eq!(full.served_start_line, full.focus_start_line);
    assert_eq!(full.served_end_line, full.focus_end_line);
    // Cap cuts the body inside the focus item (after `pub fn focus`, before
    // its body): only the focus lines that made it are reported.
    let partial = elide_around(ELIDE_SRC, &symbols, "focus", "fixture.rs", 6).unwrap();
    assert!(partial.body.contains("more lines (elided view truncated"));
    assert_eq!(partial.served_start_line, partial.focus_start_line);
    assert!(
        partial.served_end_line < partial.focus_end_line,
        "focus must be cut: {}..{}",
        partial.served_start_line,
        partial.served_end_line
    );
    // The served range must match exactly the focus lines present in the body.
    let present = (partial.focus_start_line..=partial.focus_end_line)
        .filter(|n| {
            partial
                .body
                .lines()
                .any(|l| l.starts_with(&format!("{n} ")))
        })
        .collect::<Vec<_>>();
    assert_eq!(present.first(), Some(&partial.served_start_line));
    assert_eq!(present.last(), Some(&partial.served_end_line));
    // Cap cuts BEFORE the focus: nothing served (0..0; line numbers are
    // 1-based, so a zero end can never be a real served line).
    let none = elide_around(ELIDE_SRC, &symbols, "focus", "fixture.rs", 4).unwrap();
    assert_eq!((none.served_start_line, none.served_end_line), (0, 0));
}
