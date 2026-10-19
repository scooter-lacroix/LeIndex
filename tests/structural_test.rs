//! Structural (AST-pattern) search — end-to-end tests.
//!
//! Covers the four layers a caller can hit:
//! * the engine ([`search`]) over real files on disk,
//! * language resolution (`resolve_language`, `language_for_path`),
//! * the MCP branch's gating (`unavailable_reason`) and its JSON shape,
//! * the `leindex_explore` router entry and the payload trimmer.
//!
//! ## The expando regression
//!
//! `test_metavariables_match_in_dollar_hostile_languages` is the reason the
//! module rewrites `$` to `µ` before parsing: Rust, Python and Go lexers cannot
//! start an identifier with `$`, so a pattern written with literal `$` parses
//! into error nodes and matches nothing. That test fails if anyone "simplifies"
//! the expando away.

#![cfg(feature = "structural-search")]

use leindex::cli::mcp::output::trim::trim_llm_payload;
use leindex::cli::mcp::structural_handler::StructuralHandler;
use leindex::feature_flags::{FeatureFlag, with_flag_override};
use leindex::search::textsearch::{
    CompiledPattern, FileFilter, NO_CAP, StructuralError, StructuralOptions, StructuralOutput,
    language_for_path, resolve_language, search_structural,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

// =============================================================================
// Fixtures
// =============================================================================

/// A small tree with two Rust functions, two Python functions, a Go function
/// and a Rust test file — enough to tell scope, globs and caps apart.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write(
        dir.path(),
        "src/alpha.rs",
        "fn alpha() {}\n\nfn beta(x: i32) -> i32 {\n    x + 1\n}\n",
    );
    write(
        dir.path(),
        "src/deep/alpha_extra.rs",
        "fn gamma() {\n    println!(\"gamma\");\n}\n",
    );
    write(dir.path(), "src/alpha_test.rs", "fn delta() {}\n");
    write(
        dir.path(),
        "py/mod.py",
        "def one():\n    return 1\n\ndef two(a, b):\n    return a + b\n",
    );
    write(
        dir.path(),
        "go/main.go",
        "package main\n\nfunc main() {\n}\n",
    );
    write(dir.path(), "README.md", "# not code\n");
    dir
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, contents).expect("write fixture");
}

fn filter(include: &[&str], exclude: &[&str], scope: Option<&str>) -> FileFilter {
    let owned = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    FileFilter::new(&owned(include), &owned(exclude), scope)
}

fn options(limit: usize, per_file_cap: usize) -> StructuralOptions {
    StructuralOptions {
        limit,
        per_file_cap,
        ..StructuralOptions::default()
    }
}

/// Every reported hit as `(path, line)`.
fn hits(output: &StructuralOutput) -> Vec<(String, u32)> {
    let mut all = Vec::new();
    for file in &output.files {
        for hit in &file.hits {
            all.push((file.rel.clone(), hit.line));
        }
    }
    all
}

// =============================================================================
// Language resolution
// =============================================================================

#[test]
fn test_resolve_language_accepts_name_display_name_and_extension() {
    for spelling in ["rust", "Rust", "rs"] {
        assert_eq!(
            resolve_language(spelling),
            Some(leindex::parse::grammar::LanguageId::Rust),
            "{spelling} should resolve to Rust"
        );
    }
    for spelling in ["python", "py", "Python"] {
        assert_eq!(
            resolve_language(spelling),
            Some(leindex::parse::grammar::LanguageId::Python),
            "{spelling} should resolve to Python"
        );
    }
    assert_eq!(
        resolve_language("  go  "),
        Some(leindex::parse::grammar::LanguageId::Go)
    );
    assert_eq!(resolve_language(""), None);
    assert_eq!(resolve_language("not-a-language"), None);
}

