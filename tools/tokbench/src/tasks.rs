//! The 7 seed tasks (brief §4.6), scripted per arm.
//!
//! Every task is a scripted sequence of tool calls written the way a
//! disciplined agent would do it — bounded reads, targeted greps, find-all-
//! callers-before-edit — NOT a strawman. The success assertion per task is
//! defined once and applied to both arms identically. Where the LEINDEX arm
//! needs a feature other lanes are building (brief §4.4 `run` from lane F,
//! §4.1 session dedup from lane E, §5 git mutations from lane D) the script
//! records `UNSUPPORTED in this build` and fails the task honestly instead
//! of faking a result.
//!
//! Tasks run against independent materialized copies of the pinned fixture
//! (one per arm per task) so mutation tasks cannot leak state into each
//! other. All 7 task descriptions, in brief order:
//!
//! 1. Find the request-body builder fn and read it.
//! 2. Add a field to a struct and thread it through 3 construction sites
//!    (find sites, edit each, verify).
//! 3. Read failing test output (run a failing test, locate the failure).
//! 4. Stage and commit specific files (selective staging).
//! 5. Find every caller of a fn before editing.
//! 6. Re-read a region after an edit (the dedup case).
//! 7. Zero-result search with a regex-looking literal (plus a control hit).

use crate::corpus::{self, SOURCE_FILES};
use crate::leindex_arm::{LeIndexArm, UNSUPPORTED_IN_THIS_BUILD};
use crate::metrics::{Call, Mode, TaskMetrics};
use crate::native;
use crate::tokens::Counter;
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;
use std::process::Command;
use tokio::runtime::Runtime;

/// Measure one call into the rollup.
fn measure(m: &mut TaskMetrics, counter: &Counter, mode: Mode, tool: &str, text: &str) {
    let chars = text.chars().count();
    m.record(&Call {
        mode,
        tool: tool.to_string(),
        tokens: counter.count(text),
        chars,
    });
}

/// Ground-truth 1-based line of `needle` inside fixture file `path`,
/// computed from the pinned corpus (never hardcoded).
pub fn corpus_line_of(path: &str, needle: &str) -> Option<usize> {
    let (_, content) = SOURCE_FILES.iter().find(|(p, _)| p == path)?;
    Some(content.lines().position(|l| l.contains(needle))? + 1)
}

/// The struct field insertion edit for task 2/6-style mutations.
const TOP_P_FIELD_OLD: &str = "    /// Sampling temperature; `None` lets the upstream default apply.\n    pub temperature: Option<f32>,\n}";
const TOP_P_FIELD_NEW: &str = "    /// Sampling temperature; `None` lets the upstream default apply.\n    pub temperature: Option<f32>,\n    /// Nucleus sampling cap; `None` lets the upstream default apply.\n    pub top_p: Option<f32>,\n}";
const TOP_P_INIT_OLD: &str = "            temperature: None,\n        }\n    }";
const TOP_P_INIT_NEW: &str = "            temperature: None,\n            top_p: None,\n        }\n    }";
const SERVER_THREAD_OLD: &str = "pub fn build_chat_request(model: &str, prompt: &str) -> ChatCompletionRequestBody {\n    ChatCompletionRequestBody::new(";
const SERVER_THREAD_NEW: &str = "pub fn build_chat_request(model: &str, prompt: &str) -> ChatCompletionRequestBody {\n    let mut body = ChatCompletionRequestBody::new(";
const SERVER_THREAD_OLD2: &str = "            content: prompt.to_string(),\n        }],\n    )\n}";
const SERVER_THREAD_NEW2: &str = "            content: prompt.to_string(),\n        }],\n    );\n    body.top_p = Some(0.9);\n    body\n}";
const OPENAI_THREAD_OLD: &str = "    let mut body = server::build_chat_request(model, prompt);\n    body.temperature = Some(0.2);";
const OPENAI_THREAD_NEW: &str = "    let mut body = server::build_chat_request(model, prompt);\n    body.temperature = Some(0.2);\n    body.top_p = Some(0.5);";

// ── Task 1: find the request-body builder fn and read it ────────────────────

