//! Advisory per-project single-instance lock for `leindex mcp` (D-3).
//!
//! Memory-pressure remediation: the swap-saturated workstation showed **8+
//! concurrent `leindex mcp` instances** for the same projects — every agent
//! session spawned its own server, each holding a ~2.4 GiB loaded engine
//! resident forever. This module writes a project-scoped lockfile
//! (`~/.leindex/run/leindex-mcp-<project-hash>.{lock,start}`) so a second
//! server for the same canonical project can *at least* know a live sibling
//! exists.
//!
//! **Advisory only — deliberately NOT a hard exit.** GrayHill flagged the
//! original D-3 design as a flaw: stdio MCP servers are 1:1 with the agent's
//! pipe, so a second instance hard-exiting 0 would break that agent's client.
//! The agreed design logs the overlap and continues; the *real* dedup lever
//! is the D-1 idle self-exit + D-2 engine eviction, which make duplicate
//! servers self-terminate and drop their engines.
//!
//! **Platform scope:** the ownership liveness check is Linux-only (it reads
//! `/proc/<pid>/stat` for process start time). On macOS/Windows the lock is a
//! documented **advisory no-op** — [`McpProjectLock::try_acquire`] returns
//! `NotAvailable` *before any file is written*, so the half-written sidecar
//! that a failed start-time write would otherwise leave can never be produced.
//! The dup-instance advisory warning therefore only fires on Linux; the real
//! dedup levers (D-1 idle self-exit, D-2 engine eviction) are platform-
//! independent.

use std::io;
use std::path::{Path, PathBuf};

/// Outcome of [`McpProjectLock::try_acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    /// This process now owns the lock (freshly written or stale-stolen).
    Acquired,
    /// A live sibling `leindex mcp` serves the same canonical project. The
    /// caller should log a warning and continue (advisory semantics).
    AlreadyOwned {
        /// PID of the live sibling instance owning the lock.
        pid: u32,
    },
    /// The lock could not be established (no home dir / write failure).
    /// Treat as advisory-no-op: never block the server on a lock problem.
    NotAvailable,
}

/// Held lock guard. Removing the sidecars on `Drop` keeps `~/.leindex/run/`
/// self-healing when a server exits cleanly (the D-7 GC covers SIGKILL debris).
#[derive(Debug)]
pub struct McpProjectLock {
    run_dir: PathBuf,
    stem: String,
}

impl McpProjectLock {
    /// Try to acquire the advisory lock for a canonical project path.
    ///
    /// Steals the lock when the owning PID is dead (start-time mismatch or
    /// missing `/proc` entry), mirroring `daemon_pid_is_owned` semantics.
    /// Never fails the server: all error paths degrade to `NotAvailable`.
    pub fn try_acquire(canonical: &Path) -> (LockOutcome, Option<McpProjectLock>) {
        let Some(home) = crate::config::resolve_leindex_home() else {
            return (LockOutcome::NotAvailable, None);
        };
        let run_dir = home.join("run");
        Self::try_acquire_in_dir(canonical, &run_dir)
    }

    /// Testable core: acquires the lock under an explicit run directory.
    ///
    /// On non-Linux platforms this is a **documented advisory no-op**: returns
    /// `NotAvailable` without touching the filesystem, so no partial sidecar is
    /// ever written. See the module doc for the rationale.
    pub fn try_acquire_in_dir(
        canonical: &Path,
        run_dir: &Path,
    ) -> (LockOutcome, Option<McpProjectLock>) {
        #[cfg(target_os = "linux")]
        {
            Self::try_acquire_in_dir_linux(canonical, run_dir)
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Advisory no-op: the liveness check that makes stale-steal safe is
            // Linux-only (/proc), so on other platforms the lock must not write
            // a sidecar it cannot later validate (a half-written sidecar would
            // silently bypass the dup-instance warning AND leave debris).
            let _ = (canonical, run_dir);
            (LockOutcome::NotAvailable, None)
        }
    }

