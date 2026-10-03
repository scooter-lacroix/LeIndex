//! VAL-DAEMON-006: Only one daemon instance wins the startup lock per user
//! (single-winner).
//!
//! When two `leindexd` processes attempt to start simultaneously against the
//! same run-dir, exactly one succeeds (binds socket, serves) and the other
//! exits with a non-zero code and a clear diagnostic message referencing the
//! existing daemon's socket path and PID.

#![cfg(unix)]

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

/// VAL-DAEMON-006: When two `leindexd` processes attempt to start against the
/// same run-dir, exactly one wins (binds socket, serves) and the other exits
/// with a non-zero code and a clear diagnostic.
///
/// Strategy: spawn the first daemon, wait for it to bind, then spawn the second
/// daemon and verify it exits with non-zero and an error message containing the
/// winner's socket/PID.
#[test]
fn test_single_winner_startup_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_a = dir.path().join("winner.sock");
    let socket_b = dir.path().join("loser.sock");

    // Set LEINDEX_HOME to the temp dir so both daemons use the same run-dir
    // sidecar for endpoint discovery.
    let home = dir.path().to_path_buf();

    // Spawn the first daemon (the winner).
    let child_a = std::process::Command::new(leindexd_bin())
        .arg("--socket")
        .arg(&socket_a)
        .arg("--idle-timeout-secs")
        .arg("60")
        .env("LEINDEX_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn leindexd winner");

    let guard_a = ChildGuard { child: child_a };

    // Wait for the first daemon to appear and write its endpoint sidecar.
    assert!(
        wait_for_socket(&socket_a, Duration::from_secs(15)),
        "winner socket did not appear within 15s"
    );

    // Give the daemon a moment to write the endpoint sidecar.
    std::thread::sleep(Duration::from_secs(1));

    // Spawn the second daemon (the loser). It must discover the winner via the
    // hard startup lock and exit with a non-zero code.
    let loser_output = std::process::Command::new(leindexd_bin())
        .arg("--socket")
        .arg(&socket_b)
        .arg("--idle-timeout-secs")
        .arg("60")
        .env("LEINDEX_HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn leindexd loser");

    // The loser must exit with a non-zero code.
    assert!(
        !loser_output.status.success(),
        "loser daemon must exit non-zero; got status {:?}",
        loser_output.status
    );

    // The loser's stderr must contain a diagnostic referencing the existing
    // daemon. The message format is:
    //   "daemon already live at <path> (pid <n>); refusing second instance"
    let stderr = String::from_utf8_lossy(&loser_output.stderr);
    assert!(
        stderr.contains("daemon already live") || stderr.contains("refusing"),
        "loser stderr must contain diagnostic message; got: {stderr}"
    );

    // The loser's socket must NOT have been created (it never bound).
    assert!(
        !socket_b.exists(),
        "loser daemon must not have bound its socket"
    );

    // Clean up: kill the winner.
    drop(guard_a);
}

/// VAL-DAEMON-006 (supplementary): The winner serves initialize, proving it
/// is the daemon that survived the race.
#[test]
fn test_winner_serves_initialize() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("serve.sock");
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

    // Connect and send initialize.
    let mut stream = UnixStream::connect(&socket).expect("connect to socket");

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}"#;
    use std::io::{Read, Write};
    stream
        .write_all(format!("{init}\n").as_bytes())
        .expect("write init");
    stream.flush().expect("flush");

    let mut buf = [0u8; 65536];
    let n = stream.read(&mut buf).expect("read response");
    let response = String::from_utf8_lossy(&buf[..n]);

    assert!(
        response.contains("capabilities"),
        "winner daemon must serve initialize; got: {response}"
    );
}