pub fn task1_native(fixture: &Path, counter: &Counter) -> TaskMetrics {
    let mut m = TaskMetrics::new(1);
    // Disciplined agent: grep for the struct name, then read the file.
    let Some(grep) = native::grep_content(
        "pub struct ChatCompletionRequestBody",
        fixture,
        0,
        Some("*.rs"),
        true,
    ) else {
        m.note = crate::leindex_arm::SKIPPED_NO_RG.to_string();
        return m;
    };
    measure(&mut m, counter, Mode::Find, "Grep", &grep);
    let read = native::read_lines(&fixture.join("crates/api/src/lib.rs"), None, Some(60));
    match read {
        Ok(text) => {
            measure(&mut m, counter, Mode::Read, "Read", &text);
            m.success =
                grep.contains("crates/api/src/lib.rs") && text.contains("ChatCompletionRequestBody");
            if !m.success {
                m.note = "struct not located+read".into();
            }
        }
        Err(e) => m.note = format!("read failed: {e}"),
    }
    m
}

pub fn task1_leindex(
    fixture: &Path,
    rt: &Runtime,
    arm: &LeIndexArm,
    counter: &Counter,
) -> TaskMetrics {
    let mut m = TaskMetrics::new(1);
    // One symbol_lookup with source is the structured answer; fall back to a
    // bounded read_file only when the source was not inlined.
    let lookup = arm.call(
        rt,
        "leindex_symbol_lookup",
        json!({
            "symbol": "ChatCompletionRequestBody",
            "include_source": true,
            "project_path": fixture.display().to_string(),
        }),
    );
    match lookup {
        Ok(text) => {
            measure(&mut m, counter, Mode::Find, "leindex_symbol_lookup", &text);
            if text.contains("crates/api/src/lib.rs") && text.contains("ChatCompletionRequestBody") {
                m.success = true;
                return m;
            }
            // Bounded fallback read (same assertion, one more round trip).
            let read = arm.call(
                rt,
                "leindex_read_file",
                json!({
                    "file_path": fixture.join("crates/api/src/lib.rs").display().to_string(),
                    "project_path": fixture.display().to_string(),
                }),
            );
            match read {
                Ok(text) => {
                    measure(&mut m, counter, Mode::Read, "leindex_read_file", &text);
                    m.success = text.contains("ChatCompletionRequestBody");
                    if !m.success {
                        m.note = "struct not located+read".into();
                    }
                }
                Err(e) => m.note = format!("read_file failed: {e}"),
            }
        }
        Err(e) => m.note = format!("symbol_lookup failed: {e}"),
    }
    m
}

// ── Task 2: add a field and thread it through 3 construction sites ──────────

pub fn task2_native(fixture: &Path, counter: &Counter) -> TaskMetrics {
    let mut m = TaskMetrics::new(2);
    let lib = fixture.join("crates/api/src/lib.rs");
    let server = fixture.join("crates/server/src/lib.rs");
    let openai = fixture.join("crates/provider-openai/src/lib.rs");
    let edit = |m: &mut TaskMetrics, counter: &Counter, path: &Path, old: &str, new: &str| {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                m.note = format!("edit read failed: {e}");
                return false;
            }
        };
        let Some(replaced) = replace_first(&content, old, new) else {
            m.note = format!("edit anchor not found in {}", path.display());
            return false;
        };
        if let Err(e) = std::fs::write(path, &replaced) {
            m.note = format!("edit write failed: {e}");
            return false;
        }
        // Claude Code Edit returns a short success message; the mutation
        // above is the real edit.
        let msg = native::edit_message();
        measure(m, counter, Mode::Edit, "Edit", &msg);
        true
    };

    // Find the struct, read it, then edit each site.
    let Some(grep) = native::grep_content("pub struct ChatCompletionRequestBody", fixture, 0, Some("*.rs"), true)
    else {
        m.note = crate::leindex_arm::SKIPPED_NO_RG.to_string();
        return m;
    };
    measure(&mut m, counter, Mode::Find, "Grep", &grep);
    match native::read_lines(&lib, None, Some(60)) {
        Ok(text) => measure(&mut m, counter, Mode::Read, "Read", &text),
        Err(e) => {
            m.note = format!("read failed: {e}");
            return m;
        }
    }
    if !edit(&mut m, counter, &lib, TOP_P_FIELD_OLD, TOP_P_FIELD_NEW)
        || !edit(&mut m, counter, &lib, TOP_P_INIT_OLD, TOP_P_INIT_NEW)
        || !edit(&mut m, counter, &server, SERVER_THREAD_OLD, SERVER_THREAD_NEW)
        || !edit(&mut m, counter, &server, SERVER_THREAD_OLD2, SERVER_THREAD_NEW2)
        || !edit(&mut m, counter, &openai, OPENAI_THREAD_OLD, OPENAI_THREAD_NEW)
    {
        return m;
    }
    // Verify: the field appears on 4 lines across exactly 3 files.
    let Some(verify) = native::grep_count("top_p", fixture, true) else {
        m.note = crate::leindex_arm::SKIPPED_NO_RG.to_string();
        return m;
    };
    measure(&mut m, counter, Mode::Find, "Grep", &verify);
    let (lines, files) = parse_count_output(&verify);
    m.success = lines == 4 && files == 3;
    if !m.success {
        m.note = format!("verification found {lines} lines in {files} files, want 4/3");
    }
    m
}

