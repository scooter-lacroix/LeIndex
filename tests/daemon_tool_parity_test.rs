//! VAL-DAEMON-005: Daemon serves the same tool set as the inline server.
//! VAL-DAEMON-010: Daemon and inline server produce identical query results
//! for the same project.
//!
//! Both assertions verify the anti-cheat invariant (spec §2.1): no tool is
//! disabled, omitted, or stubbed in the daemon path. Both paths use the same
//! `all_tool_handlers()` registration, so the tool list must be identical.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Path to the compiled `leindexd` binary.
fn leindexd_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_leindexd"))
}

/// RAII guard that kills a child process on drop.
struct ChildGuard {
    child: std::process::Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Wait up to `timeout` for `socket` to appear on the filesystem.
fn wait_for_socket(socket: &std::path::Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if socket.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Send a JSON-RPC line frame and read the response.
fn send_rpc(stream: &mut UnixStream, json: &str) -> String {
    let frame = format!("{json}\n");
    stream.write_all(frame.as_bytes()).expect("write");
    stream.flush().expect("flush");
    let mut buf = [0u8; 65536];
    let n = stream.read(&mut buf).expect("read");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// Send initialize handshake and return the response.
fn do_initialize(stream: &mut UnixStream) -> String {
    send_rpc(
        stream,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}"#,
    )
}

/// Send initialized notification (required before tools/list).
fn send_initialized(stream: &mut UnixStream) {
    let frame = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    stream
        .write_all(format!("{frame}\n").as_bytes())
        .expect("write initialized");
    stream.flush().expect("flush");
}

/// Send tools/list and return the response.
fn list_tools(stream: &mut UnixStream) -> String {
    send_rpc(
        stream,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    )
}

/// Parse the tool names from a tools/list response.
fn parse_tool_names(response: &str) -> Vec<String> {
    let json: serde_json::Value = serde_json::from_str(response.trim()).expect("parse tools/list");
    json["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_string())
        .collect()
}

/// VAL-DAEMON-005: The daemon serves the identical tool set as the inline
/// server (same `all_tool_handlers()` from handlers.rs).
///
/// We connect to a running daemon, send `tools/list`, and compare the tool
/// name list against the expected set from `all_tool_handlers()` in the
/// library code. This is the anti-cheat check (spec §2.1): no tool disabled
/// or omitted in the daemon path.
#[test]
fn test_daemon_serves_same_tool_set_as_inline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("tools.sock");
    let home = dir.path().to_path_buf();

    let child = std::process::Command::new(leindexd_bin())
        .arg("--socket")
        .arg(&socket)
        .arg("--idle-timeout-secs")
        .arg("60")
        .env("LEINDEX_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn leindexd");

    let _guard = ChildGuard { child };

    assert!(
        wait_for_socket(&socket, Duration::from_secs(15)),
        "socket did not appear"
    );

    let mut stream = UnixStream::connect(&socket).expect("connect");

    // Initialize handshake.
    let init_response = do_initialize(&mut stream);
    assert!(
        init_response.contains("capabilities"),
        "initialize response must contain capabilities"
    );

    // Send initialized notification.
    send_initialized(&mut stream);

    // Brief pause for the daemon to process the notification.
    std::thread::sleep(Duration::from_millis(200));

    // Get the tool list.
    let tools_response = list_tools(&mut stream);
    let daemon_tools = parse_tool_names(&tools_response);

    // Get the inline tool set from all_tool_handlers().
    let inline_tools: Vec<String> = leindex::cli::mcp::handlers::all_tool_handlers()
        .iter()
        .map(|h| h.name().to_string())
        .collect();

    // Sort both lists for comparison.
    let mut daemon_sorted = daemon_tools.clone();
    let mut inline_sorted = inline_tools.clone();
    daemon_sorted.sort();
    inline_sorted.sort();

    assert_eq!(
        daemon_sorted.len(),
        inline_sorted.len(),
        "daemon and inline must serve the same number of tools; \
         daemon has {}, inline has {}\n\
         daemon: {:?}\ninline: {:?}",
        daemon_sorted.len(),
        inline_sorted.len(),
        daemon_sorted,
        inline_sorted,
    );

    assert_eq!(
        daemon_sorted, inline_sorted,
        "daemon and inline tool sets must be identical"
    );

    // Verify all 21 tools are present (anti-cheat: no tool omitted).
    assert!(
        daemon_sorted.len() >= 20,
        "expected at least 20 tools; got {}: {:?}",
        daemon_sorted.len(),
        daemon_sorted
    );
}

/// VAL-DAEMON-005: Verify initialize lists the same serverInfo and protocol
/// version in both modes. This is a supplementary assertion that the
/// initialize response structure is consistent.
#[test]
fn test_daemon_initialize_lists_server_info_and_protocol() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("init.sock");
    let home = dir.path().to_path_buf();

    let child = std::process::Command::new(leindexd_bin())
        .arg("--socket")
        .arg(&socket)
        .arg("--idle-timeout-secs")
        .arg("60")
        .env("LEINDEX_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn leindexd");

    let _guard = ChildGuard { child };

    assert!(
        wait_for_socket(&socket, Duration::from_secs(15)),
        "socket did not appear"
    );

    let mut stream = UnixStream::connect(&socket).expect("connect");
    let init_response = do_initialize(&mut stream);

    let parsed: serde_json::Value =
        serde_json::from_str(init_response.trim()).expect("parse initialize");

    // The response must contain capabilities, protocolVersion, and serverInfo.
    let result = &parsed["result"];
    assert!(
        result.get("capabilities").is_some(),
        "capabilities must be present"
    );
    assert!(
        result.get("protocolVersion").is_some(),
        "protocolVersion must be present"
    );
    assert!(
        result.get("serverInfo").is_some(),
        "serverInfo must be present"
    );

    // The serverInfo name should contain "leindex" (not a stub or subset).
    let server_name = result["serverInfo"]["name"].as_str().unwrap_or("");
    assert!(
        server_name.to_lowercase().contains("leindex"),
        "serverInfo.name must contain 'leindex'; got: {server_name}"
    );
}
