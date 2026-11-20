//! Native arm: emulated Claude-Code-style tools.
//!
//! Emulated formats (documented per tool):
//! - `read`: `{:>6}\t{line}` per line (`cat -n` style), 2000-line window
//!   with offset/limit.
//! - `grep`: real `rg`. Content mode uses `-n` with optional `-C` context,
//!   files mode uses `-l`, count mode uses `-c`. When `rg` is not on PATH
//!   we report exactly `unsupported: rg missing` — never faked.
//! - `edit`: applies the replacement and replies with one line:
//!   `Edited <path> (1 change applied).`
//! - `git`: the real `git` CLI with `-C <fixture>`.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::tokbench::corpus;
use crate::tokbench::{BenchArm, CallRecord, Mode, TaskOutcome};

pub struct NativeArm;

impl BenchArm for NativeArm {
    fn run_task(&self, task_id: u8, fx: &corpus::Fixture) -> TaskOutcome {
        match task_id {
            corpus::T1_READ_BUILDER => task1_read_builder(fx),
            corpus::T2_FIND_CALLERS => task2_find_callers(fx),
            corpus::T3_ADD_FIELD => task3_add_field(fx),
            corpus::T4_READ_FAILURE => task4_read_failure(fx),
            corpus::T5_GIT_COMMIT => task5_git_commit(fx),
            corpus::T6_READ_AFTER_EDIT => task6_read_after_edit(fx),
            other => unreachable!("unknown task {other}"),
        }
    }
}

fn record(tool: &'static str, output: String) -> CallRecord {
    let output_chars = output.chars().count();
    CallRecord {
        tool,
        output_chars,
        ok: true,
    }
}

fn outcome(
    task_id: u8,
    mode: Mode,
    calls: Vec<CallRecord>,
    assertion_passed: bool,
    assertion_detail: String,
) -> TaskOutcome {
    TaskOutcome {
        task_id,
        mode,
        supported: true,
        unsupported_reason: None,
        calls,
        assertion_passed,
        assertion_detail,
    }
}

/// `read` emulation: `cat -n`-style numbered output over a window.
///
/// Format per line: `{:>6}\t{line}` (right-aligned 6-wide line number,
/// tab, then the raw line). `offset` is the number of lines to skip
/// before the window starts; `limit` caps how many lines are returned
/// (hard cap 2000, matching a full window).
fn read_window(path: &Path, offset: usize, limit: usize) -> Result<String, String> {
    let limit = limit.min(2000);
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut out = String::new();
    for (idx, line) in content.lines().skip(offset).take(limit).enumerate() {
        out.push_str(&format!("{:>6}\t{}\n", offset + idx + 1, line));
    }
    Ok(out)
}

enum GrepMode {
    Content { context: usize },
    Files,
    Count,
}

