//! End-to-end MCP stdio tests against the real `leindex` binary.
//!
//! Each test pins a defect that only showed up in a *running, long-lived*
//! server (the one-shot CLI never hit them):
//!
//! * a slow tool call must not queue `ping` (or anything else) behind it,
//! * semantic search must work on the very first call of a fresh session,
//! * `tools/list` is the four routers,
//! * `find` searches paths outside the workspace with no index,
//! * argument mistakes come back as `isError` results with a hint.

#![cfg(feature = "cli")]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_leindex"))
}

/// Isolated config/model home and no external precision indexer, so the test
/// neither reads the developer's setup nor shells out to rust-analyzer.
fn isolate(command: &mut Command, home: &Path) {
    command
        .env("LEINDEX_HOME", home)
        .env("LEINDEX_FEATURE_PRECISION_INGEST", "0")
        .env("LEINDEX_FEATURE_DAEMON_CLIENT", "0")
        .env("LEINDEX_WATCHER", "0");
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<(Instant, Value)>,
    next_id: u64,
    backlog: Vec<(Instant, Value)>,
}

impl Server {
    fn start(project: &Path, home: &Path) -> Self {
        let mut command = Command::new(binary());
        command
            .args(["mcp"])
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        isolate(&mut command, home);
        let mut child = command.spawn().expect("spawn leindex mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send((Instant::now(), value)).is_err() {
                        break;
                    }
                }
            }
        });
        let mut server = Self {
            child,
            stdin,
            replies,
            next_id: 0,
            backlog: Vec::new(),
        };
        let init = server.call(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        );
        assert!(init["result"]["serverInfo"].is_object(), "{init}");
        server.notify("notifications/initialized");
        server
    }

    fn write(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.write(&json!({"jsonrpc": "2.0", "method": method}));
    }

    /// Send a request without waiting; returns its id.
    fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Wait for the reply to `id` (other replies are kept for later).
    fn wait(&mut self, id: u64, timeout: Duration) -> (Instant, Value) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = self.backlog.iter().position(|(_, v)| v["id"] == id) {
                return self.backlog.remove(pos);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(remaining) {
                Ok(reply) => self.backlog.push(reply),
                Err(_) => panic!("no reply to request {id} within {timeout:?}"),
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.wait(id, Duration::from_secs(120)).1
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.call("tools/call", json!({"name": name, "arguments": arguments}))["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn write_project(root: &Path, files: usize) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..files {
        let mut source = String::new();
        for f in 0..25 {
            source.push_str(&format!(
                "pub fn handler_{i}_{f}(input: u32) -> u32 {{\n    let value = input + {f};\n    helper_{}(value)\n}}\n\n",
                f % 4
            ));
        }
        source.push_str("pub fn helper_0(x: u32) -> u32 { x }\npub fn helper_1(x: u32) -> u32 { x }\npub fn helper_2(x: u32) -> u32 { x }\npub fn helper_3(x: u32) -> u32 { x }\n");
        std::fs::write(root.join(format!("src/module_{i}.rs")), source).unwrap();
    }
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn retry_with_backoff() -> u32 { 3 }\n",
    )
    .unwrap();
}

/// A small project, indexed once with the CLI and shared by the read-only tests.
fn indexed_fixture() -> &'static (tempfile::TempDir, tempfile::TempDir) {
    static FIXTURE: OnceLock<(tempfile::TempDir, tempfile::TempDir)> = OnceLock::new();
    static BUILD: Mutex<()> = Mutex::new(());
    let _guard = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    FIXTURE.get_or_init(|| {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write_project(project.path(), 6);
        let mut command = Command::new(binary());
        command
            .args(["index"])
            .arg(project.path())
            .current_dir(project.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        isolate(&mut command, home.path());
        assert!(command.status().unwrap().success(), "fixture index failed");
        (project, home)
    })
}

#[test]
fn test_tools_list_is_the_four_routers_with_a_discoverable_guide() {
    let (project, home) = indexed_fixture();
    let mut server = Server::start(project.path(), home.path());
    let listed = server.call("tools/list", json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "leindex_explore",
            "leindex_analyze",
            "leindex_edit",
            "leindex_manage"
        ]
    );
    let bytes = listed["result"]["tools"].to_string().len();
    assert!(bytes < 14_000, "tools/list is {bytes} bytes");

    let resources = server.call("resources/list", json!({}));
    assert!(
        resources["result"]["resources"]
            .to_string()
            .contains("leindex://tools/guide"),
        "{resources}"
    );
    let guide = server.call("resources/read", json!({"uri": "leindex://tools/guide"}));
    let text = guide["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("`leindex_explore` = `find`") && text.contains("git_diff"),
        "{text}"
    );
}

#[test]
fn test_semantic_search_works_on_the_first_call_of_a_fresh_session() {
    // Regression: after lazy hydration, `search` saw an empty engine and
    // answered "Project not indexed" for a project the CLI had indexed.
    let (project, home) = indexed_fixture();
    let mut server = Server::start(project.path(), home.path());
    let result = server.tool(
        "leindex_explore",
        json!({"mode": "search", "query": "retry with backoff", "top_k": 3}),
    );
    let text = text_of(&result);
    assert_ne!(result["isError"], true, "first search failed: {text}");
    assert!(text.contains("retry_with_backoff"), "{text}");
}

