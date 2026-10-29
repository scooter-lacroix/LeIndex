//! Managed model-server service lifecycle.
//!
//! A `[services.<name>]` config entry describes a llama-server-compatible
//! process. This module owns starting, health-checking, and stopping those
//! processes on the user's behalf:
//!
//! - spawn is detached into its own process group (stdout/stderr appended to
//!   `~/.leindex/logs/services/<name>.log`),
//! - the child pid is recorded in `~/.leindex/run/services/<name>.pid`,
//! - health is an HTTP `GET /health` (llama-server binds its port before the
//!   model is resident and answers 503 on every route until loading
//!   completes — a TCP connect alone is NOT readiness),
//! - a start holds an flock on `<name>.lock` so the daemon and the CLI can
//!   not double-spawn,
//! - stop only ever signals pids recorded in our own pidfiles, after
//!   re-verifying the process arguments (Linux `/proc`, macOS `sysctl`)
//!   that the process is still ours
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
    /// True when HTTP `GET /health` reports readiness, not merely TCP accept.
    pub http_ready: bool,
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
    /// Process identity and signal delivery are unsupported on this target.
    #[error("managed service lifecycle is supported only on Linux and macOS")]
    UnsupportedPlatform,
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

#[cfg(unix)]
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

fn first_addr(host: &str, port: u16) -> Option<std::net::SocketAddr> {
    (host, port).to_socket_addrs().ok()?.next()
}

/// True when `host:port` accepts a TCP connection. NOT readiness: modern
/// llama-server builds bind the port before the model is resident and answer
/// HTTP 503 on every route until the load finishes.
pub fn port_open(host: &str, port: u16) -> bool {
    let Some(addr) = first_addr(host, port) else {
        return false;
    };
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok()
}

/// HTTP-level readiness: managed services must implement the llama-server
/// contract (`GET /health` answers 503 while loading and 200 when usable).
/// A 404, redirect, or an unrelated HTTP listener is not proof of readiness.
/// This is the signal starts must wait for; a bare TCP accept would pass
/// mid-load and hand the caller a 503.
pub fn http_ready(host: &str, port: u16) -> bool {
    use std::io::Write;
    let Some(addr) = first_addr(host, port) else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    let request = format!("GET /health HTTP/1.0\r\nHost: {host}\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    // TCP reads need not contain the complete status line. Bound both the
    // line length and total read time so a slow drip cannot hold startup.
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut line = Vec::new();
    while line.len() < 256 {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        let mut byte = [0];
        if stream.read_exact(&mut byte).is_err() {
            return false;
        }
        if byte[0] == b'\n' {
            let Ok(head) = std::str::from_utf8(&line) else {
                return false;
            };
            let mut parts = head.split_whitespace();
            return matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0"))
                && parts.next() == Some("200");
        }
        line.push(byte[0]);
    }
    false
}

fn service_port_open(cfg: &ServiceConfig) -> bool {
    port_open(&cfg.host, cfg.port)
}

fn service_http_ready(cfg: &ServiceConfig) -> bool {
    http_ready(&cfg.host, cfg.port)
}

/// Poll until HTTP-ready or `secs` elapse. Used to converge on a start that
/// another process owns (lock held, or the port is held mid-load).
fn wait_ready(cfg: &ServiceConfig, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs.max(1));
    while Instant::now() < deadline {
        if service_http_ready(cfg) {
            return true;
        }
        std::thread::sleep(HEALTH_POLL_INTERVAL);
    }
    service_http_ready(cfg)
}

// ── Pid identity ──────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn proc_exists(pid: i32) -> bool {
    // A zombie is dead for our purposes: its cmdline is empty and it only
    // lingers until the parent reaps it.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat_is_zombie(&stat),
        Err(_) => false,
    }
}

