//! Pure output parsers for the `run` capability (§4.4).
//!
//! Every parser turns raw tool output into a [`Parsed`] structure with zero
//! I/O; the caller supplies exit code, duration and log path, and
//! [`summarize`] assembles the final [`Summary`]. The rendered summary is meant
//! to replace the 3-call `cmd > log; cp log scratch; find pattern in log`
//! workflow with one call and roughly 300 chars of output.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Aggregate summary of one tool run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    /// Process exit code.
    pub exit_code: i32,
    /// Wall-clock run duration, milliseconds.
    pub duration_ms: u64,
    /// One-line headline (e.g. `cargo test: 3 failed, 6 passed (2 suites)`).
    pub headline: String,
    /// Failing tests / compiler errors, each with a human detail line.
    pub failures: Vec<Failure>,
    /// Number of distinct error diagnostics (compiler errors, pytest errors,
    /// tsc errors, generic issues). Not the same as failing tests.
    pub error_count: usize,
    /// Warnings counted per lint/error code, never listed in full.
    pub warning_counts: BTreeMap<String, usize>,
    /// Absolute path of the stored log, if any. [`render`] always ends with it.
    pub log_path: Option<PathBuf>,
}

/// One failure (failing test, compiler error, pytest error, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    /// Failing test name, or `file:line [code]`.
    pub name: String,
    /// First panic/assertion line, or the compiler message.
    pub detail: String,
}

/// Parser result: everything a [`Summary`] needs except the run metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    /// Headline for the summary.
    pub headline: String,
    /// Failures, in output order.
    pub failures: Vec<Failure>,
    /// Distinct error-diagnostic count.
    pub error_count: usize,
    /// Warning counts keyed by lint code.
    pub warning_counts: BTreeMap<String, usize>,
}

/// Which parser to run. The caller selects this from the tool it executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    /// `cargo test` text output.
    CargoTest,
    /// `cargo --message-format=json` output.
    CargoJson,
    /// `pytest` text output.
    Pytest,
    /// `tsc` text output.
    Tsc,
    /// Generic `file:line:col` diagnostics.
    FileLineCol,
    /// Unknown tool: fall back to exit code + last lines.
    UnknownTool,
}

/// Hard cap on [`render`] output. Anything beyond is cut with an explicit note.
pub(crate) const MAX_RENDER_CHARS: usize = 1200;

impl Summary {
    /// Render the summary. Ends with the absolute log path, hard-capped at
    /// [`MAX_RENDER_CHARS`] with an explicit truncation note.
    pub fn render(&self) -> String {
        let mut body = String::new();
        body.push_str(&self.headline);
        body.push('\n');

        if self.error_count > 0 && !self.headline.contains("error") {
            body.push_str(&format!("{} errors\n", self.error_count));
        }
        if !self.warning_counts.is_empty() {
            let total: usize = self.warning_counts.values().sum();
            body.push_str(&format!("{total} warnings\n"));
        }
        for failure in &self.failures {
            let detail = failure.detail.lines().next().unwrap_or("").trim();
            if detail.is_empty() {
                body.push_str(&format!("✗ {}\n", failure.name));
            } else {
                body.push_str(&format!("✗ {} — {detail}\n", failure.name));
            }
        }

        let log_part = match &self.log_path {
            Some(path) => format!("\nlog: {}", path.display()),
            None => String::new(),
        };
        let budget = MAX_RENDER_CHARS.saturating_sub(log_part.chars().count());
        if body.chars().count() > budget {
            body = truncate_to_chars(&body, budget);
        }
        body.push_str(&log_part);
        body
    }
}

/// Cut `text` to at most `limit` chars, announcing the cut line count.
fn truncate_to_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit.saturating_sub(48)).collect();
    format!(
        "{kept}… [output truncated — {} chars cut]",
        text.chars().count() - kept.chars().count()
    )
}

