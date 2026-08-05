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

    #[test]
    fn test_resolve_leindexd_path_does_not_panic() {
        // The function should either find the binary or return an error,
        // but never panic.
        let _ = resolve_leindexd_path();
    }
}
