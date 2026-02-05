//! legraphe - Graph Intelligence Core
//!
//! *Le Graphe* (The Graph) - Program Dependence Graph with gravity-based traversal

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Multi-project graph integration and cross-referencing.
pub mod cross_project;
/// Graph node embedding and vector representation.
pub mod embedding;
/// Extraction logic for building PDGs from signatures.
pub mod extraction;
/// Program Dependence Graph implementation.
pub mod pdg;
/// Gravity-based graph traversal algorithms.
pub mod traversal;

pub use cross_project::{CrossProjectPDG, ExternalNodeRef, MergeError};
pub use embedding::NodeEmbedding;
pub use extraction::extract_pdg_from_signatures;
pub use pdg::{Edge, Node, ProgramDependenceGraph};
pub use traversal::{GravityTraversal, TraversalConfig};

/// Graph library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