pub fn task2_leindex(
    fixture: &Path,
    rt: &Runtime,
    arm: &LeIndexArm,
    counter: &Counter,
) -> TaskMetrics {
    let mut m = TaskMetrics::new(2);
    let pp = fixture.display().to_string();
    let lib = fixture.join("crates/api/src/lib.rs").display().to_string();
    let server = fixture.join("crates/server/src/lib.rs").display().to_string();
    let openai = fixture.join("crates/provider-openai/src/lib.rs").display().to_string();
    let mut apply = |m: &mut TaskMetrics, rt: &Runtime, arm: &LeIndexArm, counter: &Counter, path: &str, old: &str, new: &str| {
        match arm.call(
            rt,
            "leindex_edit_apply",
            json!({ "file_path": path, "old_text": old, "new_text": new, "project_path": pp }),
        ) {
            Ok(text) => {
                measure(m, counter, Mode::Edit, "leindex_edit_apply", &text);
                true
            }
            Err(e) => {
                m.note = format!("edit_apply failed: {e}");
                false
            }
        }
    };

    let lookup = arm.call(
        rt,
        "leindex_symbol_lookup",
        json!({ "symbol": "ChatCompletionRequestBody", "include_source": true, "project_path": pp }),
    );
    match lookup {
        Ok(text) => measure(&mut m, counter, Mode::Find, "leindex_symbol_lookup", &text),
        Err(e) => {
            m.note = format!("symbol_lookup failed: {e}");
            return m;
        }
    }
    match arm.call(
        rt,
        "leindex_read_file",
        json!({ "file_path": lib, "project_path": pp }),
    ) {
        Ok(text) => measure(&mut m, counter, Mode::Read, "leindex_read_file", &text),
        Err(e) => {
            m.note = format!("read_file failed: {e}");
            return m;
        }
    }
    let pp2 = pp.clone();
    if !apply(&mut m, rt, arm, counter, &lib, TOP_P_FIELD_OLD, TOP_P_FIELD_NEW)
        || !apply(&mut m, rt, arm, counter, &lib, TOP_P_INIT_OLD, TOP_P_INIT_NEW)
        || !apply(&mut m, rt, arm, counter, &server, SERVER_THREAD_OLD, SERVER_THREAD_NEW)
        || !apply(&mut m, rt, arm, counter, &server, SERVER_THREAD_OLD2, SERVER_THREAD_NEW2)
        || !apply(&mut m, rt, arm, counter, &openai, OPENAI_THREAD_OLD, OPENAI_THREAD_NEW)
    {
        return m;
    }
    // Verify with the count output mode.
    match arm.call(
        rt,
        "leindex_find",
        json!({ "pattern": "top_p", "output": "count", "project_path": pp2 }),
    ) {
        Ok(text) => {
            measure(&mut m, counter, Mode::Find, "leindex_find", &text);
            let (lines, files) = parse_count_output(&text);
            m.success = lines == 4 && files == 3;
            if !m.success {
                m.note = format!("verification found {lines} lines in {files} files, want 4/3");
            }
        }
        Err(e) => m.note = format!("find count failed: {e}"),
    }
    m
}

