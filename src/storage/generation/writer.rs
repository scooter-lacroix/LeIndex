//! Generation writer: blob staging, dedup, crash-safe atomic publication.
//!
//! The [`GenerationWriter`] accumulates layer data into CAS blobs via
//! [`stage`](GenerationWriter::stage), then atomically publishes a complete
//! generation manifest via [`publish`](GenerationWriter::publish).
//!
//! ## Atomic publish sequence
//!
//! ```text
//! 1. stage(layer, bytes)  → cas.put(bytes)
//! 2. write manifest.partial
//! 3. fsync manifest.partial
//! 4. rename manifest.partial → manifest
//! 5. write CURRENT.tmp
//! 6. fsync CURRENT.tmp
//! 7. rename CURRENT.tmp → CURRENT
//! ```
//!
//! ## Crash safety
//!
//! The publish sequence is designed so that a crash at any point leaves the
//! `CURRENT`-referenced generation fully intact:
//!
//! - **Before manifest rename** (steps 1–3): `manifest` does not exist yet;
//!   `CURRENT` is unchanged. On restart, `sweep_partial_manifests` cleans
//!   stale `.partial` files.
//!
//! - **After manifest rename, before CURRENT update** (step 4 done, step 7
//!   not): The new manifest is on disk but `CURRENT` still points to the
//!   previous generation. The orphaned manifest is either picked up on the
//!   next publish or cleaned by retention.
//!
//! - **During CURRENT update** (steps 5–7): `CURRENT` is updated via its own
//!   atomic rename (`CURRENT.tmp` → `CURRENT`), so it either contains the old
//!   value or the new value, never a partial write.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::storage::cas::CasStore;
use crate::storage::cas::blob::fsync_file;

use super::lease::{CURRENT_FILE, GENERATIONS_DIR, MANIFEST_FILE};
use super::manifest::{ALL_LAYER_KINDS, LayerKind, MANIFEST_VERSION, Manifest, ModelIdentity};

/// Filename for the temporary manifest file written before atomic rename.
const MANIFEST_PARTIAL: &str = "manifest.partial";

/// Filename for the temporary CURRENT file written before atomic rename.
const CURRENT_TMP: &str = "CURRENT.tmp";