/// `grep` emulation: the real `rg` when present.
///
/// Returns `Err("unsupported: rg missing")` when `rg` cannot be spawned;
/// rg's "no matches" exit code (1) is treated as an empty result.
fn rg(pattern: &str, target: &Path, mode: GrepMode) -> Result<String, String> {
    let mut cmd = Command::new("rg");
    cmd.arg("--no-messages")
        .arg("-g")
        .arg("!target/**")
        .arg("-g")
        .arg("!.git/**");
    match mode {
        GrepMode::Content { context } => {
            cmd.arg("-n");
            if context > 0 {
                cmd.arg("-C").arg(context.to_string());
            }
        }
        GrepMode::Files => {
            cmd.arg("-l");
        }
        GrepMode::Count => {
            cmd.arg("-c");
        }
    }
    cmd.arg(pattern).arg(target);
    let out = cmd
        .output()
        .map_err(|e| format!("unsupported: rg missing ({e})"))?;
    if !out.status.success() {
        if out.status.code() == Some(1) {
            return Ok(String::new());
        }
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `edit` emulation: apply the replacement, reply with one line.
///
/// Replaces the first occurrence of `old` with `new`.
fn native_edit(path: &Path, old: &str, new: &str) -> Result<String, String> {
    let mut content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let pos = content
        .find(old)
        .ok_or_else(|| format!("old text not found in {}", path.display()))?;
    content.replace_range(pos..pos + old.len(), new);
    fs::write(path, content).map_err(|e| e.to_string())?;
    Ok(format!("Edited {} (1 change applied).", path.display()))
}

/// `git` emulation: the real CLI, scoped to the fixture.
fn git(fx: &corpus::Fixture, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(&fx.root)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run cargo inside the fixture and report a one-line summary.
fn cargo_run(
    fx: &corpus::Fixture,
    args: &[&str],
    log_rel: &str,
) -> Result<String, String> {
    let log = fx.path(log_rel);
    let out = Command::new("cargo")
        .args(args)
        .current_dir(&fx.root)
        .env("CARGO_TARGET_DIR", fx.path("target"))
        .output()
        .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    fs::write(&log, text).map_err(|e| e.to_string())?;
    Ok(format!(
        "exit {}; log -> {}",
        out.status.code().unwrap_or(-1),
        log.display()
    ))
}

/// Extract the line number from an `rg -n` match line for a given rel path.
fn line_from_match(match_line: &str, rel: &str) -> Option<usize> {
    let rest = match_line.strip_prefix(rel)?;
    let rest = rest.strip_prefix(':')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// T1: find the request-body builder fn and read it (assert the line number).
fn task1_read_builder(fx: &corpus::Fixture) -> TaskOutcome {
    let mut calls = Vec::new();
    let (rel, expected_line) = match corpus::expected_builder_location(fx) {
        Ok(v) => v,
        Err(e) => {
            return TaskOutcome::failed(corpus::T1_READ_BUILDER, Mode::Read, calls, e.to_string())
        }
    };
    let grep_out = match rg("fn build_request_body", &fx.root, GrepMode::Content { context: 0 }) {
        Ok(o) => o,
        Err(e) => return unsupported_or_failed(corpus::T1_READ_BUILDER, Mode::Read, e),
    };
    calls.push(record("grep", grep_out.clone()));
    let found_line = grep_out
        .lines()
        .find_map(|l| line_from_match(l, rel.as_str()));
    let start = found_line.unwrap_or(expected_line).saturating_sub(3);
    let read_out = match read_window(&fx.path(&rel), start, 28) {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T1_READ_BUILDER, Mode::Read, calls, e),
    };
    calls.push(record("read", read_out.clone()));

    let numbered = format!("{:>6}\t", found_line.unwrap_or(0));
    let pass = found_line == Some(expected_line)
        && read_out.lines().any(|l| l.starts_with(&numbered) && l.contains("fn build_request_body"));
    let detail = format!(
        "builder line {} (expected {}); read window covers it: {}",
        found_line.unwrap_or(0),
        expected_line,
        pass
    );
    outcome(corpus::T1_READ_BUILDER, Mode::Read, calls, pass, detail)
}

/// T2: find every caller of `perform_request` before editing (assert count).
fn task2_find_callers(fx: &corpus::Fixture) -> TaskOutcome {
    let mut calls = Vec::new();
    let (expected_count, expected_lines) = match corpus::expected_caller_matches(fx) {
        Ok(v) => v,
        Err(e) => {
            return TaskOutcome::failed(corpus::T2_FIND_CALLERS, Mode::Find, calls, e.to_string())
        }
    };
    let grep_out = match rg("perform_request\\(", &fx.root, GrepMode::Content { context: 0 }) {
        Ok(o) => o,
        Err(e) => return unsupported_or_failed(corpus::T2_FIND_CALLERS, Mode::Find, e),
    };
    calls.push(record("grep", grep_out.clone()));

    let lines: Vec<&str> = grep_out.lines().collect();
    let count = lines.len();
    let all_present = expected_lines.iter().all(|(rel, line)| {
        lines.iter().any(|l| line_from_match(l, rel) == Some(*line))
    });
    let pass = count == expected_count && all_present;
    let detail = format!("caller matches {count} (expected {expected_count})");
    outcome(corpus::T2_FIND_CALLERS, Mode::Find, calls, pass, detail)
}

/// T3: add a struct field and update all 3 construction sites; assert
/// exactly the 3 site edits applied and `cargo check -q` passes.
fn task3_add_field(fx: &corpus::Fixture) -> TaskOutcome {
    let mut calls = Vec::new();

    // (rel, old, new) — the struct field edit plus the 3 sites.
    let edits: [(&str, &str, &str); 4] = [
        (
            corpus::CORE_LIB_REL,
            "    pub retries: u32,\n}",
            "    pub retries: u32,\n    pub debug: bool,\n}",
        ),
        (
            corpus::CLIENT_REL,
            "RequestBody { payload: \"default\".to_string(), retries: 2 }",
            "RequestBody { payload: \"default\".to_string(), retries: 2, debug: false }",
        ),
        (
            corpus::HTTP_REL,
            "RequestBody { payload: payload.to_string(), retries: 1 }",
            "RequestBody { payload: payload.to_string(), retries: 1, debug: false }",
        ),
        (
            corpus::CLI_MAIN_REL,
            "RequestBody { payload: \"cli\".to_string(), retries: 3 }",
            "RequestBody { payload: \"cli\".to_string(), retries: 3, debug: false }",
        ),
    ];

    let mut site_edits_ok = 0usize;
    let mut saw_error = false;
    for (rel, old, new) in edits {
        match native_edit(&fx.path(rel), old, new) {
            Ok(msg) => {
                if rel != corpus::CORE_LIB_REL {
                    site_edits_ok += 1;
                }
                calls.push(record("edit", msg));
            }
            Err(e) => {
                saw_error = true;
                calls.push(CallRecord {
                    tool: "edit",
                    output_chars: e.chars().count(),
                    ok: false,
                });
            }
        }
    }

    let check_out = match cargo_run(fx, &["check", "-q"], "check-output.txt") {
        Ok(o) => o,
        Err(e) => {
            calls.push(CallRecord { tool: "cargo check", output_chars: e.chars().count(), ok: false });
            return TaskOutcome::failed(corpus::T3_ADD_FIELD, Mode::Edit, calls, e);
        }
    };
    calls.push(record("cargo check", check_out.clone()));
    let check_ok = check_out.starts_with("exit 0");

    let pass = !saw_error && site_edits_ok == 3 && check_ok;
    let detail = format!(
        "construction sites edited {site_edits_ok}/3; cargo check -q exit 0: {check_ok}"
    );
    outcome(corpus::T3_ADD_FIELD, Mode::Edit, calls, pass, detail)
}

/// T4: read failing test output and locate the failure (assert file:line).
fn task4_read_failure(fx: &corpus::Fixture) -> TaskOutcome {
    let mut calls = Vec::new();
    let (rel, expected_line) = match corpus::expected_failure_location(fx) {
        Ok(v) => v,
        Err(e) => {
            return TaskOutcome::failed(corpus::T4_READ_FAILURE, Mode::Run, calls, e.to_string())
        }
    };
    let run_out = match cargo_run(fx, &["test", "-p", "api"], "test-output.txt") {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T4_READ_FAILURE, Mode::Run, calls, e),
    };
    calls.push(record("cargo test", run_out));

    let grep_out = match rg("-->", &fx.path("test-output.txt"), GrepMode::Content { context: 0 }) {
        Ok(o) => o,
        Err(e) => return unsupported_or_failed(corpus::T4_READ_FAILURE, Mode::Run, e),
    };
    calls.push(record("grep", grep_out.clone()));

    let pass = grep_out
        .lines()
        .any(|l| l.contains(&format!("{rel}:{expected_line}")) && l.contains("-->"));
    let detail = format!("failure located at {rel}:{expected_line}: {pass}");
    outcome(corpus::T4_READ_FAILURE, Mode::Run, calls, pass, detail)
}

/// T5: stage and commit 2 named files; assert the commit exists and
/// touches exactly those 2.
fn task5_git_commit(fx: &corpus::Fixture) -> TaskOutcome {
    let mut calls = Vec::new();

    // Mark both named files dirty (setup, not a measured tool call).
    for rel in [corpus::README_REL, corpus::COMMANDS_REL] {
        let path = fx.path(rel);
        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&path) {
            use std::io::Write as _;
            let _ = writeln!(f, "// tokbench T5 marker");
        }
    }

    let add_out = match git(fx, &["add", corpus::README_REL, corpus::COMMANDS_REL]) {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T5_GIT_COMMIT, Mode::Git, calls, e),
    };
    calls.push(record("git add", add_out));

    let commit_out = match git(fx, &["commit", "-m", "tokbench: stage two named files"]) {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T5_GIT_COMMIT, Mode::Git, calls, e),
    };
    calls.push(record("git commit", commit_out));

    let show_out = match git(fx, &["show", "--name-only", "--format=", "HEAD"]) {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T5_GIT_COMMIT, Mode::Git, calls, e),
    };
    calls.push(record("git show", show_out.clone()));

    let touched: Vec<&str> = show_out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let pass = touched.len() == 2
        && touched.contains(&corpus::README_REL)
        && touched.contains(&corpus::COMMANDS_REL);
    let detail = format!(
        "commit touches {} files: {}",
        touched.len(),
        touched.join(", ")
    );
    outcome(corpus::T5_GIT_COMMIT, Mode::Git, calls, pass, detail)
}

