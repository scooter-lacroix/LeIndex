//! Generation lease: a refcount guard that pins CAS blobs for the duration
//! of a read.
//!
//! When a reader wants to access a generation's mmap'd data it calls
//! [`GenerationLease::acquire`]. The lease increments the refcount of every
//! blob referenced by that generation's manifest. When the lease is dropped
//! the refcounts are decremented. As long as at least one lease is live,
//! the blobs cannot be garbage-collected.
//!
//! The lease does **not** touch the LeIndex writer Mutex or `ProjectWriteLock`
//! (flock) — it operates purely at the CAS refcount level via a dedicated
//! `Mutex<CasStore>`. This is the no-stall concurrency invariant
//! (architecture section 4.1): reads proceed even while a write is in
//! progress on the previous generation.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::storage::cas::CasStore;

use super::manifest::{Manifest, ManifestError};

/// Filename of the current-generation pointer written at the storage root.
pub const CURRENT_FILE: &str = "CURRENT";

/// Directory containing per-generation subdirectories.
pub const GENERATIONS_DIR: &str = "generations";

/// Filename of the manifest inside a generation directory.
pub const MANIFEST_FILE: &str = "manifest";

/// Read the current generation number from a storage root.
///
/// The `CURRENT` file contains the generation number as ASCII digits
/// (optionally newline-terminated). Returns `None` if the file is absent
/// or unparseable.
pub fn read_current_generation(storage_root: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(storage_root.join(CURRENT_FILE)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    trimmed.parse::<u64>().ok()
}

/// Read the manifest for `generation` from `storage_root/generations/<N>/manifest`.
///
/// Returns an error if the manifest file is absent or fails validation.
pub fn read_generation_manifest(
    storage_root: &Path,
    generation: u64,
) -> Result<Manifest, ManifestError> {
    let manifest_path = storage_root
        .join(GENERATIONS_DIR)
        .join(generation.to_string())
        .join(MANIFEST_FILE);
    let bytes = std::fs::read(&manifest_path).map_err(|_| ManifestError::MissingManifest {
        generation,
        path: manifest_path.to_string_lossy().into_owned(),
    })?;
    Manifest::from_bytes(&bytes)
}

/// A refcount guard that keeps a generation's CAS blobs alive for the
/// lifetime of the lease. Created by [`GenerationLease::acquire`] and
/// consumed by `Drop`.
pub struct GenerationLease {
    /// Shared CAS store handle. Refcounts are incremented on acquire and
    /// decremented on drop through this handle.
    store: Arc<Mutex<CasStore>>,
    /// The original manifest this lease was acquired against.
    manifest: Manifest,
    /// Snapshot of the layer hashes captured at acquire time so that `Drop`
    /// knows exactly which refcounts to decrement.
    hashes: Vec<[u8; 32]>,
}

impl GenerationLease {
    /// Acquire a lease on `manifest`, incrementing the refcount of every
    /// layer blob hash in the CAS store and recording a hold on the
    /// generation itself.
    ///
    /// Returns a [`GenerationLease`] whose `Drop` impl will decrement the
    /// same refcounts and release the generation hold. If any refcount
    /// increment fails the error is returned and no refcounts are modified.
    ///
    /// The generation hold lets retention identify leased generations by
    /// identity: blob refcounts alone cannot distinguish a leased generation
    /// from a historical one that happens to share its entire (unchanged)
    /// layer set.
    pub fn acquire(store: Arc<Mutex<CasStore>>, manifest: &Manifest) -> Result<Self, LeaseError> {
        let hashes = manifest.layer_hashes();
        {
            let mut s = store.lock().expect("cas store mutex poisoned");
            for hash in &hashes {
                s.incr(hash);
            }
            s.record_generation_hold(manifest.generation);
            // Persist refcounts so they survive a crash while the lease is held.
            if let Err(persist_error) = s.persist() {
                // Nothing was durably recorded, and unpersisted deltas are
                // folded into this handle's visible counts and held
                // generations — without this rollback they would pin the
                // blobs and the generation for the rest of the process (no
                // GenerationLease exists, so Drop never releases them).
                s.release_generation_hold(manifest.generation);
                for hash in &hashes {
                    // A concurrent handle can legitimately take a count to
                    // zero between our incr above and this decr, which then
                    // reports RefcountUnderflow. The pending delta is still
                    // rewound either way, but swallowing the error silently
                    // would hide a state where the sidecar under-counts a
                    // blob another live reader holds — exactly what lets GC
                    // free in-use data. Make it loud.
                    if let Err(error) = s.decr(hash) {
                        tracing::warn!(
                            hash = %crate::storage::cas::blob::hash_to_hex(hash),
                            %error,
                            "generation lease rollback: decr underflowed; another handle released this blob concurrently"
                        );
                    }
                }
                return Err(LeaseError::Persist(persist_error));
            }
        }
        Ok(GenerationLease {
            store,
            manifest: manifest.clone(),
            hashes,
        })
    }

    /// The generation number this lease was acquired against.
    pub fn generation(&self) -> u64 {
        self.manifest.generation
    }

    /// The CAS blob hashes pinned by this lease.
    pub fn layer_hashes(&self) -> &[[u8; 32]] {
        &self.hashes
    }

    /// Access the manifest metadata associated with this lease.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

impl Drop for GenerationLease {
    fn drop(&mut self) {
        // Best-effort decrement. If the mutex is poisoned we cannot do much;
        // the blobs may remain pinned until the store is reopened.
        if let Ok(mut store) = self.store.lock() {
            for hash in &self.hashes {
                let _ = store.decr(hash);
            }
            store.release_generation_hold(self.manifest.generation);
            let _ = store.persist();
        }
    }
}

/// Errors that can occur when acquiring a lease.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// Failed to persist refcounts after incrementing.
    #[error("failed to persist lease refcounts: {0}")]
    Persist(#[from] crate::storage::cas::CasError),
    /// The manifest is invalid (missing layers, bad version, etc.).
    #[error("invalid manifest for lease: {0}")]
    InvalidManifest(#[from] super::manifest::ManifestError),
    /// The project does not have a current generation.
    #[error("no current generation for project: {0}")]
    NoCurrentGeneration(String),
    /// I/O error reading the CURRENT or manifest file.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
#[path = "lease_test.rs"]
mod tests;
