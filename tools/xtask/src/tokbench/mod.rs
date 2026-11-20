//! Token-efficiency benchmark harness (TOK-BENCH-001).
//!
//! Replays six scripted agent tasks against two arms:
//! - `native`: emulated Claude-Code-style tools (Read/Grep/Edit/Git).
//! - `leindex`: the built `leindex` binary driven the way an agent would.
//!
//! Both arms operate on freshly generated copies of the same deterministic
//! 4-crate Rust workspace fixture. Per task and arm the harness records
//! output chars, tokens, round trips and the success assertion, prints a
//! markdown report and writes JSON.

pub mod corpus;
pub mod leindex;
pub mod native;

use anyhow::{Context, Result};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Token estimates use `chars / 4` (no tokenizer dep is pullable into
/// xtask from this workspace; the label is explicit in every report).
pub const TOKEN_ESTIMATE_LABEL: &str = "chars/4 (estimate)";

pub fn token_estimate(chars: usize) -> usize {
    chars / 4
}

/// Minimal owned temp directory, removed on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Result<Self> {
        let pid = std::process::id();
        let base = std::env::temp_dir().join(format!("tokbench-{label}-{pid}"));
        let mut path = base.clone();
        let mut n = 0u32;
        while path.exists() {
            n += 1;
            path = base.with_file_name(format!("tokbench-{label}-{pid}-{n}"));
        }
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Workspace root — xtask lives at `<root>/tools/xtask`.
pub fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .context("CARGO_MANIFEST_DIR must be set by cargo when running xtask")?;
    let manifest_path = PathBuf::from(manifest_dir);
    manifest_path
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .context("expected xtask crate to live at <workspace>/tools/xtask")
}

/// The dominant mode a task exercises; used for the per-mode share report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Read,
    Find,
    Edit,
    Git,
    Run,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Read => "read",
            Mode::Find => "find",
            Mode::Edit => "edit",
            Mode::Git => "git",
            Mode::Run => "run",
        }
    }
}

/// One measured tool invocation within a task.
#[derive(Debug)]
pub struct CallRecord {
    pub tool: &'static str,
    pub output_chars: usize,
    pub ok: bool,
}

#[derive(Debug)]
pub struct TaskOutcome {
    pub task_id: u8,
    pub mode: Mode,
    pub supported: bool,
    pub unsupported_reason: Option<String>,
    pub calls: Vec<CallRecord>,
    pub assertion_passed: bool,
    pub assertion_detail: String,
}

impl TaskOutcome {
    pub fn unsupported(task_id: u8, mode: Mode, reason: impl Into<String>) -> Self {
        Self {
            task_id,
            mode,
            supported: false,
            unsupported_reason: Some(reason.into()),
            calls: Vec::new(),
            assertion_passed: false,
            assertion_detail: String::new(),
        }
    }

    pub fn failed(task_id: u8, mode: Mode, calls: Vec<CallRecord>, detail: String) -> Self {
        Self {
            task_id,
            mode,
            supported: true,
            unsupported_reason: None,
            calls,
            assertion_passed: false,
            assertion_detail: detail,
        }
    }

    pub fn output_chars(&self) -> usize {
        self.calls.iter().map(|c| c.output_chars).sum()
    }

    pub fn round_trips(&self) -> usize {
        self.calls.len()
    }

    pub fn tokens(&self) -> usize {
        token_estimate(self.output_chars())
    }
}

/// One arm of the benchmark.
pub trait BenchArm {
    fn run_task(&self, task_id: u8, fx: &corpus::Fixture) -> TaskOutcome;
}

