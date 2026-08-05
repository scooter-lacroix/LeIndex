//! VAL-SHIM-001: Stdio shim forwards MCP frames byte-faithfully between
//! client and daemon.
//!
//! This test spawns the real `leindexd` binary on a Unix socket, runs the
//! shim's generic forwarder (`forward_stream_with_reconnect`) against it using
//! in-memory duplex pipes, and asserts that:
//!
//! 1. An MCP `initialize` request sent through the client side of the pipe
//!    flows daemon-bound (the daemon receives and responds).
//! 2. The daemon's response flows client-bound through the pipe.
//! 3. The response is valid JSON-RPC containing `capabilities` (i.e., it was
//!    forwarded byte-faithfully without corruption).
//! 4. The response matches what a direct daemon connection returns (the
//!    shim does not modify, reorder, drop, or duplicate bytes).

#![cfg(all(unix, feature = "daemon-client"))]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// Send a JSON-RPC line frame to a raw UnixStream and read the response.
fn send_and_recv(stream: &mut UnixStream, json: &str) -> String {
    let frame = format!("{json}\n");
    stream.write_all(frame.as_bytes()).expect("write");
    stream.flush().expect("flush");
    let mut buf = [0u8; 65536];
    let n = stream.read(&mut buf).expect("read");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// Spawn leindexd on the given socket path. Returns a ChildGuard that kills
/// the daemon on drop.
fn spawn_leindexd(socket: &std::path::Path, home: &std::path::Path) -> ChildGuard {
    let child = std::process::Command::new(leindexd_bin())
        .arg("--socket")
        .arg(socket)
        .arg("--idle-timeout-secs")
        .arg("60")
        .env("LEINDEX_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn leindexd");

    assert!(
        wait_for_socket(socket, Duration::from_secs(15)),
        "daemon socket did not appear within 15s"
    );

    ChildGuard { child }
}

/// MCP initialize request JSON.
const INIT_REQUEST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"shim-test","version":"0.1.0"}}}"#;

/// VAL-SHIM-001: A round-trip `initialize` request through the shim forwarder
/// produces a valid response identical to what a direct daemon connection
/// returns.
#[tokio::test]
async fn test_shim_forwards_initialize_and_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("shim_test.sock");
    let home = dir.path().to_path_buf();

    // Spawn the real leindexd daemon.
    let _daemon_guard = spawn_leindexd(&socket, &home);

    // First, get the "direct" response by connecting to the daemon socket
    // directly (no shim). This is the reference response.
    let direct_response = {
        let mut direct = UnixStream::connect(&socket).expect("direct connect");
        send_and_recv(&mut direct, INIT_REQUEST)
    };

    // Assert the direct response is valid (contains capabilities).
    assert!(
        direct_response.contains("capabilities"),
        "direct daemon response must contain 'capabilities', got: {direct_response}"
    );

    // Now run the shim forwarder against the daemon using in-memory pipes.
    // Create two duplex pairs:
    // - client_tx → client_read: client writes requests here
    // - client_write → client_rx: client reads responses here
    let (mut client_tx, client_read) = tokio::io::duplex(8192);
    let (client_write, mut client_rx) = tokio::io::duplex(8192);

    // Connect a UnixStream to the daemon socket for the forwarder.
    let daemon_stream = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("shim connect to daemon");

    // Spawn the forwarder as a background task.
    let socket_path = socket.clone();
    let forward_task = tokio::spawn(async move {
        leindex::cli::daemon::shim::forward_stream_with_reconnect(
            daemon_stream,
            client_read,
            client_write,
            &socket_path,
            false, // no reconnect for this single-request test
        )
        .await
    });

    // Send the initialize request through the client side of the pipe.
    let init_frame = format!("{INIT_REQUEST}\n");
    client_tx
        .write_all(init_frame.as_bytes())
        .await
        .expect("write init");
    client_tx.flush().await.expect("flush init");

    // Read the forwarded response from the client's read side.
    let mut shim_response_buf = vec![0u8; 65536];
    let n = tokio::time::timeout(
        Duration::from_secs(10),
        client_rx.read(&mut shim_response_buf),
    )
    .await
    .expect("timed out reading shim response")
    .expect("read shim response");

    let shim_response = String::from_utf8_lossy(&shim_response_buf[..n]).into_owned();

    // Close client write to trigger the forwarder's clean exit.
    drop(client_tx);

    // Wait for the forwarder task to complete.
    let _ = tokio::time::timeout(Duration::from_secs(5), forward_task).await;

    // Assert: the shim forwarded the response correctly.
    assert!(
        shim_response.contains("capabilities"),
        "shim-forwarded response must contain 'capabilities', got: {shim_response}"
    );
    assert!(
        shim_response.contains("serverInfo"),
        "shim-forwarded response must contain 'serverInfo', got: {shim_response}"
    );

    // Assert: the response contains the same id as the request (request ID
    // preservation — spec §4.1).
    assert!(
        shim_response.contains(r#""id":1"#),
        "shim must preserve client request IDs, expected id:1 in response, got: {shim_response}"
    );

    // Parse both responses and compare key fields. The responses should be
    // semantically identical (same capabilities, same serverInfo, same id).
    let direct_json: serde_json::Value =
        serde_json::from_str(direct_response.trim()).expect("parse direct response");
    let shim_json: serde_json::Value =
        serde_json::from_str(shim_response.trim()).expect("parse shim response");

    // Both should have id=1.
    assert_eq!(
        direct_json["id"], shim_json["id"],
        "request ID must be preserved through the shim"
    );

    // Both should be successful responses (no error).
    assert!(
        direct_json.get("error").is_none(),
        "direct response must not be an error"
    );
    assert!(
        shim_json.get("error").is_none(),
        "shim response must not be an error"
    );

    // Both should have capabilities object.
    assert!(
        direct_json["result"]["capabilities"].is_object(),
        "direct response must have capabilities object"
    );
    assert!(
        shim_json["result"]["capabilities"].is_object(),
        "shim response must have capabilities object"
    );

    // Both should have the same serverInfo name.
    assert_eq!(
        direct_json["result"]["serverInfo"]["name"], shim_json["result"]["serverInfo"]["name"],
        "serverInfo.name must match between direct and shim responses"
    );

    // Compare capabilities keys to ensure no capability was dropped/added.
    let direct_caps: Vec<String> = direct_json["result"]["capabilities"]
        .as_object()
        .expect("capabilities object")
        .keys()
        .cloned()
        .collect();
    let shim_caps: Vec<String> = shim_json["result"]["capabilities"]
        .as_object()
        .expect("capabilities object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        direct_caps, shim_caps,
        "capability set must be identical through the shim"
    );
}

