//! CAS blob refcount store: persistence backend behind the [`RefcountStore`]
//! trait.
//!
//! Refcounts are mutated in-memory by [`RefcountStore::incr`] /
//! [`RefcountStore::decr`] and flushed to disk by
//! [`RefcountStore::persist`]. This mirrors the write-barrier semantics
//! required by VAL-CAS-016 (kill-before-fsync may lose increments but never
//! produces phantom counts).
//!
//! WS4 Task 11 benchmarked two backends — JSON sidecar (`cas/refs.json`) and
//! SQLite (`cas/refs.db`) — and the measured decision (JSON sidecar wins)
//! is digested into BENCHMARKS.md Section 9 "CAS engineering decisions":
//!
//! | Metric (10k-blob fixture) | JSON sidecar | SQLite | Verdict |
//! |---|---|---|---|
//! | incr ×10k | 177.3 µs | 176.7 µs | tie |
//! | decr ×10k | 347.5 µs | 348.3 µs | tie |
//! | persist (fsync) ×10k | 12.1 ms | 28.6 ms | JSON 2.36× faster |
//! | reopen ×10k | 3.17 ms | 2.88 ms | SQLite 10% faster |
//! | resident-memory delta | 2,664 KiB | 4,140 KiB | JSON 36% lower |
//! | on-disk footprint | 730 KB | 1,581 KB | JSON 2.2× smaller |
//! | crash-recovery (persist → reopen exact; no-persist → no phantom) | PASS | PASS | tie |
//!
//! **DECISION: JSON sidecar is the winner and the default.** It matches SQLite
//! on in-memory incr/decr throughput, dominates the durable-write path
//! (`persist` is the hot path — it is the write barrier for every lease
//! acquire/release and every GC sweep), uses ~36% less resident memory, and
//! writes ~2.2× less data to disk. SQLite's only edge is a ~10% faster cold
//! reopen, which happens once per process start. The SQLite implementation
//! was deleted after the decision; this module carries only the winner behind
//! the trait.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::blob::{fsync_file, hash_to_hex, hex_to_hash};

/// JSON sidecar filename written into the CAS root.
pub const REFS_SIDECAR: &str = "refs.json";
/// Advisory-lock file serialising sidecar read-modify-write across handles
/// and processes.
pub const REFS_LOCK: &str = "refs.lock";
/// Directory of per-owner ledgers recording which counts each live handle
/// contributed, so counts left behind by a crashed owner can be reclaimed.
pub const REFS_OWNERS_DIR: &str = "refs.owners";
/// On-disk auxiliary files the CAS blob-count / stored-hash walkers skip.
pub const REFS_AUX_FILES: &[&str] = &[REFS_SIDECAR, REFS_LOCK, REFS_OWNERS_DIR];

/// Monotonic sequence that guarantees a unique temp-sidecar path per
/// `persist` call in this process, so concurrent readers can each atomically
/// swap the refcount sidecar without racing on a shared temp filename.
static PERSIST_SEQ: AtomicU64 = AtomicU64::new(0);

/// Monotonic sequence giving every store handle in this process a distinct
/// owner ledger, so overlapping handles never share (or clobber) a ledger.
static OWNER_SEQ: AtomicU64 = AtomicU64::new(0);

/// Interchangeable refcount persistence backend.
pub trait RefcountStore: Send {
    /// Open (or create) a backend rooted at `cas_root`.
    fn open(cas_root: &Path) -> Result<Self>
    where
        Self: Sized;

    /// Increment the refcount for `hash` and return the new value.
    fn incr(&mut self, hash: &[u8; 32]) -> u64;

    /// Decrement the refcount for `hash` and return the new value, or
    /// [`CasError::RefcountUnderflow`] if the count is already zero.
    fn decr(&mut self, hash: &[u8; 32]) -> Result<u64>;

    /// Current refcount for `hash` (0 if absent).
    fn refcount(&self, hash: &[u8; 32]) -> u64;

    /// Returns `true` if `hash` is tracked by this store with any refcount.
    fn contains(&self, hash: &[u8; 32]) -> bool;

    /// Remove the refcount entry for `hash` (used after GC deletes the blob).
    fn remove(&mut self, hash: &[u8; 32]);

    /// All `(hash, refcount)` pairs currently tracked.
    fn iter(&self) -> Vec<([u8; 32], u64)>;

    /// Hashes whose refcount is exactly zero.
    fn zero_refcount_hashes(&self) -> Vec<[u8; 32]>;

    /// The set of all hashes tracked by this store.
    fn tracked_hashes(&self) -> HashSet<[u8; 32]>;

    /// Durably flush the current refcounts to disk (write barrier).
    fn persist(&self) -> Result<()>;