#[test]
fn test_document_flavours_are_not_structural_languages() {
    // Markdown/Rst/Adoc/PlainText resolve (they are real LanguageIds) but have
    // no AST: matching them would hand ast-grep a stand-in grammar.
    assert!(resolve_language("markdown").is_some());
    assert_eq!(language_for_path(Path::new("README.md")), None);
    assert_eq!(language_for_path(Path::new("notes.txt")), None);
    assert_eq!(
        language_for_path(Path::new("src/lib.rs")),
        Some(leindex::parse::grammar::LanguageId::Rust)
    );
    assert_eq!(language_for_path(Path::new("no-extension")), None);

    let err = CompiledPattern::new("markdown", "# heading").unwrap_err();
    assert!(
        matches!(err, StructuralError::GrammarUnavailable(_)),
        "expected GrammarUnavailable, got {err:?}"
    );
    match err {
        StructuralError::GrammarUnavailable(name) => assert_eq!(name, "Markdown"),
        other => panic!("unexpected error: {other:?}"),
    }
}

// =============================================================================
// Pattern compilation
// =============================================================================

#[test]
fn test_pattern_compiles_for_rust_and_python() {
    let rust = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("rust pattern");
    assert_eq!(rust.language(), "Rust");

    let python = CompiledPattern::new("py", "def $NAME($$$ARGS): $$$BODY").expect("python pattern");
    assert_eq!(python.language(), "Python");

    // Go needs a receiver-free function to be a single node.
    let go = CompiledPattern::new("go", "func $NAME() { $$$BODY }").expect("go pattern");
    assert_eq!(go.language(), "Go");
}

#[test]
fn test_unsupported_and_unusable_patterns_are_rejected() {
    let err = CompiledPattern::new("brainfuck", "fn $A() {}").unwrap_err();
    assert_eq!(
        err,
        StructuralError::UnsupportedLanguage("brainfuck".to_string())
    );
    assert!(err.to_string().contains("does not support language"));

    let empty = CompiledPattern::new("rust", "   ").unwrap_err();
    assert!(matches!(empty, StructuralError::UnusablePattern(_)));

    let huge = "fn $A() {}".repeat(leindex::search::textsearch::structural::MAX_PATTERN_BYTES);
    let too_big = CompiledPattern::new("rust", &huge).unwrap_err();
    assert!(
        matches!(too_big, StructuralError::UnusablePattern(ref d) if d.contains("maximum")),
        "expected size error, got {too_big:?}"
    );

    // Two top-level nodes: a pattern must name one node to match one node.
    // (Note what is *not* rejected: tree-sitter's error recovery turns
    // syntactically broken source like `fn ( {` into a single ERROR node, so
    // it compiles and then simply matches nothing. That is ast-grep's
    // behaviour, not a gate we can add without re-parsing the pattern.)
    let multi = CompiledPattern::new("rust", "fn a() {}\nfn b() {}").unwrap_err();
    assert!(
        matches!(multi, StructuralError::InvalidPattern { .. }),
        "expected InvalidPattern, got {multi:?}"
    );
    assert!(
        multi.to_string().contains("not valid Rust code"),
        "error should name the language: {multi}"
    );
}

// =============================================================================
// Matching
// =============================================================================

#[test]
fn test_matches_rust_functions_in_fixture_tree() {
    let dir = fixture();
    let pattern = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("pattern");
    let output = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &options(100, NO_CAP),
    );

    let found = hits(&output);
    // `alpha` and `delta` take no arguments; `beta` does, `gamma` has a body
    // but the pattern's `$$$BODY` covers it, so gamma matches too.
    assert!(
        found.contains(&("src/alpha.rs".to_string(), 1)),
        "line 1 of src/alpha.rs (fn alpha) missing from {found:?}"
    );
    assert!(
        found.iter().any(|(path, _)| path == "src/alpha_test.rs"),
        "test-file function should match: {found:?}"
    );
    assert_eq!(found.len(), 3, "expected alpha, delta and gamma: {found:?}");
    assert_eq!(output.returned, 3);
    assert_eq!(output.stats.total_matches, 3);
    assert_eq!(output.stats.files_matched, 3);
    assert!(!output.has_more);

    let alpha = output
        .files
        .iter()
        .find(|f| f.rel == "src/alpha.rs")
        .expect("src/alpha.rs in results");
    assert_eq!(alpha.lang, "Rust");
    assert_eq!(alpha.hits[0].kind, "function_item");
    assert_eq!(alpha.hits[0].line, 1);
    assert_eq!(alpha.hits[0].col, 1);
    assert_eq!(alpha.hits[0].text, "fn alpha() {}");
    assert_eq!(alpha.hits[0].capture.as_deref(), Some("NAME"));
    assert_eq!(alpha.hits[0].captured.as_deref(), Some("alpha"));
    assert!(alpha.hits[0].start_byte < alpha.hits[0].end_byte);
}