/// Errors returned by the generation writer.
#[derive(Debug, thiserror::Error)]
pub enum WriterError {
    /// I/O layer error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// CAS store error during staging or retrieval.
    #[error("cas error: {0}")]
    Cas(#[from] crate::storage::cas::CasError),
    /// Manifest serialization error.
    #[error("manifest error: {0}")]
    Manifest(#[from] super::manifest::ManifestError),
    /// Attempted to publish without staging all required layers.
    #[error("cannot publish: missing layers: {0:?}")]
    MissingLayers(Vec<LayerKind>),
}

/// Writer that stages layer blobs into CAS and atomically publishes
/// generation manifests.
///
/// Usage:
/// ```ignore
/// let mut writer = GenerationWriter::new(storage_root, cas_store);
/// writer.stage(LayerKind::Db, &db_bytes)?;
/// writer.stage(LayerKind::Neural, &neural_bytes)?;
/// // ... stage all 5 layers
/// writer.publish(42)?;
/// ```
pub struct GenerationWriter {
    /// Root directory of the generation store (where `CURRENT` and
    /// `generations/` live).
    storage_root: PathBuf,
    /// Shared CAS store handle. Wrapped in `Arc<Mutex<...>>` to match the
    /// lease API and allow the writer to coexist with readers.
    cas: Arc<Mutex<CasStore>>,
    /// Layer-kind to CAS blob hash, accumulated by `stage()`.
    staged: HashMap<LayerKind, [u8; 32]>,
    /// Model identity to embed in the published manifest.
    model_identity: ModelIdentity,
    /// The last successfully published manifest (if any), so callers can
    /// inspect it after `publish()`.
    last_manifest: Option<Manifest>,
}

impl GenerationWriter {
    /// Create a new writer operating on `storage_root`, using the CAS at
    /// `storage_root/cas/` (the CAS root is derived from the store handle).
    pub fn new(storage_root: impl AsRef<Path>, cas: Arc<Mutex<CasStore>>) -> Self {
        GenerationWriter {
            storage_root: storage_root.as_ref().to_path_buf(),
            cas,
            staged: HashMap::new(),
            model_identity: ModelIdentity {
                name: String::new(),
                digest: String::new(),
                dimensions: 0,
            },
            last_manifest: None,
        }
    }

    /// Returns the storage root this writer operates on.
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// Set the model identity for the next published manifest.
    pub fn set_model_identity(&mut self, identity: ModelIdentity) {
        self.model_identity = identity;
    }

    /// Stage a layer's bytes into the CAS.
    ///
    /// Deduplication happens automatically: if the identical bytes were
    /// already staged (by this writer, a previous writer, or any other CAS
    /// client), the CAS `put` is a no-op and the same hash is returned.
    ///
    /// Staging the same layer twice with different bytes replaces the
    /// previously staged hash (the old blob remains in CAS but will be
    /// subject to normal GC rules).
    pub fn stage(&mut self, layer: LayerKind, bytes: &[u8]) -> Result<[u8; 32], WriterError> {
        let hash = {
            let cas = self.cas.lock().expect("cas mutex poisoned");
            cas.put(bytes)?
        };
        self.staged.insert(layer, hash);
        Ok(hash)
    }

    /// Get the CAS hash staged for `layer`, if any.
    pub fn staged_hash(&self, layer: LayerKind) -> Option<&[u8; 32]> {
        self.staged.get(&layer)
    }

    /// Clear staging state after a successful publish so the writer can be
    /// reused for the next generation.
    pub fn finish_publish(&mut self) {
        self.staged.clear();
    }

    /// Atomically publish generation `generation_number`.
    ///
    /// This method:
    /// 1. Validates all 5 layers are staged.
    /// 2. Builds a [`Manifest`] with computed fingerprints.
    /// 3. Writes `generations/<N>/manifest.partial`, fsyncs.
    /// 4. Renames to `generations/<N>/manifest`.
    /// 5. Atomically updates `CURRENT` to contain `<N>`.
    ///
    /// Returns `Err` if not all layers are staged.
    pub fn publish(&mut self, generation_number: u64) -> Result<(), WriterError> {
        self.publish_internal(generation_number, CrashPoint::None, true)
    }

    /// Write the manifest for `generation_number` WITHOUT updating `CURRENT`.
    ///
    /// This is the "publication without the final pointer swap" used by the
    /// one-time legacy→CAS migration (WS4 Task 10). It performs every step of
    /// [`publish`](Self::publish) except the `CURRENT` update: all 5 layers
    /// must be staged, the manifest is written via `manifest.partial` →
    /// fsync → rename, and the generation directory is fsynced. `CURRENT` is
    /// left untouched so the store continues to serve the pre-swap generation
    /// until the caller performs the final atomic swap.
    ///
    /// Returns `Err` if not all layers are staged. Staging state is cleared
    /// for reuse.
    pub fn publish_manifest_only(&mut self, generation_number: u64) -> Result<(), WriterError> {
        self.publish_internal(generation_number, CrashPoint::None, false)
    }

    // -----------------------------------------------------------------------
    // Crash-safety test helpers
    // -----------------------------------------------------------------------

    /// Write `manifest.partial` for `generation_number` but do NOT rename or
    /// update CURRENT. Used by crash-safety tests to simulate a crash after
    /// step 4 (manifest.partial write) but before step 6 (rename).
    #[doc(hidden)]
    pub fn write_manifest_partial_for_test(
        &mut self,
        generation_number: u64,
    ) -> Result<(), WriterError> {
        let manifest = self.build_manifest(generation_number)?;
        let gen_dir = self.generation_dir(generation_number);
        fs::create_dir_all(&gen_dir)?;
        let partial_path = gen_dir.join(MANIFEST_PARTIAL);
        let bytes = manifest.to_bytes()?;
        write_and_fsync(&partial_path, &bytes)?;
        Ok(())
    }

    /// Write `manifest` (with rename from partial) for `generation_number`
    /// but do NOT update CURRENT. Simulates a crash after the rename but
    /// before the CURRENT update.
    #[doc(hidden)]
    pub fn write_and_rename_manifest_for_test(
        &mut self,
        generation_number: u64,
    ) -> Result<(), WriterError> {
        let manifest = self.build_manifest(generation_number)?;
        let gen_dir = self.generation_dir(generation_number);
        fs::create_dir_all(&gen_dir)?;
        let partial_path = gen_dir.join(MANIFEST_PARTIAL);
        let final_path = gen_dir.join(MANIFEST_FILE);
        let bytes = manifest.to_bytes()?;
        write_and_fsync(&partial_path, &bytes)?;
        fs::rename(&partial_path, &final_path)?;

        // Fsync the generation directory for durability of the rename.
        if let Ok(dir) = fs::File::open(&gen_dir) {
            let _ = fsync_file(&dir);
        }
        Ok(())
    }

    /// Simulate a publish of `generation_number` that crashes at `kill_point`.
    ///
    /// `kill_point` values:
    /// - 0: after staging blob write (before fsync)
    /// - 1: after staging fsync (before blob rename) — already handled by CAS
    /// - 2: after blob rename — identical to successful staging
    /// - 3: after manifest.partial write (before fsync)
    /// - 4: after manifest.partial fsync (before rename)
    /// - 5: after manifest rename (before CURRENT write)
    /// - 6: after CURRENT write (this is actually success)
    ///
    /// In all cases the method returns `Ok(())` if the simulated steps
    /// succeeded, or an error if they failed.
    #[doc(hidden)]
    pub fn publish_with_simulated_crash(
        &mut self,
        generation_number: u64,
        kill_point: u8,
    ) -> Result<(), WriterError> {
        let crash = match kill_point {
            0 => CrashPoint::AfterStagingBlobWrite,
            1 => CrashPoint::AfterStagingFsync,
            2 => CrashPoint::AfterBlobRename,
            3 => CrashPoint::AfterManifestPartialWrite,
            4 => CrashPoint::AfterManifestFsync,
            5 => CrashPoint::AfterManifestRename,
            6 => CrashPoint::AfterCurrentWrite,
            _ => CrashPoint::None,
        };
        self.publish_internal(generation_number, crash, true)
    }

    /// Sweep leftover `manifest.partial` files from interrupted publishes.
    ///
    /// Called on startup/recovery. For each `generations/<N>/` directory,
    /// removes any `manifest.partial` files. Generations with only a partial
    /// manifest (no final `manifest`) are cleaned up.
    pub fn sweep_partial_manifests(&mut self) -> Result<(), WriterError> {
        let gens_dir = self.storage_root.join(GENERATIONS_DIR);
        if !gens_dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&gens_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let gen_dir = entry.path();
            let partial = gen_dir.join(MANIFEST_PARTIAL);
            if partial.exists() {
                let _ = fs::remove_file(&partial);
            }
            // If the generation directory has NO final manifest, remove the
            // entire directory (it was an interrupted publish).
            let manifest = gen_dir.join(MANIFEST_FILE);
            if !manifest.exists() {
                // Only remove if empty (after partial sweep) to avoid deleting
                // a valid generation directory that happens to have other
                // files.
                let is_empty = fs::read_dir(&gen_dir)?.next().is_none();
                if is_empty {
                    let _ = fs::remove_dir(&gen_dir);
                }
            }
        }
        Ok(())
    }

    /// The last successfully published manifest, if any.
    #[doc(hidden)]
    pub fn last_published_manifest(&self) -> Option<&Manifest> {
        self.last_manifest.as_ref()
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    /// Core publish logic with optional crash simulation.
    ///
    /// The publish sequence is split into discrete crash points that match
    /// the VAL-WRITER-005 kill scenarios. CAS staging (kill points 0-2) has
    /// already been completed atomically by [`stage`](Self::stage) before this
    /// method is called, so those crash points stop before any manifest or
    /// CURRENT file is written, leaving the last-good generation intact.
    ///
    /// When `update_current` is `false` the `CURRENT` pointer is left
    /// untouched (used by the legacy migration to prepare previous-generation
    /// manifests before the final atomic swap via [`publish`](Self::publish)).
    fn publish_internal(
        &mut self,
        generation_number: u64,
        crash: CrashPoint,
        update_current: bool,
    ) -> Result<(), WriterError> {
        // 1. Validate all layers are staged.
        let missing = ALL_LAYER_KINDS
            .iter()
            .filter(|k| !self.staged.contains_key(k))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(WriterError::MissingLayers(missing));
        }

        // CAS staging (crash points 0-2) was already completed atomically by
        // prior `stage()` calls. Simulate the crash by stopping now: no
        // manifest or CURRENT file is written, so the last-good generation
        // referenced by CURRENT remains intact and fully readable.
        if matches!(
            crash,
            CrashPoint::AfterStagingBlobWrite
                | CrashPoint::AfterStagingFsync
                | CrashPoint::AfterBlobRename
        ) {
            return Ok(());
        }

        // 2. Build the manifest with computed fingerprints.
        let manifest = self.build_manifest(generation_number)?;

        // 3. Ensure the generation directory exists.
        let gen_dir = self.generation_dir(generation_number);
        fs::create_dir_all(&gen_dir)?;

        // --- Steps for atomic manifest publish ---

        let partial_path = gen_dir.join(MANIFEST_PARTIAL);
        let manifest_path = gen_dir.join(MANIFEST_FILE);
        let manifest_bytes = manifest.to_bytes()?;

        // Step 4: write manifest.partial + fsync.
        write_and_fsync(&partial_path, &manifest_bytes)?;

        if crash == CrashPoint::AfterManifestPartialWrite {
            // Simulate crash: stop here. CURRENT is unchanged.
            return Ok(());
        }

        // Step 5: fsync already done in write_and_fsync above.
        if crash == CrashPoint::AfterManifestFsync {
            return Ok(());
        }

        // Step 6: rename manifest.partial -> manifest
        fs::rename(&partial_path, &manifest_path)?;

        // Fsync the generation directory for durability of the rename.
        if let Ok(dir) = fs::File::open(&gen_dir) {
            let _ = fsync_file(&dir);
        }

        if crash == CrashPoint::AfterManifestRename {
            // Simulate crash: manifest is on disk but CURRENT is unchanged.
            return Ok(());
        }

        // Step 7: atomically update CURRENT (skipped for manifest-only
        // publication, e.g. the migration's previous generation).
        if update_current {
            self.write_current_atomic(generation_number)?;
            // Record the published manifest.
            self.last_manifest = Some(manifest);
            // Clear staging for reuse.
            self.staged.clear();
        } else {
            // Manifest-only: record the manifest but leave CURRENT untouched.
            self.last_manifest = Some(manifest);
        }

        Ok(())
    }

    /// Build a [`Manifest`] from the currently staged layers.
    fn build_manifest(&self, generation_number: u64) -> Result<Manifest, WriterError> {
        // Compute graph fingerprint: blake3 of PDG + Symbols layer hashes.
        let pdg_hash = self.staged.get(&LayerKind::Pdg).copied();
        let symbols_hash = self.staged.get(&LayerKind::Symbols).copied();
        let graph_fingerprint = compute_graph_fingerprint(pdg_hash, symbols_hash);

        // Compute search fingerprint: blake3 of TF-IDF + Neural layer hashes.
        let tfidf_hash = self.staged.get(&LayerKind::Tfidf).copied();
        let neural_hash = self.staged.get(&LayerKind::Neural).copied();
        let search_fingerprint = compute_search_fingerprint(tfidf_hash, neural_hash);

        let mut layers = HashMap::new();
        for kind in ALL_LAYER_KINDS.iter() {
            if let Some(hash) = self.staged.get(kind) {
                layers.insert(*kind, *hash);
            }
        }

        Ok(Manifest {
            version: MANIFEST_VERSION,
            generation: generation_number,
            model_identity: self.model_identity.clone(),
            graph_fingerprint,
            search_fingerprint,
            layers,
        })
    }

    /// Get the directory for generation `N`.
    fn generation_dir(&self, generation_number: u64) -> PathBuf {
        self.storage_root
            .join(GENERATIONS_DIR)
            .join(generation_number.to_string())
    }

    /// Atomically write `CURRENT` to contain `generation_number`.
    ///
    /// Writes `CURRENT.tmp`, fsyncs, then renames to `CURRENT`.
    fn write_current_atomic(&self, generation_number: u64) -> Result<(), WriterError> {
        let tmp_path = self.storage_root.join(CURRENT_TMP);
        let final_path = self.storage_root.join(CURRENT_FILE);

        let content = format!("{}\n", generation_number);
        write_and_fsync(&tmp_path, content.as_bytes())?;

        fs::rename(&tmp_path, &final_path)?;

        // Best-effort: fsync the parent directory so the rename is durable.
        if let Ok(dir) = fs::File::open(&self.storage_root) {
            let _ = fsync_file(&dir);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Crash simulation points (test-only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrashPoint {
    /// No crash; complete the full publish sequence.
    None,
    /// Crash after writing the staging blob but before fsync.
    AfterStagingBlobWrite,
    /// Crash after staging fsync but before blob rename.
    AfterStagingFsync,
    /// Crash after blob rename (staging is complete).
    AfterBlobRename,
    /// Crash after writing manifest.partial but before fsync.
    AfterManifestPartialWrite,
    /// Crash after manifest.partial fsync but before rename.
    AfterManifestFsync,
    /// Crash after manifest rename but before CURRENT update.
    AfterManifestRename,
    /// Crash after CURRENT update (this is actually success, but we stop).
    AfterCurrentWrite,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `bytes` to `path` via create + write_all + flush + fsync.
///
/// The file is fully written and durable when this function returns Ok.
fn write_and_fsync(path: &Path, bytes: &[u8]) -> Result<(), WriterError> {
    let file = fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    writer.write_all(bytes)?;
    writer.flush()?;
    fsync_file(writer.get_ref())?;
    Ok(())
}

/// Compute the graph fingerprint (blake3) from the PDG and Symbols layer
/// hashes.
///
/// This is a structural fingerprint: it combines the CAS hashes of the graph
/// layers (PDG, Symbols) so that any change in either layer produces a
/// different fingerprint. The actual blob data is already content-addressed,
/// so hashing the hashes is a fixed-cost operation that captures the full
/// graph state.
fn compute_graph_fingerprint(
    pdg_hash: Option<[u8; 32]>,
    symbols_hash: Option<[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    if let Some(h) = pdg_hash {
        hasher.update(b"pdg:");
        hasher.update(&h);
    }
    if let Some(h) = symbols_hash {
        hasher.update(b"symbols:");
        hasher.update(&h);
    }
    hasher.finalize().into()
}

/// Compute the search fingerprint (blake3) from the TF-IDF and Neural layer
/// hashes.
fn compute_search_fingerprint(
    tfidf_hash: Option<[u8; 32]>,
    neural_hash: Option<[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    if let Some(h) = tfidf_hash {
        hasher.update(b"tfidf:");
        hasher.update(&h);
    }
    if let Some(h) = neural_hash {
        hasher.update(b"neural:");
        hasher.update(&h);
    }
    hasher.finalize().into()
}

#[cfg(test)]
#[path = "writer_test.rs"]
mod tests;
