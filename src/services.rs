//! Managed model-server service lifecycle.
//!
//! A `[services.<name>]` config entry describes a llama-server-compatible
//! process. This module owns starting, health-checking, and stopping those
//! processes on the user's behalf:
//!
//! - spawn is detached into its own process group (stdout/stderr appended to
//!   `~/.leindex/logs/services/<name>.log`),
//! - the child pid is recorded in `~/.leindex/run/services/<name>.pid`,
//! - health is a stdlib TCP connect (llama-server accepts only after the
//!   model is loaded),
//! - a start holds an flock on `<name>.lock` so the daemon and the CLI can
//!   not double-spawn,
//! - stop only ever signals pids recorded in our own pidfiles, after
//!   re-verifying via `/proc/<pid>/cmdline` that the process is still ours
//!   (pid-reuse guard), and only ever its own process group.

use crate::config::{ServiceConfig, resolve_leindex_home};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Everything `leindex services status` wants to show about one service.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceStatus {
    /// Config section name.
    pub name: String,
    /// Pid from our pidfile, if a live, verified-ours process exists.
    pub pid: Option<i32>,
    /// True when the configured host:port accepts TCP connections.
    pub port_open: bool,
    /// Derived base URL (`http://host:port`).
    pub endpoint: String,
    /// Configured model file, if any.
    pub model: Option<String>,
    /// Served model name (alias or model file stem).
    pub served_model: Option<String>,
    /// `auto_start` from config.
    pub auto_start: bool,
}

/// Outcome of a stop request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// Process group terminated (SIGTERM, escalated to SIGKILL if needed).
    Stopped(i32),
    /// No live process and no port listener; nothing to do.
    AlreadyDown,
    /// Pidfile existed but pointed at a dead or foreign pid; file removed.
    StalePidfileRemoved,
}

/// Errors from service lifecycle operations.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Section has no model; the endpoint can be derived but never spawned.
    #[error("service '{0}' has no model configured; set services.{0}.model to allow starting")]
    NoModel(String),
    /// The configured binary could not be found or executed.
    #[error("cannot execute '{0}'")]
    BinaryNotFound(String),
    /// No home directory to place run/log files under.
    #[error("could not resolve LeIndex home directory")]
    NoHome,
    /// Underlying IO failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Start attempt did not become healthy in time; attempt terminated.
    #[error(
        "service '{name}' did not answer on {endpoint} within {timeout_secs}s (attempt terminated; see {log})"
    )]
    StartTimeout {
        /// Service name.
        name: String,
        /// Endpoint that never answered.
        endpoint: String,
        /// Configured timeout that elapsed.
        timeout_secs: u64,
        /// Path of the service log for debugging.
        log: PathBuf,
    },
    /// Spawned process exited before the port answered.
    #[error("service '{name}' exited during startup (see {log})")]
    DiedDuringStart {
        /// Service name.
        name: String,
        /// Path of the service log for debugging.
        log: PathBuf,
    },
    /// Pidfile names a pid that is not this service; refusing to signal it.
    #[error(
        "pidfile for '{0}' points at pid {1} which is not this service; refusing to signal (pidfile left in place)"
    )]
    NotOurs(String, i32),
    /// Another process is starting the same service.
    #[error("another start of '{0}' is already in progress")]
    Concurrent(String),
}

const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const STOP_GRACE: Duration = Duration::from_secs(10);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

// ── Paths ────────────────────────────────────────────────────────────────

fn run_dir() -> Result<PathBuf, ServiceError> {
    let home = resolve_leindex_home().ok_or(ServiceError::NoHome)?;
    Ok(home.join("run").join("services"))
}

fn pid_path(name: &str) -> Result<PathBuf, ServiceError> {
    Ok(run_dir()?.join(format!("{name}.pid")))
}

fn lock_path(name: &str) -> Result<PathBuf, ServiceError> {
    Ok(run_dir()?.join(format!("{name}.lock")))
}

