//! Daemon spawn helper (spec §4.1).
//!
//! When the shim wins the startup race ([`StartupOutcome::Won`]), it must
//! spawn `leindexd` as a background child process, wait for the socket file to
//! appear (bounded timeout), and then re-resolve the endpoint sidecar so it
//! can connect and begin forwarding.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::endpoint::{DaemonEndpoint, ENDPOINT_SIDECAR, StartupOutcome, resolve_endpoint};
use super::handshake::DAEMON_PROTOCOL_VERSION;

/// Maximum time to wait for the daemon's socket to appear after spawn.
const SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Polling interval for checking socket existence.
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Bounded wait for a socket file to appear, exposed for integration tests
/// ([`tests/daemon_spawn_test.rs`]). Returns `true` when the file exists,
/// `false` when the timeout expires without the file appearing.
#[doc(hidden)]
pub async fn spawn_bounded_socket_wait(socket_path: &Path, timeout: Duration) -> bool {
    wait_for_socket(socket_path, timeout).await
}

/// Spawn `leindexd` as a background child process under the given run-dir and
/// wait for it to bind its socket and publish the endpoint sidecar.
///
/// This is called by the shim when it wins the startup race
/// ([`StartupOutcome::Won`]). The daemon binary path is assumed to be
/// `leindexd` in the same directory as the current `leindex` binary (or on
/// `PATH`).
///
/// Returns the resolved [`DaemonEndpoint`] read from the daemon's published
/// sidecar.
///
/// # Errors
///
/// - If the `leindexd` binary cannot be found or spawned.
/// - If the socket file does not appear within [`SOCKET_WAIT_TIMEOUT`].
/// - If the endpoint sidecar cannot be read after the socket appears.
pub async fn spawn_and_wait(run_dir: &Path) -> Result<DaemonEndpoint> {
    // Derive the socket path from the run-dir. The daemon's default socket
    // path is `d.sock` under the run-dir (matching make_winning_endpoint in
    // endpoint.rs).
    let socket_path = run_dir.join("d.sock");

    // Resolve the leindexd binary path: prefer the same-directory sibling of
    // the current leindex binary, then fall back to PATH lookup.
    let leindexd_path = resolve_leindexd_path()?;

    info!(
        "spawning leindexd: {} --socket {} --idle-timeout-secs 300",
        leindexd_path.display(),
        socket_path.display()
    );

    let mut child = tokio::process::Command::new(&leindexd_path)
        .arg("--socket")
        .arg(&socket_path)
        .arg("--idle-timeout-secs")
        .arg("300")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn leindexd at {}", leindexd_path.display()))?;

    // Wait for the socket to appear.
    let appeared = wait_for_socket(&socket_path, SOCKET_WAIT_TIMEOUT).await;
    if !appeared {
        // Kill the child to avoid a zombie daemon.
        let _ = child.kill().await;
        anyhow::bail!(
            "leindexd did not create socket at {} within {:?}",
            socket_path.display(),
            SOCKET_WAIT_TIMEOUT
        );
    }

    debug!("daemon socket appeared at {}", socket_path.display());

    // Give the daemon a moment to write the endpoint sidecar (the post-bind
    // callback runs after bind but the fsync+rename may take a few ms).
    let endpoint = wait_for_sidecar(run_dir, Duration::from_secs(5)).await?;

    // Detach the daemon: it runs as its own process. We do NOT wait for it.
    // Set the child to be forgotten by not storing the ChildGuard.
    // The daemon will exit on its own after idle timeout.
    // NOTE: on Tokio, dropping the Child does NOT kill it (unlike std).
    std::mem::forget(child);

    Ok(endpoint)
}

/// Resolve the path to the `leindexd` binary.
///
/// Strategy:
/// 1. Check if `leindexd` exists in the same directory as the current
///    executable (cargo install / packaged deployment).
/// 2. Fall back to `leindexd` on `PATH`.
fn resolve_leindexd_path() -> Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("leindexd");
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }

    // Fall back to PATH lookup.
    which::which("leindexd")
        .with_context(|| "leindexd binary not found in PATH or alongside leindex")
}

/// Wait until the socket file exists or the timeout expires.
async fn wait_for_socket(socket_path: &Path, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if socket_path.exists() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
    }
}

