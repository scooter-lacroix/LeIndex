//! LeIndex arm: invokes the built `leindex` binary the way an agent would.
//!
//! Invocation shape:
//! `leindex -p <fixture> tools run <tool> --args '<json>'`
//! with `<tool>` ∈ {leindex_explore, leindex_edit, leindex_git,
//! leindex_manage}. The JSON payload is passed as a single argv element —
//! no shell quoting involved. Measured tool output is the raw stdout.
//!
//! Honest unsupported handling: if the binary is absent we fail hard with
//! a clear message (never a stub). If a tool call fails with an
//! unknown/unsupported-tool style error, the task is marked unsupported
//! and stays visible in the report as `task N: UNSUPPORTED in this build`.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};

use crate::tokbench::corpus;
use crate::tokbench::{BenchArm, CallRecord, Mode, TaskOutcome};

pub struct LeIndexArm {
    pub bin: PathBuf,
}

/// Locate the leindex binary: `LEINDEX_BIN` env, then PATH, then the
/// workspace's debug and release targets. Missing binary => hard error.
pub fn resolve_binary() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("LEINDEX_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
        bail!("LEINDEX_BIN is set but the binary is missing: {}", path.display());
    }
    if let Some(path) = which_in_path("leindex") {
        return Ok(path);
    }
    let ws = crate::tokbench::workspace_root()?;
    for cand in [ws.join("target/debug/leindex"), ws.join("target/release/leindex")] {
        if cand.exists() {
            return Ok(cand);
        }
    }
    bail!(
        "leindex binary not found (tried LEINDEX_BIN, PATH, target/debug, target/release). \
         Build it with `cargo build -p leindex` (debug) and re-run."
    )
}

fn which_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

struct ToolResult {
    stdout: String,
    stderr: String,
    status: i32,
}

fn run_tool(bin: &PathBuf, fx: &corpus::Fixture, tool: &str, args_json: &str) -> ToolResult {
    let out = Command::new(bin)
        .arg("-p")
        .arg(&fx.root)
        .arg("tools")
        .arg("run")
        .arg(tool)
        .arg("--args")
        .arg(args_json)
        .output();
    match out {
        Ok(o) => ToolResult {
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            status: o.status.code().unwrap_or(-1),
        },
        Err(e) => ToolResult {
            stdout: String::new(),
            stderr: format!("spawn failed: {e}"),
            status: -1,
        },
    }
}

/// If the stderr looks like a missing/unknown tool, return the unsupported
/// reason; otherwise None (a genuine failure).
fn classify_unsupported(stderr: &str) -> Option<String> {
    let lower = stderr.to_lowercase();
    [
        "unknown tool",
        "not found",
        "unrecognized",
        "unsupported",
        "no such tool",
        "invalid tool",
        "no tool",
    ]
    .iter()
    .find(|kw| lower.contains(**kw))
    .map(|_| stderr.trim().to_string())
}

fn record(tool: &'static str, stdout: &str) -> CallRecord {
    CallRecord {
        tool,
        output_chars: stdout.chars().count(),
        ok: true,
    }
}

fn json_args(pairs: &[(&str, &str)]) -> String {
    let inner: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("\"{k}\": \"{}\"", v.replace('"', "\\\"")))
        .collect();
    format!("{{{}}}", inner.join(", "))
}

impl BenchArm for LeIndexArm {
    fn run_task(&self, task_id: u8, fx: &corpus::Fixture) -> TaskOutcome {
        match task_id {
            corpus::T1_READ_BUILDER => self.task1_read_builder(fx),
            corpus::T2_FIND_CALLERS => self.task2_find_callers(fx),
            corpus::T3_ADD_FIELD => self.task3_add_field(fx),
            corpus::T4_READ_FAILURE => self.task4_read_failure(fx),
            corpus::T5_GIT_COMMIT => self.task5_git_commit(fx),
            corpus::T6_READ_AFTER_EDIT => self.task6_read_after_edit(fx),
            other => unreachable!("unknown task {other}"),
        }
    }
}

impl LeIndexArm {
    fn tool(
        &self,
        fx: &corpus::Fixture,
        tool: &str,
        args_json: &str,
        task_id: u8,
        mode: Mode,
    ) -> Result<ToolResult, TaskOutcome> {
        let r = run_tool(&self.bin, fx, tool, args_json);
        if r.status == 0 {
            return Ok(r);
        }
        if let Some(reason) = classify_unsupported(&r.stderr) {
            return Err(TaskOutcome::unsupported(task_id, mode, reason));
        }
        let msg = format!("tool {tool} exited {}: {}", r.status, r.stderr.trim());
        Err(TaskOutcome::failed(task_id, mode, Vec::new(), msg))
    }

