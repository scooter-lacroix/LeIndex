//! Smoke test for the `leindexd` daemon binary (spec §4.2, VAL-DAEMON-003/004/005/009).
//!
//! Spawns `leindexd`, waits for the socket, connects via `UnixStream`, sends
//! an MCP `initialize` frame, and asserts the response contains `capabilities`.
//! Also verifies that RSS stays flat at startup (no eager project/model load)
//! and that the idle-exit timeout fires.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Path to the compiled `leindexd` binary, provided by Cargo's
/// `CARGO_BIN_EXE_leindexd` env var.
fn leindexd_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_leindexd"))
}

/// RAII guard that kills a child process on drop (even on panic).
struct ChildGuard<'a> {
    child: &'a mut std::process::Child,
}

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read the RSS (VmRSS) of a process from `/proc/<pid>/status` in KiB.
fn read_rss_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let num: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
            return num.parse::<u64>().ok();
        }
    }
    None
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

/// Send a JSON-RPC line frame and read the response line. The server accepts
/// newline-delimited JSON (not just Content-Length framing).
fn send_frame_and_read_response(stream: &mut UnixStream, json: &str) -> std::io::Result<String> {
    // Send as a newline-delimited JSON line.
    let frame = format!("{json}\n");
    stream.write_all(frame.as_bytes())?;
    stream.flush()?;

    // Read until we get a newline (the response is also newline-delimited
    // when the request is not Content-Length framed).
    let mut buf = [0u8; 65536];
    let mut total = Vec::new();
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total.extend_from_slice(&buf[..n]);
        if total.contains(&b'\n') {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&total).into_owned())
}

/// VAL-DAEMON-003: leindexd binary serves MCP tools over a Unix socket.
///
/// Spawns `leindexd --socket <path> --idle-timeout-secs <n>`, waits for the
/// socket to appear, connects, sends an MCP `initialize` frame, and asserts
/// the response contains `capabilities`.
#[test]
fn test_daemon_serves_initialize_over_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");

    let mut child = std::process::Command::new(leindexd_bin())
        // Each daemon takes an exclusive per-home lock; tests run in parallel.
        .env("LEINDEX_HOME", dir.path())
        .arg("--socket")
        .arg(&socket)
        .arg("--idle-timeout-secs")
        .arg("60")
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn leindexd");

    // Clean up child on test exit (even on panic).
    let _guard = ChildGuard { child: &mut child };

    // Wait for socket to appear.
    assert!(
        wait_for_socket(&socket, Duration::from_secs(15)),
        "socket did not appear within 15s"
    );

    // Connect and send initialize.
    let mut stream = UnixStream::connect(&socket).expect("connect to socket");

    let init_request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}"#;

    let response = send_frame_and_read_response(&mut stream, init_request).expect("read response");

    // Assert the response contains capabilities (VAL-DAEMON-003).
    assert!(
        response.contains("capabilities"),
        "initialize response must contain 'capabilities', got: {response}"
    );

    // Assert protocolVersion is present.
    assert!(
        response.contains("protocolVersion"),
        "initialize response must contain 'protocolVersion', got: {response}"
    );

    // Assert serverInfo is present.
    assert!(
        response.contains("serverInfo"),
        "initialize response must contain 'serverInfo', got: {response}"
    );
}

/// VAL-DAEMON-004: Daemon has no eager project or model load at startup.
///
/// After leindexd binds the socket but before any tool call, RSS should stay
/// flat. We sample RSS at 1s intervals for 3 seconds and assert the delta is
/// small.
#[test]
fn test_daemon_rss_flat_at_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d2.sock");

    let mut child = std::process::Command::new(leindexd_bin())
        // Each daemon takes an exclusive per-home lock; tests run in parallel.
        .env("LEINDEX_HOME", dir.path())
        .arg("--socket")
        .arg(&socket)
        .arg("--idle-timeout-secs")
        .arg("60")
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn leindexd");

    let _guard = ChildGuard { child: &mut child };

    // Wait for socket — the daemon is now serving but no project is loaded.
    assert!(
        wait_for_socket(&socket, Duration::from_secs(15)),
        "socket did not appear"
    );

    // Give it a brief moment to settle after bind.
    std::thread::sleep(Duration::from_millis(500));

    let pid = _guard.child.id();
    let rss_t0 = read_rss_kib(pid).expect("read RSS at T0");

    // Sample RSS at T+1s, T+2s, T+3s.
    std::thread::sleep(Duration::from_secs(1));
    let rss_t1 = read_rss_kib(pid).expect("read RSS at T+1s");

    std::thread::sleep(Duration::from_secs(2));
    let rss_t3 = read_rss_kib(pid).expect("read RSS at T+3s");

    // RSS should be flat: no project DB opened, no model loaded.
    // Allow up to 5 MiB (5120 KiB) of variance for runtime jitter (allocators,
    // background thread stacks, etc.).
    let delta_0_1 = rss_t1.abs_diff(rss_t0);
    let delta_0_3 = rss_t3.abs_diff(rss_t0);

    assert!(
        delta_0_1 < 5120,
        "RSS changed {delta_0_1} KiB between T0 and T+1s — daemon may be eagerly loading at startup"
    );
    assert!(
        delta_0_3 < 5120,
        "RSS changed {delta_0_3} KiB between T0 and T+3s — daemon may be eagerly loading at startup"
    );
}

/// VAL-DAEMON-009: Daemon idle exit triggers after configured timeout.
///
/// With `--idle-timeout-secs 2`, the daemon should exit on its own after
/// roughly 2 seconds of no activity.
#[test]
fn test_daemon_idle_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d3.sock");

    let start = Instant::now();
    let mut child = std::process::Command::new(leindexd_bin())
        // Each daemon takes an exclusive per-home lock; tests run in parallel.
        .env("LEINDEX_HOME", dir.path())
        .arg("--socket")
        .arg(&socket)
        .arg("--idle-timeout-secs")
        .arg("2")
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn leindexd");

    // Wait for socket to confirm daemon started.
    assert!(
        wait_for_socket(&socket, Duration::from_secs(15)),
        "socket did not appear"
    );

    // The daemon should exit within ~10s (2s timeout + jitter).
    // Poll the child with a total timeout of 30s.
    let exit_status = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(_) => break None,
            }
        }
    };

    let elapsed = start.elapsed();
    assert!(
        exit_status.is_some(),
        "daemon did not exit within 30s of idle timeout"
    );

    // Verify clean exit (code 0).
    let status = exit_status.unwrap();
    assert!(
        status.success(),
        "daemon exited with non-zero status {status} after idle timeout"
    );

    // Should have taken at least 2s (the timeout) but not more than 30s.
    assert!(
        elapsed >= Duration::from_secs(2),
        "daemon exited in {elapsed:?}, before the 2s idle timeout"
    );
}