/// Assemble a [`Summary`] from a parser result. Pure: no I/O.
pub(crate) fn summarize(
    kind: ToolKind,
    output: &str,
    exit_code: i32,
    duration_ms: u64,
    log_path: Option<PathBuf>,
) -> Summary {
    let parsed = match kind {
        ToolKind::CargoTest => parse_cargo_test(output),
        ToolKind::CargoJson => parse_cargo_json(output),
        ToolKind::Pytest => parse_pytest(output),
        ToolKind::Tsc => parse_tsc(output),
        ToolKind::FileLineCol => parse_file_line_col(output),
        ToolKind::UnknownTool => parse_unknown_tool(output, exit_code),
    };
    Summary {
        exit_code,
        duration_ms,
        headline: parsed.headline,
        failures: parsed.failures,
        error_count: parsed.error_count,
        warning_counts: parsed.warning_counts,
        log_path,
    }
}

/// Parse `cargo test` text output.
///
/// - aggregate every `test result:` line into suite count and passed/failed
///   totals;
/// - collect failing test names from `failures:` blocks;
/// - for each `---- <name> stdout ----` block, take the first panic/assertion
///   line as that test's detail.
pub(crate) fn parse_cargo_test(output: &str) -> Parsed {
    let mut suites = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;

    // name -> detail from stdout blocks
    let mut block_details: BTreeMap<String, String> = BTreeMap::new();
    // failing names in output order (from `failures:` blocks)
    let mut failing_names: Vec<String> = Vec::new();

    let mut in_failures = false;
    let mut block: Option<String> = None;
    let mut block_lines: Vec<String> = Vec::new();

    let finish_block = |block: &mut Option<String>,
                        lines: &mut Vec<String>,
                        out: &mut BTreeMap<String, String>| {
        if let Some(name) = block.take() {
            let detail = lines
                .iter()
                .find(|line| line.contains("panicked") || line.contains("assertion"))
                .cloned()
                .unwrap_or_default();
            if !detail.is_empty() {
                out.insert(name, detail.trim().to_string());
            }
            lines.clear();
        }
    };

    for raw in output.lines() {
        let line = raw.trim();

        if let Some(rest) = line.strip_prefix("test result:") {
            suites += 1;
            let words: Vec<&str> = rest
                .split_whitespace()
                .map(|word| word.trim_matches(|c| c == ';' || c == '.'))
                .collect();
            passed += count_after(&words, "passed").unwrap_or(0);
            failed += count_after(&words, "failed").unwrap_or(0);
            continue;
        }

        if line == "failures:" || line.starts_with("failures:") {
            in_failures = true;
            continue;
        }
        if in_failures {
            if line.is_empty() {
                in_failures = false;
                continue;
            }
            if line.starts_with("test result:")
                || line.starts_with("----")
                || line.starts_with("failures:")
            {
                in_failures = false;
            } else {
                failing_names.push(line.to_string());
                continue;
            }
        }

        if (line.starts_with("----") && line.ends_with("stdout ----"))
            || (line.starts_with("----") && line.ends_with(" stderr ----"))
        {
            finish_block(&mut block, &mut block_lines, &mut block_details);
            let name = line
                .trim_start_matches("----")
                .trim_end_matches(" stdout ----")
                .trim_end_matches(" stderr ----")
                .trim()
                .to_string();
            block = Some(name);
            continue;
        }

        if block.is_some() {
            block_lines.push(line.to_string());
        }
    }
    finish_block(&mut block, &mut block_lines, &mut block_details);

    let mut failures: Vec<Failure> = Vec::new();
    for name in &failing_names {
        failures.push(Failure {
            name: name.clone(),
            detail: block_details.remove(name).unwrap_or_default(),
        });
    }
    // Blocks not listed under `failures:` still carry a real failure.
    for (name, detail) in block_details {
        failures.push(Failure { name, detail });
    }

    let headline = if failed > 0 {
        format!("cargo test: {failed} failed, {passed} passed ({suites} suites)")
    } else {
        format!("cargo test: {passed} passed ({suites} suites)")
    };
    Parsed {
        headline,
        failures,
        error_count: 0,
        warning_counts: BTreeMap::new(),
    }
}

/// Find `N key` in a whitespace-split segment (e.g. `3 failed`).
fn count_after(words: &[&str], key: &str) -> Option<usize> {
    words
        .windows(2)
        .find(|window| window[1] == key)
        .and_then(|window| window[0].parse().ok())
}