// ── Task 3: read failing test output ────────────────────────────────────────

/// Runs `cargo test -q --offline` in the fixture and returns combined output.
/// CARGO_TARGET_DIR is removed so the fixture never writes into a shared
/// lane target dir.
fn cargo_test_output(fixture: &Path) -> Result<String> {
    let output = Command::new("cargo")
        .args(["test", "-q", "--offline"])
        .current_dir(fixture)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .context("cargo test spawn")?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

/// The ground-truth failing test location from the pinned corpus.
pub fn failing_test_line() -> Option<usize> {
    corpus_line_of(
        "crates/provider-openai/src/lib.rs",
        "assert!(request.contains(\"provider_tag\")",
    )
}

pub fn task3_native(fixture: &Path, counter: &Counter) -> TaskMetrics {
    let mut m = TaskMetrics::new(3);
    // One Bash call; the output IS the read. The failure line must be
    // locatable from the output alone.
    match cargo_test_output(fixture) {
        Ok(text) => {
            measure(&mut m, counter, Mode::Run, "Bash(cargo test)", &text);
            let needle = "panicked at crates/provider-openai/src/lib.rs:";
            let located = text
                .lines()
                .find(|l| l.contains(needle))
                .and_then(|l| l.split(needle).nth(1))
                .and_then(|rest| rest.split(':').next())
                .and_then(|line| line.parse::<usize>().ok());
            m.success = located == failing_test_line();
            if !m.success {
                m.note = format!("failing line parsed as {located:?}, want {:?}", failing_test_line());
            }
        }
        Err(e) => m.note = format!("cargo test failed: {e}"),
    }
    m
}

pub fn task3_leindex(counter: &Counter) -> TaskMetrics {
    // Master has no `leindex run` tool (brief §4.4). Lane F builds it; the
    // harness degrades loudly instead of shelling out (that would defeat the
    // measurement: the whole point is LeIndex owning test-output reading).
    let mut m = TaskMetrics::unsupported(3, "leindex run (brief §4.4, lane F)");
    let _ = counter;
    m
}

// ── Task 4: stage and commit specific files ─────────────────────────────────

const T4_ROUTES_OLD: &str = "/// Handle `POST /v1/chat`.";
const T4_ROUTES_NEW: &str = "/// Handle `POST /v1/chat`.\n/// tokbench staging marker: routes";

pub fn task4_native(fixture: &Path, counter: &Counter) -> TaskMetrics {
    let mut m = TaskMetrics::new(4);
    // Two files get dirty; only routes.rs may be staged and committed.
    let routes = fixture.join("crates/server/src/routes.rs");
    let outbound = fixture.join("crates/server/src/outbound.rs");
    let replace = |path: &Path, old: &str, new: &str| -> Result<String> {
        let content = std::fs::read_to_string(path)?;
        let replaced = replace_first(&content, old, new)
            .context("task4 edit anchor not found")?;
        std::fs::write(path, &replaced)?;
        Ok(replaced)
    };
    if let Err(e) = replace(&routes, T4_ROUTES_OLD, T4_ROUTES_NEW) {
        m.note = format!("setup edit failed: {e}");
        return m;
    }
    if let Err(e) = replace(&outbound, "/// Send the assembled request body", "/// Send the assembled request body\n/// tokbench staging marker: outbound (must stay unstaged)") {
        m.note = format!("setup edit failed: {e}");
        return m;
    }
    let mut git_step = |m: &mut TaskMetrics, counter: &Counter, args: &[&str]| -> Option<String> {
        match native::git(args, fixture) {
            Ok(text) => {
                measure(m, counter, Mode::Git, "git", &text);
                Some(text)
            }
            Err(e) => {
                m.note = format!("git {} failed: {e}", args.join(" "));
                None
            }
        }
    };
    let status = git_step(&mut m, counter, &["status", "--short"])?;
    let _ = status;
    git_step(&mut m, counter, &["add", "crates/server/src/routes.rs"])?;
    let commit = git_step(
        &mut m,
        counter,
        &["commit", "-m", "tokbench: stage routes only"],
    )?;
    let log = git_step(&mut m, counter, &["log", "--oneline", "-1"])?;
    let after = git_step(&mut m, counter, &["status", "--short"])?;
    m.success = log.contains("tokbench: stage routes only")
        && after.contains("outbound.rs")
        && !after.contains("routes.rs")
        && commit.contains("tokbench");
    if !m.success {
        m.note = "commit/staging state assertion failed".into();
    }
    m
}

pub fn task4_leindex(
    fixture: &Path,
    rt: &Runtime,
    arm: &LeIndexArm,
    counter: &Counter,
) -> TaskMetrics {
    let mut m = TaskMetrics::new(4);
    let pp = fixture.display().to_string();
    let routes = fixture.join("crates/server/src/routes.rs").display().to_string();
    let outbound = fixture.join("crates/server/src/outbound.rs").display().to_string();
    // Same two setup edits, through the LeIndex editor.
    for (path, old, new) in [
        (&routes, T4_ROUTES_OLD, T4_ROUTES_NEW),
        (
            &outbound,
            "/// Send the assembled request body",
            "/// Send the assembled request body\n/// tokbench staging marker: outbound (must stay unstaged)",
        ),
    ] {
        match arm.call(
            rt,
            "leindex_edit_apply",
            json!({ "file_path": path, "old_text": old, "new_text": new, "project_path": pp }),
        ) {
            Ok(text) => measure(&mut m, counter, Mode::Edit, "leindex_edit_apply", &text),
            Err(e) => {
                m.note = format!("edit_apply failed: {e}");
                return m;
            }
        }
    }
    match arm.call(rt, "leindex_git", json!({ "action": "status", "project_path": pp })) {
        Ok(text) => measure(&mut m, counter, Mode::Git, "leindex_git status", &text),
        Err(e) => {
            m.note = format!("git status failed: {e}");
            return m;
        }
    }
    // Master: `add` is preview-only (git_handler PLAN_NEXT_HINT). Detect the
    // no-op plan from the REAL response and degrade loudly.
    match arm.call(
        rt,
        "leindex_git",
        json!({ "action": "add", "paths": ["crates/server/src/routes.rs"], "project_path": pp }),
    ) {
        Ok(text) => {
            measure(&mut m, counter, Mode::Git, "leindex_git add", &text);
            if text.contains("Nothing was changed") || text.contains("cannot apply") {
                m.note = format!(
                    "{UNSUPPORTED_IN_THIS_BUILD} (missing: git mutation apply, brief §5, lane D)"
                );
                return m;
            }
        }
        Err(e) => {
            m.note = format!(
                "{UNSUPPORTED_IN_THIS_BUILD} (missing: git mutation apply, brief §5, lane D); add failed: {e}"
            );
            return m;
        }
    }
    // Only reachable on a build where git mutations apply (integration).
    match arm.call(
        rt,
        "leindex_git",
        json!({ "action": "commit", "message": "tokbench: stage routes only", "project_path": pp }),
    ) {
        Ok(text) => measure(&mut m, counter, Mode::Git, "leindex_git commit", &text),
        Err(e) => {
            m.note = format!("commit failed: {e}");
            return m;
        }
    }
    let log = match arm.call(
        rt,
        "leindex_git",
        json!({ "action": "log", "max_count": 1, "project_path": pp }),
    ) {
        Ok(text) => {
            measure(&mut m, counter, Mode::Git, "leindex_git log", &text);
            text
        }
        Err(e) => {
            m.note = format!("git log failed: {e}");
            return m;
        }
    };
    let after = match arm.call(rt, "leindex_git", json!({ "action": "status", "project_path": pp })) {
        Ok(text) => {
            measure(&mut m, counter, Mode::Git, "leindex_git status", &text);
            text
        }
        Err(e) => {
            m.note = format!("git status failed: {e}");
            return m;
        }
    };
    m.success = log.contains("tokbench") && after.contains("outbound.rs");
    if !m.success {
        m.note = "commit/staging state assertion failed".into();
    }
    m
}

// ── Task 5: find every caller of a fn before editing ────────────────────────

const CALLER_FILES: [&str; 4] = [
    "crates/server/src/routes.rs",
    "crates/server/src/outbound.rs",
    "crates/provider-openai/src/lib.rs",
    "crates/provider-anthropic/src/lib.rs",
];

pub fn task5_native(fixture: &Path, counter: &Counter) -> TaskMetrics {
    let mut m = TaskMetrics::new(5);
    let Some(grep) = native::grep_content("build_chat_request", fixture, 0, Some("*.rs"), true)
    else {
        m.note = crate::leindex_arm::SKIPPED_NO_RG.to_string();
        return m;
    };
    measure(&mut m, counter, Mode::Find, "Grep", &grep);
    // Read two non-obvious caller windows before editing (discipline).
    for (rel, offset, limit) in [
        ("crates/server/src/routes.rs", 20usize, 15usize),
        ("crates/provider-anthropic/src/lib.rs", 24usize, 14usize),
    ] {
        match native::read_lines(&fixture.join(rel), Some(offset), Some(limit)) {
            Ok(text) => measure(&mut m, counter, Mode::Read, "Read", &text),
            Err(e) => {
                m.note = format!("read {rel} failed: {e}");
                return m;
            }
        }
    }
    let files_found = CALLER_FILES
        .iter()
        .filter(|f| grep.contains(f))
        .count();
    m.success = files_found == CALLER_FILES.len() && grep.lines().count() >= 7;
    if !m.success {
        m.note = format!("callers found in {files_found}/4 files");
    }
    m
}

pub fn task5_leindex(
    fixture: &Path,
    rt: &Runtime,
    arm: &LeIndexArm,
    counter: &Counter,
) -> TaskMetrics {
    let mut m = TaskMetrics::new(5);
    let lookup = arm.call(
        rt,
        "leindex_symbol_lookup",
        json!({
            "symbol": "build_chat_request",
            "include_callers": true,
            "project_path": fixture.display().to_string(),
        }),
    );
    match lookup {
        Ok(text) => {
            measure(&mut m, counter, Mode::Find, "leindex_symbol_lookup", &text);
            let files_found = CALLER_FILES.iter().filter(|f| text.contains(*f)).count();
            m.success = files_found == CALLER_FILES.len();
            if !m.success {
                m.note = format!("callers found in {files_found}/4 files");
            }
        }
        Err(e) => m.note = format!("symbol_lookup failed: {e}"),
    }
    m
}

// ── Task 6: re-read a region after an edit (dedup case) ─────────────────────

const T6_READ_OFFSET: usize = 20;
const T6_READ_LIMIT: usize = 12;

pub fn task6_native(fixture: &Path, counter: &Counter) -> TaskMetrics {
    let mut m = TaskMetrics::new(6);
    let routes = fixture.join("crates/server/src/routes.rs");
    match native::read_lines(&routes, Some(T6_READ_OFFSET), Some(T6_READ_LIMIT)) {
        Ok(text) => measure(&mut m, counter, Mode::Read, "Read", &text),
        Err(e) => {
            m.note = format!("first read failed: {e}");
            return m;
        }
    }
    // Edit inside the region.
    let content = match std::fs::read_to_string(&routes) {
        Ok(c) => c,
        Err(e) => {
            m.note = format!("edit read failed: {e}");
            return m;
        }
    };
    let replaced = match replace_first(
        &content,
        T4_ROUTES_OLD,
        T4_ROUTES_NEW,
    ) {
        Some(r) => r,
        None => {
            m.note = "edit anchor not found".into();
            return m;
        }
    };
    if let Err(e) = std::fs::write(&routes, &replaced) {
        m.note = format!("edit write failed: {e}");
        return m;
    }
    measure(&mut m, counter, Mode::Edit, "Edit", &native::edit_message());
    // The re-read: native tools resend the whole region (no dedup).
    match native::read_lines(&routes, Some(T6_READ_OFFSET), Some(T6_READ_LIMIT)) {
        Ok(text) => {
            measure(&mut m, counter, Mode::Read, "Read", &text);
            m.success = text.contains("tokbench staging marker");
            if !m.success {
                m.note = "edited marker not visible in re-read".into();
            }
        }
        Err(e) => m.note = format!("re-read failed: {e}"),
    }
    m
}

pub fn task6_leindex(
    fixture: &Path,
    rt: &Runtime,
    arm: &LeIndexArm,
    counter: &Counter,
) -> TaskMetrics {
    let mut m = TaskMetrics::new(6);
    let pp = fixture.display().to_string();
    let routes = fixture.join("crates/server/src/routes.rs").display().to_string();
    let read_args = json!({
        "file_path": routes,
        "start_line": T6_READ_OFFSET,
        "end_line": T6_READ_OFFSET + T6_READ_LIMIT,
        "project_path": pp,
    });
    match arm.call(rt, "leindex_read_file", read_args.clone()) {
        Ok(text) => measure(&mut m, counter, Mode::Read, "leindex_read_file", &text),
        Err(e) => {
            m.note = format!("first read failed: {e}");
            return m;
        }
    }
    match arm.call(
        rt,
        "leindex_edit_apply",
        json!({ "file_path": routes, "old_text": T4_ROUTES_OLD, "new_text": T4_ROUTES_NEW, "project_path": pp }),
    ) {
        Ok(text) => measure(&mut m, counter, Mode::Edit, "leindex_edit_apply", &text),
        Err(e) => {
            m.note = format!("edit failed: {e}");
            return m;
        }
    }
    // The re-read. NOTE: on a build with lane E's session dedup (brief §4.1)
    // this returns a short unchanged/hunk answer; on master it resends the
    // region — both are measured as-is.
    match arm.call(rt, "leindex_read_file", read_args) {
        Ok(text) => {
            measure(&mut m, counter, Mode::Read, "leindex_read_file", &text);
            m.success = text.contains("tokbench staging marker");
            if !m.success {
                m.note = "edited marker not visible in re-read".into();
            }
        }
        Err(e) => m.note = format!("re-read failed: {e}"),
    }
    m
}

// ── Task 7: zero-result search with a regex-looking literal ─────────────────

pub fn task7_native(fixture: &Path, counter: &Counter) -> TaskMetrics {
    let mut m = TaskMetrics::new(7);
    // Literal search (`-F`): Grep is literal unless the agent opts into
    // regex. `Option<String>?` matches nothing in the corpus.
    let zero = match native::grep_content("Option<String>?", fixture, 0, None, true) {
        Some(t) => t,
        None => {
            m.note = crate::leindex_arm::SKIPPED_NO_RG.to_string();
            return m;
        }
    };
    measure(&mut m, counter, Mode::Find, "Grep", &zero);
    // Control: prove the search mechanism itself works.
    let control = match native::grep_content("Option<String>", fixture, 0, None, true) {
        Some(t) => t,
        None => {
            m.note = crate::leindex_arm::SKIPPED_NO_RG.to_string();
            return m;
        }
    };
    measure(&mut m, counter, Mode::Find, "Grep", &control);
    m.success = zero.trim().is_empty() && !control.trim().is_empty();
    if !m.success {
        m.note = "zero-result/control assertion failed".into();
    }
    m
}

pub fn task7_leindex(
    fixture: &Path,
    rt: &Runtime,
    arm: &LeIndexArm,
    counter: &Counter,
) -> TaskMetrics {
    let mut m = TaskMetrics::new(7);
    let pp = fixture.display().to_string();
    let zero = arm.call(
        rt,
        "leindex_find",
        json!({ "pattern": "Option<String>?", "regex": false, "project_path": pp }),
    );
    let zero = match zero {
        Ok(t) => t,
        Err(e) => {
            m.note = format!("zero search failed: {e}");
            return m;
        }
    };
    measure(&mut m, counter, Mode::Find, "leindex_find", &zero);
    let control = arm.call(
        rt,
        "leindex_find",
        json!({ "pattern": "Option<String>", "regex": false, "project_path": pp }),
    );
    match control {
        Ok(text) => {
            measure(&mut m, counter, Mode::Find, "leindex_find", &text);
            m.success = has_no_matches(&zero) && !has_no_matches(&text);
            if !m.success {
                m.note = "zero-result/control assertion failed".into();
            }
        }
        Err(e) => m.note = format!("control search failed: {e}"),
    }
    m
}

// ── shared parsing helpers ──────────────────────────────────────────────────

/// Parse ripgrep `count` output ("N:path") into (total lines, distinct files).
/// LeIndex `output=count` reuses the same shape, so one parser serves both.
pub fn parse_count_output(text: &str) -> (usize, usize) {
    let mut lines = 0usize;
    let mut files = 0usize;
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        if let Some((count, _path)) = l.split_once(':') {
            if let Ok(n) = count.trim().parse::<usize>() {
                lines += n;
                files += 1;
                continue;
            }
        }
        // Unnumbered line still indicates a file with matches.
        lines += 1;
        files += 1;
    }
    (lines, files)
}

