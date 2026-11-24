//! End-to-end smoke tests (assignment: "a smoke test that runs one task end
//! to end"), plus the loud-degradation contract for master-missing features.

use super::*;
use crate::leindex_arm::SKIPPED_NO_RG;
use tokio::runtime::Runtime;

/// Materialize one fresh fixture copy under `name` inside `dir`.
fn fixture_in(dir: &Path, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let fixture = dir.join(name);
    std::fs::create_dir_all(&fixture)?;
    corpus::materialize(&fixture)?;
    Ok(fixture)
}

/// The NATIVE arm's grep steps need `rg` on PATH (Claude Code's Grep does
/// too). The tasks report that case loudly with an explicit note instead of
/// faking a result; only that exact note is a permitted skip here. Any other
/// failure, and every assertion when `rg` IS present, is a real failure.
fn skipped_because_no_rg(m: &TaskMetrics) -> bool {
    m.note == SKIPPED_NO_RG
}

#[test]
fn test_task1_native_end_to_end_on_fixture() {
    let counter = Counter::load();
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = fixture_in(dir.path(), "t1-native").expect("fixture");
    let m = task1_native(&fixture, &counter);
    if skipped_because_no_rg(&m) {
        eprintln!("skipping: {SKIPPED_NO_RG}");
        return;
    }
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
    if skipped_because_no_rg(&m) {
        eprintln!("skipping: {SKIPPED_NO_RG}");
        return;
    }
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
fn test_task3_leindex_end_to_end_on_fixture() {
    // Lane F's run tool is real on the integrated tree: one `action=run`
    // call runs `cargo test` through the sandbox, and the structured summary
    // must locate the pinned failing line.
    let counter = Counter::load();
    let rt = Runtime::new().expect("tokio runtime");
    let arm = LeIndexArm::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = fixture_in(dir.path(), "t3-leindex").expect("fixture");
    arm.index(&rt, fixture.display().to_string().as_str())
        .expect("index the fixture");
    let m = task3_leindex(&fixture, &rt, arm.as_ref(), &counter);
    assert!(m.success, "note: {}", m.note);
    assert!(m.tokens > 0, "the run summary must be measured");
    assert_eq!(
        m.round_trips, 1,
        "one run call replaces Bash + copy-log + find"
    );
}

/// A tokenizer that cannot encode the measured text must never have its
/// numbers quietly replaced by an estimate under the `Tokenizer` label: the
/// rollup is flagged, and `run_all_labelled` redoes the WHOLE run with the
/// estimator and reports the estimator as the counter that produced it.
#[test]
fn test_measure_flags_tokenizer_failure_and_run_restarts_with_estimator() {
    use crate::metrics::Mode;
    use tokenizers::Tokenizer;
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;

    // Fixed vocabulary that does NOT contain the words of task 1's output.
    let vocab: std::collections::HashMap<String, u32> =
        [("only".to_string(), 0u32), ("known".to_string(), 1)]
            .into_iter()
            .collect();
    let model = WordLevel::builder().vocab(vocab).build().unwrap();
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_pre_tokenizer(Some(Whitespace));
    let brittle = Counter::from_tokenizer(tokenizer);

    let mut failed = TaskMetrics::new(1);
    measure(&mut failed, &brittle, Mode::Read, "Read", "fn main {}");
    assert!(failed.tokenizer_failed, "the failure must be flagged");
    assert!(
        failed.note.contains("tokenizer encode failed"),
        "{}",
        failed.note
    );
    assert_eq!(failed.round_trips, 0, "a failed encode contributes no call");

    let mut fine = TaskMetrics::new(1);
    measure(&mut fine, &brittle, Mode::Read, "Read", "only known");
    assert!(!fine.tokenizer_failed, "{}", fine.note);
    assert_eq!(fine.round_trips, 1);
    assert_eq!(fine.tokens, 2);

    // End to end: the full corpus under a tokenizer that cannot encode real
    // tool output is restarted with the estimator, every rollup is estimated,
    // and the estimator is the counter reported with the run.
    let rt = Runtime::new().expect("tokio runtime");
    let scratch = tempfile::tempdir().expect("scratch").keep();
    let (run, used) = run_all_in_scratch(&rt, &brittle, &scratch);
    let run = run.expect("restart run completes");
    assert!(
        run.tasks
            .iter()
            .all(|(native, leindex)| !native.tokenizer_failed && !leindex.tokenizer_failed),
        "the restarted run must be fully estimated"
    );
    assert_eq!(
        used.kind(),
        crate::tokens::TokenCounter::Estimate,
        "the restarted run is reported as an estimate"
    );
}