#[test]
fn test_matches_python_functions_in_fixture_tree() {
    let dir = fixture();
    let pattern = CompiledPattern::new("python", "def $NAME($$$ARGS): $$$BODY").expect("pattern");
    let output = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &options(100, NO_CAP),
    );

    let found = hits(&output);
    assert_eq!(found.len(), 2, "one and two: {found:?}");
    let module = output
        .files
        .iter()
        .find(|f| f.rel == "py/mod.py")
        .expect("py/mod.py in results");
    assert_eq!(module.lang, "Python");
    assert_eq!(module.hits[0].kind, "function_definition");
    assert_eq!(module.hits[0].captured.as_deref(), Some("one"));
    assert_eq!(module.hits[1].captured.as_deref(), Some("two"));
}

#[test]
fn test_metavariables_match_in_dollar_hostile_languages() {
    // The regression guard for the expando char: `$` alone yields ZERO hits in
    // these grammars because their lexers cannot start an identifier with `$`.
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.rs", "fn one() {\n    body();\n}\n");
    write(dir.path(), "b.py", "def one():\n    return 1\n");
    write(dir.path(), "c.go", "package main\n\nfunc one() {\n}\n");

    let cases = [
        ("rust", "fn $NAME() { $$$BODY }", "a.rs"),
        ("python", "def $NAME(): $$$BODY", "b.py"),
        ("go", "func $NAME() { $$$BODY }", "c.go"),
    ];
    for (lang, pattern_text, file) in cases {
        let pattern = CompiledPattern::new(lang, pattern_text).expect(pattern_text);
        let output = search_structural(
            dir.path(),
            &pattern,
            &FileFilter::default(),
            &options(100, NO_CAP),
        );
        assert_eq!(
            output.stats.total_matches,
            1,
            "{lang} pattern {pattern_text:?} should match {file} via the expando char, got {:?}",
            hits(&output)
        );
        assert_eq!(
            output.files[0].hits[0].captured.as_deref(),
            Some("one"),
            "{lang}: $NAME should capture the function name"
        );
    }
}

// =============================================================================
// Filtering
// =============================================================================

#[test]
fn test_scope_confines_the_search() {
    let dir = fixture();
    let pattern = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("pattern");
    let output = search_structural(
        dir.path(),
        &pattern,
        &filter(&[], &[], Some("src/deep")),
        &options(100, NO_CAP),
    );

    assert_eq!(
        hits(&output),
        vec![("src/deep/alpha_extra.rs".to_string(), 1)]
    );
    assert_eq!(output.stats.candidates, 1);
}

#[test]
fn test_include_and_exclude_globs_filter_paths() {
    let dir = fixture();
    let pattern = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("pattern");

    let included = search_structural(
        dir.path(),
        &pattern,
        &filter(&["src/**/*.rs"], &["*_test.rs"], None),
        &options(100, NO_CAP),
    );
    let found = hits(&included);
    assert!(
        found.iter().all(|(path, _)| !path.ends_with("_test.rs")),
        "exclude glob leaked test files: {found:?}"
    );
    assert!(
        found
            .iter()
            .any(|(path, _)| path == "src/deep/alpha_extra.rs"),
        "include glob should still admit nested sources: {found:?}"
    );
    assert_eq!(found.len(), 2, "alpha + gamma: {found:?}");

    let excluded_dir = search_structural(
        dir.path(),
        &pattern,
        &filter(&[], &["src/deep/"], None),
        &options(100, NO_CAP),
    );
    assert!(
        hits(&excluded_dir)
            .iter()
            .all(|(path, _)| path != "src/deep/alpha_extra.rs"),
        "directory exclude ignored"
    );
}

