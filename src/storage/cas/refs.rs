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

use std::collections::{BTreeMap, HashMap, HashSet};
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
/// Legacy directory of per-owner ledgers written by intermediate v2.0.0
/// builds. Absorbed into the sidecar's `owners` map on open and removed.
pub const REFS_OWNERS_DIR: &str = "refs.owners";
/// On-disk auxiliary files the CAS blob-count / stored-hash walkers skip.
pub const REFS_AUX_FILES: &[&str] = &[REFS_SIDECAR, REFS_LOCK, REFS_OWNERS_DIR];

/// Monotonic sequence that guarantees a unique temp-sidecar path per
/// `persist` call in this process, so concurrent readers can each atomically
/// swap the refcount sidecar without racing on a shared temp filename.
static PERSIST_SEQ: AtomicU64 = AtomicU64::new(0);

/// Monotonic sequence giving every store handle in this process a distinct
/// owner identity, so overlapping handles never share (or clobber) holdings.
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

    /// Record that this handle holds one lease on `generation`, so retention
    /// can identify leased generations directly instead of inferring lease
    /// state from shared blob refcounts (identical layer sets across
    /// generations would otherwise all look leased). Durably recorded by the
    /// next [`persist`](RefcountStore::persist).
    fn record_generation_hold(&mut self, generation: u64);

    /// Release one lease on `generation` previously recorded by
    /// [`record_generation_hold`](RefcountStore::record_generation_hold).
    fn release_generation_hold(&mut self, generation: u64);

    /// Generations currently held by any live owner, including this handle's
    /// not-yet-persisted holds. Dead owners' holds are reclaimed first.
    fn held_generations(&self) -> HashSet<u64>;

    /// GC decision point: for each candidate whose **fresh** on-disk
    /// refcount — re-read under the cross-process `refs` lock, immediately
    /// before the callback — is zero, invoke `unlink` to remove the blob
    /// file, then drop the sidecar entry. Returns the hashes removed.
    ///
    /// Holding the lock across check-and-unlink closes the window where a
    /// lease acquired through another handle or process between this store's
    /// last [`reload`](RefcountStore::reload) and the unlink would have its
    /// blob deleted and its just-persisted count erased.
    fn collect_zero_refcount(
        &self,
        candidates: &[[u8; 32]],
        unlink: &mut dyn FnMut(&[u8; 32]) -> std::io::Result<()>,
    ) -> Result<Vec<[u8; 32]>>;
}

/// The sidecar file: merged blob counts plus every owner's holdings, written
/// by a single atomic rename so the two can never disagree after a crash.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SidecarFile {
    /// Merged refcounts, hex hash → count.
    #[serde(default)]
    counts: HashMap<String, u64>,
    /// Holdings per live owner, keyed `"<pid>.<start_ticks>.<seq>"`.
    #[serde(default)]
    owners: BTreeMap<String, OwnerHolds>,
}

/// What one owner (store handle) currently pins: blob counts and generation
/// leases. Dead owners' holdings are subtracted from `counts` and dropped.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct OwnerHolds {
    #[serde(default)]
    blobs: HashMap<String, u64>,
    #[serde(default)]
    generations: HashMap<String, u64>,
}

impl OwnerHolds {
    fn is_empty(&self) -> bool {
        self.blobs.values().all(|count| *count == 0)
            && self.generations.values().all(|count| *count == 0)
    }
}

