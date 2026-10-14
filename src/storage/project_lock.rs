//! Cross-process exclusive lock guarding writes to a project's storage.
//!
//! Lives in `storage` (not `cli`) because every writer of project storage
//! must serialize — the indexer, the watcher, storage cleanup, and phase
//! analysis's generation publication — and `phase` builds without `cli`.

use anyhow::{Context, Result};
use std::path::Path;

/// Cross-process exclusive lock guarding writes to a project's storage.
///
/// SQLite WAL permits exactly **one writer**. When two leindex processes write
/// the same `leindex.db` at once (a second MCP instance, or MCP + CLI), they
/// contend on the database lock, exhaust the open-retry budget, and can corrupt
/// the WAL — the failure mode that bricks a generation. This advisory
/// `flock(2)` serializes writers across processes: a second writer blocks until
/// the holder drops the guard. `flock` is released automatically on process
/// death (close of the underlying fd), so a crash can never leave a stale lock.
///
/// Readers (search/load) do **not** take this lock, so concurrent reads stay
/// fast and uncontended. Held for the lifetime of a single write operation
/// (`index_project_inner` / `incremental_reindex_from_watcher` / the phase
/// generation publish) via RAII.
pub(crate) struct ProjectWriteLock {
    _file: std::fs::File,
    /// Identity captured at acquire time; powers [`Self::still_guards`].
    identity: LockIdentity,
}