/// Parse `cargo --message-format=json` output.
///
/// Collects `compiler-message` entries, deduplicated by `code + file:line`.
/// Errors become failures; warnings are counted per lint code, never listed.
pub(crate) fn parse_cargo_json(output: &str) -> Parsed {
    let mut seen: HashSet<(Option<String>, String)> = HashSet::new();
    let mut failures: Vec<Failure> = Vec::new();
    let mut warning_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut errors = 0usize;

    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"].as_str() != Some("compiler-message") {
            continue;
        }
        let message = &value["message"];
        let level = message["level"].as_str().unwrap_or("");
        let code = message["code"]["code"].as_str().map(str::to_string);
        let span = message["spans"].as_array().and_then(|spans| spans.first());
        let file = span
            .and_then(|s| s["file_name"].as_str())
            .map(str::to_string);
        let line_number = span.and_then(|s| s["line_start"].as_u64());
        let location = match (&file, line_number) {
            (Some(file), Some(line)) => format!("{file}:{line}"),
            _ => "?".to_string(),
        };
        let key = (code.clone(), location.clone());
        if !seen.insert(key) {
            continue;
        }
        let text = message["message"].as_str().unwrap_or("").trim().to_string();
        match level {
            "error" => {
                errors += 1;
                let name = match &code {
                    Some(code) => format!("{location} {code}"),
                    None => location.clone(),
                };
                failures.push(Failure { name, detail: text });
            }
            "warning" => {
                let lint = code.unwrap_or_else(|| "warning".to_string());
                *warning_counts.entry(lint).or_insert(0) += 1;
            }
            _ => {}
        }
    }

    let warnings: usize = warning_counts.values().sum();
    let headline = format!("cargo check: {errors} errors, {warnings} warnings");
    Parsed {
        headline,
        failures,
        error_count: errors,
        warning_counts,
    }
}

/// Parse `pytest` text output.
///
/// Reads the final session summary line (`=== N failed, M passed, K error in
/// <time> ===`) and the `FAILED`/`ERROR` lines, which carry the failure name
/// and a one-line detail after ` - `.
pub(crate) fn parse_pytest(output: &str) -> Parsed {
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut errors = 0usize;
    let mut error_lines = 0usize;
    let mut failures: Vec<Failure> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("===") && trimmed.contains(" in ") && trimmed.ends_with("===") {
            let words: Vec<&str> = trimmed
                .split_whitespace()
                .map(|word| word.trim_matches(','))
                .collect();
            let label_count = |label: &str| {
                words
                    .windows(2)
                    .find(|window| window[1] == label && window[0].parse::<usize>().is_ok())
                    .and_then(|window| window[0].parse().ok())
                    .unwrap_or(0)
            };
            passed += label_count("passed");
            failed += label_count("failed");
            errors = errors.max(label_count("error"));
            continue;
        }
        for kind in ["FAILED ", "ERROR "] {
            if let Some(rest) = trimmed.strip_prefix(kind) {
                let (name, detail) = match rest.split_once(" - ") {
                    Some((name, detail)) => (name.trim().to_string(), detail.trim().to_string()),
                    None => (rest.to_string(), String::new()),
                };
                if kind == "ERROR " {
                    error_lines += 1;
                }
                failures.push(Failure { name, detail });
            }
        }
    }
    // The session summary and the `ERROR` lines describe the same errors.
    let errors = errors.max(error_lines);

    let mut headline = format!("pytest: {failed} failed, {passed} passed");
    if errors > 0 {
        headline.push_str(&format!(", {errors} errors"));
    }
    Parsed {
        headline,
        failures,
        error_count: errors,
        warning_counts: BTreeMap::new(),
    }
}

/// Parse `tsc` text output.
///
/// `file(line,col): error|warning CODE: message` lines. Each distinct
/// `code + file:line` counts once; warnings are tallied per code.
pub(crate) fn parse_tsc(output: &str) -> Parsed {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut failures: Vec<Failure> = Vec::new();
    let mut warning_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut errors = 0usize;

    for line in output.lines() {
        let Some((location, severity, code, message)) = tsc_line(line) else {
            continue;
        };
        if !seen.insert((code.clone(), location.clone())) {
            continue;
        }
        if severity == "error" {
            errors += 1;
            failures.push(Failure {
                name: format!("{location} {code}"),
                detail: message,
            });
        } else {
            *warning_counts.entry(code).or_insert(0) += 1;
        }
    }

    let warnings: usize = warning_counts.values().sum();
    let headline = format!("tsc: {errors} errors, {warnings} warnings");
    Parsed {
        headline,
        failures,
        error_count: errors,
        warning_counts,
    }
}