/// T6: re-read a region after an edit; assert the second read is smaller.
fn task6_read_after_edit(fx: &corpus::Fixture) -> TaskOutcome {
    let mut calls = Vec::new();
    let perf = fx.path(corpus::PERF_LIB_REL);

    let first = match read_window(&perf, 0, 120) {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T6_READ_AFTER_EDIT, Mode::Read, calls, e),
    };
    calls.push(record("read", first.clone()));

    let edit_out = match native_edit(&perf, &corpus::perf_region_block(), "") {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T6_READ_AFTER_EDIT, Mode::Read, calls, e),
    };
    calls.push(record("edit", edit_out));

    let second = match read_window(&perf, 0, 120) {
        Ok(o) => o,
        Err(e) => return TaskOutcome::failed(corpus::T6_READ_AFTER_EDIT, Mode::Read, calls, e),
    };
    calls.push(record("read", second.clone()));

    let first_chars = first.chars().count();
    let second_chars = second.chars().count();
    let pass = second_chars < first_chars;
    let detail = format!("second read {second_chars} chars < first {first_chars} chars");
    outcome(corpus::T6_READ_AFTER_EDIT, Mode::Read, calls, pass, detail)
}

fn unsupported_or_failed(task_id: u8, mode: Mode, err: String) -> TaskOutcome {
    if err.starts_with("unsupported: rg missing") {
        TaskOutcome::unsupported(task_id, mode, err)
    } else {
        TaskOutcome::failed(task_id, mode, Vec::new(), err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokbench::TempDir;
    use std::fs;

    fn write_temp(content: &str) -> (TempDir, std::path::PathBuf) {
        let temp = TempDir::new("native-test").expect("tempdir");
        let path = temp.path().join("sample.rs");
        fs::write(&path, content).expect("write");
        (temp, path)
    }

    #[test]
    fn test_read_format_is_cat_n() {
        let (_t, path) = write_temp("first\nsecond\nthird\n");
        let out = read_window(&path, 0, 10).expect("read");
        assert_eq!(out, "     1\tfirst\n     2\tsecond\n     3\tthird\n");
    }

    #[test]
    fn test_read_window_offset_limit() {
        let (_t, path) = write_temp("a\nb\nc\nd\ne\n");
        let out = read_window(&path, 1, 2).expect("read");
        assert_eq!(out, "     2\tb\n     3\tc\n");
    }

    #[test]
    fn test_read_window_cap_2000() {
        let (_t, path) = write_temp("x\n");
        let out = read_window(&path, 0, 5000).expect("read");
        assert_eq!(out, "     1\tx\n");
    }

    #[test]
    fn test_edit_message_one_line_and_applies() {
        let (_t, path) = write_temp("let x = 1;\n");
        let msg = native_edit(&path, "x = 1", "y = 2").expect("edit");
        assert_eq!(msg, format!("Edited {} (1 change applied).", path.display()));
        assert_eq!(fs::read_to_string(&path).unwrap(), "let y = 2;\n");
    }

    #[test]
    fn test_edit_missing_old_reports_error() {
        let (_t, path) = write_temp("hello\n");
        let err = native_edit(&path, "nope", "x").unwrap_err();
        assert!(err.contains("old text not found"));
    }

    #[test]
    fn test_rg_missing_reports_unsupported() {
        // Empty PATH => rg cannot be spawned; the exact unsupported line
        // must come back, never a fake result.
        let path = std::path::Path::new("/tmp");
        let out = Command::new("rg")
            .env("PATH", "")
            .arg("-n")
            .arg("foo")
            .arg(path)
            .output();
        assert!(out.is_err(), "rg should be unreachable with empty PATH");
        let err = rg("foo", path, GrepMode::Content { context: 0 })
            .unwrap_err();
        assert!(err.starts_with("unsupported: rg missing"), "got {err}");
    }

    #[test]
    fn test_line_from_match() {
        assert_eq!(line_from_match("crates/api/src/http.rs:12:pub fn x", "crates/api/src/http.rs"), Some(12));
        assert_eq!(line_from_match("other:1:x", "crates/api/src/http.rs"), None);
    }

    /// A tiny directory tree for grep-mode tests.
    fn grep_dir() -> (TempDir, std::path::PathBuf) {
        let temp = TempDir::new("grep-test").expect("tempdir");
        fs::write(temp.path().join("a.rs"), "foo\nbar\n").expect("write a");
        fs::write(temp.path().join("b.rs"), "bar\n").expect("write b");
        (temp, temp.path().to_path_buf())
    }

    #[test]
    fn test_grep_files_with_matches() {
        let (_t, dir) = grep_dir();
        let out = rg("foo", &dir, GrepMode::Files).expect("rg files");
        assert!(out.contains("a.rs"), "got {out}");
        assert!(!out.contains("b.rs"), "got {out}");
    }

    #[test]
    fn test_grep_count() {
        let (_t, dir) = grep_dir();
        let out = rg("bar", &dir, GrepMode::Count).expect("rg count");
        assert!(out.contains("a.rs:1"), "got {out}");
        assert!(out.contains("b.rs:1"), "got {out}");
    }

    #[test]
    fn test_grep_content_context() {
        let (_t, dir) = grep_dir();
        let out = rg("foo", &dir, GrepMode::Content { context: 1 }).expect("rg content");
        assert!(out.contains("a.rs:1:foo"), "got {out}");
        // Context expands beyond the match line.
        assert!(out.lines().count() >= 2, "got {out}");
    }
}