    /// Refresh this handle's view from disk so counts contributed by other
    /// handles and processes (live leases) are visible, keeping this handle's
    /// own not-yet-persisted changes on top. Callers about to make a
    /// destructive decision (retention, GC) reload first.
    fn reload(&self) -> Result<()>;
}

/// Refcount storage backed by an in-memory map and a JSON sidecar.
///
/// Winner of the WS4 Task 11 sidecar-vs-SQLite decision; see module docs.
///
/// # Concurrency
///
/// Every `CasStore::open` creates an independent handle, and generation leases
/// open one per reader, so many handles (across threads and processes) share
/// one `refs.json`. Two rules keep that safe:
///
/// * **Merge, don't overwrite.** A handle records only its own *deltas*
///   (`incr`/`decr`/`remove`). [`persist`](RefcountStore::persist) takes the
///   `refs.lock` advisory lock, re-reads the sidecar, applies the deltas on
///   top, and writes the result, so one handle's flush can never erase
///   another's increments (the previous whole-map replacement lost updates
///   between overlapping leases).
/// * **Own what you add.** Each handle also records the net counts it holds in
///   a per-owner ledger (`refs.owners/<pid>.<start>.<seq>.json`). When a
///   process dies holding counts, the next handle to open or persist sees that
///   the owner is gone (pid absent, or its start time no longer matches) and
///   subtracts the dead owner's holdings, instead of leaving the blobs and
///   generations pinned forever. Owners are assumed to share a PID namespace;
///   on platforms with no liveness probe nothing is reclaimed.
pub struct JsonSidecarStore {
    state: Mutex<State>,
    sidecar_path: PathBuf,
    lock_path: PathBuf,
    owners_dir: PathBuf,
    /// Ledger filename for this handle.
    owner_file: String,
}

/// Mutable per-handle state, behind a lock so `&self` methods can refresh it.
#[derive(Default)]
struct State {
    /// This handle's view: last synchronised sidecar contents with the pending
    /// `delta` applied on top.
    counts: HashMap<[u8; 32], u64>,
    /// Changes made through this handle since the last successful persist. A
    /// zero entry still records that the hash was touched, so it is written
    /// explicitly (a persisted zero means "tracked, no references").
    delta: HashMap<[u8; 32], i64>,
    /// Hashes whose entry should be dropped once their count is zero.
    removed: HashSet<[u8; 32]>,
    /// Net counts this handle has persisted so far; mirrors its ledger.
    held: HashMap<[u8; 32], u64>,
}

impl State {
    fn lock(mutex: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Parse a sidecar/ledger body (hex → count), tolerating corruption.
fn parse_counts(data: &[u8]) -> HashMap<[u8; 32], u64> {
    let map: HashMap<String, u64> = serde_json::from_slice(data).unwrap_or_default();
    map.into_iter()
        .filter_map(|(hex, count)| hex_to_hash(&hex).map(|h| (h, count)))
        .collect()
}

/// Read a sidecar/ledger file; absent or unreadable means empty.
fn read_counts(path: &Path) -> HashMap<[u8; 32], u64> {
    fs::read(path)
        .map(|data| parse_counts(&data))
        .unwrap_or_default()
}

/// Apply a signed `delta` to `count`, clamping at zero.
fn apply_delta(count: u64, delta: i64) -> u64 {
    if delta >= 0 {
        count.saturating_add(delta as u64)
    } else {
        count.saturating_sub(delta.unsigned_abs())
    }
}

/// Atomically replace `path` with `counts` serialised as hex → count.
fn write_counts_atomic(path: &Path, counts: &HashMap<[u8; 32], u64>) -> Result<()> {
    // Serialize hex → count directly to avoid any lifetime pitfalls.
    let owned: HashMap<String, u64> = counts.iter().map(|(k, v)| (hash_to_hex(k), *v)).collect();
    let data = serde_json::to_vec_pretty(&owned).map_err(CasError::Serde)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CasError::Io)?;
    }
    let seq = PERSIST_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), seq));
    {
        let file = fs::File::create(&tmp_path).map_err(CasError::Io)?;
        let mut writer = std::io::BufWriter::new(file);
        writer
            .write_all(&data)
            .and_then(|_| writer.flush())
            .map_err(CasError::Io)?;
        fsync_file(writer.get_ref()).map_err(CasError::Io)?;
    }
    fs::rename(&tmp_path, path).map_err(CasError::Io)?;

    // Best-effort dir fsync.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = fsync_file(&dir);
        }
    }
    Ok(())
}

/// Exclusive advisory lock on `refs.lock`, released when dropped.
struct RefsLock {
    _file: fs::File,
}