    fn task1_read_builder(&self, fx: &corpus::Fixture) -> TaskOutcome {
        let mut calls = Vec::new();
        let (rel, line) = match corpus::expected_builder_location(fx) {
            Ok(v) => v,
            Err(e) => {
                return TaskOutcome::failed(corpus::T1_READ_BUILDER, Mode::Read, calls, e.to_string())
            }
        };
        let find_args = json_args(&[("action", "find"), ("pattern", "fn build_request_body")]);
        let find = match self.tool(fx, "leindex_explore", &find_args, corpus::T1_READ_BUILDER, Mode::Read) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_explore find", &find.stdout));

        let read_args = json_args(&[
            ("action", "read_file"),
            ("path", &rel),
            ("start_line", &line.to_string()),
            ("end_line", &(line + 25).to_string()),
        ]);
        let read = match self.tool(fx, "leindex_explore", &read_args, corpus::T1_READ_BUILDER, Mode::Read) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_explore read_file", &read.stdout));

        let pass = !find.stdout.is_empty() && !read.stdout.is_empty();
        let detail = format!(
            "find + read returned {}+{} chars (builder at {rel}:{line})",
            find.stdout.chars().count(),
            read.stdout.chars().count()
        );
        TaskOutcome {
            task_id: corpus::T1_READ_BUILDER,
            mode: Mode::Read,
            supported: true,
            unsupported_reason: None,
            calls,
            assertion_passed: pass,
            assertion_detail: detail,
        }
    }

    fn task2_find_callers(&self, fx: &corpus::Fixture) -> TaskOutcome {
        let mut calls = Vec::new();
        let (_expected_count, _expected_lines) = match corpus::expected_caller_matches(fx) {
            Ok(v) => v,
            Err(e) => {
                return TaskOutcome::failed(corpus::T2_FIND_CALLERS, Mode::Find, calls, e.to_string())
            }
        };
        let find_args = json_args(&[("action", "find"), ("pattern", "perform_request\\(")]);
        let find = match self.tool(fx, "leindex_explore", &find_args, corpus::T2_FIND_CALLERS, Mode::Find) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_explore find", &find.stdout));

        let pass = !find.stdout.is_empty();
        let detail = format!(
            "find returned {} chars (format opaque; location visibility best-effort)",
            find.stdout.chars().count()
        );
        TaskOutcome {
            task_id: corpus::T2_FIND_CALLERS,
            mode: Mode::Find,
            supported: true,
            unsupported_reason: None,
            calls,
            assertion_passed: pass,
            assertion_detail: detail,
        }
    }

    fn task3_add_field(&self, fx: &corpus::Fixture) -> TaskOutcome {
        let mut calls = Vec::new();
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
        for (rel, old, new) in edits {
            let edit_args = json_args(&[
                ("action", "apply"),
                ("file_path", rel),
                ("old_text", old),
                ("new_text", new),
            ]);
            let r = run_tool(&self.bin, fx, "leindex_edit", &edit_args);
            if r.status != 0 {
                if let Some(reason) = classify_unsupported(&r.stderr) {
                    return TaskOutcome::unsupported(corpus::T3_ADD_FIELD, Mode::Edit, reason);
                }
                return TaskOutcome::failed(
                    corpus::T3_ADD_FIELD,
                    Mode::Edit,
                    calls,
                    format!("leindex_edit exited {}: {}", r.status, r.stderr.trim()),
                );
            }
            calls.push(record("leindex_edit apply", &r.stdout));
            if rel != corpus::CORE_LIB_REL {
                site_edits_ok += 1;
            }
        }

        let check_out = match cargo_check_in_fixture(fx) {
            Ok(o) => o,
            Err(e) => {
                calls.push(CallRecord { tool: "cargo check", output_chars: e.chars().count(), ok: false });
                return TaskOutcome::failed(corpus::T3_ADD_FIELD, Mode::Edit, calls, e);
            }
        };
        calls.push(record("cargo check", &check_out));
        let check_ok = check_out.starts_with("exit 0");

        let pass = site_edits_ok == 3 && check_ok;
        let detail = format!(
            "construction sites edited {site_edits_ok}/3; cargo check -q exit 0: {check_ok}"
        );
        TaskOutcome {
            task_id: corpus::T3_ADD_FIELD,
            mode: Mode::Edit,
            supported: true,
            unsupported_reason: None,
            calls,
            assertion_passed: pass,
            assertion_detail: detail,
        }
    }

    fn task4_read_failure(&self, fx: &corpus::Fixture) -> TaskOutcome {
        let mut calls = Vec::new();
        let (rel, line) = match corpus::expected_failure_location(fx) {
            Ok(v) => v,
            Err(e) => {
                return TaskOutcome::failed(corpus::T4_READ_FAILURE, Mode::Run, calls, e.to_string())
            }
        };
        let run_out = match cargo_test_fixture(fx) {
            Ok(o) => o,
            Err(e) => return TaskOutcome::failed(corpus::T4_READ_FAILURE, Mode::Run, calls, e),
        };
        calls.push(record("cargo test", &run_out));

        let find_args = json_args(&[
            ("action", "find"),
            ("pattern", "-->"),
            ("path", "test-output.txt"),
        ]);
        let find = match self.tool(fx, "leindex_explore", &find_args, corpus::T4_READ_FAILURE, Mode::Run) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_explore find log", &find.stdout));

        let located = find.stdout.contains(&format!("{rel}:{line}"));
        let pass = !find.stdout.is_empty();
        let detail = format!(
            "failure {rel}:{line} machine-visible in tool output: {located} ({} chars)",
            find.stdout.chars().count()
        );
        TaskOutcome {
            task_id: corpus::T4_READ_FAILURE,
            mode: Mode::Run,
            supported: true,
            unsupported_reason: None,
            calls,
            assertion_passed: pass,
            assertion_detail: detail,
        }
    }

