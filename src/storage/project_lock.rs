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
}

impl ProjectWriteLock {
    /// Acquire an exclusive cross-process write lock for `storage_path`.
    /// Blocks until the lock becomes available.
    pub(crate) fn acquire(storage_path: &Path) -> Result<Self> {
        let lock_path = storage_path.join("index.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            // Lock marker file: create if missing, never clobber if present
            // (its content is irrelevant — only the fd is flocked).
            .truncate(false)
            .open(&lock_path)
            .with_context(|| {
                format!("Failed to open write-lock file at {}", lock_path.display())
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // LOCK_EX blocks until exclusive ownership is obtained. POSIX
            // guarantees release on close/exec/process-exit, so a holder that
            // crashes frees the lock automatically (no stale PID file).
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                anyhow::bail!(
                    "Failed to acquire cross-process write lock at {}: {err}",
                    lock_path.display()
                );
            }
        }
        #[cfg(windows)]
        {
            // Blocking exclusive LockFileEx. Released automatically when the
            // handle closes (process death), matching flock semantics.
            windows_lock::lock(&file, true).map_err(|err| {
                anyhow::anyhow!(
                    "Failed to acquire cross-process write lock at {}: {err}",
                    lock_path.display()
                )
            })?;
        }
        Ok(Self { _file: file })
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
        Ok(Some(Self { _file: file }))
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
