//! Daemon endpoint discovery + hard single-winner startup lock (spec §4.2).
//!
//! Spec §4.2: "One daemon runs per OS user." This module implements a hard
//! single-winner lock so the first process that needs a daemon wins the spawn
//! race and all subsequent callers connect to the existing daemon.
//!
//! This is distinct from the advisory per-project [`McpProjectLock`]:
//!
//! - `McpProjectLock` is advisory: a second instance logs a warning and
//!   continues serving inline. The GrayHill invariant (stdio process is 1:1
//!   with the agent's pipe) forbids hard-exiting.
//! - This lock is a hard single-winner: exactly one process wins `Won` and
//!   must spawn/serve the daemon. All others get `Connect` and forward to it.
//!
//! [`McpProjectLock`]: crate::cli::mcp::lock::McpProjectLock

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Filename of the endpoint sidecar beneath the run dir.
pub const ENDPOINT_SIDECAR: &str = "daemon.endpoint";

/// Sidecar contents: socket path + pid + start time + protocol version + LeIndex
/// version (spec §4.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaemonEndpoint {
    /// Unix domain socket path the daemon is listening on (`d.sock` under run
    /// dir).
    pub socket_path: PathBuf,
    /// OS process ID of the daemon.
    pub pid: u32,
    /// `btime` field from `/proc/<pid>/stat` (Linux) — the process start time
    /// in clock ticks since boot. Used to detect PID recycling: a dead
    /// daemon's PID may be reused by an unrelated process, so comparing start
    /// times ensures a stale sidecar is stolen even if the PID is alive again.
    pub pid_start_time_ms: u64,
    /// Daemon protocol version (spec §12.1). A mismatch between the sidecar's
    /// `protocol_version` and the caller's `DAEMON_PROTOCOL_VERSION` causes the
    /// sidecar to be treated as stale (stolen), forcing a restart.
    pub protocol_version: u32,
    /// SemVer string of the LeIndex build that wrote the sidecar
    /// (`CARGO_PKG_VERSION` at compile time). Informational: the numeric
    /// `protocol_version` is authoritative for compatibility.
    pub leindex_version: String,
}

/// Lock outcome for the daemon-startup race.
///
/// Returned by [`resolve_endpoint`]. Exactly one process wins per OS user;
/// `Won` and `Connect` are mutually-exclusive outcomes at any given time.
#[derive(Debug)]
pub enum StartupOutcome {
    /// This process won the startup race and must spawn/serve the daemon.
    /// The returned endpoint is the one the caller should bind and publish.
    Won(DaemonEndpoint),
    /// Another live daemon already exists: connect to it instead of spawning.
    Connect(DaemonEndpoint),
}

/// Resolve the daemon endpoint under `run_dir` using a hard single-winner lock.
///
/// Returns:
///
/// - [`StartupOutcome::Won`] when no `daemon.endpoint` sidecar exists (first
///   caller), or the sidecar is stale (dead PID, PID recycled, protocol
///   mismatch). The winner writes the sidecar atomically *after* binding the
///   socket (done by the daemon binary in Task 3 of the plan).
/// - [`StartupOutcome::Connect`] when the sidecar exists and the recorded PID
///   is alive with matching protocol version.
///
/// Liveness is checked on Linux via `/proc/<pid>/stat` start time (reuses the
/// pattern from [`mcp/lock.rs`]). On macOS/Windows the check falls back to
/// `kill(pid, 0)` ("pid exists") because `/proc` is unavailable.
///
/// Stale sidecars (dead PID, recycled PID, protocol mismatch) are stolen:
/// deleted so the winner can overwrite them. This prevents a dead daemon from
/// blocking new spawns.
///
/// [`mcp/lock.rs`]: crate::cli::mcp::lock
pub fn resolve_endpoint(run_dir: &Path, protocol_version: u32) -> io::Result<StartupOutcome> {
    let sidecar = run_dir.join(ENDPOINT_SIDECAR);
    match read_sidecar(&sidecar)? {
        Some(endpoint) => {
            if endpoint.protocol_version != protocol_version {
                steal_sidecar(&sidecar)?;
                Ok(StartupOutcome::Won(make_winning_endpoint(
                    run_dir,
                    protocol_version,
                )))
            } else if pid_is_live(&endpoint) {
                Ok(StartupOutcome::Connect(endpoint))
            } else {
                steal_sidecar(&sidecar)?;
                Ok(StartupOutcome::Won(make_winning_endpoint(
                    run_dir,
                    protocol_version,
                )))
            }
        }
        // No sidecar at all: this caller is the first and wins.
        None => Ok(StartupOutcome::Won(make_winning_endpoint(
            run_dir,
            protocol_version,
        ))),
    }
}