pub fn run_tokbench() -> Result<()> {
    let native_fx = corpus::Fixture::build()?;
    let leindex_fx = corpus::Fixture::build()?;

    let leindex_bin = leindex::resolve_binary()?;

    let native_arm = native::NativeArm;
    let leindex_arm = leindex::LeIndexArm {
        bin: leindex_bin,
    };

    let task_ids = [
        corpus::T1_READ_BUILDER,
        corpus::T2_FIND_CALLERS,
        corpus::T3_ADD_FIELD,
        corpus::T4_READ_FAILURE,
        corpus::T5_GIT_COMMIT,
        corpus::T6_READ_AFTER_EDIT,
    ];

    let mut native_outcomes = Vec::new();
    let mut leindex_outcomes = Vec::new();
    for id in task_ids {
        let n = native_arm.run_task(id, &native_fx);
        let l = leindex_arm.run_task(id, &leindex_fx);
        if !l.supported {
            println!("task {id}: UNSUPPORTED in this build");
        }
        native_outcomes.push(n);
        leindex_outcomes.push(l);
    }

    let verdict = evaluate(&native_outcomes, &leindex_outcomes);
    let report_json = write_report_json(&native_outcomes, &leindex_outcomes, &verdict)?;
    print_report(&native_outcomes, &leindex_outcomes, &verdict, &report_json);

    if verdict.pass {
        println!("\ntokbench: PASS");
        Ok(())
    } else {
        println!("\ntokbench: FAIL");
        let _ = std::io::stdout().flush();
        std::process::exit(1);
    }
}

struct Verdict {
    comparable_tasks: usize,
    violations: Vec<String>,
    pass: bool,
}

/// Gate rule (report §4.6): for every comparable task, LeIndex must use
/// at least 40% fewer tokens, at most as many round trips, and succeed
/// no worse than native. With zero comparable tasks we refuse to claim a
/// pass — an all-unsupported LeIndex run must not look green.
fn evaluate(native: &[TaskOutcome], leindex: &[TaskOutcome]) -> Verdict {
    let mut comparable = 0usize;
    let mut violations = Vec::new();
    for (n, l) in native.iter().zip(leindex) {
        if !n.supported || !l.supported {
            continue;
        }
        comparable += 1;
        let mut why = Vec::new();
        if l.tokens() * 10 > n.tokens() * 6 {
            why.push(format!(
                "tokens {}/{} (need <=60%)",
                l.tokens(),
                n.tokens()
            ));
        }
        if l.round_trips() > n.round_trips() {
            why.push(format!(
                "round trips {}/{} (need <=)",
                l.round_trips(),
                n.round_trips()
            ));
        }
        if !l.assertion_passed && n.assertion_passed {
            why.push(String::from("assertion failed (native passed)"));
        }
        if !why.is_empty() {
            violations.push(format!("T{}: {}", l.task_id, why.join("; ")));
        }
    }
    let pass = comparable > 0 && violations.is_empty();
    Verdict {
        comparable_tasks: comparable,
        violations,
        pass,
    }
}

fn print_report(
    native: &[TaskOutcome],
    leindex: &[TaskOutcome],
    verdict: &Verdict,
    report_json: &Path,
) {
    println!("\n# TokBench report");
    println!("Token estimate: {TOKEN_ESTIMATE_LABEL}");
    println!();
    println!("| Task | Mode | Arm | Status | Output chars | Tokens (est) | Round trips | Assertion |");
    println!("|---|---|---|---|---|---|---|---|");
    for (n, l) in native.iter().zip(leindex) {
        for (arm, outcome) in [("native", n), ("leindex", l)] {
            let (status, chars, tokens, rt, assertion) = if outcome.supported {
                (
                    if outcome.assertion_passed { "ok" } else { "fail" },
                    outcome.output_chars().to_string(),
                    outcome.tokens().to_string(),
                    outcome.round_trips().to_string(),
                    if outcome.assertion_passed { "PASS" } else { "FAIL" },
                )
            } else {
                (
                    "unsupported",
                    "–".to_string(),
                    "–".to_string(),
                    "–".to_string(),
                    "–",
                )
            };
            println!(
                "| T{} | {} | {} | {} | {} | {} | {} | {} |",
                outcome.task_id,
                outcome.mode.label(),
                arm,
                status,
                chars,
                tokens,
                rt,
                assertion
            );
        }
    }
    println!();
    print_mode_share(native, leindex);
    println!();
    println!("## Verdict");
    println!("Comparable tasks (both arms supported): {}", verdict.comparable_tasks);
    for v in &verdict.violations {
        println!("- violation: {v}");
    }
    println!("Result: {}", if verdict.pass { "PASS" } else { "FAIL" });
    println!();
    println!("JSON report: {}", report_json.display());
}