#[test]
fn test_find_searches_outside_the_workspace_without_an_index() {
    let (project, home) = indexed_fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("notes.txt"),
        "remember the needle_outside marker\n",
    )
    .unwrap();
    let mut server = Server::start(project.path(), home.path());

    let result = server.tool(
        "leindex_explore",
        json!({"mode": "find", "pattern": "needle_outside", "paths": [outside.path()]}),
    );
    let text = text_of(&result);
    assert_ne!(result["isError"], true, "{text}");
    assert!(
        text.contains("notes.txt") && text.contains("needle_outside"),
        "{text}"
    );
    assert!(
        !outside.path().join(".leindex").exists(),
        "searching must not litter the directory"
    );

    // In-workspace, indexed, with the enclosing symbol attached.
    let inside = server.tool(
        "leindex_explore",
        json!({"mode": "find", "pattern": "retry_with_backoff"}),
    );
    let inside_text = text_of(&inside);
    assert!(inside_text.contains("src/lib.rs"), "{inside_text}");
    assert!(inside_text.contains("indexed"), "{inside_text}");
}

#[test]
fn test_legacy_search_spellings_reach_find() {
    let (project, home) = indexed_fixture();
    let mut server = Server::start(project.path(), home.path());
    for arguments in [
        json!({"mode": "grep", "pattern": "helper_2"}),
        json!({"mode": "text", "query": "helper_2"}),
    ] {
        let text = text_of(&server.tool("leindex_explore", arguments.clone()));
        assert!(text.contains("helper_2"), "{arguments}: {text}");
    }
    // The retired tool names still answer.
    let text = text_of(&server.tool("leindex_text_search", json!({"query": "helper_2"})));
    assert!(text.contains("helper_2"), "{text}");
}

#[test]
fn test_argument_mistakes_are_iserror_results_with_a_hint() {
    let (project, home) = indexed_fixture();
    let mut server = Server::start(project.path(), home.path());

    let missing = server.tool("leindex_edit", json!({"file_path": "src/lib.rs"}));
    assert_eq!(missing["isError"], true);
    assert!(text_of(&missing).contains("action"), "{missing}");

    let typo = server.tool("leindex_analyze", json!({"mode": "impcat"}));
    assert_eq!(typo["isError"], true);
    assert!(text_of(&typo).contains("impact"), "{typo}");

    let unknown = server.tool("leindex_grep", json!({"pattern": "x"}));
    assert_eq!(unknown["isError"], true);
    assert!(text_of(&unknown).contains("leindex_explore"), "{unknown}");
}

#[test]
fn test_tiers_shape_the_response() {
    let (project, home) = indexed_fixture();
    let mut server = Server::start(project.path(), home.path());
    let args = |tier: &str| json!({"mode": "find", "pattern": "helper_", "limit": 0, "tier": tier});
    let l0 = text_of(&server.tool("leindex_explore", args("l0")));
    let l1 = text_of(&server.tool("leindex_explore", args("l1")));
    let l2 = text_of(&server.tool("leindex_explore", args("l2")));
    assert!(l0.contains("tier l0") && l0.len() < 600, "{l0}");
    assert!(l1.len() > l0.len(), "the overview is richer than the card");
    let full: Value = serde_json::from_str(&l2).expect("l2 is the complete JSON result");
    assert!(full["total_matches"].as_u64().unwrap() > 0);
}

#[test]
fn test_ping_is_answered_while_a_slow_tool_call_runs() {
    // Regression: the stdio loop handled requests serially, so one slow call
    // (a cold index) blocked `ping` and everything queued behind it — the
    // whole server looked hung while the one-shot CLI was fine.
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_project(project.path(), 120);
    let mut server = Server::start(project.path(), home.path());

    let started = Instant::now();
    let slow = server.send(
        "tools/call",
        json!({"name": "leindex_manage", "arguments": {
            "action": "index", "project_path": project.path(), "force_reindex": true, "wait": true
        }}),
    );
    std::thread::sleep(Duration::from_millis(200));
    let ping_sent = Instant::now();
    let ping = server.send("ping", json!({}));
    let (ping_at, ping_reply) = server.wait(ping, Duration::from_secs(30));
    assert!(ping_reply["result"].is_object(), "{ping_reply}");
    let ping_latency = ping_at.duration_since(ping_sent);

    let (slow_at, slow_reply) = server.wait(slow, Duration::from_secs(300));
    assert!(slow_reply["result"].is_object(), "{slow_reply}");
    let slow_latency = slow_at.duration_since(started);

    assert!(
        ping_latency < Duration::from_secs(2),
        "ping took {ping_latency:?} behind a {slow_latency:?} tool call"
    );
    assert!(
        ping_at < slow_at,
        "ping ({ping_latency:?}) must not wait for the slow call ({slow_latency:?})"
    );
}

#[test]
fn test_replies_are_flushed_before_exit_on_stdin_eof() {
    // A piped client closes stdin right after its last request; the server
    // must still answer everything it accepted.
    let (project, home) = indexed_fixture();
    let mut command = Command::new(binary());
    command
        .args(["mcp"])
        .current_dir(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    isolate(&mut command, home.path());
    let mut child = command.spawn().unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for message in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "leindex_explore", "arguments": {"mode": "find", "pattern": "helper_1"}}}),
        ] {
            writeln!(stdin, "{message}").unwrap();
        }
    } // stdin closed here
    let output = child.wait_with_output().unwrap();
    let replies: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert!(replies.iter().any(|r| r["id"] == 1), "{replies:?}");
    let call = replies
        .iter()
        .find(|r| r["id"] == 2)
        .expect("tool call answered before exit");
    assert!(text_of(&call["result"]).contains("helper_1"));
}
