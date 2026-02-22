//! lestockage - Persistent Storage Layer
//!
//! *Le Stockage* (The Storage) - Extended SQLite schema with Salsa incremental computation

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Storage analytics and metrics.
pub mod analytics;
/// Cross-project reference resolution and graph merging.
pub mod cross_project;
/// Storage and retrieval of graph edges.
pub mod edges;
/// Global symbol table for cross-project indexing.
pub mod global_symbols;
/// Persistent storage for Program Dependence Graphs.
pub mod pdg_store;
/// Project metadata storage and retrieval.
pub mod project_metadata;
/// Unique project identification with BLAKE3 path hashing.
pub mod project_id;
/// Storage and retrieval of code nodes.
pub mod nodes;
/// Salsa-inspired incremental computation and caching.
pub mod salsa;
/// Database schema and connection management.
pub mod schema;
/// Configuration for Turso and hybrid storage backends.
pub mod turso_config;

pub use analytics::Analytics;
pub use cross_project::{CrossProjectResolver, MergeError, ResolutionError, ResolvedSymbol};
pub use edges::{EdgeRecord, EdgeStore};
pub use global_symbols::{
    DepType, ExternalRef, GlobalSymbol, GlobalSymbolError, GlobalSymbolId, GlobalSymbolTable,
    ProjectDep, RefType, SymbolType,
};
pub use nodes::{NodeRecord, NodeStore};
pub use pdg_store::{
    delete_pdg, load_pdg, pdg_exists, save_pdg, PdgStoreError, Result as PdgStoreResult,
};
pub use project_id::UniqueProjectId;
pub use project_metadata::{ProjectMetadata, ProjectMetadataError};
pub use salsa::{IncrementalCache, NodeHash};
pub use schema::{Storage, StorageConfig};
pub use turso_config::{HybridStorage, MigrationStats, StorageError, StorageMode, TursoConfig};

/// Storage library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