fn log_path(name: &str) -> Result<PathBuf, ServiceError> {
    let home = resolve_leindex_home().ok_or(ServiceError::NoHome)?;
    Ok(home
        .join("logs")
        .join("services")
        .join(format!("{name}.log")))
}

// ── Health ───────────────────────────────────────────────────────────────

/// True when `host:port` accepts a TCP connection. llama-server binds its
/// port only after the model is loaded, so an accepted connect means ready.
pub fn port_open(host: &str, port: u16) -> bool {
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok()
}

fn service_port_open(cfg: &ServiceConfig) -> bool {
    port_open(&cfg.host, cfg.port)
}

// ── Pid identity ──────────────────────────────────────────────────────────

fn proc_exists(pid: i32) -> bool {
    // A zombie is dead for our purposes: its cmdline is empty and it only
    // lingers until the parent reaps it.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat_is_zombie(&stat),
        Err(_) => false,
    }
}

/// True when a `/proc/<pid>/stat` contents show state `Z`.
fn stat_is_zombie(stat: &str) -> bool {
    // `comm` may contain ')' so parse after the last one; the next token is
    // the process state.
    match stat.rsplit_once(')') {
        Some((_, rest)) => rest.split_whitespace().next() == Some("Z"),
        None => false,
    }
}

fn read_cmdline_args(pid: i32) -> Option<Vec<String>> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .read_to_end(&mut bytes)
        .ok()?;
    let args = bytes
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    Some(args)
}

fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((_, base)) => base,
        None => trimmed,
    }
}

/// Pid-reuse guard: the process must still exist AND its cmdline must match
/// the configured binary before we treat it as ours — argv[0] equal (exact
/// path or basename), or the exact path present as an argument (shebang
/// scripts run with the interpreter as argv[0] and the script as argv[1]).
fn pid_is_ours(pid: i32, binary: &str) -> bool {
    let Some(args) = read_cmdline_args(pid) else {
        return false;
    };
    let Some(argv0) = args.first() else {
        return false;
    };
    argv0 == binary || basename(argv0) == basename(binary) || args.iter().any(|a| a == binary)
}

fn read_pidfile(name: &str) -> Result<Option<i32>, ServiceError> {
    let path = pid_path(name)?;
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    match content.trim().parse::<i32>() {
        Ok(pid) if pid > 0 => Ok(Some(pid)),
        _ => Ok(None),
    }
}

fn write_pidfile(name: &str, pid: i32) -> Result<(), ServiceError> {
    let path = pid_path(name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, pid.to_string())?;
    Ok(())
}

fn remove_pidfile(name: &str) {
    let _ = std::fs::remove_file(pid_path(name).unwrap_or_else(|_| PathBuf::from("/dev/null")));
}

/// Drop a pidfile whose pid is gone or no longer ours.
fn clean_stale_pidfile(name: &str, binary: &str) -> Result<bool, ServiceError> {
    if let Some(pid) = read_pidfile(name)? {
        if !pid_is_ours(pid, binary) {
            remove_pidfile(name);
            return Ok(true);
        }
    }
    Ok(false)
}

// ── Spawn ────────────────────────────────────────────────────────────────

fn build_command(cfg: &ServiceConfig) -> Command {
    let mut cmd = Command::new(&cfg.binary);
    if let Some(model) = &cfg.model {
        cmd.arg("-m").arg(model);
    }
    cmd.arg("--host").arg(&cfg.host);
    cmd.arg("--port").arg(cfg.port.to_string());
    if let Some(alias) = &cfg.alias {
        cmd.arg("--alias").arg(alias);
    }
    cmd.args(&cfg.extra_args);
    cmd
}