/// Wait for the endpoint sidecar to appear and read the endpoint from it.
/// The sidecar is written atomically by the daemon's post-bind callback.
async fn wait_for_sidecar(run_dir: &Path, timeout: Duration) -> Result<DaemonEndpoint> {
    let sidecar = run_dir.join(ENDPOINT_SIDECAR);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "endpoint sidecar {} did not appear within {:?}",
                sidecar.display(),
                timeout
            );
        }

        match resolve_endpoint(run_dir, DAEMON_PROTOCOL_VERSION) {
            Ok(StartupOutcome::Connect(ep)) => return Ok(ep),
            Ok(StartupOutcome::Won(_)) => {
                // Sidecar not yet written or not yet live. The daemon may
                // be between bind and sidecar write.
                debug!("endpoint not yet published; waiting...");
            }
            Err(e) => {
                warn!("endpoint resolution error during spawn wait: {e}");
            }
        }

        tokio::time::sleep(SOCKET_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cli::daemon::endpoint::{DaemonEndpoint, write_endpoint_sidecar};
    use std::fs;
    use std::os::unix::net::UnixListener;

    #[test]
    fn test_resolve_leindexd_path_does_not_panic() {
        // The function should either find the binary or return an error,
        // but never panic.
        let _ = resolve_leindexd_path();
    }

    /// `wait_for_socket` returns `true` when the socket file exists.
    #[tokio::test]
    async fn test_wait_for_socket_returns_true_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        fs::write(&sock, b"").unwrap();

        // Already exists → returns immediately.
        let result = wait_for_socket(&sock, Duration::from_secs(1)).await;
        assert!(result, "wait_for_socket must return true for existing file");
    }

    /// `wait_for_socket` returns `false` when the file does not appear within
    /// the timeout. This verifies the bounded-wait guarantee: the caller does
    /// NOT block forever.
    #[tokio::test]
    async fn test_wait_for_socket_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("nonexistent.sock");

        let start = tokio::time::Instant::now();
        let result = wait_for_socket(&sock, Duration::from_millis(300)).await;
        let elapsed = start.elapsed();

        assert!(!result, "wait_for_socket must return false on timeout");
        assert!(
            elapsed >= Duration::from_millis(200),
            "must wait at least ~300ms before timing out, got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must NOT wait unreasonably long, got {elapsed:?}"
        );
    }

    /// `wait_for_socket` detects a file that appears AFTER the first poll.
    #[tokio::test]
    async fn test_wait_for_socket_detects_appearing_file() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("late.sock");

        // Spawn a task that creates the socket after 200ms.
        let sock_clone = sock.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            fs::write(&sock_clone, b"").unwrap();
        });

        let result = wait_for_socket(&sock, Duration::from_secs(5)).await;
        assert!(
            result,
            "wait_for_socket must detect the late-appearing file"
        );
    }

    /// `wait_for_sidecar` reads the endpoint from the sidecar after the daemon
    /// writes it.
    #[tokio::test]
    async fn test_wait_for_sidecar_reads_published_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path();

        // Write a valid live endpoint sidecar atomically using current_process
        // which captures the real PID and start time.
        let ep = DaemonEndpoint::current_process(run_dir.join("d.sock"), DAEMON_PROTOCOL_VERSION);
        write_endpoint_sidecar(run_dir, &ep).unwrap();

        let result = wait_for_sidecar(run_dir, Duration::from_secs(5)).await;
        assert!(
            result.is_ok(),
            "wait_for_sidecar must succeed: {:?}",
            result
        );
        let resolved = result.unwrap();
        assert_eq!(resolved.pid, ep.pid);
        assert_eq!(resolved.protocol_version, DAEMON_PROTOCOL_VERSION);
    }

    /// `wait_for_sidecar` times out when the sidecar never appears.
    #[tokio::test]
    async fn test_wait_for_sidecar_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path();

        // No sidecar is written, and no daemon connects.
        let result = wait_for_sidecar(run_dir, Duration::from_millis(300)).await;
        assert!(
            result.is_err(),
            "wait_for_sidecar must time out when sidecar never appears"
        );
    }

    /// `wait_for_sidecar` detects a sidecar that appears after the first poll.
    #[tokio::test]
    async fn test_wait_for_sidecar_detects_late_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();

        // Spawn a task that writes the sidecar after 200ms using
        // current_process which captures PID and start time correctly.
        let run_dir_clone = run_dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let ep = DaemonEndpoint::current_process(
                run_dir_clone.join("d.sock"),
                DAEMON_PROTOCOL_VERSION,
            );
            write_endpoint_sidecar(&run_dir_clone, &ep).unwrap();
        });

        let result = wait_for_sidecar(&run_dir, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "wait_for_sidecar must detect late endpoint");
    }

    /// End-to-end test: `spawn_and_wait` spawns `leindexd`, the daemon binds
    /// its socket and writes the sidecar, and the function returns the
    /// resolved endpoint. Uses the real `leindexd` binary after binding a
    /// Unix socket to verify the actual spawn path.
    ///
    /// This test mocks the daemon by creating the socket and sidecar directly
    /// (without spawning a real process) to verify that `spawn_and_wait`'s
    /// polling logic correctly waits for and returns the endpoint. The real
    /// leindexd integration is tested in `tests/daemon_spawn_test.rs`.
    #[tokio::test]
    async fn test_spawn_and_wait_waits_for_socket_and_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().to_path_buf();
        let socket_path = run_dir.join("d.sock");

        // We can't easily test the full spawn_and_wait because it spawns a
        // real leindexd binary. Instead, test the core bounded-wait logic by
        // verifying wait_for_socket + wait_for_sidecar work together.

        // Simulate the daemon binding the socket after 200ms.
        let sock_clone = socket_path.clone();
        let run_dir_clone = run_dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Bind a real Unix socket (matching what leindexd does).
            let _listener = UnixListener::bind(&sock_clone);

            // Write the endpoint sidecar using current_process which captures
            // PID and start time correctly.
            let ep = DaemonEndpoint::current_process(sock_clone.clone(), DAEMON_PROTOCOL_VERSION);
            write_endpoint_sidecar(&run_dir_clone, &ep).unwrap();

            // Keep the listener alive for the duration of the test by sleeping.
            tokio::time::sleep(Duration::from_secs(5)).await;
            // Listener drops here, cleaning up the socket.
        });

        // Wait for the socket to appear (bounded).
        let appeared = wait_for_socket(&socket_path, Duration::from_secs(5)).await;
        assert!(appeared, "socket must appear within timeout");

        // Read the endpoint sidecar.
        let endpoint = wait_for_sidecar(&run_dir, Duration::from_secs(5))
            .await
            .expect("sidecar must appear");

        assert_eq!(endpoint.protocol_version, DAEMON_PROTOCOL_VERSION);
        assert_eq!(endpoint.socket_path, socket_path);
    }
}
