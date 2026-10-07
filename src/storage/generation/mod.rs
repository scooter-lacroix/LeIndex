//! Immutable mmap generation store: manifest format, leases, readers, writers.
//!
//! A generation is an immutable snapshot of all index layers (DB, TF-IDF,
//! neural, PDG, symbols) published as a [`Manifest`] referencing content-addressed
//! CAS blobs by blake3 hash. Readers acquire a [`GenerationLease`] that pins
//! the referenced blobs via CAS refcounts, enabling zero-copy mmap reads that
//! never touch the writer Mutex. See BENCHMARKS.md Sections 2 and 9 for the
//! digested generation-store design and CAS engineering decisions.

/// DB layer normalisation: copy a live SQLite DB into CAS via VACUUM
/// (WS4 Task 8). See `db_layer` module docs.
pub mod db_layer;
pub mod graph_codec;
pub mod lease;
pub mod manifest;
pub mod migrate;
pub mod reader;
pub mod retention;
pub mod snapshot;
pub mod writer;

pub use db_layer::{DbToCasError, db_to_cas, db_to_cas_conn};
pub use lease::{
    CURRENT_FILE, GENERATIONS_DIR, GenerationLease, LeaseError, MANIFEST_FILE,
    read_current_generation, read_generation_manifest,
};
pub use manifest::{
    ALL_LAYER_KINDS, LayerKind, MANIFEST_MAGIC, MANIFEST_VERSION, Manifest, ManifestBody,
    ManifestError, ModelIdentity,
};
pub use migrate::{
    DEFAULT_FOOTPRINT_GOAL_BYTES, MigrationConfig, MigrationError, MigrationReport,
    is_legacy_full_copy_layout, is_migrated_store, migrate_legacy_store,
};
pub use reader::{
    NeuralDtype, NeuralReader, PdgEdge, PdgNode, PdgReader, ReaderError, SymbolEntry, SymbolReader,
    TfidfEntry, TfidfReader, VectorView,
};
pub use retention::{
    DEFAULT_JOB_BYTES_MAX, DEFAULT_MAX_GENERATIONS, GenerationRetentionReport, RetentionConfig,
    RetentionError, retain_after_publish, retention_report,
};
pub use snapshot::{GenerationSnapshot, SnapshotError, manifest_has_neural_vectors};
pub use writer::{GenerationWriter, WriterError};
