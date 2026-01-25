// lestockage - Persistent Storage Layer
//
// *Le Stockage* (The Storage) - Extended SQLite schema with Salsa incremental computation

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod schema;
pub mod nodes;
pub mod edges;
pub mod salsa;
pub mod analytics;

pub use schema::{Storage, StorageConfig};
pub use nodes::{NodeStore, NodeRecord};
pub use edges::{EdgeStore, EdgeRecord};
pub use salsa::{IncrementalCache, NodeHash};
pub use analytics::Analytics;

/// Storage library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