/// Refcount storage backed by an in-memory map and a JSON sidecar.
///
/// Winner of the WS4 Task 11 sidecar-vs-SQLite decision; see module docs.
///
/// # Concurrency
///
/// Every `CasStore::open` creates an independent handle, and generation leases
/// open one per reader, so many handles (across threads and processes) share
/// one `refs.json`. Three rules keep that safe:
///
/// * **Merge, don't overwrite.** A handle records only its own *deltas*
///   (`incr`/`decr`/`remove`). [`persist`](RefcountStore::persist) takes the
///   `refs.lock` advisory lock, re-reads the sidecar, applies the deltas on
///   top, and writes the result, so one handle's flush can never erase
///   another's increments.
/// * **One file, one rename.** Merged counts and the per-owner holdings that
///   back crash recovery live in the *same* sidecar file and transition in a
///   single atomic rename. The earlier two-file layout (sidecar + per-owner
///   ledger) had a crash window between the two writes in which recovery
///   subtracted a stale ledger from an already-decremented count, freeing a
///   blob another live reader still used.
/// * **Own what you add.** Each handle records the net counts and generation
///   leases it holds in its `owners` entry. When a process dies holding
///   counts, the next handle to open or persist sees that the owner is gone
///   (pid absent, or its `/proc` start time no longer matches — the start
///   time is compared even when the pid is our own, so a pid reused by a
///   restarted process does not pin forever) and subtracts the dead owner's
///   holdings. Owners are assumed to share a PID namespace; on platforms
///   with no liveness probe nothing is reclaimed.
pub struct JsonSidecarStore {
    state: Mutex<State>,
    sidecar_path: PathBuf,
    lock_path: PathBuf,
    owners_dir: PathBuf,
    /// Owner identity of this handle: `"<pid>.<start_ticks>.<seq>"`.
    owner_file: String,
    /// Own pid and start time; holders with the same pair are this process's
    /// sibling handles and are always live.
    owner_pid: u32,
    owner_start: u64,
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
    /// Net blob counts this handle has persisted so far; mirrors its ledger.
    held: HashMap<[u8; 32], u64>,
    /// Pending generation-lease changes since the last successful persist.
    gen_delta: HashMap<u64, i64>,
    /// Net generation leases this handle has persisted so far.
    held_generations: HashMap<u64, u64>,
}

impl State {
    fn lock(mutex: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Decode a hex → count map, dropping keys that are not valid hashes.
fn counts_from_strings(map: &HashMap<String, u64>) -> HashMap<[u8; 32], u64> {
    map.iter()
        .filter_map(|(hex, count)| hex_to_hash(hex).map(|h| (h, *count)))
        .collect()
}

/// Encode a hash → count map as hex → count.
fn counts_to_strings(counts: &HashMap<[u8; 32], u64>) -> HashMap<String, u64> {
    counts
        .iter()
        .map(|(hash, count)| (hash_to_hex(hash), *count))
        .collect()
}

/// Parse a sidecar/ledger body (hex → count), tolerating corruption.
fn parse_counts(data: &[u8]) -> HashMap<[u8; 32], u64> {
    let map: HashMap<String, u64> = serde_json::from_slice(data).unwrap_or_default();
    counts_from_strings(&map)
}

/// Read the sidecar. Understands both the current format (an object with
/// `counts` and `owners` fields) and the legacy flat hex → count map written
/// before owner tracking moved into this file; corrupt content starts empty.
fn read_sidecar(path: &Path) -> SidecarFile {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(_) => return SidecarFile::default(),
    };
    let value: serde_json::Value = match serde_json::from_slice(&data) {
        Ok(value) => value,
        Err(_) => return SidecarFile::default(),
    };
    // "counts" is not a valid 64-hex hash, so the two formats are
    // unambiguous.
    if value
        .as_object()
        .is_some_and(|map| map.contains_key("counts"))
    {
        return serde_json::from_value(value).unwrap_or_default();
    }
    let counts = parse_counts(&data);
    SidecarFile {
        counts: counts_to_strings(&counts),
        owners: BTreeMap::new(),
    }
}

/// Apply a signed `delta` to `count`, clamping at zero.
fn apply_delta(count: u64, delta: i64) -> u64 {
    if delta >= 0 {
        count.saturating_add(delta as u64)
    } else {
        count.saturating_sub(delta.unsigned_abs())
    }
}

/// Write `data` to `tmp_path`, fsync, then atomically rename it over `path`.
///
/// The temp file is removed on every failure path: a failed durability
/// write (EIO/ENOSPC on a degraded volume) used to leave
/// `refs.json.tmp.<pid>.<seq>` behind, and because the name carries a
/// monotonic sequence, every retry added another orphan inside the
/// directory the blob walkers scan.
fn write_tmp_and_rename(path: &Path, tmp_path: &Path, data: &[u8]) -> Result<()> {
    {
        let file = fs::File::create(tmp_path).map_err(CasError::Io)?;
        let mut writer = std::io::BufWriter::new(file);
        if let Err(error) = writer.write_all(data).and_then(|_| writer.flush()) {
            let _ = fs::remove_file(tmp_path);
            return Err(CasError::Io(error));
        }
        if let Err(error) = fsync_file(writer.get_ref()) {
            let _ = fs::remove_file(tmp_path);
            return Err(CasError::Io(error));
        }
    }
    if let Err(error) = fs::rename(tmp_path, path) {
        let _ = fs::remove_file(tmp_path);
        return Err(CasError::Io(error));
    }

    // Best-effort dir fsync.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = fsync_file(&dir);
        }
    }
    Ok(())
}