impl RefsLock {
    fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(CasError::Io)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(CasError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                // SAFETY: `flock` on a valid, owned fd; the lock is released
                // when the fd closes on drop.
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
                if rc == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(CasError::Io(error));
                }
            }
        }
        Ok(RefsLock { _file: file })
    }
}

/// Process start time in clock ticks (`/proc/<pid>/stat` field 22), the value
/// that distinguishes a live owner from an unrelated process that reused its
/// pid. `None` where `/proc` is unavailable.
#[cfg(target_os = "linux")]
fn process_start_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(") ")?.1;
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn process_start_ticks(_pid: u32) -> Option<u64> {
    None
}

/// Whether the owner `pid` started at `start_ticks` is still running.
fn owner_is_alive(pid: u32, start_ticks: u64) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        // A missing /proc entry means the process is gone; a different start
        // time means the pid was recycled by an unrelated process.
        process_start_ticks(pid) == Some(start_ticks)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let _ = start_ticks;
        // SAFETY: signal 0 only probes for existence.
        let rc = unsafe { libc::kill(pid as i32, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        // No liveness probe: never reclaim (a leaked pin beats a freed live one).
        let _ = (pid, start_ticks);
        true
    }
}

/// Parse `<pid>.<start_ticks>.<seq>.json` into `(pid, start_ticks)`.
fn parse_owner_file(name: &str) -> Option<(u32, u64)> {
    let mut parts = name.strip_suffix(".json")?.split('.');
    let pid = parts.next()?.parse().ok()?;
    let start = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some((pid, start))
}

impl JsonSidecarStore {
    /// Ledger files whose owner is no longer running.
    fn dead_owner_ledgers(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.owners_dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == self.owner_file {
                    return None;
                }
                let (pid, start) = parse_owner_file(&name)?;
                (!owner_is_alive(pid, start)).then(|| entry.path())
            })
            .collect()
    }

    /// Subtract every dead owner's holdings from `counts` and delete their
    /// ledgers. Returns whether any count changed. Caller holds the lock.
    fn reclaim_dead_owners(&self, counts: &mut HashMap<[u8; 32], u64>) -> bool {
        let mut changed = false;
        for ledger in self.dead_owner_ledgers() {
            for (hash, held) in read_counts(&ledger) {
                if let Some(count) = counts.get_mut(&hash) {
                    let reduced = count.saturating_sub(held);
                    changed |= reduced != *count;
                    *count = reduced;
                }
            }
            let _ = fs::remove_file(&ledger);
        }
        changed
    }

    /// Reclaim abandoned counts from crashed owners, if there are any.
    fn reclaim_if_needed(&self) -> Result<()> {
        if self.dead_owner_ledgers().is_empty() {
            return Ok(());
        }
        let _lock = RefsLock::acquire(&self.lock_path)?;
        let mut counts = read_counts(&self.sidecar_path);
        if self.reclaim_dead_owners(&mut counts) {
            write_counts_atomic(&self.sidecar_path, &counts)?;
        }
        Ok(())
    }

    /// Rewrite (or remove, when it holds nothing) this handle's ledger.
    fn write_ledger(&self, held: &HashMap<[u8; 32], u64>) -> Result<()> {
        let path = self.owners_dir.join(&self.owner_file);
        let positive: HashMap<[u8; 32], u64> = held
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(hash, count)| (*hash, *count))
            .collect();
        if positive.is_empty() {
            return match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(CasError::Io(e)),
            };
        }
        write_counts_atomic(&path, &positive)
    }
}

impl RefcountStore for JsonSidecarStore {
    /// Open (or create) the sidecar at `<cas_root>/refs.json`.
    ///
    /// If the file does not exist yet the store starts empty. A corrupt file
    /// is treated as empty (start fresh), matching the crash-tolerant
    /// VAL-CAS-016 semantics. Counts abandoned by crashed owners are
    /// reclaimed first, so a restart never inherits phantom leases.
    fn open(cas_root: &Path) -> Result<Self> {
        let pid = std::process::id();
        let start = process_start_ticks(pid).unwrap_or(0);
        let seq = OWNER_SEQ.fetch_add(1, Ordering::Relaxed);
        let store = JsonSidecarStore {
            state: Mutex::new(State::default()),
            sidecar_path: cas_root.join(REFS_SIDECAR),
            lock_path: cas_root.join(REFS_LOCK),
            owners_dir: cas_root.join(REFS_OWNERS_DIR),
            owner_file: format!("{pid}.{start}.{seq}.json"),
        };
        // Best effort: a read-only store must still open.
        let _ = store.reclaim_if_needed();
        State::lock(&store.state).counts = read_counts(&store.sidecar_path);
        Ok(store)
    }