    /// Linux implementation of [`McpProjectLock::try_acquire_in_dir`]. Kept
    /// as a separate helper so the non-Linux no-op stays a one-liner above.
    #[cfg(target_os = "linux")]
    fn try_acquire_in_dir_linux(
        canonical: &Path,
        run_dir: &Path,
    ) -> (LockOutcome, Option<McpProjectLock>) {
        let stem = lock_stem(canonical);
        if std::fs::create_dir_all(run_dir).is_err() {
            return (LockOutcome::NotAvailable, None);
        }
        let lock_path = run_dir.join(format!("{stem}.lock"));
        let start_path = run_dir.join(format!("{stem}.start"));

        // Fast path: a live sibling already owns the lock.
        if let Some(pid) = read_lock_owner(&lock_path) {
            if pid_is_owned(pid, &start_path) {
                return (LockOutcome::AlreadyOwned { pid }, None);
            }
        }

        // Atomic arbitration (Codex P2): exclusive-create the lock file so two
        // concurrent starters cannot both pass the ownership check and then
        // truncate each other's sidecars. Only one process can win `create_new`;
        // the loser re-reads ownership and reports AlreadyOwned (or steals a
        // provably-stale lock, see below).
        match create_lock_exclusive(&lock_path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let owner = read_lock_owner(&lock_path);
                match owner {
                    // A sibling won the race and is live.
                    Some(pid) if pid_is_owned(pid, &start_path) => {
                        return (LockOutcome::AlreadyOwned { pid }, None);
                    }
                    // The recorded owner is dead → the lock is stale. Remove it
                    // and retry the exclusive create exactly once.
                    Some(_) => {
                        let _ = std::fs::remove_file(&lock_path);
                        if create_lock_exclusive(&lock_path).is_err() {
                            return (LockOutcome::NotAvailable, None);
                        }
                    }
                    // Unparseable/empty file: a concurrent acquirer is mid-write.
                    // Never remove or steal it — treat as a temporary no-op.
                    None => return (LockOutcome::NotAvailable, None),
                }
            }
            Err(_) => return (LockOutcome::NotAvailable, None),
        }

        // We hold the lock file exclusively. Write pid + start-time sidecars;
        // on failure remove our own artifacts so no partial lock is left behind.
        let my_pid = std::process::id();
        if write_pid(&lock_path, my_pid).is_err() || write_start_time(&start_path, my_pid).is_err()
        {
            let _ = std::fs::remove_file(&lock_path);
            let _ = std::fs::remove_file(&start_path);
            return (LockOutcome::NotAvailable, None);
        }

        (
            LockOutcome::Acquired,
            Some(McpProjectLock {
                run_dir: run_dir.to_path_buf(),
                stem,
            }),
        )
    }

    /// Explicitly release the sidecars (also called by `Drop`).
    pub fn release(&self) {
        let lock_path = self.run_dir.join(format!("{}.lock", self.stem));
        // Only remove sidecars this guard still owns: if a new process stole
        // the lock (our pid dead) between our exit and this drop, deleting the
        // files would silence its dup-instance warning and leave it without a
        // lock record. Compare the recorded owner before removing (Codex P2).
        if read_lock_owner(&lock_path) == Some(std::process::id()) {
            let _ = std::fs::remove_file(&lock_path);
            let _ = std::fs::remove_file(self.run_dir.join(format!("{}.start", self.stem)));
        }
    }
}

impl Drop for McpProjectLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// Deterministic per-project lock stem: `leindex-mcp-<16-hex-hash>`.
///
/// Uses blake3 (an existing workspace dependency) instead of
/// `DefaultHasher`, whose SipHash algorithm is documented as **not stable
/// across Rust compiler versions** — a toolchain bump would silently change
/// every project's lock stem, leaving stale locks behind (Kilo #3) and letting
/// duplicate instances stop colliding. blake3 output is stable forever.
fn lock_stem(canonical: &Path) -> String {
    // Byte-exact (OsStr::as_encoded_bytes, 1.74+): `to_string_lossy` could let
    // two distinct non-UTF8 paths collide onto the same stem.
    let hash = blake3::hash(canonical.as_os_str().as_encoded_bytes());
    // First 8 bytes → u64 little-endian → 16 hex chars, matching the prior
    // `{:016x}` formatting of the lockfile name shape.
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    format!("leindex-mcp-{:016x}", u64::from_le_bytes(bytes))
}

