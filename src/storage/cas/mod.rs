//! Content-addressed blob store (CAS).
//!
//! Blobs are blake3-addressed and written via a staging-then-rename dance to
//! guarantee crash safety and deduplication at the storage layer. See
//! `blob` for the on-disk frame format and `refs` for refcount storage.
//!
//! # Layout
//!
//! ```text
//! <root>/
//!   refs.json            <- persisted refcounts (Task 2)
//!   .staging/
//!     <hex>.partial      <- pending blob write (pre-rename)
//!   ab/                  <- two-hex prefix directory
//!     <full-64-hex-hash> <- blob frame (magic + version + hash + len + payload)
//!   cd/
//!     ...
//! ```

pub mod blob;
pub mod refs;

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use blob::{encode_blob, extract_payload, fsync_file, hash_to_hex, validate_blob};
use refs::{JsonSidecarStore, RefcountStore, Result};

pub use blob::BadBlob;
pub use refs::{CasError, RetentionReport};

/// Directory holding staging partial files.
const STAGING_DIR: &str = ".staging";

/// Monotonic sequence making every staging path unique per `put` call, so
/// concurrent writers of the same blob never truncate each other's in-flight
/// file.
static PUT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Content-addressed blob store with refcount and garbage collection.
pub struct CasStore {
    root: PathBuf,
    refs: Box<dyn RefcountStore>,
}

