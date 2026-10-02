//! Stdio shim (`leindex mcp` when a daemon is available).
//!
//! The shim exists so that ten editors cost one index, not ten. It does the
//! least it can: connect to the user's `leindexd`, tell it where the client is
//! working, then copy bytes both ways. It is synchronous, `std`-only code that
//! runs *before* the async runtime, the config loader and the CLI parser are
//! built, so an MCP client sees it start in about a millisecond and it holds a
//! couple of megabytes.
//!
//! Failure never strands the user: any problem before the first byte is
//! forwarded (no `leindexd` binary, spawn failure, a daemon of another
//! version with other clients attached) makes `run` return
//! `Outcome::Fallback`, and the caller runs the ordinary inline server.
//!
//! Lifecycle
//! 1. `connect(run/leindexd.sock)`. Success is the common, ~50 µs, path.
//! 2. Otherwise start `leindexd` detached (`setsid`) and poll the socket. Any
//!    number of shims may do this at once; `leindexd` takes an exclusive
//!    `flock` and the losers exit, so exactly one daemon wins.
//! 3. Exchange hello/ack (see `super::proto`). A daemon older than its own
//!    binary on disk, or of another version, is replaced when nobody else is
//!    attached.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::proto::{self, Ack, Hello, WIRE_VERSION};

/// Socket file name under the run directory.
pub const SOCKET_NAME: &str = "leindexd.sock";

/// Exclusive lock file the daemon holds for its whole life.
pub const LOCK_NAME: &str = "leindexd.lock";

/// How long to wait for a freshly spawned daemon to start listening.
const SPAWN_WAIT: Duration = Duration::from_secs(5);

/// How long to wait for the ack.
const ACK_WAIT: Duration = Duration::from_secs(3);

/// Cooldown between daemon spawn attempts: a spawned daemon that loses the
/// startup-lock race (typically to the outgoing daemon's still-held lock
/// during a replacement) exits immediately, and the connect loop must be
/// able to try again instead of polling a socket that will never answer.
/// The overall budget is the connect deadline, not an attempt count: a
/// slow-to-bind daemon (cold cache, loaded host) gets the full window.
const SPAWN_RETRY_COOLDOWN: Duration = Duration::from_secs(1);

/// Result of trying to serve this process through the daemon.
#[derive(Debug)]
pub enum Outcome {
    /// The session ran through the daemon and ended; exit with this status.
    Done(i32),
    /// The daemon path is unavailable; run the inline server. The string says
    /// why (for the log).
    Fallback(String),
}

/// If `args` (without `argv[0]`) is a plain stdio MCP launch, return the
/// project the client asked for (`Some(None)` = "use the cwd").
///
/// Anything else (`index`, `--socket`, an unknown flag) is not the shim's
/// business and returns `None`.
pub fn eligible_project(args: &[String]) -> Option<Option<PathBuf>> {
    let mut project = None;
    let mut saw_mcp = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "mcp" if !saw_mcp => saw_mcp = true,
            "--stdio" => {}
            "--project" | "-p" => project = Some(PathBuf::from(iter.next()?)),
            other => {
                let value = other.strip_prefix("--project=")?;
                project = Some(PathBuf::from(value));
            }
        }
    }
    Some(project)
}

/// Directory holding the daemon socket and lock.
pub fn run_dir() -> Option<PathBuf> {
    crate::config::resolve_leindex_home().map(|home| home.join("run"))
}