// =============================================================================
// Caps, paging and limits
// =============================================================================

#[test]
fn test_per_file_cap_withholds_matches_and_reports_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body: String = (0..6).map(|i| format!("fn f{i}() {{}}\n")).collect();
    write(dir.path(), "many.rs", &body);
    let pattern = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("pattern");

    let capped = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &options(100, 2),
    );
    assert_eq!(capped.files[0].hits.len(), 2, "cap limits reported hits");
    assert!(capped.files[0].capped, "file must be flagged as capped");
    assert_eq!(capped.returned, 2);
    // Matches withheld by the cap appear on no page, so they are not "more".
    assert!(
        !capped.has_more,
        "cap-withheld matches must not imply has_more"
    );
    assert_eq!(
        capped.stats.total_matches, 6,
        "totals still count every match"
    );
    assert_eq!(capped.stats.files_capped, 1);

    let uncapped = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &options(100, NO_CAP),
    );
    assert_eq!(uncapped.files[0].hits.len(), 6);
    assert!(!uncapped.files[0].capped);
}

#[test]
fn test_limit_and_offset_page_deterministically() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (i, name) in ["a", "b", "c"].iter().enumerate() {
        write(
            dir.path(),
            &format!("{name}.rs"),
            &format!("fn f{i}() {{}}\n"),
        );
    }
    let pattern = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("pattern");

    let first = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &options(2, NO_CAP),
    );
    assert_eq!(first.returned, 2);
    assert!(first.has_more, "3 matches with limit 2 means more exist");
    assert_eq!(
        first
            .files
            .iter()
            .map(|f| f.rel.as_str())
            .collect::<Vec<_>>(),
        vec!["a.rs", "b.rs"]
    );

    let second = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &StructuralOptions {
            offset: 2,
            ..options(2, NO_CAP)
        },
    );
    assert_eq!(second.returned, 1);
    assert!(!second.has_more);
    assert_eq!(
        second
            .files
            .iter()
            .map(|f| f.rel.as_str())
            .collect::<Vec<_>>(),
        vec!["c.rs"]
    );
    // Totals describe the whole tree, not just the page.
    assert_eq!(second.stats.total_matches, 3);
}

#[test]
fn test_zero_limit_returns_totals_without_hits() {
    let dir = fixture();
    let pattern = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("pattern");
    let output = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &options(0, NO_CAP),
    );

    assert_eq!(output.returned, 0);
    assert!(output.files.is_empty());
    assert_eq!(output.stats.total_matches, 3);
    assert!(output.has_more, "everything is still ahead of offset 0");
}

#[test]
fn test_no_candidates_is_an_empty_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "only.py", "def one():\n    return 1\n");
    let pattern = CompiledPattern::new("rust", "fn $NAME() { $$$BODY }").expect("pattern");
    let output = search_structural(
        dir.path(),
        &pattern,
        &FileFilter::default(),
        &options(100, NO_CAP),
    );

    assert_eq!(output.returned, 0);
    assert_eq!(
        output.stats.scanned, 0,
        "no .rs files, so nothing was parsed"
    );
    assert_eq!(output.stats.total_matches, 0);
    assert!(!output.has_more);
}

// =============================================================================
// MCP gate and shape (through the real `execute` path)
// =============================================================================

fn registry() -> std::sync::Arc<leindex::cli::registry::ProjectRegistry> {
    std::sync::Arc::new(leindex::cli::registry::ProjectRegistry::new(4))
}

