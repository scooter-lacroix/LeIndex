//! End-to-end smoke tests (assignment: "a smoke test that runs one task end
//! to end"), plus the loud-degradation contract for master-missing features.

use super::*;
use tokio::runtime::Runtime;

/// Materialize one fresh fixture copy under `name` inside `dir`.
fn fixture_in(dir: &Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let fixture = dir.join(name);
    std::fs::create_dir_all(&fixture)?;
    corpus::materialize(&fixture)?;
    Ok(fixture)
}

#[test]
fn test_task1_native_end_to_end_on_fixture() {
    let counter = Counter::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = fixture_in(dir.path(), "t1-native").expect("fixture");
    let m = task1_native(&fixture, &counter);
    assert!(m.success, "note: {}", m.note);
    assert!(m.tokens > 0, "grep+read outputs must be measured");
    assert_eq!(m.round_trips, 2, "one grep, one bounded read");
}

#[test]
fn test_task5_native_finds_every_caller_file() {
    let counter = Counter::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = fixture_in(dir.path(), "t5-native").expect("fixture");
    let m = task5_native(&fixture, &counter);
    assert!(m.success, "note: {}", m.note);
    // files_with_matches + content + 2 bounded caller reads.
    assert_eq!(m.round_trips, 4);
}

#[test]
fn test_task1_leindex_end_to_end_on_fixture() {
    let counter = Counter::load();
    let rt = Runtime::new().expect("tokio runtime");
    let arm = LeIndexArm::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = fixture_in(dir.path(), "t1-leindex").expect("fixture");
    arm.index(&rt, fixture.display().to_string().as_str())
        .expect("index the fixture");
    let m = task1_leindex(&fixture, &rt, arm.as_ref(), &counter);
    assert!(m.success, "note: {}", m.note);
    assert!(m.tokens > 0);
    assert!(m.round_trips >= 1);
}

#[test]
fn test_parse_count_output_ripgrep_count_shape() {
    // rg count mode prints `path:count` — count AFTER the last colon.
    let rg = "crates/api/src/lib.rs:2\ncrates/server/src/lib.rs:1\ncrates/provider-openai/src/lib.rs:1\n";
    assert_eq!(parse_count_output(rg), (4, 3));
}

#[test]
fn test_parse_count_output_leindex_json_payload() {
    // `leindex_find output=count` returns the structured tier verbatim.
    let payload = "{\n  \"total_files\": 3,\n  \"total_matches\": 4\n}";
    assert_eq!(parse_count_output(payload), (4, 3));
}

#[test]
fn test_has_no_matches_covers_both_arm_shapes() {
    assert!(has_no_matches(""));
    assert!(has_no_matches("0 match(es) in 0 file(s) for \"x\""));
    assert!(has_no_matches(
        "{\n  \"total_matches\": 0,\n  \"total_files\": 0\n}"
    ));
    assert!(!has_no_matches(
        "2 match(es) in 2 file(s) for \"Option<String>\""
    ));
    assert!(!has_no_matches("{\n  \"total_matches\": 4\n}"));
}

#[test]
fn test_task3_leindex_degrades_loudly_not_faked() {
    // Master has no `leindex run` (lane F). The harness must FAIL the task
    // with the UNSUPPORTED marker rather than fabricate a result.
    let m = task3_leindex();
    assert!(!m.success);
    assert!(
        m.note.contains(UNSUPPORTED_IN_THIS_BUILD),
        "note: {}",
        m.note
    );
    assert!(
        m.note.contains("lane F"),
        "note must name the missing feature"
    );
    assert_eq!(m.round_trips, 0, "no calls were made, none invented");
}