/// Parse one tsc line into `(location, severity, code, message)`.
fn tsc_line(line: &str) -> Option<(String, String, String, String)> {
    // Shape: `src/app.ts(10,7): error TS2322: message`
    let bytes = line.as_bytes();
    let mut column = 0usize;
    while column < bytes.len() {
        if bytes[column] != b'(' {
            column += 1;
            continue;
        }
        let open = column;
        let close = line[open..].find(')').map(|offset| open + offset)?;
        let inside = &line[open + 1..close];
        let mut parts = inside.split(',');
        let (Ok(line_number), Ok(col_number)) = (
            parts.next()?.trim().parse::<usize>(),
            parts.next()?.trim().parse::<usize>(),
        ) else {
            column += 1;
            continue;
        };
        let after = &line[close + 1..];
        let Some(after) = after.strip_prefix(": ") else {
            column += 1;
            continue;
        };
        let (severity, rest) = if let Some(rest) = after.strip_prefix("error ") {
            ("error", rest)
        } else if let Some(rest) = after.strip_prefix("warning ") {
            ("warning", rest)
        } else {
            column += 1;
            continue;
        };
        let code_end = rest.find(':')?;
        let code = rest[..code_end].trim().to_string();
        let message = rest[code_end + 1..].trim().to_string();
        let file = line[..open].trim();
        if file.is_empty() {
            return None;
        }
        return Some((
            format!("{file}({line_number},{col_number})"),
            severity.to_string(),
            code,
            message,
        ));
    }

    // Shape: `error TS18003: No inputs were found…` (no file/location).
    let (severity, rest) = if let Some(rest) = line.strip_prefix("error ") {
        ("error", rest)
    } else {
        let rest = line.strip_prefix("warning ")?;
        ("warning", rest)
    };
    let code_end = rest.find(':')?;
    let code = rest[..code_end].trim().to_string();
    let message = rest[code_end + 1..].trim().to_string();
    Some((
        "(tsconfig)".to_string(),
        severity.to_string(),
        code,
        message,
    ))
}

/// Parse generic `file:line:col` diagnostics.
///
/// Every line whose prefix looks like a path followed by `:line:col` counts as
/// one distinct issue; the rest of the line is its detail.
pub(crate) fn parse_file_line_col(output: &str) -> Parsed {
    let mut seen: HashSet<String> = HashSet::new();
    let mut failures: Vec<Failure> = Vec::new();

    for line in output.lines() {
        if let Some((location, message)) = file_line_col(line)
            && seen.insert(location.clone())
        {
            failures.push(Failure {
                name: location,
                detail: message,
            });
        }
    }

    let count = failures.len();
    let headline = format!("{count} issues found");
    Parsed {
        headline,
        failures,
        error_count: count,
        warning_counts: BTreeMap::new(),
    }
}

/// Split a `file:line:col: message` line into `(location, message)`.
fn file_line_col(line: &str) -> Option<(String, String)> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b':' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == i + 1 || j >= bytes.len() || bytes[j] != b':' {
            i += 1;
            continue;
        }
        let mut k = j + 1;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        if k == j + 1 {
            i += 1;
            continue;
        }
        let path = line[..i].trim();
        if path.is_empty() || path.contains(' ') || (!path.contains('/') && !path.contains('.')) {
            i += 1;
            continue;
        }
        let location = format!("{path}{}", &line[i..k]);
        let message = line[k..].trim_start_matches(':').trim().to_string();
        return Some((location, message));
    }
    None
}

/// Unknown-tool fallback: exit code headline plus the last non-empty lines.
pub(crate) fn parse_unknown_tool(output: &str, exit_code: i32) -> Parsed {
    const TAIL_LINES: usize = 8;
    let tail: Vec<String> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(TAIL_LINES)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let failures = if tail.is_empty() {
        Vec::new()
    } else {
        vec![Failure {
            name: "last output".to_string(),
            detail: tail.join("\n"),
        }]
    };
    let headline = if exit_code == 0 {
        "tool completed (exit 0)".to_string()
    } else {
        format!("tool exited with code {exit_code}")
    };
    Parsed {
        headline,
        failures,
        error_count: 0,
        warning_counts: BTreeMap::new(),
    }
}