/// macOS/BSD have no /proc: probe with signal 0 (permission-safe — EPERM
/// still proves the pid exists).
#[cfg(all(unix, not(target_os = "linux")))]
fn proc_exists(pid: i32) -> bool {
    // Reap our exited child before signal 0: zombies still answer that probe.
    // ECHILD for an unrelated process is harmless; fall back to signal 0.
    if unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) } == pid {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Unsupported targets never spawn or signal managed services. Status
/// cannot prove liveness there and preserves any existing pidfile.
#[cfg(not(unix))]
fn proc_exists(_pid: i32) -> bool {
    false
}

/// True when a `/proc/<pid>/stat` contents show state `Z`.
#[cfg(target_os = "linux")]
fn stat_is_zombie(stat: &str) -> bool {
    // `comm` may contain ')' so parse after the last one; the next token is
    // the process state.
    match stat.rsplit_once(')') {
        Some((_, rest)) => rest.split_whitespace().next() == Some("Z"),
        None => false,
    }
}

#[cfg(target_os = "linux")]
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

/// Native macOS argument lookup: `/proc` is not available on that target.
#[cfg(target_os = "macos")]
fn read_cmdline_args(pid: i32) -> Option<Vec<String>> {
    let mut argmax = 0i32;
    let mut size = std::mem::size_of_val(&argmax);
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: sysctl receives valid buffers and their exact byte lengths.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&mut argmax as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || argmax <= 0
    {
        return None;
    }
    let mut bytes = vec![0u8; argmax as usize];
    size = bytes.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: sysctl writes at most `size` bytes to the allocated buffer.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    bytes.truncate(size);
    parse_macos_procargs(&bytes)
}

/// KERN_PROCARGS2 is argc, executable path, NUL padding, argc NUL-terminated
/// arguments, then environment entries. Never count environment as argv.
#[cfg(any(target_os = "macos", test))]
fn parse_macos_procargs(bytes: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    if argc <= 0 || argc as usize > bytes.len() {
        return None;
    }
    let mut rest = bytes.get(4..)?;
    rest = rest.get(rest.iter().position(|b| *b == 0)? + 1..)?;
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    let mut args = Vec::new();
    for _ in 0..argc {
        let end = rest.iter().position(|b| *b == 0)?;
        args.push(std::str::from_utf8(&rest[..end]).ok()?.to_string());
        rest = &rest[end + 1..];
    }
    Some(args)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_cmdline_args(_pid: i32) -> Option<Vec<String>> {
    None
}

fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((_, base)) => base,
        None => trimmed,
    }
}

/// Pid-reuse guard: the process must still exist AND its cmdline must match
/// the configured service before we treat it as ours — argv[0] equal (exact
/// path, or basename for a PATH-resolved binary), or the exact path as
/// argv[1] (shebang scripts run with an interpreter as argv[0]).
/// Binary shape alone is not enough: a stale pidfile reused by an unrelated
/// `llama-server` (different model/host/port) must not be signaled, so the
/// distinctive arguments `build_command` always passes must be present too.
fn pid_is_ours(pid: i32, cfg: &ServiceConfig) -> bool {
    if !proc_exists(pid) {
        return false;
    }
    let Some(args) = read_cmdline_args(pid) else {
        return false;
    };
    command_matches(&args, cfg)
}