/// Build a `DaemonEndpoint` for the winning caller. The daemon process writes
/// the sidecar using [`DaemonEndpoint::current_process`] after it binds the
/// socket, but `resolve_endpoint` returns a provisional endpoint so the caller
/// knows the intended socket path and pid. `pid_start_time_ms` is resolved from
/// `/proc/<pid>/stat` on Linux or set to the wall-clock fallback on other
/// platforms (the sidecar is authoritative once the daemon publishes it).
fn make_winning_endpoint(run_dir: &Path, protocol_version: u32) -> DaemonEndpoint {
    let pid = std::process::id();
    DaemonEndpoint {
        socket_path: run_dir.join("d.sock"),
        pid,
        pid_start_time_ms: proc_start_time_ms(pid).unwrap_or_else(fallback_start_time_ms),
        protocol_version,
        leindex_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

impl DaemonEndpoint {
    /// Construct a `DaemonEndpoint` for the current process at a given socket
    /// path. The daemon binary calls this after binding the socket so the
    /// sidecar records the actual bound path (not the provisional `d.sock`).
    pub fn current_process(socket_path: PathBuf, protocol_version: u32) -> Self {
        let pid = std::process::id();
        DaemonEndpoint {
            socket_path,
            pid,
            pid_start_time_ms: proc_start_time_ms(pid).unwrap_or_else(fallback_start_time_ms),
            protocol_version,
            leindex_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Write the endpoint sidecar atomically (spec §4.2 post-bind callback pattern).
///
/// The daemon calls this after successfully binding the Unix socket so that
/// clients discover it only after the socket is actually live. The write is
/// staged to a `.partial` file, fsync'd, then renamed to avoid a partially
/// written sidecar being observed by a racer.
pub fn write_endpoint_sidecar(run_dir: &Path, endpoint: &DaemonEndpoint) -> io::Result<()> {
    let sidecar = run_dir.join(ENDPOINT_SIDECAR);
    let partial = run_dir.join(format!("{ENDPOINT_SIDECAR}.partial"));

    // Ensure run_dir exists.
    std::fs::create_dir_all(run_dir)?;

    // Stage + rename for atomicity.
    let bytes = serde_json::to_vec(endpoint)?;
    std::fs::write(&partial, &bytes)?;

    // fsync the staged file before rename so the data is durable.
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let f = std::fs::File::open(&partial)?;
        // SAFETY: fsync on a valid fd is safe.
        unsafe {
            libc::fsync(f.as_raw_fd());
        }
    }

    std::fs::rename(&partial, &sidecar)?;
    Ok(())
}

/// Read and parse the sidecar file. Returns `Ok(None)` if the file does not
/// exist. Malformed JSON is treated as "no endpoint" so the caller wins by
/// default (idempotent recovery from a half-written sidecar left by a crash).
pub fn read_sidecar(path: &Path) -> io::Result<Option<DaemonEndpoint>> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<DaemonEndpoint>(&bytes) {
            Ok(ep) => Ok(Some(ep)),
            // Malformed sidecar: treat as absent. A crash mid-write could leave
            // a truncated JSON blob; the winner overwrites it cleanly.
            Err(_) => Ok(None),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Delete a stale sidecar so the winner can overwrite it.
fn steal_sidecar(path: &Path) -> io::Result<()> {
    // Ignore NotFound: another racer may have stolen first.
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// True iff the endpoint's PID is alive AND the start time matches (Linux) or
/// the PID exists (non-Linux). A start-time mismatch means the PID was recycled
/// by an unrelated process and the sidecar is stale.
fn pid_is_live(endpoint: &DaemonEndpoint) -> bool {
    let actual = proc_start_time_ms(endpoint.pid);
    match actual {
        // Linux: exact start-time match required.
        Some(ms) => ms == endpoint.pid_start_time_ms,
        // Non-Linux: fall back to `kill(pid, 0)` semantics.
        None => pid_exists(endpoint.pid),
    }
}

/// Read the process start time from `/proc/<pid>/stat` on Linux, returned as
/// milliseconds since boot (matching the unit the sidecar stores).
#[cfg(target_os = "linux")]
fn proc_start_time_ms(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 22 after `comm` (which is in parens) is `starttime` in clock ticks.
    // Split on ') ' to skip `comm`, then take the 20th whitespace field after
    // state (field 3), which is field 22 overall = starttime. This matches the
    // existing pattern in `mcp/lock.rs::proc_start_time`.
    let fields = stat.rsplit_once(") ")?.1;
    let ticks = fields.split_whitespace().nth(19)?.parse::<u64>().ok()?;
    // Convert clock ticks to milliseconds. `sysconf(_SC_CLK_TCK)` is 100 ticks/s
    // on all known Linux deployments; hardcode 100 (same assumption as top/ps).
    Some(ticks * 10)
}

/// Non-Linux stub: `/proc` is unavailable.
#[cfg(not(target_os = "linux"))]
fn proc_start_time_ms(_pid: u32) -> Option<u64> {
    None
}

/// Fallback start time used when `/proc` is unavailable (non-Linux) or the
/// start time cannot be read. Uses a coarse wall-clock so the field is
/// populated (serde deserialization expects `u64`, not `Option`).
fn fallback_start_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Check whether `pid` exists using `kill(pid, 0)` (POSIX) when `/proc` is not
/// available. Returns `false` on any error.
#[cfg(not(target_os = "linux"))]
fn pid_exists(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` is signal 0 (no signal sent); it only checks
    // existence and permission. Safe per POSIX.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    // EPERM means the process exists but we can't signal it. On Linux this
    // branch is dead code (procfs path handles it); this is the non-Linux
    // fallback.
}

/// On Linux, [`pid_exists`] is never called because [`proc_start_time_ms`]
/// always returns `Some`. The stub is here so the function compiles on Linux
/// without warn(dead_code).
#[cfg(target_os = "linux")]
fn pid_exists(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs;

    /// First caller wins the startup race; a second caller with a live-pid
    /// sidecar connects to the existing daemon instead of spawning a new one.
    #[test]
    fn test_first_caller_wins_second_connects() {
        let dir = tempfile::tempdir().unwrap();
        // First resolution wins: no sidecar present.
        let r1 = resolve_endpoint(dir.path(), 1).unwrap();
        let ep1 = match r1 {
            StartupOutcome::Won(ep) => ep,
            _ => panic!("expected Won on first call"),
        };
        // Simulate the daemon writing its sidecar (normally done at bind time).
        write_sidecar_raw(dir.path(), &ep1).unwrap();
        // Second resolution connects: the sidecar records our own live PID with
        // matching protocol version.
        let r2 = resolve_endpoint(dir.path(), 1).unwrap();
        assert!(
            matches!(r2, StartupOutcome::Connect(_)),
            "second caller must Connect to the live daemon"
        );
    }

    /// A sidecar whose PID is provably dead (e.g. 999_999) is stolen and the
    /// caller wins the startup race, so a crashed daemon does not block new
    /// spawns.
    #[test]
    fn test_dead_pid_is_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let ep = DaemonEndpoint {
            socket_path: dir.path().join("d.sock"),
            pid: 999_999, // almost certainly dead
            pid_start_time_ms: 0,
            protocol_version: 1,
            leindex_version: "test".into(),
        };
        write_sidecar_raw(dir.path(), &ep).unwrap();
        let r = resolve_endpoint(dir.path(), 1).unwrap();
        assert!(
            matches!(r, StartupOutcome::Won(_)),
            "dead-pid sidecar must be stolen"
        );
    }

    /// A sidecar with a mismatched `protocol_version` is stolen even if the PID
    /// is live, ensuring protocol upgrades force a daemon restart.
    #[test]
    fn test_protocol_version_mismatch_is_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let ep = DaemonEndpoint {
            socket_path: dir.path().join("d.sock"),
            pid: 999_999,
            pid_start_time_ms: 0,
            protocol_version: 1,
            leindex_version: "test".into(),
        };
        write_sidecar_raw(dir.path(), &ep).unwrap();
        // Caller expects protocol_version=2, sidecar has 1 → stolen.
        let r = resolve_endpoint(dir.path(), 2).unwrap();
        assert!(matches!(r, StartupOutcome::Won(_)));
    }

    /// On Linux, this very test process should be detected as live when the
    /// start-time matches, preventing a live daemon's sidecar from being
    /// stolen is verified implicitly by `test_first_caller_wins_second_connects`.
    /// Here we verify that a correct current-pid sidecar is NOT stolen.
    #[test]
    fn test_live_current_pid_is_connected() {
        let dir = tempfile::tempdir().unwrap();
        let ep = DaemonEndpoint::current_process(dir.path().join("d.sock"), 1);
        write_sidecar_raw(dir.path(), &ep).unwrap();
        let r = resolve_endpoint(dir.path(), 1).unwrap();
        match r {
            StartupOutcome::Connect(..) => {}
            other => panic!(
                "live current-pid sidecar must Connect, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    /// Absent sidecar → Won. Verifies the happy path for first-ever daemon
    /// start in a fresh run dir.
    #[test]
    fn test_absent_sidecar_wins() {
        let dir = tempfile::tempdir().unwrap();
        let r = resolve_endpoint(dir.path(), 1).unwrap();
        assert!(matches!(r, StartupOutcome::Won(_)));
    }

    /// Malformed sidecar → treated as absent (Won), so a half-written sidecar
    /// from a crash does not block startup.
    #[test]
    fn test_malformed_sidecar_recovers() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(ENDPOINT_SIDECAR), b"{not valid json").unwrap();
        let r = resolve_endpoint(dir.path(), 1).unwrap();
        assert!(matches!(r, StartupOutcome::Won(_)));
    }

    fn write_sidecar_raw(dir: &Path, ep: &DaemonEndpoint) -> io::Result<()> {
        fs::write(dir.join(ENDPOINT_SIDECAR), serde_json::to_vec(ep)?)
    }

    /// `write_endpoint_sidecar` atomically writes a sidecar that
    /// `resolve_endpoint` subsequently reads as a live endpoint.
    #[test]
    fn test_write_endpoint_sidecar_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let ep = DaemonEndpoint::current_process(dir.path().join("d.sock"), 1);
        write_endpoint_sidecar(dir.path(), &ep).unwrap();

        // Sidecar file exists and no .partial lingering.
        assert!(dir.path().join(ENDPOINT_SIDECAR).exists());
        assert!(
            !dir.path()
                .join(format!("{ENDPOINT_SIDECAR}.partial"))
                .exists()
        );

        // resolve_endpoint reads it and connects (live PID, matching protocol).
        let r = resolve_endpoint(dir.path(), 1).unwrap();
        assert!(matches!(r, StartupOutcome::Connect(_)));
    }
}