/// Per-mode token share across both arms (supported tasks only).
fn print_mode_share(native: &[TaskOutcome], leindex: &[TaskOutcome]) {
    println!("## Per-mode share");
    println!("| Mode | native chars | native tokens | leindex chars | leindex tokens |");
    println!("|---|---|---|---|---|");
    let modes = [Mode::Read, Mode::Find, Mode::Edit, Mode::Git, Mode::Run];
    for mode in modes {
        let nat: (usize, usize) = native
            .iter()
            .filter(|o| o.supported && o.mode == mode)
            .fold((0, 0), |acc, o| (acc.0 + o.output_chars(), acc.1 + o.tokens()));
        let lei: (usize, usize) = leindex
            .iter()
            .filter(|o| o.supported && o.mode == mode)
            .fold((0, 0), |acc, o| (acc.0 + o.output_chars(), acc.1 + o.tokens()));
        println!(
            "| {} | {} | {} | {} | {} |",
            mode.label(),
            nat.0,
            nat.1,
            lei.0,
            lei.1
        );
    }
}

fn write_report_json(
    native: &[TaskOutcome],
    leindex: &[TaskOutcome],
    verdict: &Verdict,
) -> Result<PathBuf> {
    let out_dir = workspace_root()?.join("target");
    fs::create_dir_all(&out_dir)?;
    let path = out_dir.join("tokbench-report.json");

    let mut s = String::new();
    s.push_str("{\n");
    s.push_str(&format!("  \"tool\": \"cargo xtask tokbench\",\n"));
    s.push_str(&format!(
        "  \"token_label\": \"{}\",\n",
        json_escape(TOKEN_ESTIMATE_LABEL)
    ));
    s.push_str("  \"arms\": [\"native\", \"leindex\"],\n");
    s.push_str("  \"tasks\": [\n");
    let mut first_row = true;
    for (n, l) in native.iter().zip(leindex) {
        for (arm, o) in [("native", n), ("leindex", l)] {
            if !first_row {
                s.push_str(",\n");
            }
            first_row = false;
            s.push_str("    {");
            s.push_str(&format!("\"task\": \"T{}\", ", o.task_id));
            s.push_str(&format!("\"mode\": \"{}\", ", o.mode.label()));
            s.push_str(&format!("\"arm\": \"{}\", ", arm));
            s.push_str(&format!("\"supported\": {}, ", o.supported));
            if o.supported {
                s.push_str(&format!("\"output_chars\": {}, ", o.output_chars()));
                s.push_str(&format!("\"tokens\": {}, ", o.tokens()));
                s.push_str(&format!("\"round_trips\": {}, ", o.round_trips()));
                s.push_str(&format!("\"assertion_passed\": {}, ", o.assertion_passed));
                s.push_str(&format!(
                    "\"assertion_detail\": \"{}\"",
                    json_escape(&o.assertion_detail)
                ));
            } else {
                s.push_str(&format!(
                    "\"unsupported_reason\": \"{}\"",
                    json_escape(o.unsupported_reason.as_deref().unwrap_or(""))
                ));
            }
            s.push_str("}");
        }
    }
    s.push_str("\n  ],\n");
    s.push_str("  \"verdict\": {\n");
    s.push_str(&format!(
        "    \"comparable_tasks\": {},\n",
        verdict.comparable_tasks
    ));
    s.push_str(&format!("    \"pass\": {},\n", verdict.pass));
    s.push_str(&format!(
        "    \"violations\": [{}]\n",
        verdict
            .violations
            .iter()
            .map(|v| format!("\"{}\"", json_escape(v)))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    s.push_str("  }\n");
    s.push_str("}\n");

    fs::write(&path, s)?;
    Ok(path)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}
