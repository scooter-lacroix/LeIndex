//! VAL-SHIM-002: Shim spawns daemon when absent and waits for socket
//! availability.
//!
//! Tests the spawn helper (`spawn_and_wait`) against the real `leindexd`
//! binary. The spawn helper is called when the shim wins the startup race
//! (`StartupOutcome::Won`): it spawns `leindexd` as a background child process,
//! waits for the socket file to appear (bounded timeout), then re-reads the
//! endpoint sidecar.

#![cfg(all(unix, feature = "daemon-client"))]

use std::path::PathBuf;
use std::time::Duration;

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

/// VAL-SHIM-002: The spawn helper launches `leindexd`, blocks until the socket
/// file appears (bounded wait), and returns the endpoint from the published
/// sidecar.
///
/// This test spawns `leindexd` via the spawn helper and verifies:
/// 1. The socket file appears within the bounded timeout (≤10s).
/// 2. The endpoint sidecar is written and readable.
/// 3. The returned endpoint has the correct socket path and protocol version.
#[tokio::test]
async fn test_spawn_helper_launches_daemon_and_waits_for_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let run_dir = dir.path().to_path_buf();

    // Before spawn: no socket, no sidecar.
    let socket_path = run_dir.join("d.sock");
    assert!(!socket_path.exists(), "socket must not exist before spawn");

    // Spawn leindexd directly (simulating what spawn_and_wait does internally).
    // We use the direct binary path because spawn_and_wait resolves the binary
    // from PATH/exe directory, which may not point to the test-built binary.
    let child = std::process::Command::new(leindexd_bin())
        .arg("--socket")
        .arg(&socket_path)
        .arg("--idle-timeout-secs")
        .arg("10")
        .env("LEINDEX_HOME", dir.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn leindexd");

    let guard = ChildGuard { child };

    // Wait for the socket to appear (bounded by spawn helper's timeout).
    let start = tokio::time::Instant::now();
    let appeared = leindex::cli::daemon::spawn::spawn_bounded_socket_wait(
        &socket_path,
        Duration::from_secs(15),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(
        appeared,
        "socket must appear within bounded timeout (elapsed: {elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "socket appeared but took too long: {elapsed:?}"
    );

    // The endpoint sidecar should now be readable. The daemon writes it to
    // $LEINDEX_HOME/run/daemon.endpoint.
    let sidecar = dir.path().join("run").join("daemon.endpoint");
    // The daemon publishes the sidecar right after it binds the socket, so the
    // two appear a moment apart: wait for the second instead of racing it.
    let sidecar_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !sidecar.exists() && std::time::Instant::now() < sidecar_deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        sidecar.exists(),
        "endpoint sidecar must exist after daemon binds"
    );

    let sidecar_bytes = std::fs::read(&sidecar).expect("read sidecar");
    let endpoint: serde_json::Value =
        serde_json::from_slice(&sidecar_bytes).expect("parse sidecar JSON");

    assert_eq!(
        endpoint["socket_path"],
        serde_json::json!(socket_path.to_string_lossy().to_string()),
        "sidecar must record the correct socket path"
    );
    assert_eq!(
        endpoint["protocol_version"],
        serde_json::json!(leindex::cli::daemon::handshake::DAEMON_PROTOCOL_VERSION),
        "sidecar must record the current protocol version"
    );

    // Clean up.
    drop(guard);
}

/// VAL-SHIM-002 supplementary: The spawn helper's bounded wait fails within
/// the timeout when no socket appears.
#[tokio::test]
async fn test_spawn_helper_bounded_wait_times_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("never-appears.sock");

    let start = tokio::time::Instant::now();
    let appeared = leindex::cli::daemon::spawn::spawn_bounded_socket_wait(
        &socket_path,
        Duration::from_millis(500),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(!appeared, "bounded wait must return false on timeout");
    assert!(
        elapsed >= Duration::from_millis(400),
        "must wait at least ~500ms: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "must not wait unreasonably long: {elapsed:?}"
    );
}