/// True when `pid` is alive AND matches the `start` sidecar (a reused PID for
/// a dead process fails the start-time comparison, so stale locks are stolen).
/// Linux-only: the `/proc` start-time + cmdline reads do not exist on
/// macOS/Windows, and this function is only reachable from the Linux
/// implementation above.
#[cfg(target_os = "linux")]
fn pid_is_owned(pid: u32, start_path: &Path) -> bool {
    let expected = std::fs::read_to_string(start_path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let Some(expected) = expected else {
        return false;
    };
    let actual = proc_start_time(pid);
    if actual != Some(expected) {
        return false;
    }
    // Secondary sanity: the owning process should be a leindex mcp server.
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
    cmdline.is_some_and(|raw| {
        let command = String::from_utf8_lossy(&raw);
        command
            .split('\0')
            .any(|arg| arg.contains("leindex") || arg.contains("mcp"))
    })
}

#[cfg(target_os = "linux")]
fn proc_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat.rsplit_once(") ")?.1;
    fields.split_whitespace().nth(19)?.parse::<u64>().ok()
}

/// Create the lock file exclusively (`create_new`): fails with
/// `AlreadyExists` when another process already holds it. This is the atomic
/// arbitration primitive that makes concurrent acquisition race-free.
fn create_lock_exclusive(path: &Path) -> io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
}

/// Read the PID recorded in a lock file, if parseable.
fn read_lock_owner(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
}

fn write_pid(path: &Path, pid: u32) -> io::Result<()> {
    std::fs::write(path, format!("{pid}\n"))
}

