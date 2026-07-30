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
fn test_render_diagnostics_plain_output() {
    let payload = v(r#"{
                "project_path": "/repo",
                "indexed_files": 2,
                "system_health": {
                    "index_health": "healthy",
                    "pdg_loaded": true,
                    "pdg_nodes": 3,
                    "embedding_model": "model"
                },
                "issues": [{"severity": "warning", "message": "slow"}]
            }"#);
    assert_eq!(
            render_diagnostics(&payload, false),
            "── Diagnostics ──\n  Project: /repo\n  Indexed files: 2\n\n  System Health:\n    Index health: healthy\n    PDG loaded: true\n    PDG nodes: 3\n    Embedding model: model\n\n  Issues:\n    warning slow\n"
        );
}

#[test]
fn test_render_impact_plain_output() {
    let payload = v(r#"{
                "symbol": "alpha",
                "file": "src/lib.rs",
                "change_type": "modify",
                "risk_level": "high",
                "direct_callers": ["caller"],
                "transitive_affected_symbols": [{"name": "affected"}],
                "summary": "one caller",
                "transitive_affected_files": 2,
                "transitive_callers": 1
            }"#);
    assert_eq!(
            render_impact(&payload, false),
            "── Impact Analysis ──\n  Symbol: alpha\n  File: src/lib.rs\n  Change type: modify\n  Risk: ● high\n\n  Direct callers (1):\n    ← caller\n\n  Transitive affected symbols (1):\n    → affected\n\n  Summary: one caller\n\n  Affected files: 2\n  Transitive callers: 1\n"
        );
}

#[test]
fn test_render_edit_metadata_plain_output() {
    let preview = v(r#"{
                "affected_symbols": ["alpha"],
                "affected_files": ["src/lib.rs"],
                "risk_level": "low",
                "change_count": 1,
                "breaking_changes": ["none"]
            }"#);
    assert_eq!(
            render_edit_preview(&preview, false),
            "  Affected symbols: alpha\n  Affected files: src/lib.rs\n  Risk level: low\n  Change count: 1\n  Breaking: none\n"
        );

    let rename = v(r#"{
                "old_name": "alpha",
                "new_name": "beta",
                "files_affected": 2,
                "diffs_more": 1,
                "preview_only": true
            }"#);
    assert_eq!(
            render_rename_symbol(&rename, false),
            "  Rename: alpha → beta\n  Files affected: 2\n  Additional diffs: 1 more (not shown)\n  Preview only: changes not applied\n"
        );
}

#[test]
fn test_render_file_summary_plain_output() {
    let payload = v(r#"{
                "file_path": "src/lib.rs",
                "language": "rust",
                "line_count": 10,
                "symbol_count": 2,
                "module_role": "library",
                "symbols": [
                    {"name": "alpha", "type": "function"},
                    {"name": "Beta", "type": "struct"}
                ]
            }"#);

    let output = render_tool_output_plain("leindex_file_summary", &payload, &v("{}"));

    assert_eq!(
            output,
            "── File Summary ──\n  File: src/lib.rs\n  Language: rust\n  Lines: 10\n  Symbols: 2\n  Role: library\n\n  Symbols:\n    • alpha\n    • Beta\n"
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