    fn incr(&mut self, hash: &[u8; 32]) -> u64 {
        let state = self.state.get_mut().unwrap_or_else(|p| p.into_inner());
        *state.delta.entry(*hash).or_insert(0) += 1;
        state.removed.remove(hash);
        let entry = state.counts.entry(*hash).or_insert(0);
        *entry += 1;
        *entry
    }

    fn decr(&mut self, hash: &[u8; 32]) -> Result<u64> {
        let state = self.state.get_mut().unwrap_or_else(|p| p.into_inner());
        let entry = state.counts.entry(*hash).or_insert(0);
        if *entry == 0 {
            return Err(CasError::RefcountUnderflow);
        }
        *entry -= 1;
        let remaining = *entry;
        *state.delta.entry(*hash).or_insert(0) -= 1;
        Ok(remaining)
    }

    fn refcount(&self, hash: &[u8; 32]) -> u64 {
        State::lock(&self.state)
            .counts
            .get(hash)
            .copied()
            .unwrap_or(0)
    }

    fn contains(&self, hash: &[u8; 32]) -> bool {
        State::lock(&self.state).counts.contains_key(hash)
    }

    fn remove(&mut self, hash: &[u8; 32]) {
        let state = self.state.get_mut().unwrap_or_else(|p| p.into_inner());
        state.counts.remove(hash);
        state.delta.remove(hash);
        state.removed.insert(*hash);
    }

    fn iter(&self) -> Vec<([u8; 32], u64)> {
        State::lock(&self.state)
            .counts
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect()
    }

    fn zero_refcount_hashes(&self) -> Vec<[u8; 32]> {
        State::lock(&self.state)
            .counts
            .iter()
            .filter(|(_, v)| **v == 0)
            .map(|(k, _)| *k)
            .collect()
    }

    fn tracked_hashes(&self) -> HashSet<[u8; 32]> {
        State::lock(&self.state).counts.keys().copied().collect()
    }

    /// Merge this handle's changes into the shared sidecar atomically.
    ///
    /// Under the `refs.lock` advisory lock: re-read the sidecar, reclaim
    /// counts held by dead owners, apply this handle's deltas, then write a
    /// uniquely-named temp sidecar, fsync, and rename it into place. The lock
    /// serialises overlapping handles (threads or processes) so no update is
    /// lost; the unique temp name keeps a crash from leaving a torn sidecar.
    fn persist(&self) -> Result<()> {
        let _lock = RefsLock::acquire(&self.lock_path)?;
        let mut state = State::lock(&self.state);

        let mut merged = read_counts(&self.sidecar_path);
        self.reclaim_dead_owners(&mut merged);

        let State {
            delta,
            removed,
            held,
            ..
        } = &mut *state;
        for (hash, change) in delta.iter() {
            let entry = merged.entry(*hash).or_insert(0);
            *entry = apply_delta(*entry, *change);
            let own = held.entry(*hash).or_insert(0);
            *own = apply_delta(*own, *change);
        }
        for hash in removed.iter() {
            if merged.get(hash).is_none_or(|count| *count == 0) {
                merged.remove(hash);
            }
        }

        write_counts_atomic(&self.sidecar_path, &merged)?;
        self.write_ledger(held)?;

        delta.clear();
        removed.clear();
        state.counts = merged;
        Ok(())
    }

    fn reload(&self) -> Result<()> {
        self.reclaim_if_needed()?;
        let mut merged = read_counts(&self.sidecar_path);
        let mut state = State::lock(&self.state);
        for (hash, change) in &state.delta {
            let entry = merged.entry(*hash).or_insert(0);
            *entry = apply_delta(*entry, *change);
        }
        for hash in &state.removed {
            if merged.get(hash).is_none_or(|count| *count == 0) {
                merged.remove(hash);
            }
        }
        state.counts = merged;
        Ok(())
    }
}

/// Top-level CAS error type covering IO, blob validation, and refcount logic.
#[derive(Debug, thiserror::Error)]
pub enum CasError {
    /// I/O layer error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Blame blob not present in the store.
    #[error("blob not found")]
    NotFound,
    /// Blob frame failed validation.
    #[error("blob validation failed: {0}")]
    BadBlob(#[from] super::blob::BadBlob),
    /// Refcount underflow: `decr` called on a hash whose count is already zero.
    #[error("refcount underflow: cannot decrement zero refcount")]
    RefcountUnderflow,
    /// Serialization error writing the refcount sidecar.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Result type for CAS operations.
pub type Result<T> = std::result::Result<T, CasError>;

/// Report returned by [`CasStore::gc`](super::CasStore::gc).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    /// Total bytes reclaimed (on-disk size of removed blob files).
    pub reclaimed_bytes: u64,
    /// Number of blob files removed.
    pub blobs_removed: usize,
}

#[cfg(test)]
#[path = "refs_test.rs"]
mod tests;
