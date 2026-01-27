//! lestockage - Persistent Storage Layer
//!
//! *Le Stockage* (The Storage) - Extended SQLite schema with Salsa incremental computation

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Database schema and connection management.
pub mod schema;
/// Storage and retrieval of code nodes.
pub mod nodes;
/// Storage and retrieval of graph edges.
pub mod edges;
/// Salsa-inspired incremental computation and caching.
pub mod salsa;
/// Storage analytics and metrics.
pub mod analytics;
/// Persistent storage for Program Dependence Graphs.
pub mod pdg_store;
/// Global symbol table for cross-project indexing.
pub mod global_symbols;
/// Cross-project reference resolution and graph merging.
pub mod cross_project;
/// Configuration for Turso and hybrid storage backends.
pub mod turso_config;

pub use schema::{Storage, StorageConfig};
pub use nodes::{NodeStore, NodeRecord};
pub use edges::{EdgeStore, EdgeRecord};
pub use salsa::{IncrementalCache, NodeHash};
pub use analytics::Analytics;
pub use pdg_store::{save_pdg, load_pdg, pdg_exists, delete_pdg, PdgStoreError, Result as PdgStoreResult};
pub use global_symbols::{
    GlobalSymbolTable, GlobalSymbol, GlobalSymbolId, SymbolType,
    ExternalRef, RefType, ProjectDep, DepType, GlobalSymbolError
};
pub use cross_project::{
    CrossProjectResolver, ResolvedSymbol, ResolutionError,
    MergeError
};
pub use turso_config::{
    TursoConfig, HybridStorage, StorageMode, MigrationStats, StorageError
};

/// Storage library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