/// The gate is a hard error that says *why* and that nothing ran — never an
/// empty result set the caller could mistake for "the code does not exist".
#[test]
fn test_disabled_flag_fails_with_actionable_error() {
    with_flag_override(FeatureFlag::StructuralSearch, false, || {
        let err = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(StructuralHandler.execute(
                &registry(),
                json!({
                    "pattern": "fn $NAME() { $$$BODY }",
                    "project_path": "/tmp"
                }),
            ))
            .expect_err("flag off must fail, not return an empty result");

        assert_eq!(err.code, -32602, "invalid_params");
        assert!(
            err.message.contains("unavailable"),
            "message must say the tool is unavailable: {}",
            err.message
        );
        assert!(
            err.message.contains("LEINDEX_FEATURE_STRUCTURAL_SEARCH=1"),
            "message must name the env var to set: {}",
            err.message
        );
        let hint = err.suggestion().expect("gate error carries a hint");
        assert!(
            hint.contains("NOT searched"),
            "hint must state the pattern was not searched: {hint}"
        );
        assert!(
            hint.contains("no file was scanned"),
            "hint must state nothing was scanned: {hint}"
        );
        assert!(
            hint.contains("fn $NAME() { $$$BODY }"),
            "hint must echo the pattern back: {hint}"
        );
    });
}

#[test]
fn test_enabled_handler_searches_a_real_project() {
    let dir = fixture();
    with_flag_override(FeatureFlag::StructuralSearch, true, || {
        let value = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(StructuralHandler.execute(
                &registry(),
                json!({
                    "pattern": "fn $NAME() { $$$BODY }",
                    "lang": "rust",
                    "project_path": dir.path()
                }),
            ))
            .expect("structural search should succeed with the gate open");

        assert_eq!(value["mode"], "structural");
        assert_eq!(value["pattern"], "fn $NAME() { $$$BODY }");
        assert_eq!(value["lang"], "Rust");
        assert_eq!(value["returned"], 3);
        assert_eq!(value["total_matches"], 3);
        assert_eq!(value["has_more"], false);
        assert!(value["stats"]["scanned"].as_u64().unwrap() >= 3);

        let files = value["files"].as_array().expect("files array");
        let alpha = files
            .iter()
            .find(|f| f["path"] == "src/alpha.rs")
            .expect("src/alpha.rs");
        assert_eq!(alpha["lang"], "Rust");
        assert_eq!(alpha["capped"], false);
        assert_eq!(alpha["hits"][0]["line"], 1);
        assert_eq!(alpha["hits"][0]["kind"], "function_item");
        assert_eq!(alpha["hits"][0]["capture"], "NAME");
        assert_eq!(alpha["hits"][0]["captured"], "alpha");
    });
}

#[test]
fn test_handler_infers_language_and_refuses_ambiguity() {
    with_flag_override(FeatureFlag::StructuralSearch, true, || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");

        // Single-language scope: no `lang` needed.
        let rust_only = tempfile::tempdir().expect("tempdir");
        write(rust_only.path(), "lib.rs", "fn only() {}\n");
        runtime
            .block_on(StructuralHandler.execute(
                &registry(),
                json!({
                    "pattern": "fn $NAME() { $$$BODY }",
                    "project_path": rust_only.path()
                }),
            ))
            .expect("unambiguous scope should infer Rust");

        // Mixed scope: refusing beats parsing Rust patterns as Python.
        let mixed = fixture();
        let err = runtime
            .block_on(StructuralHandler.execute(
                &registry(),
                json!({
                    "pattern": "fn $NAME() { $$$BODY }",
                    "project_path": mixed.path()
                }),
            ))
            .expect_err("two languages in scope must be refused");
        assert!(
            err.message.contains("several languages") && err.message.contains("refusing to guess"),
            "error should explain the ambiguity: {}",
            err.message
        );

        // Nothing parseable at all: say so instead of scanning nothing.
        let empty = tempfile::tempdir().expect("tempdir");
        write(empty.path(), "notes.md", "# docs\n");
        let err = runtime
            .block_on(StructuralHandler.execute(
                &registry(),
                json!({
                    "pattern": "fn $NAME() { $$$BODY }",
                    "project_path": empty.path()
                }),
            ))
            .expect_err("no language in scope must be an error");
        assert!(
            err.message.contains("No structural-search language"),
            "error should explain the empty scope: {}",
            err.message
        );
    });
}