#[cfg(target_os = "linux")]
fn write_start_time(path: &Path, pid: u32) -> io::Result<()> {
    let start = proc_start_time(pid)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no /proc stat for pid"))?;
    std::fs::write(path, format!("{start}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_run_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn test_lock_stem_is_deterministic_and_stable() {
        // Same canonical path → same stem every call (blake3, not DefaultHasher).
        let a = lock_stem(Path::new("/tmp/leindex/proj-alpha"));
        let b = lock_stem(Path::new("/tmp/leindex/proj-alpha"));
        assert_eq!(a, b);
        assert!(a.starts_with("leindex-mcp-"));
        // Different projects → different stems.
        let other = lock_stem(Path::new("/tmp/leindex/proj-beta"));
        assert_ne!(a, other);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_acquire_writes_and_releases_sidecars() {
        let dir = temp_run_dir();
        let canonical = Path::new("/tmp/proj-a");
        let (outcome, guard) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        assert_eq!(outcome, LockOutcome::Acquired);
        let guard = guard.expect("guard");
        let lock_path = dir.path().join(format!("{}.lock", lock_stem(canonical)));
        assert!(lock_path.exists());
        // Dropping the guard removes the sidecars.
        drop(guard);
        assert!(!lock_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_second_live_instance_reports_owned() {
        let dir = temp_run_dir();
        let canonical = Path::new("/tmp/proj-b");
        let (_o1, guard1) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        assert!(guard1.is_some());

        // Second acquire from the same live process (this test binary) must
        // report AlreadyOwned with our own pid.
        let (outcome, guard2) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        assert_eq!(
            outcome,
            LockOutcome::AlreadyOwned {
                pid: std::process::id()
            }
        );
        assert!(guard2.is_none());
        // After the first guard drops, a fresh acquire succeeds again.
        drop(guard1);
        let (outcome, _guard3) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        assert_eq!(outcome, LockOutcome::Acquired);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_unparseable_lock_file_is_not_stolen() {
        // Codex P2: an empty/mid-write lock file must never be removed+stolen —
        // that would let a concurrent acquirer lose its lock mid-write. The
        // acquirer must degrade to NotAvailable and leave the file intact.
        let dir = temp_run_dir();
        let canonical = Path::new("/tmp/proj-empty-lock");
        let stem = lock_stem(canonical);
        std::fs::write(dir.path().join(format!("{stem}.lock")), b"").unwrap();

        let (outcome, guard) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        assert_eq!(outcome, LockOutcome::NotAvailable);
        assert!(guard.is_none());
        assert!(
            dir.path().join(format!("{stem}.lock")).exists(),
            "unparseable lock file must be left intact"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_release_only_removes_owned_sidecars() {
        // Codex P2: a guard must not delete sidecars it no longer owns (a newer
        // process may have stolen the lock after this one exited).
        let dir = temp_run_dir();
        let canonical = Path::new("/tmp/proj-stolen-lock");
        let (_outcome, guard) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        let guard = guard.expect("guard");
        let stem = lock_stem(canonical);
        // Simulate a stale-steal: overwrite the lock with a different owner
        // before dropping the guard.
        std::fs::write(dir.path().join(format!("{stem}.lock")), "12345\n").unwrap();
        drop(guard);
        // release() saw a foreign owner → left the sidecars alone.
        assert!(
            dir.path().join(format!("{stem}.lock")).exists(),
            "foreign-owned sidecars must survive a guard drop"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_stale_lock_with_dead_pid_is_stolen() {
        let dir = temp_run_dir();
        let canonical = Path::new("/tmp/proj-c");
        let stem = lock_stem(canonical);
        // Dead PID sidecars (pid 2^22 will not exist as this process).
        let dead_pid = 1 << 22;
        std::fs::write(
            dir.path().join(format!("{stem}.lock")),
            format!("{dead_pid}\n"),
        )
        .unwrap();
        std::fs::write(dir.path().join(format!("{stem}.start")), "12345\n").unwrap();

        let (outcome, guard) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        assert_eq!(outcome, LockOutcome::Acquired, "stale lock must be stolen");
        assert!(guard.is_some());
        // The stolen lock now records our own pid.
        let lock_contents =
            std::fs::read_to_string(dir.path().join(format!("{stem}.lock"))).unwrap();
        assert_eq!(
            lock_contents.trim().parse::<u32>().unwrap(),
            std::process::id()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_different_projects_coexist() {
        let dir = temp_run_dir();
        let (o1, g1) = McpProjectLock::try_acquire_in_dir(Path::new("/tmp/proj-d1"), dir.path());
        let (o2, g2) = McpProjectLock::try_acquire_in_dir(Path::new("/tmp/proj-d2"), dir.path());
        assert_eq!(o1, LockOutcome::Acquired);
        assert_eq!(o2, LockOutcome::Acquired);
        assert!(g1.is_some());
        assert!(g2.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_pid_is_owned_self() {
        // Our own process + our own start-time must be reported as owned.
        let dir = temp_run_dir();
        let start_path = dir.path().join("self.start");
        write_start_time(&start_path, std::process::id()).unwrap();
        assert!(pid_is_owned(std::process::id(), &start_path));
        // A wrong start-time (bogus pid) must be reported as not owned.
        assert!(!pid_is_owned(
            std::process::id(),
            &dir.path().join("missing.start")
        ));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_non_linux_acquire_is_documented_noop() {
        // Non-Linux: advisory no-op. Must return NotAvailable and write NO
        // sidecars (no half-written lock file), per the module doc contract.
        let dir = temp_run_dir();
        let canonical = Path::new("/tmp/proj-nonlinux");
        let (outcome, guard) = McpProjectLock::try_acquire_in_dir(canonical, dir.path());
        assert_eq!(outcome, LockOutcome::NotAvailable);
        assert!(guard.is_none());
        let stem = lock_stem(canonical);
        assert!(!dir.path().join(format!("{stem}.lock")).exists());
        assert!(!dir.path().join(format!("{stem}.start")).exists());
    }
}
