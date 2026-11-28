//! Report rendering tests: the markdown table must stay a valid table no
//! matter what a task note contains, and a token increase must read as a
//! negative reduction rather than `0.0`.

use super::*;
use crate::metrics::{Call, Mode, TaskMetrics};

fn task(number: usize, tokens: usize, success: bool, note: &str) -> TaskMetrics {
    let mut metrics = TaskMetrics::new(number);
    metrics.record(&Call {
        mode: Mode::Read,
        tool: "test".to_string(),
        tokens,
        chars: tokens * 4,
    });
    metrics.success = success;
    metrics.note = note.to_string();
    metrics
}

fn run_with(native: TaskMetrics, leindex: TaskMetrics) -> CorpusRun {
    CorpusRun {
        tasks: vec![(native, leindex)],
    }
}

#[test]
fn test_table_cell_collapses_line_breaks_and_escapes_pipes() {
    assert_eq!(table_cell("plain"), "plain");
    assert_eq!(table_cell("a\nb"), "a · b");
    assert_eq!(table_cell("a|b"), "a\\|b");
    assert_eq!(table_cell("  a  \n\n  b  \n"), "a · b");
}

/// Task 2 can place multiline tool output in a note. Interpolating that
/// text directly would split the task row into several broken rows.
#[test]
fn test_markdown_keeps_a_multiline_note_inside_its_row() {
    let native = task(2, 100, true, "");
    let leindex = task(
        2,
        50,
        true,
        "git status --short:\n M crates/server/src/outbound.rs\n| exit 1",
    );
    let run = run_with(native, leindex);
    let markdown = markdown(&run, &Counter::estimate());

    let task_row = markdown
        .lines()
        .find(|line| line.starts_with("| 2 |"))
        .expect("task 2 row");
    // Exactly one row for the task, with the same number of cells as the
    // header (a split row would show up as extra lines starting with the
    // note's content).
    assert!(
        !markdown
            .lines()
            .any(|line| line.starts_with(" M crates/server")),
        "note leaked onto its own row:\n{markdown}"
    );
    // Count cells on unescaped pipes only (an escaped `\|` is cell text).
    let cells = |row: &str| row.replace("\\|", "").split('|').count();
    let header = markdown
        .lines()
        .find(|line| line.starts_with("| task |"))
        .unwrap();
    assert_eq!(cells(task_row), cells(header), "{task_row}");
    // The pipe inside the note is escaped, so it does not open a new cell.
    assert!(task_row.contains("\\| exit 1"), "{task_row}");
    // The JSON report keeps the full note verbatim.
    let json = json_report(&run, &Counter::estimate());
    assert_eq!(
        json["tasks"][0]["leindex"]["note"],
        "git status --short:\n M crates/server/src/outbound.rs\n| exit 1"
    );
}

#[test]
fn test_markdown_shows_a_token_increase_as_a_negative_reduction() {
    let native = task(1, 100, true, "");
    let leindex = task(1, 150, true, "");
    let markdown = markdown(&run_with(native, leindex), &Counter::estimate());
    let row = markdown
        .lines()
        .find(|line| line.starts_with("| 1 |"))
        .unwrap();
    assert!(row.contains("-50.0%"), "regression hidden as 0.0: {row}");
    assert!(row.contains("FAIL"), "{row}");
}
