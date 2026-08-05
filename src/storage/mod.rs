//! lestockage - Persistent Storage Layer
//!
//! *Le Stockage* (The Storage) - Extended SQLite schema with Salsa incremental computation

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Storage analytics and metrics.
pub mod analytics;
/// Content-addressed blob store (CAS) and refcount/GC infrastructure
/// (WS4 Tasks 1-2). See `docs/superpowers/plans/2026-08-04-ws4-generation-store.md`.
pub mod cas;
/// Bounded read-only catalog queries for exact MCP reads.
pub mod catalog;
/// Cross-project reference resolution and graph merging.
pub mod cross_project;
/// Storage and retrieval of graph edges.
pub mod edges;
/// Immutable mmap generation store: manifest format, leases, readers, writers
/// (WS4 Tasks 3+). See `docs/superpowers/plans/2026-08-04-ws4-generation-store.md`.
pub mod generation;
/// Global symbol table for cross-project indexing.
pub mod global_symbols;

/// Storage and retrieval of code nodes.
pub mod nodes;
/// Persistent storage for Program Dependence Graphs.
pub mod pdg_store;
/// Unique project identification with BLAKE3 path hashing.
pub mod project_id;
/// Project metadata storage and retrieval.
pub mod project_metadata;
/// Salsa-inspired incremental computation and caching.
pub mod salsa;
/// Database schema and connection management.
pub mod schema;
/// Configuration for Turso and hybrid storage backends.
#[cfg(feature = "turso")]
pub mod turso_config;

pub use analytics::Analytics;
/// CAS (content-addressed blob store) types.
pub use cas::{CasError as CasStoreError, CasStore, RetentionReport};
pub use catalog::{CatalogReader, CatalogSymbol};
pub use cross_project::{CrossProjectResolver, MergeError, ResolutionError, ResolvedSymbol};
pub use edges::{EdgeRecord, EdgeStore};
pub use generation::{
    GenerationLease, LayerKind, LeaseError, Manifest, ManifestError, ModelIdentity, NeuralDtype,
    NeuralReader, PdgEdge, PdgNode, PdgReader, ReaderError, SymbolEntry, SymbolReader, TfidfEntry,
    TfidfReader, VectorView,
};
pub use global_symbols::{
    DepType, ExternalRef, GlobalSymbol, GlobalSymbolError, GlobalSymbolId, GlobalSymbolTable,
    ProjectDep, RefType, SymbolType,
};
pub use nodes::{NodeRecord, NodeStore};
pub use pdg_store::{
    PdgStoreError, Result as PdgStoreResult, delete_pdg, load_pdg, pdg_exists, save_pdg,
};
pub use project_id::UniqueProjectId;
pub use project_metadata::{ProjectMetadata, ProjectMetadataError};
pub use salsa::{IncrementalCache, NodeHash};
pub use schema::{
    DEFAULT_READER_POOL_SIZE, Storage, StorageConfig, StoragePool, StoragePoolError, StorageRole,
};

#[cfg(feature = "turso")]
pub use turso_config::{HybridStorage, MigrationStats, StorageError, StorageMode, TursoConfig};

/// Storage library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