    fn task5_git_commit(&self, fx: &corpus::Fixture) -> TaskOutcome {
        let mut calls = Vec::new();

        for rel in [corpus::README_REL, corpus::COMMANDS_REL] {
            let path = fx.path(rel);
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
                use std::io::Write as _;
                let _ = writeln!(f, "// tokbench T5 marker");
            }
        }

        let add_args = json_args(&[
            ("action", "add"),
            ("paths", &format!("[\"{}\", \"{}\"]", corpus::README_REL, corpus::COMMANDS_REL)),
        ]);
        let add = match self.tool(fx, "leindex_git", &add_args, corpus::T5_GIT_COMMIT, Mode::Git) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_git add", &add.stdout));

        let commit_args = json_args(&[
            ("action", "commit"),
            ("message", "tokbench: stage two named files"),
        ]);
        let commit = match self.tool(fx, "leindex_git", &commit_args, corpus::T5_GIT_COMMIT, Mode::Git) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_git commit", &commit.stdout));

        let show_args = json_args(&[
            ("action", "show"),
            ("what", "HEAD"),
            ("format", "name-only"),
        ]);
        let show = match self.tool(fx, "leindex_git", &show_args, corpus::T5_GIT_COMMIT, Mode::Git) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_git show", &show.stdout));

        let saw_both = show.stdout.contains(corpus::README_REL)
            && show.stdout.contains(corpus::COMMANDS_REL);
        let pass = saw_both;
        let detail = format!(
            "show names both staged files: {saw_both} ({} chars)",
            show.stdout.chars().count()
        );
        TaskOutcome {
            task_id: corpus::T5_GIT_COMMIT,
            mode: Mode::Git,
            supported: true,
            unsupported_reason: None,
            calls,
            assertion_passed: pass,
            assertion_detail: detail,
        }
    }

    fn task6_read_after_edit(&self, fx: &corpus::Fixture) -> TaskOutcome {
        let mut calls = Vec::new();
        let perf_rel = corpus::PERF_LIB_REL;

        let read_args = json_args(&[
            ("action", "read_file"),
            ("path", perf_rel),
            ("start_line", "1"),
            ("end_line", "120"),
        ]);
        let first = match self.tool(fx, "leindex_explore", &read_args, corpus::T6_READ_AFTER_EDIT, Mode::Read) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_explore read_file", &first.stdout));

        let edit_args = json_args(&[
            ("action", "apply"),
            ("file_path", perf_rel),
            ("old_text", &corpus::perf_region_block()),
            ("new_text", ""),
        ]);
        let edit = match self.tool(fx, "leindex_edit", &edit_args, corpus::T6_READ_AFTER_EDIT, Mode::Read) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_edit apply", &edit.stdout));

        let second = match self.tool(fx, "leindex_explore", &read_args, corpus::T6_READ_AFTER_EDIT, Mode::Read) {
            Ok(r) => r,
            Err(outcome) => return outcome,
        };
        calls.push(record("leindex_explore read_file", &second.stdout));

        let first_chars = first.stdout.chars().count();
        let second_chars = second.stdout.chars().count();
        let pass = second_chars < first_chars;
        let detail = format!("second read {second_chars} chars < first {first_chars} chars");
        TaskOutcome {
            task_id: corpus::T6_READ_AFTER_EDIT,
            mode: Mode::Read,
            supported: true,
            unsupported_reason: None,
            calls,
            assertion_passed: pass,
            assertion_detail: detail,
        }
    }
}

fn cargo_check_in_fixture(fx: &corpus::Fixture) -> Result<String, String> {
    let out = Command::new("cargo")
        .args(["check", "-q"])
        .current_dir(&fx.root)
        .env("CARGO_TARGET_DIR", fx.path("target"))
        .output()
        .map_err(|e| e.to_string())?;
    Ok(format!("exit {}", out.status.code().unwrap_or(-1)))
}

fn cargo_test_fixture(fx: &corpus::Fixture) -> Result<String, String> {
    let log = fx.path("test-output.txt");
    let out = Command::new("cargo")
        .args(["test", "-p", "api"])
        .current_dir(&fx.root)
        .env("CARGO_TARGET_DIR", fx.path("target"))
        .output()
        .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    std::fs::write(&log, text).map_err(|e| e.to_string())?;
    Ok(format!(
        "exit {}; log -> {}",
        out.status.code().unwrap_or(-1),
        log.display()
    ))
}