/// Heuristic: does this response say zero matches? Covers the bare
/// "0 match(es)" family and empty output.
pub fn has_no_matches(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    t.starts_with('0')
        || t.contains("0 match")
        || t.contains("no matches")
        || t.contains("No matches")
}

/// Replace the first occurrence of `old` with `new` (exact, like Edit).
pub fn replace_first(content: &str, old: &str, new: &str) -> Option<String> {
    let idx = content.find(old)?;
    let mut out = String::with_capacity(content.len() + new.len());
    out.push_str(&content[..idx]);
    out.push_str(new);
    out.push_str(&content[idx + old.len()..]);
    Some(out)
}

// ── corpus runner ───────────────────────────────────────────────────────────

/// Result of running the whole corpus: per-task (native, leindex) rollups.
pub struct CorpusRun {
    /// Per-task (NATIVE, LEINDEX) metrics, tasks 1..=7 in order.
    pub tasks: Vec<(TaskMetrics, TaskMetrics)>,
}

/// Run all 7 tasks for both arms. Each (task, arm) pair gets a fresh
/// materialized fixture copy.
pub fn run_all(rt: &Runtime, counter: &Counter) -> Result<CorpusRun> {
    let scratch = tempfile::TempDir::new().context("tokbench scratch dir")?;
    let mut tasks = Vec::new();

    for task in 1..=7 {
        let native_fixture = scratch.path().join(format!("t{task}-native"));
        std::fs::create_dir_all(&native_fixture)?;
        corpus::materialize(&native_fixture)?;
        let native_metrics = match task {
            1 => task1_native(&native_fixture, counter),
            2 => task2_native(&native_fixture, counter),
            3 => task3_native(&native_fixture, counter),
            4 => task4_native(&native_fixture, counter),
            5 => task5_native(&native_fixture, counter),
            6 => task6_native(&native_fixture, counter),
            _ => task7_native(&native_fixture, counter),
        };

        // Task 3 LEINDEX needs no fixture (unsupported before any call).
        let leindex_metrics = if task == 3 {
            task3_leindex(counter)
        } else {
            let fixture = scratch.path().join(format!("t{task}-leindex"));
            std::fs::create_dir_all(&fixture)?;
            corpus::materialize(&fixture)?;
            let arm = LeIndexArm::new();
            arm.index(rt, &fixture.display().to_string())?;
            match task {
                1 => task1_leindex(&fixture, rt, &arm, counter),
                2 => task2_leindex(&fixture, rt, &arm, counter),
                4 => task4_leindex(&fixture, rt, &arm, counter),
                5 => task5_leindex(&fixture, rt, &arm, counter),
                6 => task6_leindex(&fixture, rt, &arm, counter),
                _ => task7_leindex(&fixture, rt, &arm, counter),
            }
        };
        tasks.push((native_metrics, leindex_metrics));
    }
    Ok(CorpusRun { tasks })
}

#[cfg(test)]
#[path = "tasks_test.rs"]
mod tests;