#[test]
fn test_missing_pattern_is_rejected() {
    with_flag_override(FeatureFlag::StructuralSearch, true, || {
        let err = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(StructuralHandler.execute(&registry(), json!({ "lang": "rust" })))
            .expect_err("pattern is required");
        assert!(
            err.message.contains("pattern"),
            "error must name the missing argument: {}",
            err.message
        );
    });
}

#[test]
fn test_default_tool_listing_exposes_structural() {
    let handler = StructuralHandler;
    assert_eq!(handler.name(), "leindex_structural");
    assert_eq!(handler.title(), "LeIndex [Structural]");

    let schema = handler.argument_schema();
    let required = schema["required"].as_array().expect("required array");
    assert_eq!(required, &vec![Value::String("pattern".to_string())]);
    let props = schema["properties"].as_object().expect("properties");
    for arg in [
        "pattern",
        "lang",
        "scope",
        "include_globs",
        "exclude_globs",
        "limit",
        "offset",
        "per_file_cap",
        "timeout_ms",
        "project_path",
    ] {
        assert!(props.contains_key(arg), "schema is missing `{arg}`");
    }
}

// =============================================================================
// Trim
// =============================================================================

#[test]
fn test_trim_keeps_actionable_fields_drops_offsets() {
    let data = json!({
        "mode": "structural",
        "pattern": "fn $NAME() { $$$BODY }",
        "lang": "Rust",
        "returned": 1,
        "total_matches": 1,
        "has_more": false,
        "stats": { "scanned": 3, "millis": 1 },
        "files": [{
            "path": "src/alpha.rs",
            "lang": "Rust",
            "total_matches": 1,
            "capped": false,
            "hits": [{
                "line": 1,
                "col": 1,
                "kind": "function_item",
                "text": "fn alpha() {}",
                "start_byte": 0,
                "end_byte": 14,
                "capture": "NAME",
                "captured": "alpha"
            }]
        }]
    });

    let trimmed = trim_llm_payload("leindex_structural", &data);
    assert_eq!(trimmed["pattern"], "fn $NAME() { $$$BODY }");
    assert_eq!(trimmed["lang"], "Rust");
    assert_eq!(trimmed["total_matches"], 1);
    assert_eq!(trimmed["stats"]["scanned"], 3);

    let hit = &trimmed["files"][0]["hits"][0];
    assert_eq!(hit["line"], 1);
    assert_eq!(hit["kind"], "function_item");
    assert_eq!(hit["captured"], "alpha");
    assert!(hit.get("start_byte").is_none(), "byte offsets are noise");
    assert!(
        hit.get("col").is_none(),
        "line+kind is enough to open the file"
    );
}

#[test]
fn test_trim_preserves_stale_index_warning() {
    let data = json!({
        "mode": "structural",
        "pattern": "fn $A() {}",
        "lang": "Rust",
        "returned": 0,
        "total_matches": 0,
        "has_more": false,
        "files": [],
        "_warning": "index is stale"
    });
    let trimmed = trim_llm_payload("leindex_structural", &data);
    assert_eq!(trimmed["_warning"], "index is stale");
}

// =============================================================================
// CLI rendering
// =============================================================================

