//! Content-addressed blob store (CAS).
//!
//! Blobs are blake3-addressed and written via a staging-then-rename dance to
//! guarantee crash safety and deduplication at the storage layer. See
//! [`blob`] for the on-disk frame format and [`refs`] for refcount storage.
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

use blob::{encode_blob, extract_payload, fsync_file, hash_to_hex, validate_blob};
use refs::{RefcountStore, Result};

pub use blob::BadBlob;
pub use refs::{CasError, RetentionReport};

/// Directory holding staging partial files.
const STAGING_DIR: &str = ".staging";

/// Content-addressed blob store with refcount and garbage collection.
pub struct CasStore {
    root: PathBuf,
    refs: RefcountStore,
}

impl CasStore {
    /// Open (or initialise) a CAS at `root`.
    ///
    /// Creates the root, staging, and prefix directories on demand.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join(STAGING_DIR))?;
        let refs = RefcountStore::open(&root)?;
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
    fn blob_path(&self, hash: &[u8; 32]) -> PathBuf {
        let hex = hash_to_hex(hash);
        self.prefix_dir(hash).join(&hex)
    }

    /// Staging partial path: `<root>/.staging/<hash>.partial`.
    fn staging_path(&self, hash: &[u8; 32]) -> PathBuf {
        let hex = hash_to_hex(hash);
        self.root.join(STAGING_DIR).join(format!("{hex}.partial"))
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

    /// Count the number of stored blobs (excluding `.staging/` and `refs.json`).
    pub fn blob_count(&self) -> Result<usize> {
        let mut count = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip the staging dir and sidecar files.
            if name == STAGING_DIR || name == refs::REFS_SIDECAR {
                continue;
            }
            if entry.file_type()?.is_dir() {
                for sub_entry in fs::read_dir(entry.path())? {
                    let sub_entry = sub_entry?;
                    if sub_entry.file_type()?.is_file() {
                        count += 1;
                    }
                }
            }

            // Also count top-level files whose names look like blob hashes
            // (legacy / flat layout tolerance).
            if entry.file_type()?.is_file()
                && name != refs::REFS_SIDECAR
                && !name.ends_with(".tmp")
                && name.len() == 64
            {
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
    pub fn persist(&self) -> Result<()> {
        Ok(self.refs.persist()?)
    }

    /// Garbage-collect blobs with refcount 0 that are not in `pinned_hashes`.
    ///
    /// `pinned_hashes` is the set of blob hashes referenced by retained
    /// generation manifests. When no manifests exist yet (Tasks 1-2), pass an
    /// empty set or use [`gc`](Self::gc).
    pub fn gc_with_pins(&mut self, pinned_hashes: &HashSet<[u8; 32]>) -> Result<RetentionReport> {
        let mut report = RetentionReport::default();
        // Walk all blobs on disk so that we catch both blobs that were
        // decr'd to 0 AND blobs that were `put` but never `incr`'d.
        let candidates = self.stored_hashes()?;

        for hash in candidates {
            let refcount = self.refs.refcount(&hash);
            if refcount != 0 {
                continue;
            }
            if pinned_hashes.contains(&hash) {
                continue;
            }
            let path = self.blob_path(&hash);
            let size = match fs::metadata(&path) {
                Ok(meta) => meta.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(CasError::Io(e)),
            };
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(CasError::Io(e));
                }
            }
            report.reclaimed_bytes += size;
            report.blobs_removed += 1;
            self.refs.remove(&hash);
        }

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
            if name == STAGING_DIR || name == refs::REFS_SIDECAR {
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

#[cfg(test)]
#[path = "cas_test.rs"]
mod tests;