/// Atomically replace `path` with `file` serialised as JSON.
fn write_sidecar_atomic(path: &Path, file: &SidecarFile) -> Result<()> {
    let data = serde_json::to_vec_pretty(file).map_err(CasError::Serde)?;
    prepare_parent(path)?;
    let seq = PERSIST_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), seq));
    write_tmp_and_rename(path, &tmp_path, &data)
}

/// Atomically replace `path` with `counts` serialised as hex → count.
/// Test-facing helper for fixtures that write legacy-format files.
#[cfg(test)]
fn write_counts_atomic(path: &Path, counts: &HashMap<[u8; 32], u64>) -> Result<()> {
    let owned = counts_to_strings(counts);
    let data = serde_json::to_vec_pretty(&owned).map_err(CasError::Serde)?;
    prepare_parent(path)?;
    let seq = PERSIST_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), seq));
    write_tmp_and_rename(path, &tmp_path, &data)
}

fn prepare_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CasError::Io)?;
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

/// Whether the owner `pid` started at `start_ticks` is still running. The
/// start time is always compared on Linux, including for our own pid: a
/// restarted process frequently receives a recycled pid (in containers,
/// often pid 1), and treating that as "alive" would pin its predecessor's
/// holdings forever.
fn owner_is_alive(pid: u32, start_ticks: u64) -> bool {
    #[cfg(target_os = "linux")]
    {
        // A missing /proc entry means the process is gone; a different start
        // time means the pid was recycled by an unrelated process.
        process_start_ticks(pid) == Some(start_ticks)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // No /proc start time; probe existence.
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

/// Parse an owner key `<pid>.<start_ticks>.<seq>` into `(pid, start_ticks)`.
fn parse_owner_key(name: &str) -> Option<(u32, u64)> {
    let mut parts = name.split('.');
    let pid = parts.next()?.parse().ok()?;
    let start = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some((pid, start))
}

impl JsonSidecarStore {
    /// Whether the sidecar's `owners` entry `key` belongs to a live owner.
    /// This process's own handles (same pid and start time) are live by
    /// definition; everything else is probed.
    fn owner_key_is_live(&self, key: &str) -> bool {
        if key == self.owner_file {
            return true;
        }
        match parse_owner_key(key) {
            Some((pid, start)) => {
                (pid == self.owner_pid && start == self.owner_start) || owner_is_alive(pid, start)
            }
            // Unparseable identity: cannot probe, never reclaimed.
            None => true,
        }
    }

    /// Subtract every dead owner's holdings from `counts` and drop their
    /// entries. Returns whether the file changed. Caller holds the lock.
    fn reclaim_dead_owners(&self, file: &mut SidecarFile) -> bool {
        let dead: Vec<String> = file
            .owners
            .keys()
            .filter(|key| !self.owner_key_is_live(key))
            .cloned()
            .collect();
        let mut changed = !dead.is_empty();
        for key in dead {
            let Some(holds) = file.owners.remove(&key) else {
                continue;
            };
            for (hex, held) in holds.blobs {
                if let Some(count) = file.counts.get_mut(&hex) {
                    let reduced = count.saturating_sub(held);
                    changed |= reduced != *count;
                    *count = reduced;
                }
            }
        }
        changed
    }

    /// Reclaim abandoned counts from crashed owners, if there are any.
    fn reclaim_if_needed(&self) -> Result<()> {
        if self
            .read_sidecar_unlocked()
            .owners
            .keys()
            .all(|key| self.owner_key_is_live(key))
        {
            return Ok(());
        }
        let _lock = RefsLock::acquire(&self.lock_path)?;
        let mut file = self.read_sidecar_unlocked();
        if self.reclaim_dead_owners(&mut file) {
            write_sidecar_atomic(&self.sidecar_path, &file)?;
        }
        Ok(())
    }

    fn read_sidecar_unlocked(&self) -> SidecarFile {
        read_sidecar(&self.sidecar_path)
    }

    /// Absorb the legacy per-owner ledger directory (`refs.owners/`) written
    /// by intermediate v2.0.0 builds into the sidecar's `owners` map, then
    /// remove the directory. One-time, idempotent.
    ///
    /// The directory is enumerated UNDER the refs lock, and any ledger that
    /// cannot be read, parsed, or identified ABORTS the whole absorption
    /// with the directory left intact: a truncated ledger read as "owner
    /// holding nothing" would otherwise delete the only record of live pins
    /// while keeping the counts they were protecting.
    fn absorb_legacy_owner_ledgers(&self) -> Result<()> {
        if !self.owners_dir.is_dir() {
            return Ok(());
        }
        let _lock = RefsLock::acquire(&self.lock_path)?;
        let mut file = self.read_sidecar_unlocked();
        let mut absorbed = false;
        for entry in fs::read_dir(&self.owners_dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(ledger) = name.strip_suffix(".json") else {
                continue;
            };
            // Only well-formed owner identities can ever be probed for
            // liveness; anything else would become a permanent pin. Leave
            // the directory for manual inspection rather than guessing.
            let Some((pid, start)) = parse_owner_key(ledger) else {
                tracing::warn!(
                    ledger = %entry.path().display(),
                    "refs.owners: unrecognized ledger identity; skipping legacy-ledger absorption"
                );
                return Ok(());
            };
            if file.owners.contains_key(ledger) {
                continue;
            }
            // Distinguish "absent" from "unreadable": a read or parse
            // failure aborts absorption instead of recording the owner as
            // holding nothing.
            let data = match fs::read(entry.path()) {
                Ok(data) => data,
                Err(error) => {
                    tracing::warn!(
                        ledger = %entry.path().display(),
                        %error,
                        "refs.owners: unreadable ledger; skipping legacy-ledger absorption"
                    );
                    return Ok(());
                }
            };
            let blobs: HashMap<String, u64> = match serde_json::from_slice(&data) {
                Ok(map) => map,
                Err(error) => {
                    tracing::warn!(
                        ledger = %entry.path().display(),
                        %error,
                        "refs.owners: unparseable ledger; skipping legacy-ledger absorption"
                    );
                    return Ok(());
                }
            };
            let _ = (pid, start);
            file.owners.insert(
                ledger.to_string(),
                OwnerHolds {
                    blobs,
                    generations: HashMap::new(),
                },
            );
            absorbed = true;
        }
        if absorbed {
            write_sidecar_atomic(&self.sidecar_path, &file)?;
        }
        let _ = fs::remove_dir_all(&self.owners_dir);
        Ok(())
    }

    /// Remove leftover `refs.json.tmp.<pid>.<seq>` files whose owning
    /// process is gone (a crash between temp creation and rename orphaned
    /// them). Temps belonging to a live pid — including ours, which may be
    /// mid-write on another handle — are left alone.
    fn sweep_orphaned_sidecar_temps(&self) {
        let Some(parent) = self.sidecar_path.parent() else {
            return;
        };
        let prefix = format!("{REFS_SIDECAR}.tmp.");
        let Ok(entries) = fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some(pid) = rest.split('.').next().and_then(|p| p.parse::<u32>().ok()) else {
                continue;
            };
            if pid == std::process::id() || pid_is_running(pid) {
                continue;
            }
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Whether `pid` has a running process. A recycled pid can only make a dead
/// writer's orphan look live (kept, safe) — never the reverse on Linux,
/// where /proc existence is authoritative.
fn pid_is_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: signal 0 only probes for existence.
        let rc = unsafe { libc::kill(pid as i32, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        false
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
            owner_file: format!("{pid}.{start}.{seq}"),
            owner_pid: pid,
            owner_start: start,
        };
        // Best effort: a read-only store must still open.
        store.sweep_orphaned_sidecar_temps();
        let _ = store.absorb_legacy_owner_ledgers();
        let _ = store.reclaim_if_needed();
        State::lock(&store.state).counts =
            counts_from_strings(&store.read_sidecar_unlocked().counts);
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
    /// counts held by dead owners, apply this handle's deltas, refresh this
    /// handle's owner entry, then write the whole file — counts *and* owner
    /// holdings — through one uniquely-named temp file, fsync, and rename.
    /// The lock serialises overlapping handles (threads or processes) so no
    /// update is lost; the single rename means a crash can never leave the
    /// counts and the owner holdings that back recovery disagreeing.
    ///
    /// The in-memory view (`held`/`held_generations`, pending deltas) is
    /// only advanced AFTER a successful write: the new holdings are computed
    /// into copies first, so a failed write (ENOSPC/EIO) leaves the pending
    /// deltas intact and the next `persist` re-applies them onto a fresh
    /// read instead of double-counting them into this handle's holdings —
    /// which would later let dead-owner reclaim subtract more than this
    /// handle ever owned and free a blob another live reader still holds.
    fn persist(&self) -> Result<()> {
        let _lock = RefsLock::acquire(&self.lock_path)?;
        let mut file = self.read_sidecar_unlocked();
        self.reclaim_dead_owners(&mut file);

        let mut state = State::lock(&self.state);
        // Work on copies of the net holdings; committed to `state` only
        // after the write succeeds.
        let mut held = state.held.clone();
        let mut held_gens = state.held_generations.clone();
        for (hash, change) in state.delta.iter() {
            let entry = file.counts.entry(hash_to_hex(hash)).or_insert(0);
            *entry = apply_delta(*entry, *change);
            let own = held.entry(*hash).or_insert(0);
            *own = apply_delta(*own, *change);
        }
        for hash in state.removed.iter() {
            let hex = hash_to_hex(hash);
            if file.counts.get(&hex).is_none_or(|count| *count == 0) {
                file.counts.remove(&hex);
            }
        }
        for (generation, change) in state.gen_delta.iter() {
            let own = held_gens.entry(*generation).or_insert(0);
            *own = apply_delta(*own, *change);
        }

        // Refresh this handle's owner entry: positive holdings only, dropped
        // entirely when it holds nothing.
        let holds = OwnerHolds {
            blobs: counts_to_strings(
                &held
                    .iter()
                    .filter(|(_, count)| **count > 0)
                    .map(|(hash, count)| (*hash, *count))
                    .collect(),
            ),
            generations: held_gens
                .iter()
                .filter(|(_, count)| **count > 0)
                .map(|(generation, count)| (generation.to_string(), *count))
                .collect(),
        };
        if holds.is_empty() {
            file.owners.remove(&self.owner_file);
        } else {
            file.owners.insert(self.owner_file.clone(), holds);
        }

        write_sidecar_atomic(&self.sidecar_path, &file)?;

        state.held = held;
        state.held_generations = held_gens;
        state.delta.clear();
        state.removed.clear();
        state.gen_delta.clear();
        state.counts = counts_from_strings(&file.counts);
        Ok(())
    }

    fn reload(&self) -> Result<()> {
        self.reclaim_if_needed()?;
        let file = self.read_sidecar_unlocked();
        let mut merged = counts_from_strings(&file.counts);
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

    fn record_generation_hold(&mut self, generation: u64) {
        let state = self.state.get_mut().unwrap_or_else(|p| p.into_inner());
        *state.gen_delta.entry(generation).or_insert(0) += 1;
    }

    fn release_generation_hold(&mut self, generation: u64) {
        let state = self.state.get_mut().unwrap_or_else(|p| p.into_inner());
        // Best-effort release: without a recorded or pending hold there is
        // nothing to give back.
        let pending = state.gen_delta.get(&generation).copied().unwrap_or(0);
        let held = state
            .held_generations
            .get(&generation)
            .copied()
            .unwrap_or(0);
        if pending <= 0 && held == 0 {
            return;
        }
        *state.gen_delta.entry(generation).or_insert(0) -= 1;
    }

    fn held_generations(&self) -> HashSet<u64> {
        let _ = self.reclaim_if_needed();
        let file = self.read_sidecar_unlocked();
        let state = State::lock(&self.state);

        // This handle's own entry: what the file records, ADJUSTED by this
        // handle's pending (not-yet-persisted) changes. Subtraction matters:
        // a pending release must suppress the generation NOW (its lease is
        // gone even though the Drop's persist failed), while a pending
        // acquire pins it NOW. Unioning the pending view into the file view
        // left on-disk holds un-cancellable by pending releases.
        let mut effective: HashMap<u64, u64> = HashMap::new();
        if let Some(own) = file.owners.get(&self.owner_file) {
            for (generation, count) in &own.generations {
                if let Ok(generation) = generation.parse::<u64>() {
                    effective.insert(generation, *count);
                }
            }
        }
        for (generation, held) in &state.held_generations {
            effective.entry(*generation).or_insert(*held);
        }
        for (generation, change) in &state.gen_delta {
            let entry = effective.entry(*generation).or_insert(0);
            *entry = apply_delta(*entry, *change);
        }

        let mut generations: HashSet<u64> = effective
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(generation, _)| generation)
            .collect();
        for (key, holds) in &file.owners {
            if key == &self.owner_file || !self.owner_key_is_live(key) {
                continue;
            }
            for (generation, count) in &holds.generations {
                if *count > 0 {
                    if let Ok(generation) = generation.parse::<u64>() {
                        generations.insert(generation);
                    }
                }
            }
        }
        generations
    }

    fn collect_zero_refcount(
        &self,
        candidates: &[[u8; 32]],
        unlink: &mut dyn FnMut(&[u8; 32]) -> std::io::Result<()>,
    ) -> Result<Vec<[u8; 32]>> {
        // The lock spans the fresh read and every unlink, so a lease persist
        // from another handle/process either lands before the read (count
        // visible → blob kept) or after the sweep (blob already gone, its
        // reader fails to open rather than reading a half-deleted file).
        let _lock = RefsLock::acquire(&self.lock_path)?;
        let mut file = self.read_sidecar_unlocked();
        self.reclaim_dead_owners(&mut file);

        let mut removed = Vec::new();
        {
            let mut state = State::lock(&self.state);
            for hash in candidates {
                let hex = hash_to_hex(hash);
                // Effective count = the fresh on-disk value plus THIS
                // handle's not-yet-persisted changes (an incr without a
                // persist still has a live reader behind it), and zero when
                // this handle already removed the entry.
                let on_disk = file.counts.get(&hex).copied().unwrap_or(0);
                let pending = state.delta.get(hash).copied().unwrap_or(0);
                let fresh = if state.removed.contains(hash) {
                    0
                } else {
                    apply_delta(on_disk, pending)
                };
                if fresh != 0 {
                    continue;
                }
                if let Err(error) = unlink(hash) {
                    // Candidates unlinked before this failure are already
                    // gone from the filesystem. Persist the partial sweep so
                    // the sidecar keeps matching reality (the count entries
                    // were already dropped above), and surface both the
                    // error and what was swept.
                    if !removed.is_empty() {
                        if let Err(write_error) = write_sidecar_atomic(&self.sidecar_path, &file) {
                            tracing::warn!(
                                %write_error,
                                "refs: failed to persist partial GC sweep; sidecar may hold entries for deleted blobs"
                            );
                        }
                    }
                    tracing::warn!(
                        %error,
                        swept = removed.len(),
                        "refs: GC sweep aborted part-way; already-swept entries were persisted"
                    );
                    return Err(CasError::Io(error));
                }
                file.counts.remove(&hex);
                state.counts.remove(hash);
                state.removed.insert(*hash);
                removed.push(*hash);
            }
        }

        if !removed.is_empty() {
            write_sidecar_atomic(&self.sidecar_path, &file)?;
        }
        Ok(removed)
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
