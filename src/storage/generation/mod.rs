//! Immutable mmap generation store: manifest format, leases, readers, writers.
//!
//! A generation is an immutable snapshot of all index layers (DB, TF-IDF,
//! neural, PDG, symbols) published as a [`Manifest`] referencing content-addressed
//! CAS blobs by blake3 hash. Readers acquire a [`GenerationLease`] that pins
//! the referenced blobs via CAS refcounts, enabling zero-copy mmap reads that
//! never touch the writer Mutex. See
//! `docs/superpowers/plans/2026-08-04-ws4-generation-store.md` for the full
//! design.

pub mod lease;
pub mod manifest;
pub mod reader;

pub use lease::{
    CURRENT_FILE, GENERATIONS_DIR, GenerationLease, LeaseError, MANIFEST_FILE,
    read_current_generation, read_generation_manifest,
};
pub use manifest::{
    LayerKind, MANIFEST_MAGIC, MANIFEST_VERSION, Manifest, ManifestBody, ManifestError,
    ModelIdentity,
};
pub use reader::{
    NeuralDtype, NeuralReader, PdgEdge, PdgNode, PdgReader, ReaderError, SymbolEntry, SymbolReader,
    TfidfEntry, TfidfReader, VectorView,
};