/// Spawn the service detached: own process group, logs appended to the
/// service log, nothing inherited that ties it to this process' lifetime.
/// Returns the child pid (== its process-group id).
fn spawn_detached(name: &str, cfg: &ServiceConfig) -> Result<i32, ServiceError> {
    let log = log_path(name)?;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log_file = OpenOptions::new().create(true).append(true).open(&log)?;

    let mut cmd = build_command(cfg);
    cmd.stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file))
        .stdin(Stdio::null())
        .current_dir("/")
        .process_group(0);

    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ServiceError::BinaryNotFound(cfg.binary.clone())
        } else {
            e.into()
        }
    })?;
    Ok(child.id() as i32)
}

/// Hold on an exclusive start lock for one service.
struct StartGuard {
    _file: File,
}

impl StartGuard {
    /// Non-blocking exclusive flock; `Ok(None)` when someone else holds it.
    fn acquire(name: &str) -> Result<Option<Self>, ServiceError> {
        let path = lock_path(name)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            Ok(Some(Self { _file: file }))
        } else {
            Ok(None)
        }
    }
}

use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;

/// Wait until the port answers or the process dies.
fn wait_healthy(name: &str, cfg: &ServiceConfig, pid: i32) -> Result<(), ServiceError> {
    let deadline = Instant::now() + Duration::from_secs(cfg.start_timeout_secs.max(1));
    loop {
        if service_port_open(cfg) {
            return Ok(());
        }
        if !proc_exists(pid) {
            return Err(ServiceError::DiedDuringStart {
                name: name.to_string(),
                log: log_path(name)?,
            });
        }
        if Instant::now() >= deadline {
            return Err(ServiceError::StartTimeout {
                name: name.to_string(),
                endpoint: cfg.base_url(),
                timeout_secs: cfg.start_timeout_secs,
                log: log_path(name)?,
            });
        }
        std::thread::sleep(HEALTH_POLL_INTERVAL);
    }
}

