//! Gate-math tests: the §7 threshold behavior, per-mode shares, rollups.

use super::*;

fn call(mode: Mode, tokens: usize) -> Call {
    Call {
        mode,
        tool: "test".to_string(),
        tokens,
        chars: tokens * 4,
    }
}

#[test]
fn test_reduction_pct_known_values() {
    assert_eq!(token_reduction_pct(100, 60), 40.0);
    assert_eq!(token_reduction_pct(100, 0), 100.0);
    assert_eq!(token_reduction_pct(100, 100), 0.0);
    // An INCREASE is a negative reduction, not 0.0: the size of the
    // regression must stay visible in every report.
    assert_eq!(token_reduction_pct(100, 150), -50.0);
    assert_eq!(token_reduction_pct(60, 100), -66.66666666666667);
    assert_eq!(token_reduction_pct(0, 0), 100.0);
    // A non-free arm against a free native arm is a regression, never a
    // perfect score.
    assert_eq!(token_reduction_pct(0, 10), -100.0);
}

#[test]
fn test_gate_fails_on_token_increase() {
    let mut native = TaskMetrics::new(1);
    let mut leindex = TaskMetrics::new(1);
    native.record(&call(Mode::Read, 100));
    native.success = true;
    leindex.record(&call(Mode::Read, 100));
    leindex.success = true;
    assert!(
        !task_passes_gate(&native, &leindex),
        "equal tokens is not a 40% reduction"
    );
    leindex.tokens = 150;
    assert!(
        !task_passes_gate(&native, &leindex),
        "a token increase must fail the gate"
    );
    assert!(token_reduction_pct(native.tokens, leindex.tokens) < 0.0);
}

#[test]
fn test_gate_requires_all_three_conditions() {
    let mut native = TaskMetrics::new(1);
    let mut leindex = TaskMetrics::new(1);
    // Baseline: 100 tok / 2 rt success on both sides.
    native.record(&call(Mode::Read, 60));
    native.record(&call(Mode::Find, 40));
    native.success = true;
    leindex.record(&call(Mode::Read, 30));
    leindex.record(&call(Mode::Find, 20));
    leindex.success = true;
    assert!(task_passes_gate(&native, &leindex), "50% fewer, equal rt");

    // (a) reduction below 40% fails.
    let mut shallow = leindex.clone();
    shallow.tokens = 0;
    shallow.by_mode.clear();
    shallow.record(&call(Mode::Read, 80));
    shallow.round_trips = 1;
    assert!(!task_passes_gate(&native, &shallow), "20% < 40%");

    // (b) more round trips fails even at 60% reduction.
    let mut chattier = leindex.clone();
    chattier.round_trips += 1;
    assert!(!task_passes_gate(&native, &chattier));

    // (c) success regression fails.
    let mut broken = leindex.clone();
    broken.success = false;
    assert!(!task_passes_gate(&native, &broken));

    // (d) UNSUPPORTED tasks always fail, loudly.
    let unsupported = TaskMetrics::unsupported(3, "some lane feature");
    let ok_native = native.clone();
    assert!(!task_passes_gate(&ok_native, &unsupported));
}

#[test]
fn test_record_accumulates_mode_shares_and_round_trips() {
    let mut m = TaskMetrics::new(2);
    m.record(&call(Mode::Read, 10));
    m.record(&call(Mode::Read, 15));
    m.record(&call(Mode::Find, 5));
    assert_eq!(m.tokens, 30);
    assert_eq!(m.round_trips, 3);
    assert_eq!(m.by_mode.get("read"), Some(&25));
    assert_eq!(m.by_mode.get("find"), Some(&5));
    assert_eq!(m.by_mode.get("edit"), None);
}

#[test]
fn test_unsupported_note_names_the_missing_feature() {
    let m = TaskMetrics::unsupported(4, "git mutations (lane D)");
    assert!(m.note.contains(UNSUPPORTED_IN_THIS_BUILD));
    assert!(m.note.contains("lane D"));
    assert!(!m.success);
}