/// The (device, inode) identity of the lock file a guard flocked, kept so a
/// long-running writer can detect that its lock file was **deleted or
/// replaced** while it held the lock.
///
/// `flock(2)` locks the open file description, not the path: after someone
/// unlinks `index.lock`, the holder still guards the *original* inode, while
/// any new writer that re-creates the file flocks a *different* inode and
/// works beside it without conflict. Two processes then write the same SQLite
/// store — the exact WAL-corruption failure that bricks a generation.
/// [`Self::still_guards`] compares the path's current identity against the
/// captured one so the writer can refuse to publish before doing damage.
#[derive(Clone, Debug)]
pub(crate) struct LockIdentity {
    lock_path: std::path::PathBuf,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl LockIdentity {
    /// Capture the identity of a successfully flocked descriptor, falling
    /// back to the path's identity when `fstat(2)` fails (best-effort: an
    /// all-zero pair makes [`Self::still_guards`] always pass).
    fn capture(file: &std::fs::File, lock_path: &Path) -> Self {
        #[cfg(unix)]
        {
            let (dev, ino) = descriptor_identity(file)
                .or_else(|| path_identity(lock_path))
                .unwrap_or((0, 0));
            Self {
                lock_path: lock_path.to_path_buf(),
                dev,
                ino,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = file;
            Self {
                lock_path: lock_path.to_path_buf(),
            }
        }
    }

    /// The path of the lock file this identity was captured from.
    pub(crate) fn lock_file_path(&self) -> &std::path::Path {
        &self.lock_path
    }

    /// Whether the lock file at [`Self::lock_path`] is still the same inode
    /// this identity was captured from. `false` means the file was deleted or
    /// replaced while a writer held it, so a second concurrent writer may
    /// exist; the caller must not publish to the store.
    ///
    /// Best-effort by design: an identity-capture failure (all-zero pair)
    /// always answers `true` — never block a writer on a diagnostic failure,
    /// only on the provable unlink/recreate race.
    pub(crate) fn still_guards(&self) -> bool {
        #[cfg(unix)]
        {
            if self.dev == 0 && self.ino == 0 {
                return true;
            }
            std::fs::metadata(&self.lock_path)
                .map(|meta| {
                    use std::os::unix::fs::MetadataExt;
                    meta.dev() == self.dev && meta.ino() == self.ino
                })
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            // Windows LockFileEx locks a byte range of the open handle; the
            // same unlink-then-recreate race exists, but querying the file
            // index needs unstable APIs. Conservative default: assume intact.
            true
        }
    }
}

/// Capture the (dev, ino) of the open lock descriptor via `fstat(2)`.
#[cfg(unix)]
fn descriptor_identity(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::fd::AsRawFd;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fstat` on a descriptor we own; writes into a zeroed `stat`.
    if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } != 0 {
        return None;
    }
    Some((stat.st_dev as u64, stat.st_ino as u64))
}

/// Capture the (dev, ino) of the lock path via `stat(2)`.
#[cfg(unix)]
fn path_identity(lock_path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(lock_path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// Environment variable overriding the bounded-acquire timeout (milliseconds).
/// `0` disables the bound (legacy unbounded blocking behaviour).
const WRITE_LOCK_TIMEOUT_ENV: &str = "LEINDEX_WRITE_LOCK_TIMEOUT_MS";

/// Default bound on `acquire_bounded` and the coalescing poll: five minutes.
/// The lock is held for a whole index run, so this must exceed a legitimate
/// large-project index — but it converts "wedged holder parks every tool call
/// forever" into a transient, retryable failure.
const DEFAULT_WRITE_LOCK_TIMEOUT_MS: u64 = 300_000;

/// Test-only override for the bounded-acquire timeout (avoids racing the
/// process-wide environment against parallel tests).
#[cfg(test)]
static TEST_TIMEOUT_OVERRIDE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The configured acquire timeout in milliseconds (`0` = unbounded).
pub(crate) fn configured_timeout_ms() -> u64 {
    #[cfg(test)]
    {
        let override_ms = TEST_TIMEOUT_OVERRIDE.load(std::sync::atomic::Ordering::Acquire);
        if override_ms != 0 {
            return override_ms;
        }
    }
    std::env::var(WRITE_LOCK_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_WRITE_LOCK_TIMEOUT_MS)
}

impl ProjectWriteLock {
    /// Bounded variant of the legacy blocking acquire: polls the cheap
    /// probe until the lock frees or the configured timeout elapses
    /// ([`WRITE_LOCK_TIMEOUT_ENV`]), then fails fast with a transient,
    /// retryable error naming the lock path and the wait budget.
    ///
    /// Tool-reachable index paths run inside uncancellable `spawn_blocking`
    /// tasks; the legacy blocking acquire parked those (and the async workers
    /// they occupy) forever behind a wedged lock holder. The timeout error is
    /// tagged `[transient:lock-contention]` so the registry's failure
    /// classifier does not brick the generation (see
    /// `is_transient_storage_open_failure`).
    pub(crate) fn acquire_bounded(storage_path: &Path) -> Result<Self> {
        let timeout_ms = configured_timeout_ms();
        let started = std::time::Instant::now();
        loop {
            match Self::try_acquire(storage_path)? {
                Some(guard) => return Ok(guard),
                None => {
                    if timeout_ms > 0 && started.elapsed().as_millis() as u64 >= timeout_ms {
                        anyhow::bail!(
                            "[transient:lock-contention] Timed out after {}ms waiting for the \
                             cross-process write lock at {} (another leindex writer is active). \
                             The index data is intact; retry shortly, or set {} to adjust the \
                             wait budget.",
                            timeout_ms,
                            storage_path.join("index.lock").display(),
                            WRITE_LOCK_TIMEOUT_ENV
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }

    /// Non-blocking variant: returns `Ok(Some(guard))` if the lock was free,
    /// `Ok(None)` if another process holds it. Used by the watcher (skip a
    /// reindex when another process is already writing) and by the
    /// mutual-exclusion self-check.
    pub(crate) fn try_acquire(storage_path: &Path) -> Result<Option<Self>> {
        let lock_path = storage_path.join("index.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| {
                format!("Failed to open write-lock file at {}", lock_path.display())
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                // EWOULDBLOCK / EAGAIN = locked by someone else (expected).
                // Compare by value (not pattern): on Linux these two errno
                // constants are identical, which would make an alternation
                // pattern unreachable.
                let raw = err.raw_os_error();
                if raw == Some(libc::EWOULDBLOCK) || raw == Some(libc::EAGAIN) {
                    return Ok(None);
                }
                anyhow::bail!(
                    "Failed to probe write lock at {}: {err}",
                    lock_path.display()
                );
            }
        }
        #[cfg(windows)]
        {
            // LOCKFILE_FAIL_IMMEDIATELY: returns ERROR_LOCK_VIOLATION (33) or
            // ERROR_SHARING_VIOLATION (32) if another process holds it.
            match windows_lock::lock(&file, false) {
                Ok(()) => {}
                Err(err)
                    if matches!(
                        err.raw_os_error(),
                        Some(33) | Some(32) // LOCK_VIOLATION | SHARING_VIOLATION
                    ) =>
                {
                    return Ok(None);
                }
                Err(err) => {
                    anyhow::bail!(
                        "Failed to probe write lock at {}: {err}",
                        lock_path.display()
                    );
                }
            }
        }
        let identity = LockIdentity::capture(&file, &lock_path);
        Ok(Some(Self {
            _file: file,
            identity,
        }))
    }

    /// Whether the lock file this guard flocked is still the live `index.lock`
    /// on disk. A `false` result means someone deleted or replaced the lock
    /// file while this writer held it — a second writer may now be active, so
    /// publishing would corrupt the store. See [`LockIdentity`].
    pub(crate) fn still_guards(&self) -> bool {
        self.identity.still_guards()
    }

    /// The captured identity of this guard's lock file, for later
    /// [`LockIdentity::still_guards`] checks at publish choke points.
    pub(crate) fn identity(&self) -> LockIdentity {
        self.identity.clone()
    }
}

impl Drop for ProjectWriteLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // Explicit unlock for determinism (the File drop also closes the fd,
            // which releases flock, but be explicit about intent).
            let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            windows_lock::unlock(&self._file);
        }
    }
}

/// Windows cross-process file locking via `LockFileEx`/`UnlockFileEx`
/// (kernel32), used by `ProjectWriteLock` on the Windows release target
/// (see AGENTS.md: builds Linux/macOS/Windows). No extra crate — raw FFI.
/// Locks byte `[0, 1)` exclusively; a second exclusive lock on the same byte
/// blocks (blocking) or fails immediately (try), providing mutual exclusion.
/// The handle close on `File` drop releases the lock, matching `flock`.
#[cfg(windows)]
mod windows_lock {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    #[derive(Default)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset_low: u32,
        offset_high: u32,
        event: usize,
    }

    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;

    unsafe extern "system" {
        fn LockFileEx(
            handle: usize,
            flags: u32,
            reserved: u32,
            len_low: u32,
            len_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            handle: usize,
            reserved: u32,
            len_low: u32,
            len_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    /// Lock byte `[0, 1)` exclusively. `blocking = false` adds
    /// `LOCKFILE_FAIL_IMMEDIATELY`.
    pub(super) fn lock(file: &File, blocking: bool) -> std::io::Result<()> {
        let handle = file.as_raw_handle() as usize;
        let mut overlapped = Overlapped::default();
        let mut flags = LOCKFILE_EXCLUSIVE_LOCK;
        if !blocking {
            flags |= LOCKFILE_FAIL_IMMEDIATELY;
        }
        // SAFETY: FFI to kernel32 `LockFileEx` with a valid file handle and a
        // valid `Overlapped` pointer. Byte range [0,1).
        let ok = unsafe { LockFileEx(handle, flags, 0, 1, 0, &mut overlapped) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Release the byte `[0, 1)` lock. Best-effort — the handle close on drop
    /// also releases it.
    pub(super) fn unlock(file: &File) {
        let handle = file.as_raw_handle() as usize;
        let mut overlapped = Overlapped::default();
        let _ = unsafe { UnlockFileEx(handle, 0, 1, 0, &mut overlapped) };
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// T1 (RC1): a wedged lock holder must produce a bounded, transient,
    /// sentinel-tagged failure — not an infinite park. Pre-fix, the
    /// tool-reachable blocking acquire looped forever and this test would
    /// time out on the channel receive instead of seeing the error.
    #[test]
    fn test_acquire_bounded_times_out_behind_wedged_holder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = dir.path();

        // Wedge the lock with a holder in this process (flock is per
        // open-file-description, so a second open in the same process
        // still conflicts).
        let holder = ProjectWriteLock::try_acquire(storage)
            .expect("first acquire")
            .expect("lock free");

        TEST_TIMEOUT_OVERRIDE.store(300, std::sync::atomic::Ordering::Release);
        let (tx, rx) = std::sync::mpsc::channel();
        let storage_owned = storage.to_path_buf();
        let waiter = std::thread::spawn(move || {
            let result = ProjectWriteLock::acquire_bounded(&storage_owned);
            let _ = tx.send(result.map(|_| ()).map_err(|e| e.to_string()));
        });

        let outcome = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("acquire_bounded must terminate within the timeout");
        TEST_TIMEOUT_OVERRIDE.store(0, std::sync::atomic::Ordering::Release);
        let message = outcome.expect_err("lock is held; must time out");
        assert!(
            message.contains("[transient:lock-contention]"),
            "timeout error must carry the transient sentinel so the registry \
             does not brick the generation; got: {message}"
        );
        assert!(
            message.contains("index.lock"),
            "timeout error must name the lock path for remediation; got: {message}"
        );
        waiter.join().expect("waiter thread");

        // After the holder releases, a bounded acquire succeeds again.
        drop(holder);
        TEST_TIMEOUT_OVERRIDE.store(2_000, std::sync::atomic::Ordering::Release);
        let reacquired = ProjectWriteLock::acquire_bounded(storage);
        TEST_TIMEOUT_OVERRIDE.store(0, std::sync::atomic::Ordering::Release);
        reacquired.expect("lock freed; bounded acquire must succeed");
    }

    /// T5 (RC5a): `still_guards` must detect that the lock file was deleted
    /// or replaced while held — the condition that forks a second writer.
    /// Pre-fix there was no identity check at all, so a deleted lock file
    /// silently allowed two concurrent writers onto one SQLite store.
    #[test]
    fn test_still_guards_detects_lock_file_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = dir.path();

        let guard = ProjectWriteLock::try_acquire(storage)
            .expect("acquire")
            .expect("lock free");
        assert!(guard.still_guards(), "freshly held lock must guard");

        // Delete the lock file out from under the holder: the guard still
        // holds the original inode, but the path no longer matches.
        std::fs::remove_file(storage.join("index.lock")).expect("remove lock file");
        assert!(
            !guard.still_guards(),
            "deleted lock file must invalidate the guard"
        );

        // Re-create the path: a *different* inode lives there now; a second
        // writer could flock it beside us. The guard must still refuse.
        std::fs::write(storage.join("index.lock"), b"").expect("recreate lock file");
        assert!(
            !guard.still_guards(),
            "re-created lock file is a different inode; guard must be invalidated"
        );
    }
}