#[test]
fn test_render_structural_prints_paths_lines_and_captures() {
    let data = json!({
        "mode": "structural",
        "pattern": "fn $NAME() { $$$BODY }",
        "lang": "Rust",
        "returned": 2,
        "total_matches": 2,
        "has_more": false,
        "files": [
            {
                "path": "src/alpha.rs",
                "lang": "Rust",
                "total_matches": 2,
                "capped": false,
                "hits": [
                    {
                        "line": 1,
                        "kind": "function_item",
                        // A real match spans lines; the renderer must not
                        // print a raw newline mid-row.
                        "text": "fn alpha() {\n    body();\n}",
                        "capture": "NAME",
                        "captured": "alpha"
                    },
                    { "line": 9, "kind": "function_item", "text": "fn beta() {}" }
                ]
            }
        ]
    });

    let out = leindex::cli::mcp::output::render::render_tool_output_plain(
        "leindex_structural",
        &data,
        &json!({}),
    );

    assert!(
        out.contains("2 structural match(es)"),
        "header should carry the totals: {out}"
    );
    assert!(out.contains("src/alpha.rs"), "path line missing: {out}");
    assert!(
        out.contains("1: [function_item]"),
        "first hit missing: {out}"
    );
    assert!(
        out.contains("fn alpha() { body(); }"),
        "multi-line match should collapse onto one row: {out}"
    );
    assert!(out.contains("alpha"), "captured name missing: {out}");
    assert!(
        out.contains("9: [function_item]"),
        "second hit missing: {out}"
    );
    assert!(
        !out.contains("more matches"),
        "no pagination hint expected: {out}"
    );
}

#[test]
fn test_render_structural_reports_empty_results_with_the_note() {
    let data = json!({
        "mode": "structural",
        "pattern": "fn $NAME() { $$$BODY }",
        "lang": "Rust",
        "returned": 0,
        "total_matches": 0,
        "has_more": false,
        "files": [],
        "note": "3 file(s) held more matches than per_file_cap"
    });

    let out = leindex::cli::mcp::output::render::render_tool_output_plain(
        "leindex_structural",
        &data,
        &json!({}),
    );

    assert!(
        out.contains("No structural matches"),
        "expected empty header: {out}"
    );
    assert!(
        out.contains("per_file_cap"),
        "the note must survive an empty page so the caller can widen the cap: {out}"
    );
}

// =============================================================================
// Router
// =============================================================================

#[test]
fn test_explore_router_advertises_and_resolves_structural() {
    let group =
        leindex::cli::mcp::grouped::group_by_name("leindex_explore").expect("explore group");
    let branch = group
        .branches
        .iter()
        .find(|b| b.branch == "structural")
        .expect("structural branch must be advertised");

    assert_eq!(branch.tool, "leindex_structural");
    for alias in ["ast", "ast_grep", "astgrep", "pattern"] {
        assert!(
            branch.aliases.contains(&alias),
            "alias `{alias}` should route to structural"
        );
    }
    assert!(
        group.description.contains("structural"),
        "router description must mention the branch: {}",
        group.description
    );

    let (tool, args) = leindex::cli::mcp::grouped::resolve_call(
        "leindex_explore",
        json!({
            "mode": "structural",
            "pattern": "fn $NAME() { $$$BODY }"
        }),
    )
    .expect("structural mode must resolve");
    assert_eq!(tool, "leindex_structural");
    assert_eq!(args["pattern"], "fn $NAME() { $$$BODY }");
    assert!(args.get("mode").is_none(), "the discriminator is consumed");
}

#[test]
fn test_every_router_branch_names_a_real_handler() {
    // The invariant behind adding a branch: `tools/list` must never name a tool
    // that cannot be dispatched.
    let registered: Vec<String> = leindex::cli::mcp::handlers::all_tool_handlers()
        .iter()
        .map(|h| h.name().to_string())
        .collect();
    for group in leindex::cli::mcp::grouped::GROUPS.iter() {
        for branch in group.branches {
            assert!(
                registered.iter().any(|name| name == branch.tool),
                "{} branch `{}` targets unregistered tool `{}`",
                group.name,
                branch.branch,
                branch.tool
            );
        }
    }
}
