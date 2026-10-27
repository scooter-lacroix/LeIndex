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
        (DaemonState::Running, Some(endpoint)) => stop_running(&endpoint, run_dir),
        (DaemonState::Stale, Some(_)) => {
            cleanup_sidecars(run_dir)?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// How long to wait for the daemon to exit after SIGTERM before giving up.
const STOP_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Terminate one known-live daemon, wait for it to exit, then remove its
/// sidecars.
///
/// When the daemon ignores SIGTERM and the timeout elapses, this returns an
/// error WITHOUT removing the sidecars: deleting them would make `state()`
/// report `Stopped` while the orphaned daemon is still alive and holding its
/// lifetime lock.
fn stop_running(endpoint: &DaemonEndpoint, run_dir: &Path) -> Result<bool> {
    signal_term(endpoint.pid)?;
    if !wait_for_exit(endpoint.pid, STOP_EXIT_TIMEOUT) {
        bail!(
            "leindexd did not exit within {} seconds; sidecars retained",
            STOP_EXIT_TIMEOUT.as_secs()
        );
    }
    cleanup_sidecars(run_dir)?;
    Ok(true)
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

/// Strip the `" (deleted)"` suffix Linux appends to the `/proc/<pid>/exe`
/// symlink target after the executable file is replaced while the process keeps
/// running (e.g. `leindexd` upgraded by an installer). Without this
/// normalization a live post-upgrade daemon is misclassified as stale.
fn normalize_exe_name(name: &str) -> &str {
    name.strip_suffix(" (deleted)").unwrap_or(name)
}

/// True when the resolved `/proc/<pid>/exe` target names the `leindexd`
/// daemon binary (including the deleted-file variant, see
/// [`normalize_exe_name`]).
fn is_leindex_daemon_exe(exe_link: Option<&Path>) -> bool {
    let Some(name) = exe_link.and_then(Path::file_name).and_then(|n| n.to_str()) else {
        return false;
    };
    normalize_exe_name(name) == "leindexd"
}

#[cfg(unix)]
fn pid_is_daemon(endpoint: &DaemonEndpoint) -> bool {
    if endpoint.pid == 0 || unsafe { libc::kill(endpoint.pid as libc::pid_t, 0) } != 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let expected = std::fs::read_link(format!("/proc/{}/exe", endpoint.pid)).ok();
        is_leindex_daemon_exe(expected.as_deref())
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

    #[test]
    fn normalize_exe_name_strips_deleted_suffix() {
        assert_eq!(normalize_exe_name("leindexd"), "leindexd");
        assert_eq!(normalize_exe_name("leindexd (deleted)"), "leindexd");
        assert_eq!(
            normalize_exe_name("leindexd (deleted) "),
            "leindexd (deleted) "
        );
        assert_eq!(normalize_exe_name("leindexd-deleted"), "leindexd-deleted");
        assert_eq!(normalize_exe_name(""), "");
    }

    #[test]
    fn daemon_exe_matches_plain_and_deleted_binary_names() {
        use std::path::Path;
        assert!(is_leindex_daemon_exe(Some(Path::new(
            "/usr/local/bin/leindexd"
        ))));
        assert!(is_leindex_daemon_exe(Some(Path::new(
            "/usr/local/bin/leindexd (deleted)"
        ))));
        assert!(!is_leindex_daemon_exe(Some(Path::new("/usr/bin/sleep"))));
        assert!(!is_leindex_daemon_exe(Some(Path::new(
            "/usr/local/bin/leindexd.bak"
        ))));
        assert!(!is_leindex_daemon_exe(None));
        // The deleted suffix is only recognized at the very end of the name.
        assert!(!is_leindex_daemon_exe(Some(Path::new(
            "/usr/local/bin/leindexd (deleted)~"
        ))));
    }

    /// Spawn a detached child that outlives this test until killed.
    fn spawn_sleep_child() -> std::process::Child {
        use std::process::{Command, Stdio};
        Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep")
    }

    #[test]
    fn wait_for_exit_reports_live_process_as_not_exited_then_exited_after_reap() {
        let mut child = spawn_sleep_child();
        let pid = child.id();
        // Alive: must NOT report exit within a short window.
        assert!(!wait_for_exit(pid, std::time::Duration::from_millis(80)));
        // Reap it, then the probe must report that it exited.
        child.kill().expect("kill sleep");
        child.wait().expect("reap sleep");
        assert!(wait_for_exit(pid, std::time::Duration::from_secs(2)));
    }

    #[test]
    fn stop_running_retains_sidecars_when_daemon_ignores_sigterm() {
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ready");
        // A child that installs a SIGTERM handler (so it stays alive), signals
        // readiness, then idles forever — mimicking a wedged daemon.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; touch \"$1\"; while :; do sleep 1; done")
            .arg("sh")
            .arg(&marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn term-ignoring child");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let endpoint = DaemonEndpoint {
            socket_path: dir.path().join("d.sock"),
            pid: child.id(),
            pid_start_time_ms: 0,
            protocol_version: DAEMON_PROTOCOL_VERSION,
            leindex_version: "test".into(),
        };
        let sidecar = dir.path().join(ENDPOINT_SIDECAR);
        std::fs::write(&sidecar, serde_json::to_vec(&endpoint).unwrap()).unwrap();

        let err = stop_running(&endpoint, dir.path()).unwrap_err();
        assert!(err.to_string().contains("did not exit"), "{err}");
        assert!(
            sidecar.exists(),
            "sidecars must be retained when the daemon refuses to exit"
        );

        child.kill().expect("kill child");
        child.wait().expect("reap child");
    }

    #[test]
    fn stop_running_removes_sidecars_after_daemon_exits() {
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().unwrap();
        // `sleep` terminates on SIGTERM, unlike the trap-installing child above.
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let endpoint = DaemonEndpoint {
            socket_path: dir.path().join("d.sock"),
            pid,
            pid_start_time_ms: 0,
            protocol_version: DAEMON_PROTOCOL_VERSION,
            leindex_version: "test".into(),
        };
        let sidecar = dir.path().join(ENDPOINT_SIDECAR);
        std::fs::write(&sidecar, serde_json::to_vec(&endpoint).unwrap()).unwrap();

        // The test process would hold the child as a zombie until reaped, and
        // `wait_for_exit` probes with `kill(pid, 0)` which succeeds on zombies
        // — mirror what init does for a real daemon by reaping the moment the
        // child dies.
        let reaper = std::thread::spawn(move || {
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return status,
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    Err(e) => panic!("wait on child failed: {e}"),
                }
            }
        });

        assert!(stop_running(&endpoint, dir.path()).unwrap());
        assert!(!sidecar.exists(), "sidecars must be removed after exit");

        reaper.join().expect("reaper thread");
    }
}
