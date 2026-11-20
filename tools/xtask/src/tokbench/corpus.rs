//! Deterministic corpus fixture for the token benchmark harness.
//!
//! Generates a self-contained 4-crate Rust workspace under a tempdir:
//! 14 files, no network, no vendored blobs. All task expectations are
//! derived from these same byte-exact sources, so the assertions stay in
//! lockstep with what the arms operate on.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::tokbench::TempDir;

/// Task ids shared by both arms.
pub const T1_READ_BUILDER: u8 = 1;
pub const T2_FIND_CALLERS: u8 = 2;
pub const T3_ADD_FIELD: u8 = 3;
pub const T4_READ_FAILURE: u8 = 4;
pub const T5_GIT_COMMIT: u8 = 5;
pub const T6_READ_AFTER_EDIT: u8 = 6;

pub const HTTP_REL: &str = "crates/api/src/http.rs";
pub const CLIENT_REL: &str = "crates/api/src/client.rs";
pub const CORE_LIB_REL: &str = "crates/core/src/lib.rs";
pub const CLI_MAIN_REL: &str = "crates/cli/src/main.rs";
pub const COMMANDS_REL: &str = "crates/core/src/commands.rs";
pub const PERF_LIB_REL: &str = "crates/perf/src/lib.rs";
pub const API_TEST_REL: &str = "crates/api/tests/api_test.rs";
pub const README_REL: &str = "README.md";

/// The fixture lives in a tempdir that is removed on drop.
pub struct Fixture {
    _temp: TempDir,
    pub root: PathBuf,
}

impl Fixture {
    /// Generate a fresh fixture at a new tempdir, then git-init a baseline
    /// commit so git-mode tasks have a repository to act on.
    pub fn build() -> Result<Self> {
        let temp = TempDir::new("corpus")?;
        let root = temp.path().to_path_buf();
        for (rel, content) in files() {
            let path = root.join(&rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, content)
                .with_context(|| format!("write fixture file {rel}"))?;
        }
        let fx = Self { _temp: temp, root };
        fx.git_init_baseline()?;
        Ok(fx)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn read_rel(&self, rel: &str) -> Result<String> {
        fs::read_to_string(self.path(rel)).with_context(|| format!("read fixture {rel}"))
    }

    fn git_init_baseline(&self) -> Result<()> {
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "tokbench@example.invalid"],
            &["config", "user.name", "tokbench"],
            &["add", "-A"],
            &["commit", "-q", "-m", "tokbench: baseline"],
        ] {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.root)
                .args(args)
                .output()
                .context("spawn git")?;
            if !out.status.success() {
                bail!(
                    "git {} failed: {}",
                    args[0],
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
        }
        Ok(())
    }
}

const WORKSPACE_MANIFEST: &str = r#"[workspace]
resolver = "3"
members = [
    "crates/core",
    "crates/api",
    "crates/cli",
    "crates/perf",
]

[profile.dev]
debug = false
"#;

const README: &str = r#"# tokbench fixture

A deterministic workspace used by `cargo xtask tokbench`.

## Layout

- crates/core — shared primitives
- crates/api — client wiring (needs `core`)
- crates/cli — binary entrypoint (needs `api`)
- crates/perf — standalone scaffolding crate
"#;

const GITIGNORE: &str = "/target\n";

const CORE_MANIFEST: &str = r#"[package]
name = "core"
version = "0.1.0"
edition = "2021"

[lib]
name = "core"
path = "src/lib.rs"
"#;

const CORE_LIB: &str = r#"//! Core primitives for the tokbench fixture.

pub mod commands;

/// A request body carried by the API client.
#[derive(Debug, Clone)]
pub struct RequestBody {
    pub payload: String,
    pub retries: u32,
}

/// Shared mutable transport context.
pub struct Cx {
    pub calls: u32,
}

impl Default for Cx {
    fn default() -> Self {
        Self { calls: 0 }
    }
}

/// Dispatch a command through the shared transport.
///
/// T2 must find every caller of this function before editing.
pub fn perform_request(cmd: &str, cx: &mut Cx) -> bool {
    let _ = cmd;
    cx.calls += 1;
    true
}
"#;

const CORE_COMMANDS: &str = r#"//! Command wrappers that forward to the shared transport.

use crate::{perform_request, Cx};

/// Ping the transport.
pub fn ping(cx: &mut Cx) -> bool {
    perform_request("ping", cx)
}
"#;

const API_MANIFEST: &str = r#"[package]
name = "api"
version = "0.1.0"
edition = "2021"

[dependencies]
core = { path = "../core" }
"#;

const API_LIB: &str = r#"//! API client crate for the tokbench fixture.

pub mod client;
pub mod http;

use core::{perform_request, Cx};

/// Report overall health.
pub fn health(cx: &mut Cx) -> bool {
    perform_request("health", cx)
}
"#;

const API_CLIENT: &str = r#"//! Client wiring: builds bodies and forwards them.

use core::{perform_request, Cx, RequestBody};

/// Send a prepared body through the transport.
pub fn send(body: RequestBody, cx: &mut Cx) -> bool {
    let cmd = format!("payload={}", body.payload);
    perform_request(&cmd, cx)
}

/// Construction site 1 of `RequestBody`.
pub fn make_default_body() -> RequestBody {
    RequestBody { payload: "default".to_string(), retries: 2 }
}
"#;

const API_HTTP: &str = r#"//! HTTP-ish layer: builds the request body (T1 target).

