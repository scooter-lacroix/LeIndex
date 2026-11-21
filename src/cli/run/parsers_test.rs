//! Tests for the pure output parsers (`src/cli/run/parsers.rs`).
//!
//! Each parser is exercised against a realistic fixture and the asserted
//! numbers are counts, not string equality (so a formatting change to the
//! fixture does not silently break the test).

use super::parsers::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

const CARGO_TEST_FIXTURE: &str = r#"running 5 tests
test tests::a::test_one ... FAILED
test tests::a::test_two ... FAILED
test tests::b::test_three ... FAILED
test tests::b::test_four ... ok
test tests::b::test_five ... ok

failures:

---- tests::a::test_one stdout ----
thread 'tests::a::test_one' panicked at src/a.rs:12:5:
assertion `left == right` failed
  left: 1
  right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- tests::a::test_two stdout ----
thread 'tests::a::test_two' panicked at src/a.rs:40:9:
assertion `left == right` failed
  left: 3
  right: 4
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- tests::b::test_three stdout ----
thread 'tests::b::test_three' panicked at src/b.rs:7:5:
assertion `left == right` failed
  left: 5
  right: 6
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    tests::a::test_one
    tests::a::test_two
    tests::b::test_three

test result: FAILED. 0 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

running 6 tests
test tests::c::test_six ... ok
test tests::c::test_seven ... ok
test tests::c::test_eight ... ok
test tests::d::test_nine ... ok
test tests::d::test_ten ... ok
test tests::d::test_eleven ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
"#;

#[test]
fn test_parse_cargo_test_aggregates_suites_and_failures() {
    let parsed = parse_cargo_test(CARGO_TEST_FIXTURE);
    assert_eq!(parsed.failures.len(), 3, "three failing tests");
    assert!(parsed.headline.contains("3 failed"), "{}", parsed.headline);
    assert!(parsed.headline.contains("6 passed"), "{}", parsed.headline);
    assert!(parsed.headline.contains("2 suites"), "{}", parsed.headline);
    assert_eq!(parsed.error_count, 0);
    assert!(parsed.warning_counts.is_empty());
    // Every failing test carries exactly the first panic line of its block.
    let details: Vec<&str> = parsed.failures.iter().map(|f| f.detail.as_str()).collect();
    assert_eq!(
        details,
        vec![
            "thread 'tests::a::test_one' panicked at src/a.rs:12:5:",
            "thread 'tests::a::test_two' panicked at src/a.rs:40:9:",
            "thread 'tests::b::test_three' panicked at src/b.rs:7:5:",
        ]
    );
    let names: Vec<&str> = parsed.failures.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "tests::a::test_one",
            "tests::a::test_two",
            "tests::b::test_three"
        ]
    );
}

#[test]
fn test_parse_cargo_test_all_green() {
    let output = "running 0 tests\n\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    let parsed = parse_cargo_test(output);
    assert!(parsed.failures.is_empty());
    assert_eq!(parsed.headline, "cargo test: 4 passed (1 suites)");
}