/// Serve this process's stdio through the daemon, or say why not.
pub fn run(project: Option<PathBuf>) -> Outcome {
    if !crate::feature_flags::FeatureFlag::DaemonClient.is_enabled() {
        return Outcome::Fallback("daemon client disabled".into());
    }
    let Some(dir) = run_dir() else {
        return Outcome::Fallback("no LeIndex home directory".into());
    };
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return Outcome::Fallback(format!("cannot create {}: {error}", dir.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The socket is a capability: only this user may connect. If the
        // directory's permissions cannot be restricted, the daemon we are
        // about to spawn would bind a world-connectable socket on a
        // multi-user host — any local user could drive its write tools —
        // so fail closed and run inline instead.
        if let Err(error) = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)) {
            return Outcome::Fallback(format!(
                "cannot restrict {} to 0700: {error}",
                dir.display()
            ));
        }
    }
    let socket = dir.join(SOCKET_NAME);
    let cwd = project
        .or_else(|| std::env::current_dir().ok())
        .and_then(|path| path.canonicalize().ok())
        .filter(|path| proto::is_projectish_cwd(path))
        .map(|path| path.to_string_lossy().into_owned());

    let mut replaced = false;
    let mut next_spawn_allowed = Instant::now();
    let deadline = Instant::now() + SPAWN_WAIT + ACK_WAIT;
    loop {
        match connect_and_greet(&socket, cwd.as_deref()) {
            Ok((stream, ack)) => {
                if !ack.ok {
                    return Outcome::Fallback(format!(
                        "daemon refused this client: {}",
                        ack.error.unwrap_or_default()
                    ));
                }
                if is_stale(&ack) {
                    if ack.clients > 1 || replaced {
                        return Outcome::Fallback(format!(
                            "daemon {} (pid {}) is out of date but has other clients",
                            ack.version, ack.pid
                        ));
                    }
                    replace(&stream, &socket, ack.pid);
                    drop(stream);
                    replaced = true;
                    // The replacement may lose the startup-lock race to the
                    // outgoing daemon's still-held lock and exit without
                    // ever binding; allow a fresh spawn attempt.
                    next_spawn_allowed = Instant::now() + SPAWN_RETRY_COOLDOWN;
                    continue;
                }
                return Outcome::Done(forward(stream));
            }
            Err(error) => {
                let now = Instant::now();
                if now >= deadline {
                    return Outcome::Fallback(format!("daemon unavailable: {error}"));
                }
                // Spawn (and re-spawn) under the remaining deadline budget
                // with a cooldown between forks: latching "spawned" on the
                // fork alone never retried a daemon that died during
                // startup, and a fixed attempt count gave up with most of
                // the budget unspent — orphaning a slow-to-bind daemon that
                // then raced the inline fallback to the same project. The
                // deadline, not the attempt count, decides when to stop.
                if now >= next_spawn_allowed {
                    match spawn_daemon(&socket) {
                        Ok(()) => next_spawn_allowed = now + SPAWN_RETRY_COOLDOWN,
                        Err(reason) => return Outcome::Fallback(reason),
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Connect and exchange hello/ack.
fn connect_and_greet(socket: &Path, cwd: Option<&str>) -> Result<(UnixStream, Ack), String> {
    let mut stream = UnixStream::connect(socket).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(ACK_WAIT))
        .map_err(|error| error.to_string())?;
    let hello = Hello {
        wire: WIRE_VERSION,
        version: env!("CARGO_PKG_VERSION").to_string(),
        cwd: cwd.map(str::to_string),
        pid: std::process::id(),
    };
    stream
        .write_all(proto::hello_line(&hello).as_bytes())
        .map_err(|error| error.to_string())?;
    // Read the ack byte by byte: anything buffered past the newline would be
    // lost to the forwarding loop.
    let mut line = Vec::with_capacity(160);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err("daemon closed the connection".into()),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > 4096 {
                    return Err("oversized ack".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let ack = proto::parse_ack(&String::from_utf8_lossy(&line))
        .ok_or_else(|| "daemon sent no ack (older version?)".to_string())?;
    stream
        .set_read_timeout(None)
        .map_err(|error| error.to_string())?;
    Ok((stream, ack))
}

/// Whether the daemon predates this build.
fn is_stale(ack: &Ack) -> bool {
    if ack.wire != WIRE_VERSION || ack.version != env!("CARGO_PKG_VERSION") {
        return true;
    }
    // Same version string but rebuilt since the daemon started (development
    // and in-place upgrades).
    daemon_binary()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|since_epoch| since_epoch.as_millis() as u64 > ack.started_ms)
}

/// Where `leindexd` lives: `LEINDEXD_BIN`, else next to this executable.
fn daemon_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("LEINDEXD_BIN") {
        return Some(PathBuf::from(explicit));
    }
    let exe = std::env::current_exe().ok()?;
    let sibling = exe.parent()?.join("leindexd");
    sibling.is_file().then_some(sibling)
}

/// Start `leindexd` detached from this process and its terminal.
fn spawn_daemon(socket: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let binary = daemon_binary().ok_or_else(|| "leindexd binary not found".to_string())?;
    let mut command = std::process::Command::new(&binary);
    command
        .arg("--socket")
        .arg(socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and touches no Rust state; it puts
    // the daemon in its own session so the shim's terminal and process group
    // signals never reach it.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", binary.display()))?;
    // Reap it if it exits (it does when another daemon already holds the
    // lock), without keeping the shim waiting.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// Ask a stale daemon to exit and wait until its socket stops answering.
///
/// The pid comes from the ack of whatever process answered the socket — an
/// unauthenticated wire value. Before signalling, verify it identifies the
/// process we are actually connected to: the kernel-provided peer
/// credentials of the connection must match, and (where `/proc` exists) the
/// target's executable must be the `leindexd` binary. This also covers pid
/// recycling between the ack and the kill. If either check fails or cannot
/// be performed, do not signal: the caller's deadline falls back to the
/// inline server instead of terminating an arbitrary process.
fn replace(stream: &UnixStream, socket: &Path, pid: u32) {
    if !pid_is_the_connected_daemon(stream, pid) {
        return;
    }
    // SAFETY: signal delivery to a pid verified above to be the process on
    // the other end of our own daemon connection.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if UnixStream::connect(socket).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `pid` really is the daemon on the other end of `stream`.
///
/// Verified with a kernel-provided peer credential where the platform has
/// one (SO_PEERCRED on Linux, LOCAL_PEERCRED on macOS — both primary
/// release targets), plus — where `/proc` exists — the target's executable
/// must be `leindexd` (guards a pid recycled between ack and kill).
fn pid_is_the_connected_daemon(stream: &UnixStream, pid: u32) -> bool {
    // 1. Kernel credential check: the peer that answered us must BE that pid
    //    (SO_PEERCRED; `UnixStream::peer_cred` is not stable in std yet).
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `getsockopt` on a valid, owned fd with a correctly sized
        // out-buffer; it writes only into `cred`.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 || cred.pid != pid as libc::pid_t {
            return false;
        }
    }
    // macOS: LOCAL_PEERCRED's xucred carries the peer's uid (the libc crate
    // does not yet mirror the kernel's pid extension, so pid-exact matching
    // is not available there). Requiring the peer to run as OUR effective
    // uid still ties the connection to this user — combined with the 0700
    // run directory, no other account can be the listener — and a liveness
    // probe guards a pid recycled between ack and kill. Without any check
    // here, a stale daemon could never be replaced on a primary release
    // platform and every session silently fell back to an inline server
    // (the double-index-per-editor problem the shim exists to prevent).
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
        // SAFETY: `getsockopt` on a valid, owned fd with a correctly sized
        // out-buffer; it writes only into `cred`. LOCAL_PEERCRED is a
        // socket-level (SOL_LOCAL = 0) option on macOS.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                0,
                libc::LOCAL_PEERCRED,
                &mut cred as *mut libc::xucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 || cred.cr_uid != libc::geteuid() {
            return false;
        }
        // SAFETY: signal 0 only probes for existence.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return false;
        }
    }
    // 2. The target must be running the leindexd binary (guards a pid
    //    recycled between ack and kill).
    #[cfg(target_os = "linux")]
    {
        let exe = std::fs::read_link(format!("/proc/{pid}/exe"));
        match exe {
            Ok(path) => {
                let mut name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                // `cargo install` / rustup upgrade by renaming over the
                // binary: the running daemon's /proc/<pid>/exe then reads
                // "leindexd (deleted)". Strip that suffix, and require an
                // exact match — a `starts_with` prefix also accepted
                // unrelated executables like "leindexd-malware".
                if let Some(stripped) = name.strip_suffix(" (deleted)") {
                    name = stripped.to_string();
                }
                if name != "leindexd" {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // No portable peer-credential probe on this platform: the pid cannot
        // be verified, so never signal it (the connect deadline in the
        // caller falls back to the inline server instead).
        let _ = (stream, pid);
        return false;
    }
    true
}

/// Copy stdin to the daemon and the daemon to stdout until either side ends.
/// Returns the process exit status.
fn forward(stream: UnixStream) -> i32 {
    let Ok(mut upstream) = stream.try_clone() else {
        return 1;
    };
    let mut downstream = stream;
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if upstream.write_all(&buffer[..read]).is_err() {
                        return;
                    }
                }
            }
        }
        // Client closed its end: let the daemon finish what it has and hang up.
        let _ = upstream.shutdown(std::net::Shutdown::Write);
    });

    let mut stdout = std::io::stdout().lock();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match downstream.read(&mut buffer) {
            Ok(0) => return 0,
            Ok(read) => {
                if stdout.write_all(&buffer[..read]).is_err() || stdout.flush().is_err() {
                    return 0;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return 1,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_plain_stdio_launches_are_eligible() {
        assert_eq!(eligible_project(&args(&[])), Some(None));
        assert_eq!(eligible_project(&args(&["mcp"])), Some(None));
        assert_eq!(eligible_project(&args(&["mcp", "--stdio"])), Some(None));
        assert_eq!(eligible_project(&args(&["--stdio"])), Some(None));
    }

    #[test]
    fn test_project_flag_forms_are_carried() {
        let expected = Some(Some(PathBuf::from("/w/p")));
        assert_eq!(
            eligible_project(&args(&["--project", "/w/p", "mcp"])),
            expected
        );
        assert_eq!(eligible_project(&args(&["mcp", "-p", "/w/p"])), expected);
        assert_eq!(
            eligible_project(&args(&["--project=/w/p", "mcp"])),
            expected
        );
    }

    #[test]
    fn test_other_commands_and_flags_are_not_eligible() {
        assert_eq!(eligible_project(&args(&["index", "."])), None);
        assert_eq!(
            eligible_project(&args(&["mcp", "--socket", "/tmp/x"])),
            None
        );
        assert_eq!(
            eligible_project(&args(&["mcp", "--mcp-idle-timeout-secs", "5"])),
            None
        );
        assert_eq!(
            eligible_project(&args(&["--project"])),
            None,
            "flag without a value"
        );
    }

    #[test]
    fn test_stale_detection_on_version_and_wire() {
        let mut ack = Ack {
            wire: WIRE_VERSION,
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: 1,
            started_ms: u64::MAX,
            clients: 1,
            ok: true,
            error: None,
        };
        assert!(
            !is_stale(&ack),
            "same version, started after any binary mtime"
        );
        ack.version = "0.0.1".into();
        assert!(is_stale(&ack));
        ack.version = env!("CARGO_PKG_VERSION").to_string();
        ack.wire = WIRE_VERSION + 1;
        assert!(is_stale(&ack));
    }
}