impl CasStore {
    /// Open (or initialise) a CAS at `root`.
    ///
    /// Creates the root, staging, and prefix directories on demand. Refcount
    /// persistence uses the JSON sidecar winner of the WS4 Task 11 decision
    /// (see `src/storage/cas/refs.rs`).
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join(STAGING_DIR))?;
        let refs = Box::new(JsonSidecarStore::open(&root)?);
        Ok(CasStore { root, refs })
    }

    /// Root directory of the CAS.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory path for a given hash's two-hex prefix.
    fn prefix_dir(&self, hash: &[u8; 32]) -> PathBuf {
        let hex = hash_to_hex(hash);
        self.root.join(&hex[0..2])
    }

    /// Final blob path for `hash`: `<root>/<hh>/<hash>`.
    ///
    /// The file at this path is the full CAS frame (`LIDX-BLB1` header +
    /// payload). Generation mmap readers open it directly so reads never copy
    /// the payload into heap.
    pub fn blob_path(&self, hash: &[u8; 32]) -> PathBuf {
        let hex = hash_to_hex(hash);
        self.prefix_dir(hash).join(&hex)
    }

    /// Staging partial path: `<root>/.staging/<hash>.<pid>.<seq>.partial`.
    ///
    /// The pid/seq suffix makes the name unique per writer and per `put`
    /// call: a deterministic `<hash>.partial` name let two concurrent writers
    /// of the same blob truncate each other's file mid-write, publishing a
    /// short/growing blob to readers.
    fn staging_path(&self, hash: &[u8; 32]) -> PathBuf {
        let hex = hash_to_hex(hash);
        let seq = PUT_SEQ.fetch_add(1, Ordering::Relaxed);
        self.root
            .join(STAGING_DIR)
            .join(format!("{hex}.{}.{}.partial", std::process::id(), seq))
    }

    /// Store `payload` in the CAS.
    ///
    /// Writes to `cas/.staging/<hash>.partial`, fsyncs, then atomically renames
    /// to `cas/<prefix>/<hash>`. If a blob with the same hash already exists
    /// the call is a no-op at the storage layer (dedup): no second file or
    /// duplicate flush is produced.
    pub fn put(&self, payload: &[u8]) -> Result<[u8; 32]> {
        let hash = blob::blob_hash(payload);
        let final_path = self.blob_path(&hash);
        if final_path.exists() {
            return Ok(hash);
        }
        let staging_path = self.staging_path(&hash);
        let blob_bytes = encode_blob(payload);

        // Ensure the prefix directory exists so the rename target is valid.
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write + fsync the staging file.
        {
            let file = fs::File::create(&staging_path)?;
            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(&blob_bytes)?;
            writer.flush()?;
            fsync_file(writer.get_ref())?;
        }

        // Atomic rename. If a concurrent writer won (multi-process case), the
        // final path already exists; handle the race by cleaning up our
        // staging file.
        match fs::rename(&staging_path, &final_path) {
            Ok(()) => {}
            Err(_) if final_path.exists() => {
                // Another writer published; remove our staging copy.
                let _ = fs::remove_file(&staging_path);
            }
            Err(e) => {
                // Clean up the staging file before propagating the error so
                // the next `put` attempt starts clean.
                let _ = fs::remove_file(&staging_path);
                return Err(CasError::Io(e));
            }
        }
        // Best-effort: fsync the prefix directory so the rename is durable.
        if let Some(parent) = final_path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = fsync_file(&dir);
            }
        }

        Ok(hash)
    }

    /// Retrieve the payload bytes for `hash`.
    ///
    /// Re-validates the blob frame (magic, version, hash) before returning.
    /// Returns [`CasError::NotFound`] if no blob exists at `hash`.
    pub fn get(&self, hash: &[u8; 32]) -> Result<Vec<u8>> {
        let path = self.blob_path(hash);
        if !path.exists() {
            return Err(CasError::NotFound);
        }
        let bytes = fs::read(&path)?;
        let (payload, _) = extract_payload(&bytes).map_err(CasError::BadBlob)?;
        Ok(payload.to_vec())
    }

    /// Validate `hash` on disk without reading the payload into a new
    /// allocation. Returns the stored hash on success.
    pub fn validate(&self, hash: &[u8; 32]) -> Result<()> {
        let path = self.blob_path(hash);
        if !path.exists() {
            return Err(CasError::NotFound);
        }
        let bytes = fs::read(&path)?;
        validate_blob(&bytes).map_err(CasError::BadBlob)?;
        Ok(())
    }

    /// Count the number of stored blobs (excluding `.staging/` and aux files).
    pub fn blob_count(&self) -> Result<usize> {
        let mut count = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip the staging dir and refcount aux files.
            if name == STAGING_DIR || refs::REFS_AUX_FILES.contains(&name.as_ref()) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                count += count_shard_blobs(&entry.path())?;
            } else if is_flat_layout_blob(&name) {
                // Top-level files whose names look like blob hashes
                // (legacy / flat layout tolerance).
                count += 1;
            }
        }
        Ok(count)
    }

    /// Check whether a blob exists at `hash` on disk.
    pub fn exists(&self, hash: &[u8; 32]) -> bool {
        self.blob_path(hash).exists()
    }

    /// Increment the refcount for `hash`.
    ///
    /// Refcount updates are held in-memory until [`persist`](Self::persist)
    /// is called.
    pub fn incr(&mut self, hash: &[u8; 32]) -> u64 {
        self.refs.incr(hash)
    }

    /// Decrement the refcount for `hash`, returning the new value or
    /// [`CasError::RefcountUnderflow`] if already zero.
    pub fn decr(&mut self, hash: &[u8; 32]) -> Result<u64> {
        self.refs.decr(hash)
    }

    /// Current refcount for `hash`, or 0 if unknown.
    pub fn refcount(&self, hash: &[u8; 32]) -> u64 {
        self.refs.refcount(hash)
    }

    /// Persist the refcount sidecar to disk.
    ///
    /// Merges this handle's changes into the shared sidecar under a lock, so
    /// overlapping handles (threads or processes) never overwrite each other.
    pub fn persist(&self) -> Result<()> {
        self.refs.persist()
    }

    /// Refresh refcounts from disk so leases taken through other handles or
    /// processes since this one opened are visible. Callers about to delete
    /// data (retention, GC) call this first.
    pub fn reload(&self) -> Result<()> {
        self.refs.reload()
    }

    /// Record that this store handle holds one lease on `generation` (see
    /// [`GenerationLease`](crate::storage::generation::GenerationLease)), so
    /// retention identifies leased generations by identity instead of
    /// inferring lease state from shared blob refcounts. Durably recorded by
    /// the next [`persist`](Self::persist).
    pub fn record_generation_hold(&mut self, generation: u64) {
        self.refs.record_generation_hold(generation)
    }

    /// Release one generation lease recorded via
    /// [`record_generation_hold`](Self::record_generation_hold).
    pub fn release_generation_hold(&mut self, generation: u64) {
        self.refs.release_generation_hold(generation)
    }

    /// Generations currently held by any live owner (including this handle's
    /// not-yet-persisted holds); dead owners' holds are reclaimed first.
    pub fn held_generations(&self) -> HashSet<u64> {
        self.refs.held_generations()
    }

    /// Garbage-collect blobs with refcount 0 that are not in `pinned_hashes`.
    ///
    /// `pinned_hashes` is the set of blob hashes referenced by retained
    /// generation manifests. When no manifests exist yet (Tasks 1-2), pass an
    /// empty set or use [`gc`](Self::gc).
    ///
    /// Each blob is unlinked only after its refcount is re-read from disk
    /// under the cross-process `refs` lock, so a lease acquired and persisted
    /// by another handle or process since this store's last
    /// [`reload`](Self::reload) is honoured instead of deleted from under the
    /// live reader.
    pub fn gc_with_pins(&mut self, pinned_hashes: &HashSet<[u8; 32]>) -> Result<RetentionReport> {
        let mut report = RetentionReport::default();
        // See leases taken through other handles since this one opened.
        self.refs.reload()?;
        // Walk all blobs on disk so that we catch both blobs that were
        // decr'd to 0 AND blobs that were `put` but never `incr`'d.
        let candidates: Vec<[u8; 32]> = self
            .stored_hashes()?
            .into_iter()
            .filter(|hash| !pinned_hashes.contains(hash))
            .collect();

        let mut reclaimed_bytes = 0u64;
        let mut blobs_removed = 0usize;
        let swept = self.refs.collect_zero_refcount(&candidates, &mut |hash| {
            let path = self.blob_path(hash);
            let size = match fs::metadata(&path) {
                Ok(meta) => meta.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e),
            };
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
            reclaimed_bytes += size;
            blobs_removed += 1;
            Ok(())
        });
        let removed = match swept {
            Ok(removed) => removed,
            Err(error) => {
                // The sweep aborted part-way but the already-unlinked blobs
                // are gone and their sidecar entries were persisted (see
                // `collect_zero_refcount`). Surface the partial accounting
                // before propagating so operators know what was reclaimed.
                tracing::warn!(
                    %error,
                    blobs_removed,
                    reclaimed_bytes,
                    "GC sweep failed part-way; the already-swept blobs above were reclaimed"
                );
                return Err(error);
            }
        };
        report.reclaimed_bytes += reclaimed_bytes;
        report.blobs_removed += blobs_removed;
        debug_assert_eq!(removed.len(), report.blobs_removed);

        // Persist the cleaned-up refcount map so the blobs stay gone after a
        // restart.
        self.refs.persist()?;
        Ok(report)
    }

    /// Simplified GC: collect every blob with refcount 0.
    ///
    /// Equivalent to [`gc_with_pins`](Self::gc_with_pins) with an empty pin
    /// set. When generation manifests are introduced in a later task the pin
    /// set will be populated with their layer hashes.
    pub fn gc(&mut self) -> Result<RetentionReport> {
        self.gc_with_pins(&HashSet::new())
    }

    /// Snapshot all hashes referenced by tracked refcounts whose blob file
    /// currently exists on disk.
    pub fn stored_hashes(&self) -> Result<Vec<[u8; 32]>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == STAGING_DIR || refs::REFS_AUX_FILES.contains(&name.as_ref()) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                for sub_entry in fs::read_dir(entry.path())? {
                    let sub_entry = sub_entry?;
                    if sub_entry.file_type()?.is_file() {
                        if let Some(hash) =
                            blob::hex_to_hash(&sub_entry.file_name().to_string_lossy())
                        {
                            out.push(hash);
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Count blob files one level deep inside a shard directory. Every regular
/// file counts; nested directories are not traversed.
fn count_shard_blobs(shard: &Path) -> Result<usize> {
    let mut count = 0;
    for sub_entry in fs::read_dir(shard)? {
        if sub_entry?.file_type()?.is_file() {
            count += 1;
        }
    }
    Ok(count)
}

/// Legacy flat-layout tolerance: a top-level file counts as a blob when its
/// name looks like a 64-character hash and it is not an aux/temp file.
fn is_flat_layout_blob(name: &str) -> bool {
    !refs::REFS_AUX_FILES.contains(&name) && !name.ends_with(".tmp") && name.len() == 64
}

#[cfg(test)]
#[path = "cas_test.rs"]
mod tests;