const CARGO_JSON_FIXTURE: &str = r#"{"reason":"compiler-message","package_id":"leindex","target":{"kind":["lib"],"name":"leindex"},"message":{"level":"error","code":{"code":"E0308","explanation":null},"message":"mismatched types","spans":[{"file_name":"src/lib.rs","line_start":12,"column_start":5,"line_end":12,"column_end":9,"is_primary":true}]}}
{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types","spans":[{"file_name":"src/lib.rs","line_start":12,"column_start":5}]}}
{"reason":"compiler-message","message":{"level":"warning","code":{"code":"unused_variables"},"message":"unused variable: `x`","spans":[{"file_name":"src/lib.rs","line_start":40,"column_start":9}]}}
{"reason":"compiler-message","message":{"level":"warning","code":{"code":"unused_variables"},"message":"unused variable: `y`","spans":[{"file_name":"src/lib.rs","line_start":41,"column_start":9}]}}
{"reason":"compiler-message","message":{"level":"warning","code":{"code":"dead_code"},"message":"function is never used","spans":[{"file_name":"src/lib.rs","line_start":50,"column_start":1}]}}
{"reason":"compiler-artifact","package_id":"leindex","target":{"kind":["lib"],"name":"leindex"},"profile":{"opt_level":0},"features":[],"filenames":[],"executable":null,"fresh":true}
"#;

#[test]
fn test_parse_cargo_json_dedupes_and_counts_warnings_per_code() {
    let parsed = parse_cargo_json(CARGO_JSON_FIXTURE);
    // The duplicate E0308 at src/lib.rs:12 must collapse into one.
    assert_eq!(parsed.error_count, 1, "one distinct error");
    assert_eq!(parsed.failures.len(), 1);
    assert_eq!(parsed.failures[0].name, "src/lib.rs:12 E0308");
    assert!(parsed.failures[0].detail.contains("mismatched types"));
    assert_eq!(parsed.warning_counts.get("unused_variables"), Some(&2));
    assert_eq!(parsed.warning_counts.get("dead_code"), Some(&1));
    assert!(parsed.headline.contains("1 errors"));
    assert!(parsed.headline.contains("3 warnings"));
}

const PYTEST_FIXTURE: &str = r#"============================= test session starts ==============================
platform linux -- Python 3.12.0
collected 8 items

tests/test_a.py ..F                                             [ 37%]
tests/test_b.py .FF                                               [100%]

=============== 2 failed, 5 passed, 1 error in 12.34s ================
FAILED tests/test_a.py::test_add - AssertionError: 1 + 1 != 3
FAILED tests/test_b.py::test_mul - TypeError: unsupported operand
ERROR tests/test_b.py::test_bad - ValueError: invalid literal
"#;

#[test]
fn test_parse_pytest_counts_and_failures() {
    let parsed = parse_pytest(PYTEST_FIXTURE);
    assert_eq!(parsed.failures.len(), 3);
    assert_eq!(parsed.error_count, 1, "one ERROR line");
    assert!(parsed.headline.contains("2 failed"));
    assert!(parsed.headline.contains("5 passed"));
    assert!(parsed.headline.contains("1 errors"));
    assert_eq!(parsed.failures[0].name, "tests/test_a.py::test_add");
    assert_eq!(parsed.failures[0].detail, "AssertionError: 1 + 1 != 3");
}

const TSC_FIXTURE: &str = r#"src/app.ts(10,7): error TS2322: Type 'string' is not assignable to type 'number'.
src/app.ts(22,3): error TS2554: Expected 2 arguments, but got 1.
src/util.ts(5,1): warning TS6192: All imports are unused.
src/app.ts(10,7): error TS2322: Type 'string' is not assignable to type 'number'.
"#;

#[test]
fn test_parse_tsc_dedupes_errors_counts_warnings() {
    let parsed = parse_tsc(TSC_FIXTURE);
    assert_eq!(parsed.error_count, 2, "duplicate line deduped");
    assert_eq!(parsed.failures.len(), 2);
    assert_eq!(parsed.warning_counts.get("TS6192"), Some(&1));
    assert_eq!(parsed.failures[0].name, "src/app.ts(10,7) TS2322");
    assert!(parsed.failures[0].detail.contains("not assignable"));
    assert!(parsed.headline.contains("2 errors"));
    assert!(parsed.headline.contains("1 warnings"));
}

#[test]
fn test_parse_tsc_no_inputs_report() {
    let output = "error TS18003: No inputs were found in config file 'tsconfig.json'.\n";
    let parsed = parse_tsc(output);
    assert_eq!(parsed.error_count, 1);
    assert_eq!(parsed.failures[0].name, "(tsconfig) TS18003");
}

const GENERIC_FIXTURE: &str = r#"src/main.rs:12:5: error: variable does not need to be mutable
src/main.rs:30:9: warning: unused import
docs/guide.md:99:1: broken link
src/main.rs:12:5: error: duplicate line must not count twice
"#;

#[test]
fn test_parse_file_line_col_counts_unique_locations() {
    let parsed = parse_file_line_col(GENERIC_FIXTURE);
    assert_eq!(parsed.error_count, 3, "unique locations counted once");
    assert_eq!(parsed.failures.len(), 3);
    assert_eq!(parsed.failures[0].name, "src/main.rs:12:5");
    assert_eq!(
        parsed.failures[0].detail,
        "error: variable does not need to be mutable"
    );
    assert!(parsed.headline.contains("3 issues"));
}

#[test]
fn test_parse_unknown_tool_keeps_exit_code_and_tail() {
    let output = "line one\nline two\n\nline three\nline four\nline five\nline six\nline seven\nline eight\nline nine\nline ten\n";
    let parsed = parse_unknown_tool(output, 1);
    assert_eq!(parsed.headline, "tool exited with code 1");
    assert_eq!(parsed.failures.len(), 1);
    assert_eq!(parsed.failures[0].name, "last output");
    // Last 8 non-empty lines, in original order.
    let lines: Vec<&str> = parsed.failures[0].detail.lines().collect();
    assert_eq!(lines.len(), 8);
    assert_eq!(lines[0], "line three");
    assert_eq!(lines[7], "line ten");
}

#[test]
fn test_parse_unknown_tool_zero_exit() {
    let parsed = parse_unknown_tool("", 0);
    assert_eq!(parsed.headline, "tool completed (exit 0)");
    assert!(parsed.failures.is_empty());
}

#[test]
fn test_render_ends_with_absolute_log_path() {
    let summary = summarize(
        ToolKind::CargoTest,
        CARGO_TEST_FIXTURE,
        1,
        1234,
        Some(PathBuf::from("/mnt/storage/logs/run-test.log")),
    );
    let rendered = summary.render();
    assert!(
        rendered.ends_with("/mnt/storage/logs/run-test.log"),
        "{rendered}"
    );
    assert!(rendered.contains("cargo test: 3 failed, 6 passed (2 suites)"));
    assert!(rendered.contains("✗ tests::a::test_one"));
    assert!(rendered.chars().count() <= MAX_RENDER_CHARS);
}

#[test]
fn test_render_caps_at_1200_with_note() {
    let mut failures = Vec::new();
    for i in 0..300 {
        failures.push(Failure {
            name: format!("test_case_{i}"),
            detail: "assertion `left == right` failed with a long explanatory message line"
                .to_string(),
        });
    }
    let summary = Summary {
        exit_code: 1,
        duration_ms: 5,
        headline: "cargo test: 300 failed, 0 passed (2 suites)".to_string(),
        failures,
        error_count: 0,
        warning_counts: BTreeMap::new(),
        log_path: Some(PathBuf::from("/var/logs/x.log")),
    };
    let rendered = summary.render();
    assert!(
        rendered.chars().count() <= MAX_RENDER_CHARS,
        "len={}",
        rendered.chars().count()
    );
    assert!(
        rendered.contains("output truncated"),
        "truncation announced"
    );
    assert!(rendered.ends_with("/var/logs/x.log"));
}

#[test]
fn test_render_typical_failing_cargo_test_summary_is_small() {
    let summary = summarize(
        ToolKind::CargoTest,
        CARGO_TEST_FIXTURE,
        1,
        1234,
        Some(PathBuf::from("/mnt/storage/logs/run-test.log")),
    );
    let rendered = summary.render();
    // The §4.4 target is ~300 chars; keep a generous ceiling so fixture
    // wording changes cannot break this, while still catching a renderer
    // regression that dumps full blocks.
    assert!(
        rendered.chars().count() <= 600,
        "expected a small summary, got {} chars: {rendered}",
        rendered.chars().count()
    );
}