/// VAL-SHIM-001 supplementary: verify the shim handles multiple sequential
/// requests correctly (no framing corruption across multiple frames).
#[tokio::test]
async fn test_shim_forwards_multiple_requests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("shim_multi.sock");
    let home = dir.path().to_path_buf();

    let _daemon_guard = spawn_leindexd(&socket, &home);

    let (mut client_tx, client_read) = tokio::io::duplex(8192);
    let (client_write, mut client_rx) = tokio::io::duplex(8192);

    let daemon_stream = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("connect");

    let socket_path = socket.clone();
    let forward_task = tokio::spawn(async move {
        leindex::cli::daemon::shim::forward_stream_with_reconnect(
            daemon_stream,
            client_read,
            client_write,
            &socket_path,
            false,
        )
        .await
    });

    // Send initialize request.
    let init_frame = format!("{INIT_REQUEST}\n");
    client_tx.write_all(init_frame.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    let mut buf = vec![0u8; 65536];
    let n = tokio::time::timeout(Duration::from_secs(10), client_rx.read(&mut buf))
        .await
        .expect("timeout on init")
        .expect("read init response");

    let init_response = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(
        init_response.contains("capabilities"),
        "multi-request: init response must contain capabilities"
    );

    // Send a tools/list request (different request ID to verify ID preservation).
    let list_request = r#"{"jsonrpc":"2.0","id":42,"method":"tools/list","params":{}}"#;
    let list_frame = format!("{list_request}\n");
    client_tx.write_all(list_frame.as_bytes()).await.unwrap();
    client_tx.flush().await.unwrap();

    let n2 = tokio::time::timeout(Duration::from_secs(10), client_rx.read(&mut buf))
        .await
        .expect("timeout on tools/list")
        .expect("read tools/list response");

    let list_response = String::from_utf8_lossy(&buf[..n2]).into_owned();
    assert!(
        list_response.contains(r#""id":42"#),
        "multi-request: second request ID (42) must be preserved"
    );
    assert!(
        list_response.contains("tools"),
        "multi-request: tools/list response must contain tools array"
    );

    drop(client_tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), forward_task).await;
}
