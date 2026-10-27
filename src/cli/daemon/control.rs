//! User-facing lifecycle controls for the user-scoped `leindexd` daemon.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const SOCKET_NAME: &str = "leindexd.sock";
use super::endpoint::{DaemonEndpoint, ENDPOINT_SIDECAR, read_sidecar};
use super::handshake::DAEMON_PROTOCOL_VERSION;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Lifecycle state reported for the user-scoped `leindexd` process.
pub enum DaemonState {
    /// A daemon endpoint exists and belongs to a live `leindexd` process.
    Running,
    /// No daemon endpoint is present.
    Stopped,
    /// An endpoint exists but its protocol or process identity is invalid.
    Stale,
}

/// Return the directory containing the user-scoped daemon endpoint sidecars.
pub fn run_dir() -> Result<PathBuf> {
    Ok(crate::config::resolve_leindex_home()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve LeIndex home directory"))?
        .join("run"))
}

/// Read the daemon endpoint sidecar when present.
pub fn endpoint(run_dir: &Path) -> Result<Option<DaemonEndpoint>> {
    Ok(read_sidecar(&run_dir.join(ENDPOINT_SIDECAR))?)
}

/// Determine whether the daemon is running, stopped, or has stale metadata.
pub fn state(run_dir: &Path) -> Result<(DaemonState, Option<DaemonEndpoint>)> {
    let endpoint = endpoint(run_dir)?;
    let Some(endpoint) = endpoint else {
        return Ok((DaemonState::Stopped, None));
    };
    if endpoint.protocol_version == DAEMON_PROTOCOL_VERSION && pid_is_daemon(&endpoint) {
        Ok((DaemonState::Running, Some(endpoint)))
    } else {
        Ok((DaemonState::Stale, Some(endpoint)))
    }
}

/// Start `leindexd` detached from the invoking terminal.
pub fn start(run_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(run_dir)?;
    let (current, _) = state(run_dir)?;
    if current == DaemonState::Running {
        bail!("leindexd is already running");
    }
    if current == DaemonState::Stale {
        cleanup_sidecars(run_dir)?;
    }
    let binary = daemon_binary().context("leindexd binary not found; set LEINDEXD_BIN")?;
    let socket = run_dir.join(SOCKET_NAME);
    let mut command = std::process::Command::new(binary);
    command
        .arg("--socket")
        .arg(&socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn().context("failed to start leindexd")?;
    Ok(())
}

/// Request graceful daemon termination and remove endpoint sidecars.
pub fn stop(run_dir: &Path) -> Result<bool> {
    let (current, endpoint) = state(run_dir)?;
    match (current, endpoint) {
        (DaemonState::Running, Some(endpoint)) => {
            signal_term(endpoint.pid)?;
            if !wait_for_exit(endpoint.pid, std::time::Duration::from_secs(5)) {
                bail!("leindexd did not exit within 5 seconds; sidecars retained");
            }
            cleanup_sidecars(run_dir)?;
            Ok(true)
        }
        (DaemonState::Stale, Some(_)) => {
            cleanup_sidecars(run_dir)?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Stop any running daemon and start a fresh detached instance.
pub fn restart(run_dir: &Path) -> Result<()> {
    let _ = stop(run_dir)?;
    start(run_dir)
}

fn cleanup_sidecars(run_dir: &Path) -> Result<()> {
    for name in [ENDPOINT_SIDECAR, "daemon.endpoint.partial"] {
        let path = run_dir.join(name);
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn daemon_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LEINDEXD_BIN") {
        return Some(path.into());
    }
    let exe = std::env::current_exe().ok()?;
    let sibling = exe.parent()?.join("leindexd");
    sibling.is_file().then_some(sibling)
}

#[cfg(unix)]
fn signal_term(pid: u32) -> Result<()> {
    if pid == 0 || pid > i32::MAX as u32 {
        bail!("invalid daemon pid {pid}");
    }
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(unix)]
fn wait_for_exit(pid: u32, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[cfg(unix)]
fn pid_is_daemon(endpoint: &DaemonEndpoint) -> bool {
    if endpoint.pid == 0 || unsafe { libc::kill(endpoint.pid as libc::pid_t, 0) } != 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let expected = std::fs::read_link(format!("/proc/{}/exe", endpoint.pid)).ok();
        let expected_name = expected
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .map(|name| name.strip_suffix(" (deleted)").unwrap_or(name));
        expected_name == Some("leindexd")
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(not(unix))]
pub fn unsupported() -> Result<()> {
    bail!("leindexd lifecycle management is only supported on Unix platforms")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_endpoint_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(state(dir.path()).unwrap().0, DaemonState::Stopped);
    }
}