fn terminate_group(pid: i32) {
    // Only the process group we created (pgid == child pid); never a
    // foreign group even if the pid was recycled into one.
    let pgid = unsafe { libc::getpgid(pid) };
    if pgid == pid {
        unsafe { libc::kill(-pid, libc::SIGTERM) };
    } else {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

fn kill_group(pid: i32) {
    let pgid = unsafe { libc::getpgid(pid) };
    if pgid == pid {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    } else {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

fn wait_exit(pid: i32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !proc_exists(pid) {
            return true;
        }
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
    proc_exists(pid)
}

// ── Public lifecycle ─────────────────────────────────────────────────────

/// Ensure the service is running, spawning it if `auto_start` allows and the
/// port is down. Returns `true` when this call spawned it. Never spawns a
/// service whose section lacks `auto_start` or `model`.
pub fn ensure_running(name: &str, cfg: &ServiceConfig) -> Result<bool, ServiceError> {
    if !cfg.auto_start || cfg.model.is_none() {
        return Ok(false);
    }
    if service_port_open(cfg) {
        return Ok(false);
    }
    clean_stale_pidfile(name, &cfg.binary)?;
    match StartGuard::acquire(name)? {
        Some(_guard) => start_locked(name, cfg).map(|_| true),
        // Another leindex process is mid-start; converge by waiting for the
        // port instead of erroring into the caller's fail-open path.
        None => {
            let deadline = Instant::now() + Duration::from_secs(cfg.start_timeout_secs.max(1));
            while Instant::now() < deadline {
                if service_port_open(cfg) {
                    return Ok(false);
                }
                std::thread::sleep(HEALTH_POLL_INTERVAL);
            }
            Err(ServiceError::Concurrent(name.to_string()))
        }
    }
}

/// Explicit start (the `leindex services start` path; ignores `auto_start`,
/// still requires `model`). Returns `true` when this call spawned it.
pub fn start(name: &str, cfg: &ServiceConfig) -> Result<bool, ServiceError> {
    if service_port_open(cfg) {
        return Ok(false);
    }
    clean_stale_pidfile(name, &cfg.binary)?;
    let Some(_guard) = StartGuard::acquire(name)? else {
        return Err(ServiceError::Concurrent(name.to_string()));
    };
    start_locked(name, cfg).map(|_| true)
}

/// Spawn + health-wait under the start lock. On failure the attempt is
/// terminated and the pidfile removed — no half-loaded zombies.
fn start_locked(name: &str, cfg: &ServiceConfig) -> Result<(), ServiceError> {
    if cfg.model.is_none() {
        return Err(ServiceError::NoModel(name.to_string()));
    }
    let pid = spawn_detached(name, cfg)?;
    write_pidfile(name, pid)?;
    if let Err(e) = wait_healthy(name, cfg, pid) {
        if proc_exists(pid) {
            terminate_group(pid);
            if wait_exit(pid, STOP_GRACE) {
                // graceful exit
            } else {
                kill_group(pid);
                let _ = wait_exit(pid, STOP_GRACE);
            }
        }
        remove_pidfile(name);
        return Err(e);
    }
    Ok(())
}

/// Stop the service. Only ever signals pids from our own pidfiles, only
/// after the `/proc/<pid>/cmdline` pid-reuse check, and only the child's own
/// process group. Unmanaged listeners are never touched.
pub fn stop(name: &str, cfg: &ServiceConfig) -> Result<StopOutcome, ServiceError> {
    let Some(pid) = read_pidfile(name)? else {
        return Ok(StopOutcome::AlreadyDown);
    };
    if !proc_exists(pid) {
        remove_pidfile(name);
        return Ok(StopOutcome::StalePidfileRemoved);
    }
    if !pid_is_ours(pid, &cfg.binary) {
        // Foreign process wearing our pid — clean nothing, signal nothing.
        return Err(ServiceError::NotOurs(name.to_string(), pid));
    }
    terminate_group(pid);
    if !wait_exit(pid, STOP_GRACE) {
        kill_group(pid);
        let _ = wait_exit(pid, STOP_GRACE);
    }
    remove_pidfile(name);
    Ok(StopOutcome::Stopped(pid))
}

/// Current status: live verified pid, port health, endpoint, model info.
pub fn status(name: &str, cfg: &ServiceConfig) -> Result<ServiceStatus, ServiceError> {
    clean_stale_pidfile(name, &cfg.binary)?;
    let pid = read_pidfile(name)?.filter(|pid| pid_is_ours(*pid, &cfg.binary));
    Ok(ServiceStatus {
        name: name.to_string(),
        pid,
        port_open: service_port_open(cfg),
        endpoint: cfg.base_url(),
        model: cfg.model.clone(),
        served_model: cfg.served_name(),
        auto_start: cfg.auto_start,
    })
}

/// Restart: stop (if running) then start.
pub fn restart(name: &str, cfg: &ServiceConfig) -> Result<bool, ServiceError> {
    stop(name, cfg)?;
    start(name, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hold LEINDEX_HOME for the whole test body; parallel tests serialize.
    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        home: PathBuf,
    }

    impl HomeGuard {
        fn new() -> Self {
            static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            let lock = LOCK
                .get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|p| p.into_inner()); // poison-proof: env is reset each acquire
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().to_path_buf();
            // SAFETY: env mutation serialized by LOCK; restored on drop.
            unsafe { std::env::set_var(crate::config::LEINDEX_HOME_ENV, &home) };
            std::mem::forget(tmp); // keep dir alive; OS tmp cleanup handles it
            Self { _lock: lock, home }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: same lock discipline as set.
            unsafe { std::env::remove_var(crate::config::LEINDEX_HOME_ENV) };
        }
    }

    /// A fake llama-server: ignores all argv, sleeps in the foreground (no
    /// exec, so argv[0] stays the script path and the group kill has a child
    /// to catch).
    fn fake_server(home: &std::path::Path, port: u16) -> ServiceConfig {
        let script = home.join("fake-server.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 300\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        ServiceConfig {
            binary: script.display().to_string(),
            model: Some("/models/fake.gguf".to_string()),
            alias: None,
            host: "127.0.0.1".to_string(),
            port,
            extra_args: Vec::new(),
            auto_start: true,
            start_timeout_secs: 2,
        }
    }

    fn sh_service(port: u16) -> ServiceConfig {
        ServiceConfig {
            binary: "/bin/sh".to_string(),
            model: None,
            alias: None,
            host: "127.0.0.1".to_string(),
            port,
            extra_args: Vec::new(),
            auto_start: false,
            start_timeout_secs: 2,
        }
    }

    #[test]
    fn test_ensure_running_skips_when_port_answers() {
        let _home = HomeGuard::new();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // A binary that would fail instantly if spawned: proves no spawn.
        let mut cfg = sh_service(port);
        cfg.binary = "/nonexistent/fake-server".to_string();
        cfg.auto_start = true;
        assert!(!ensure_running("x", &cfg).unwrap());
    }

    #[test]
    fn test_ensure_running_requires_auto_start_and_model() {
        let _home = HomeGuard::new();
        let mut cfg = sh_service(1); // port 1: nothing answers
        cfg.auto_start = false;
        assert!(!ensure_running("x", &cfg).unwrap());
        cfg.auto_start = true;
        // No model: the section may derive an endpoint but never spawn.
        assert!(!ensure_running("x", &cfg).unwrap());
    }

    #[test]
    fn test_spawn_pidfile_stop_lifecycle() {
        let home = HomeGuard::new();
        let cfg = fake_server(&home.home, 9); // port 9: never the point here
        let pid = spawn_detached("life", &cfg).unwrap();
        write_pidfile("life", pid).unwrap();
        assert!(proc_exists(pid));
        assert!(pid_is_ours(pid, &cfg.binary));

        let st = status("life", &cfg).unwrap();
        assert_eq!(st.pid, Some(pid));
        assert!(!st.port_open);

        match stop("life", &cfg).unwrap() {
            StopOutcome::Stopped(p) => assert_eq!(p, pid),
            other => panic!("expected Stopped, got {other:?}"),
        }
        assert!(!proc_exists(pid));
        assert_eq!(read_pidfile("life").unwrap(), None);
    }

    #[test]
    fn test_stop_refuses_foreign_pid() {
        let _home = HomeGuard::new();
        let cfg = sh_service(9);
        // Point the pidfile at THIS test process: alive, but argv[0] is the
        // test binary, not /bin/sh — must refuse and leave the file alone.
        write_pidfile("mine", std::process::id() as i32).unwrap();
        match stop("mine", &cfg) {
            Err(ServiceError::NotOurs(name, pid)) => {
                assert_eq!(name, "mine");
                assert_eq!(pid, std::process::id() as i32);
            }
            other => panic!("expected NotOurs, got {other:?}"),
        }
        // We are still alive.
        assert!(proc_exists(std::process::id() as i32));
    }

    #[test]
    fn test_stale_pidfile_cleaned_by_status() {
        let _home = HomeGuard::new();
        let cfg = sh_service(9);
        // Spawn a real process, reap it, then point the pidfile at the
        // now-dead pid.
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead_pid = child.id() as i32;
        let _ = child.wait();
        assert!(!proc_exists(dead_pid));

        write_pidfile("stale", dead_pid).unwrap();
        let st = status("stale", &cfg).unwrap();
        assert_eq!(st.pid, None, "dead pid must not report as running");
        assert_eq!(read_pidfile("stale").unwrap(), None, "pidfile must be gone");
    }

    #[test]
    fn test_start_times_out_and_cleans_up() {
        let home = HomeGuard::new();
        // Port 9 never answers; the fake server will be terminated on timeout.
        let cfg = fake_server(&home.home, 9);
        let err = start("t", &cfg).unwrap_err();
        assert!(
            matches!(err, ServiceError::StartTimeout { .. }),
            "got {err:?}"
        );
        assert_eq!(read_pidfile("t").unwrap(), None);
    }
}
