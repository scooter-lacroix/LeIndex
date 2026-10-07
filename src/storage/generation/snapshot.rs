//! Generation read-path snapshot: a leased, immutable view over the current
//! generation (WS4 Task 14).
//!
//! The search/symbol/deep-analyze MCP read path acquires a
//! [`GenerationLease`] and reads from the generation's content-addressed mmap
//! layers instead of the legacy heap-mirror structures. This module is the
//! read side of that contract:
//!
//! - The `Db` layer is the SQLite catalog. SQLite needs a regular file (it
//!   cannot mmap-read out of a framed CAS blob), so the decoded payload is
//!   materialized once into a temp-owned file. Reads are served by SQLite's
//!   own page cache; the genesis data still lives in the generation blob which
//!   the lease pins for our whole lifetime.
//! - Every other layer (`Neural`, `Pdg`, `Symbols`, `Tfidf`) is opened
//!   **directly on the CAS blob file** via [`NeuralReader`] / [`PdgReader`] /
//!   [`SymbolReader`] / [`TfidfReader`]. Those readers `mmap` the blob in
//!   place, so no payload bytes are copied into heap. The decoded `Db` layer
//!   file is exposed via [`GenerationSnapshot::db_path`] for read-only
//!   hydration.
//!
//! Acquiring a lease never touches the LeIndex writer `Mutex` or
//! `ProjectWriteLock`; it operates purely on CAS refcounts. That is the
//! no-stall / no-writer-contention invariant (architecture section 4.1,
//! VAL-EQUIV-002/003).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::storage::cas::CasStore;

use super::lease::{GenerationLease, read_current_generation, read_generation_manifest};
use super::manifest::{LayerKind, Manifest};
use super::reader::{NeuralReader, PdgReader, SymbolReader, TfidfReader};

