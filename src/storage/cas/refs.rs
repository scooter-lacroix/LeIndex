//! CAS blob refcount store: in-memory `HashMap` + JSON sidecar for persistence.
//!
//! Refcounts are mutated in-memory by [`incr`](RefcountStore::incr) /
//! [`decr`](RefcountStore::decr) and flushed to disk by
//! [`persist`](RefcountStore::persist). This mirrors the write-barrier
//! semantics required by VAL-CAS-016 (kill-before-fsync may lose increments
//! but never produces phantom counts).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::blob::{fsync_file, hash_to_hex, hex_to_hash};

/// JSON sidecar filename written into the CAS root.
pub const REFS_SIDECAR: &str = "refs.json";

/// Refcount storage backed by an in-memory map and a JSON sidecar.
pub struct RefcountStore {
    counts: HashMap<[u8; 32], u64>,
    sidecar_path: PathBuf,
}

impl RefcountStore {
    /// Open (or create) the sidecar at `<cas_root>/refs.json`.
    ///
    /// If the file does not exist yet the store starts empty.
    pub fn open(cas_root: &Path) -> std::io::Result<Self> {
        let sidecar_path = cas_root.join(REFS_SIDECAR);
        let counts = if sidecar_path.exists() {
            let data = fs::read(&sidecar_path)?;
            // Map hex-string → count, tolerant of corruption.
            let map: HashMap<String, u64> = serde_json::from_slice(&data).unwrap_or_default();
            map.into_iter()
                .filter_map(|(hex, count)| hex_to_hash(&hex).map(|h| (h, count)))
                .collect()
        } else {
            HashMap::new()
        };
        Ok(RefcountStore {
            counts,
            sidecar_path,
        })
    }

    /// Increment the refcount for `hash` and return the new value.
    pub fn incr(&mut self, hash: &[u8; 32]) -> u64 {
        let entry = self.counts.entry(*hash).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Decrement the refcount for `hash` and return the new value, or
    /// [`CasError::RefcountUnderflow`] if the count is already zero.
    pub fn decr(&mut self, hash: &[u8; 32]) -> Result<u64> {
        let entry = self.counts.entry(*hash).or_insert(0);
        if *entry == 0 {
            return Err(CasError::RefcountUnderflow);
        }
        *entry -= 1;
        Ok(*entry)
    }

    /// Current refcount for `hash` (0 if absent).
    pub fn refcount(&self, hash: &[u8; 32]) -> u64 {
        self.counts.get(hash).copied().unwrap_or(0)
    }

    /// Returns `true` if `hash` is tracked by this store with any refcount.
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.counts.contains_key(hash)
    }

    /// Remove the refcount entry for `hash` (used after GC deletes the blob).
    pub fn remove(&mut self, hash: &[u8; 32]) {
        self.counts.remove(hash);
    }

    /// Iterate over all `(hash, refcount)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8; 32], u64)> {
        self.counts.iter().map(|(k, v)| (k, *v))
    }

    /// Hashes whose refcount is exactly zero.
    pub fn zero_refcount_hashes(&self) -> Vec<[u8; 32]> {
        self.counts
            .iter()
            .filter(|(_, v)| **v == 0)
            .map(|(k, _)| *k)
            .collect()
    }

    /// Return the set of all hashes tracked by this store.
    pub fn tracked_hashes(&self) -> HashSet<[u8; 32]> {
        self.counts.keys().copied().collect()
    }

    /// Persist the current refcounts to disk atomically.
    ///
    /// Writes `<sidecar>.tmp`, fsyncs, then renames to `<sidecar>`.
    pub fn persist(&self) -> std::io::Result<()> {
        // Serialize hex → count directly to avoid any lifetime pitfalls.
        let owned: HashMap<String, u64> = self
            .counts
            .iter()
            .map(|(k, v)| (hash_to_hex(k), *v))
            .collect();
        let data = serde_json::to_vec_pretty(&owned)?;

        if let Some(parent) = self.sidecar_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = self.sidecar_path.with_extension("json.tmp");
        {
            let file = fs::File::create(&tmp_path)?;
            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(&data)?;
            writer.flush()?;
            fsync_file(writer.get_ref())?;
        }
        fs::rename(&tmp_path, &self.sidecar_path)?;

        // Best-effort dir fsync.
        if let Some(parent) = self.sidecar_path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = fsync_file(&dir);
            }
        }
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