fn command_matches(args: &[String], cfg: &ServiceConfig) -> bool {
    let Some(argv0) = args.first() else {
        return false;
    };
    let binary = &cfg.binary;
    let binary_matches = argv0 == binary
        || (!binary.contains('/') && basename(argv0) == binary)
        || args.get(1) == Some(binary);
    if !binary_matches {
        return false;
    }
    let has_option = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    if let Some(model) = &cfg.model
        && !has_option("-m", model)
    {
        return false;
    }
    has_option("--host", &cfg.host) && has_option("--port", &cfg.port.to_string())
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

/// Drop only a provably dead pidfile. Failed identity reads (permissions,
/// unsupported platforms) must never erase tracking for a live service.
fn clean_stale_pidfile(name: &str, _cfg: &ServiceConfig) -> Result<bool, ServiceError> {
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        return Ok(false);
    }
    if let Some(pid) = read_pidfile(name)?
        && !proc_exists(pid)
    {
        remove_pidfile(name);
        return Ok(true);
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
        .current_dir("/");
    #[cfg(unix)]
    cmd.process_group(0);

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
    #[cfg(unix)]
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

    /// Windows has no flock(2); a start lock is unavailable, so concurrent
    /// starts are serialized only by the OS. Windows builds have no writer
    /// path today (see setup docs); revisit if one ships.
    #[cfg(not(unix))]
    fn acquire(_name: &str) -> Result<Option<Self>, ServiceError> {
        Ok(None)
    }
}

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Wait until HTTP-ready (model resident), the process dies, or timeout.
fn wait_healthy(name: &str, cfg: &ServiceConfig, pid: i32) -> Result<(), ServiceError> {
    let deadline = Instant::now() + Duration::from_secs(cfg.start_timeout_secs.max(1));
    loop {
        if service_http_ready(cfg) {
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

#[cfg(unix)]
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

#[cfg(unix)]
fn kill_group(pid: i32) {
    let pgid = unsafe { libc::getpgid(pid) };
    if pgid == pid {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    } else {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Windows has no signal delivery; managed starts cannot produce a child to
/// signal there (see `StartGuard::acquire`), so this is unreachable in
/// practice and a no-op for compile completeness.
#[cfg(not(unix))]
fn terminate_group(_pid: i32) {}

#[cfg(not(unix))]
fn kill_group(_pid: i32) {}

fn wait_exit(pid: i32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !proc_exists(pid) {
            return true;
        }
        std::thread::sleep(STOP_POLL_INTERVAL);
    }
    // True only when the process actually exited. Returning `proc_exists`
    // here inverted the result: a server that ignored SIGTERM for the whole
    // grace period read as "exited", so callers skipped SIGKILL, dropped the
    // pidfile, and reported Stopped over a still-running process.
    !proc_exists(pid)
}

// ── Public lifecycle ─────────────────────────────────────────────────────

fn require_managed_platform() -> Result<(), ServiceError> {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        Ok(())
    } else {
        Err(ServiceError::UnsupportedPlatform)
    }
}

/// Ensure the service is running, spawning it if `auto_start` allows and the
/// port is down. Returns `true` when this call spawned it. Never spawns a
/// service whose section lacks `auto_start` or `model`.
pub fn ensure_running(name: &str, cfg: &ServiceConfig) -> Result<bool, ServiceError> {
    if !cfg.auto_start || cfg.model.is_none() {
        return Ok(false);
    }
    require_managed_platform()?;
    if service_http_ready(cfg) {
        return Ok(false);
    }
    if service_port_open(cfg) {
        // Port bound but model still loading (503 window). Someone's start is
        // in flight — converge on it instead of racing a doomed duplicate.
        return Ok(wait_ready(cfg, cfg.start_timeout_secs));
    }
    clean_stale_pidfile(name, cfg)?;
    match StartGuard::acquire(name)? {
        Some(_guard) => start_locked(name, cfg).map(|_| true),
        // Another leindex process is mid-start; converge by waiting for HTTP
        // readiness instead of erroring into the caller's fail-open path.
        None => {
            if wait_ready(cfg, cfg.start_timeout_secs) {
                Ok(false)
            } else {
                Err(ServiceError::Concurrent(name.to_string()))
            }
        }
    }
}

/// Explicit start (the `leindex services start` path; ignores `auto_start`,
/// still requires `model`). Returns `true` when this call spawned it.
pub fn start(name: &str, cfg: &ServiceConfig) -> Result<bool, ServiceError> {
    require_managed_platform()?;
    if service_http_ready(cfg) {
        return Ok(false);
    }
    if service_port_open(cfg) {
        // Mid-load window owned by another start; wait it out.
        if !wait_ready(cfg, cfg.start_timeout_secs) {
            return Err(ServiceError::StartTimeout {
                name: name.to_string(),
                endpoint: cfg.base_url(),
                timeout_secs: cfg.start_timeout_secs,
                log: log_path(name)?,
            });
        }
        return Ok(false);
    }
    clean_stale_pidfile(name, cfg)?;
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
            cleanup_failed_start(pid);
        }
        remove_pidfile(name);
        return Err(e);
    }
    Ok(())
}

/// Terminate and reap a failed start attempt (best effort).
#[cfg(unix)]
fn cleanup_failed_start(pid: i32) {
    terminate_group(pid);
    if !wait_exit(pid, STOP_GRACE) {
        kill_group(pid);
        let _ = wait_exit(pid, STOP_GRACE);
    }
}

/// Windows: no signal delivery; managed starts are a no-op there today
/// (see `StartGuard::acquire`).
#[cfg(not(unix))]
fn cleanup_failed_start(_pid: i32) {}

/// Stop the service. Only ever signals pids from our own pidfiles, only
/// after the native process-argument pid-reuse check, and only the child's own
/// process group. Unmanaged listeners are never touched.
pub fn stop(name: &str, cfg: &ServiceConfig) -> Result<StopOutcome, ServiceError> {
    require_managed_platform()?;
    let Some(pid) = read_pidfile(name)? else {
        return Ok(StopOutcome::AlreadyDown);
    };
    if !proc_exists(pid) {
        remove_pidfile(name);
        return Ok(StopOutcome::StalePidfileRemoved);
    }
    if !pid_is_ours(pid, cfg) {
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

// On unsupported targets lifecycle entry points fail before spawning,
// signaling, or altering a pidfile; status/health remain available.

/// Current status: live verified pid, port health, endpoint, model info.
pub fn status(name: &str, cfg: &ServiceConfig) -> Result<ServiceStatus, ServiceError> {
    clean_stale_pidfile(name, cfg)?;
    let pid = read_pidfile(name)?.filter(|pid| pid_is_ours(*pid, cfg));
    Ok(ServiceStatus {
        name: name.to_string(),
        pid,
        port_open: service_port_open(cfg),
        http_ready: service_http_ready(cfg),
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
mod test {
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
    #[cfg(unix)]
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
    fn test_parse_macos_procargs_preserves_spaces_and_excludes_environment() {
        let mut bytes = 4i32.to_ne_bytes().to_vec();
        bytes.extend_from_slice(
            b"/usr/bin/server\0\0\0server\0-m\0/models/my model.gguf\0\0PORT=9000\0",
        );
        assert_eq!(
            parse_macos_procargs(&bytes).unwrap(),
            ["server", "-m", "/models/my model.gguf", ""]
        );
        bytes[..4].copy_from_slice(&(-1i32).to_ne_bytes());
        assert!(parse_macos_procargs(&bytes).is_none());
        assert!(parse_macos_procargs(b"bad").is_none());
    }

    #[test]
    fn test_command_matches_requires_binary_and_flag_values() {
        let mut cfg = sh_service(9000);
        cfg.binary = "/managed/llama-server".to_string();
        cfg.model = Some("/models/model.gguf".to_string());
        let args: Vec<String> = [
            "/managed/llama-server",
            "-m",
            "/models/model.gguf",
            "--host",
            "127.0.0.1",
            "--port",
            "9000",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(command_matches(&args, &cfg));
        let mut foreign = args.clone();
        foreign[0] = "/other/llama-server".to_string();
        assert!(!command_matches(&foreign, &cfg));
        foreign = args.clone();
        foreign[3] = "--alias".to_string();
        assert!(
            !command_matches(&foreign, &cfg),
            "host must be a host argument"
        );
        foreign = args.clone();
        foreign[6] = "9001".to_string();
        assert!(!command_matches(&foreign, &cfg));
        foreign = args.clone();
        foreign[2] = "/models/other.gguf".to_string();
        assert!(!command_matches(&foreign, &cfg));
        cfg.binary = "llama-server".to_string();
        assert!(command_matches(&args, &cfg));
    }

    #[test]
    fn test_status_distinguishes_tcp_from_http_readiness() {
        use std::io::Write;
        let _home = HomeGuard::new();
        for code in [200, 503, 404, 2000] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                // status first probes TCP, then sends the HTTP health request.
                drop(listener.accept().unwrap());
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 128];
                let _ = stream.read(&mut request);
                write!(stream, "HTTP/1.1 {code} Test\r\n\r\n").unwrap();
            });
            let st = status("http-status", &sh_service(port)).unwrap();
            server.join().unwrap();
            assert!(st.port_open);
            assert_eq!(st.http_ready, code == 200);
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_status_preserves_live_unverified_pidfile() {
        let _home = HomeGuard::new();
        let pid = std::process::id() as i32;
        write_pidfile("unverified", pid).unwrap();
        let st = status("unverified", &sh_service(9)).unwrap();
        assert_eq!(st.pid, None);
        assert_eq!(read_pidfile("unverified").unwrap(), Some(pid));
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn test_unsupported_lifecycle_preserves_pidfile() {
        let _home = HomeGuard::new();
        let mut cfg = sh_service(9);
        cfg.auto_start = true;
        cfg.model = Some("model.gguf".to_string());
        write_pidfile("unsupported", 123).unwrap();
        assert!(matches!(
            start("unsupported", &cfg),
            Err(ServiceError::UnsupportedPlatform)
        ));
        assert!(matches!(
            ensure_running("unsupported", &cfg),
            Err(ServiceError::UnsupportedPlatform)
        ));
        assert!(matches!(
            stop("unsupported", &cfg),
            Err(ServiceError::UnsupportedPlatform)
        ));
        assert_eq!(read_pidfile("unsupported").unwrap(), Some(123));
    }

    #[test]
    fn test_http_ready_accepts_fragmented_status_line() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 128];
            let _ = stream.read(&mut request);
            stream.write_all(b"HTTP/1.").unwrap();
            std::thread::sleep(Duration::from_millis(30));
            let _ = stream.write_all(b"1 200 OK\r\n\r\n");
        });
        let ready = http_ready("127.0.0.1", port);
        server.join().unwrap();
        assert!(ready, "a fragmented HTTP 200 status must be ready");
    }

    #[test]
    fn test_http_ready_bounds_slow_status_line() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 128];
            let _ = stream.read(&mut request);
            for byte in b"HTTP/1.1 200 OK\r\n" {
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        });
        let began = Instant::now();
        let ready = http_ready("127.0.0.1", port);
        let elapsed = began.elapsed();
        server.join().unwrap();
        assert!(!ready);
        assert!(elapsed < Duration::from_secs(1), "probe took {elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn test_wait_exit_reports_live_process_after_grace() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 300"])
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let exited = wait_exit(pid, Duration::ZERO);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!exited, "a live process must not count as exited");
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

    #[cfg(unix)]
    #[test]
    fn test_spawn_pidfile_stop_lifecycle() {
        let home = HomeGuard::new();
        let cfg = fake_server(&home.home, 9); // port 9: never the point here
        let pid = spawn_detached("life", &cfg).unwrap();
        write_pidfile("life", pid).unwrap();
        assert!(proc_exists(pid));
        // spawn() returns after fork, before exec: /proc/<pid>/cmdline still
        // shows the pre-exec image for a scheduling-dependent instant, and
        // pid_is_ours correctly refuses to claim a process whose argv is not
        // yet the configured binary. Wait (bounded) for exec to land instead
        // of racing it.
        let mut ours = false;
        for _ in 0..200 {
            if pid_is_ours(pid, &cfg) {
                ours = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ours, "child argv never matched the configured binary");

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

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