/// Errors returned while opening a [`GenerationSnapshot`].
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// The storage root has no `CURRENT` pointer, so there is no generation to
    /// read (the project was never migrated / published).
    #[error("no current generation for storage root {0}")]
    NoCurrentGeneration(PathBuf),
    /// The current generation's manifest could not be read or validated.
    #[error("manifest error: {0}")]
    Manifest(#[from] super::manifest::ManifestError),
    /// The lease could not be acquired (refcount persistence failure, etc.).
    #[error("lease error: {0}")]
    Lease(#[from] super::lease::LeaseError),
    /// A CAS I/O error while resolving blob data.
    #[error("cas error: {0}")]
    Cas(#[from] crate::storage::cas::CasError),
    /// Plain I/O error materializing the Db layer or opening a reader.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A layer reader failed to open (missing/corrupt blob).
    #[error("reader error: {0}")]
    Reader(#[from] super::reader::ReaderError),
    /// The generation manifest is missing the `Db` layer, which the read path
    /// requires to hydrate the PDG and search index.
    #[error("generation {generation} is missing the Db layer")]
    MissingDbLayer {
        /// The generation number whose manifest lacks the `Db` layer.
        generation: u64,
    },
}

/// A leased, immutable read view over the current generation.
///
/// The snapshot holds a [`GenerationLease`] that pins every layer blob of the
/// generation for the lifetime of the snapshot, so garbage collection cannot
/// reap the data a reader depends on. The four mmap layers are opened directly
/// on the CAS blob files (zero-copy); the `Db` layer is materialized into a
/// temp-owned SQLite file exposed via [`GenerationSnapshot::db_path`] for
/// read-only hydration.
pub struct GenerationSnapshot {
    /// Storage root the snapshot was opened against (`.leindex/`).
    storage_root: PathBuf,
    /// The generation's manifest (metadata + layer→hash map).
    manifest: Manifest,
    /// Lease pinning the generation's layer blobs. Dropping the snapshot
    /// releases the pins.
    _lease: GenerationLease,
    /// Shared CAS handle used to resolve blob file paths.
    _cas: Arc<Mutex<CasStore>>,
    /// Temp-owned directory holding the decoded `Db` layer.
    _db_tempdir: tempfile::TempDir,
    /// Path to the decoded `Db` layer (a regular SQLite file).
    db_path: PathBuf,
    /// Direct mmap readers over the generation's CAS blob files.
    neural: Option<NeuralReader>,
    tfidf: Option<TfidfReader>,
    pdg: Option<PdgReader>,
    symbols: Option<SymbolReader>,
}

impl GenerationSnapshot {
    /// Open a leased snapshot over the current generation at `storage_root`.
    ///
    /// Steps:
    /// 1. Read `CURRENT` and the generation's `manifest`.
    /// 2. Open the CAS store and acquire a [`GenerationLease`] over every
    ///    layer blob (pins them via refcount).
    /// 3. Materialize the `Db` layer into a temp-owned SQLite file.
    /// 4. Open the `Neural`/`Pdg`/`Symbols`/`Tfidf` mmap readers directly on
    ///    their CAS blob files.
    ///
    /// Fails if there is no current generation, the manifest is invalid, or
    /// any required layer is missing/corrupt.
    pub fn open(storage_root: impl AsRef<Path>) -> Result<Self, SnapshotError> {
        let storage_root = storage_root.as_ref().to_path_buf();
        let generation = read_current_generation(&storage_root)
            .ok_or_else(|| SnapshotError::NoCurrentGeneration(storage_root.clone()))?;
        let manifest = read_generation_manifest(&storage_root, generation)?;

        let cas = Arc::new(Mutex::new(CasStore::open(storage_root.join("cas"))?));
        let lease = GenerationLease::acquire(cas.clone(), &manifest)?;

        // Db layer: decode + materialize the SQLite catalog into a temp file.
        let db_hash = manifest
            .layers
            .get(&LayerKind::Db)
            .copied()
            .ok_or(SnapshotError::MissingDbLayer { generation })?;
        let db_bytes = {
            let store = cas.lock().expect("cas store mutex poisoned");
            store.get(&db_hash)?
        };
        let db_tempdir = tempfile::tempdir()?;
        let db_path = db_tempdir.path().join("leindex.db");
        fs::write(&db_path, &db_bytes)?;

        // Mmap layers: open the readers directly on the CAS blob files so no
        // payload bytes are copied into heap.
        let blob = |cas: &Arc<Mutex<CasStore>>, hash: &[u8; 32]| {
            cas.lock()
                .expect("cas store mutex poisoned")
                .blob_path(hash)
        };
        let neural = manifest
            .layers
            .get(&LayerKind::Neural)
            .map(|h| NeuralReader::open(&blob(&cas, h)))
            .transpose()?;
        let tfidf = manifest
            .layers
            .get(&LayerKind::Tfidf)
            .map(|h| TfidfReader::open(&blob(&cas, h)))
            .transpose()?;
        let pdg = manifest
            .layers
            .get(&LayerKind::Pdg)
            .map(|h| PdgReader::open(&blob(&cas, h)))
            .transpose()?;
        let symbols = manifest
            .layers
            .get(&LayerKind::Symbols)
            .map(|h| SymbolReader::open(&blob(&cas, h)))
            .transpose()?;

        Ok(GenerationSnapshot {
            storage_root,
            manifest,
            _lease: lease,
            _cas: cas,
            _db_tempdir: db_tempdir,
            db_path,
            neural,
            tfidf,
            pdg,
            symbols,
        })
    }

    /// The generation number this snapshot reads.
    pub fn generation(&self) -> u64 {
        self.manifest.generation
    }

    /// The generation's manifest (metadata + layer→hash map).
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The storage root the snapshot was opened against.
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// Path to the decoded `Db` layer (a regular SQLite file). Callers use
    /// this to hydrate the PDG and search index from the generation.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The snapshot-owned temp directory (it holds the decoded `Db` layer and
    /// is deleted with the snapshot). Hydration materializes the search
    /// layers here under their legacy file names.
    pub fn artifact_dir(&self) -> &Path {
        self._db_tempdir.path()
    }

    /// Neural embedding reader (mmap over the generation's neural blob).
    pub fn neural(&self) -> Option<&NeuralReader> {
        self.neural.as_ref()
    }

    /// TF-IDF reader (mmap over the generation's tfidf blob).
    pub fn tfidf(&self) -> Option<&TfidfReader> {
        self.tfidf.as_ref()
    }

    /// PDG reader (mmap over the generation's pdg blob).
    pub fn pdg(&self) -> Option<&PdgReader> {
        self.pdg.as_ref()
    }

    /// Symbols reader (mmap over the generation's symbols blob).
    pub fn symbols(&self) -> Option<&SymbolReader> {
        self.symbols.as_ref()
    }

    /// The decoded payload of `kind`'s CAS blob, or `None` when the manifest
    /// does not list the layer (optional layers are absent on older stores).
    pub fn layer_bytes(&self, kind: LayerKind) -> Result<Option<Vec<u8>>, SnapshotError> {
        let Some(hash) = self.manifest.layers.get(&kind) else {
            return Ok(None);
        };
        let store = self._cas.lock().expect("cas store mutex poisoned");
        Ok(Some(store.get(hash)?))
    }
}

/// Whether `manifest`'s Neural layer carries vectors, i.e. is not the
/// canonical empty payload staged when no neural model ran. Decided from the
/// manifest hash alone — no blob or file is opened.
pub fn manifest_has_neural_vectors(manifest: &Manifest) -> bool {
    let empty = crate::storage::cas::blob::blob_hash(&super::migrate::encode_empty_neural());
    manifest
        .layers
        .get(&LayerKind::Neural)
        .is_some_and(|hash| *hash != empty)
}

#[cfg(test)]
#[path = "snapshot_test.rs"]
mod tests;