use core::{perform_request, Cx, RequestBody};

/// Build the request body for an outgoing call. T1 finds this fn.
pub fn build_request_body(payload: &str) -> RequestBody {
    RequestBody { payload: payload.to_string(), retries: 1 }
}

/// Send a body, using the builder first.
pub fn post(payload: &str, cx: &mut Cx) -> bool {
    let body = build_request_body(payload);
    let cmd = format!("post/{}", body.payload);
    perform_request(&cmd, cx)
}
"#;

const API_TEST: &str = r#"//! Behavioural tests for the api crate.

use api::http::build_request_body;

/// Seeded deterministic failure: the payload is parsed, not echoed.
#[test]
fn test_parse_payload() {
    let body = build_request_body("hello");
    assert_eq!(body.payload, "goodbye");
}
"#;

const CLI_MANIFEST: &str = r#"[package]
name = "cli"
version = "0.1.0"
edition = "2021"

[dependencies]
api = { path = "../api" }
core = { path = "../core" }
"#;

const CLI_MAIN: &str = r#"//! CLI binary crate for the tokbench fixture.

use api::client::make_default_body;
use core::RequestBody;

fn main() {
    let dflt = make_default_body();
    // Construction site 3 of `RequestBody`.
    let req = RequestBody { payload: "cli".to_string(), retries: 3 };
    println!("{} {:?} ({})", req.payload, dflt, req.retries);
}
"#;

const PERF_MANIFEST: &str = r#"[package]
name = "perf"
version = "0.1.0"
edition = "2021"

[lib]
name = "perf"
path = "src/lib.rs"
"#;

/// The perf crate carries a large comment region (T1's re-read target).
fn perf_lib() -> String {
    let mut s = String::new();
    s.push_str("//! Performance scaffolding crate for the tokbench fixture.\n//\n");
    s.push_str(&perf_region_block());
    s.push_str("\npub fn tick() -> u64 {\n    42\n}\n");
    s
}

/// The full 62-line region block, byte-exact for edits and reads.
pub fn perf_region_block() -> String {
    let mut s = String::new();
    s.push_str("// ── Region A start marker ─────────────────────────────\n");
    for i in 1..=60 {
        s.push_str(&format!(
            "// tokbench region A line {i:03} — padding padding padding\n"
        ));
    }
    s.push_str("// ── Region A end marker ───────────────────────────────\n");
    s
}

/// (relative path, content) pairs written by `Fixture::build`.
fn files() -> Vec<(String, String)> {
    vec![
        ("Cargo.toml".to_string(), WORKSPACE_MANIFEST.to_string()),
        ("README.md".to_string(), README.to_string()),
        (".gitignore".to_string(), GITIGNORE.to_string()),
        ("crates/core/Cargo.toml".to_string(), CORE_MANIFEST.to_string()),
        ("crates/core/src/lib.rs".to_string(), CORE_LIB.to_string()),
        ("crates/core/src/commands.rs".to_string(), CORE_COMMANDS.to_string()),
        ("crates/api/Cargo.toml".to_string(), API_MANIFEST.to_string()),
        ("crates/api/src/lib.rs".to_string(), API_LIB.to_string()),
        ("crates/api/src/client.rs".to_string(), API_CLIENT.to_string()),
        ("crates/api/src/http.rs".to_string(), API_HTTP.to_string()),
        ("crates/api/tests/api_test.rs".to_string(), API_TEST.to_string()),
        ("crates/cli/Cargo.toml".to_string(), CLI_MANIFEST.to_string()),
        ("crates/cli/src/main.rs".to_string(), CLI_MAIN.to_string()),
        ("crates/perf/Cargo.toml".to_string(), PERF_MANIFEST.to_string()),
        ("crates/perf/src/lib.rs".to_string(), perf_lib()),
    ]
}

/// 1-based line number of the first line containing `needle`.
pub fn line_of(content: &str, needle: &str) -> Option<usize> {
    content.lines().position(|l| l.contains(needle)).map(|i| i + 1)
}

/// T1 expectation: `(rel, line)` of the request-body builder fn.
pub fn expected_builder_location(fx: &Fixture) -> Result<(String, usize)> {
    let content = fx.read_rel(HTTP_REL)?;
    let line = line_of(&content, "fn build_request_body")
        .context("builder fn missing from http.rs")?;
    Ok((HTTP_REL.to_string(), line))
}

/// T2 expectation: total lines matching `perform_request(` across the
/// workspace (the definition plus its four call sites), and each one's
/// `(rel, line)`.
pub fn expected_caller_matches(fx: &Fixture) -> Result<(usize, Vec<(String, usize)>)> {
    let rels = [
        CORE_LIB_REL,
        COMMANDS_REL,
        "crates/api/src/lib.rs",
        CLIENT_REL,
        HTTP_REL,
    ];
    let mut lines = Vec::new();
    for rel in rels {
        let content = fx.read_rel(rel)?;
        if let Some(line) = line_of(&content, "perform_request(") {
            lines.push((rel.to_string(), line));
        }
    }
    Ok((lines.len(), lines))
}

/// T4 expectation: `(rel, line)` of the deterministic failing assertion.
pub fn expected_failure_location(fx: &Fixture) -> Result<(String, usize)> {
    let content = fx.read_rel(API_TEST_REL)?;
    let line = line_of(&content, "assert_eq!(1, 2)")
        .context("failing assertion missing from api_test.rs")?;
    Ok((API_TEST_REL.to_string(), line))
}
