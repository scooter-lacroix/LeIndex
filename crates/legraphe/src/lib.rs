//! legraphe - Graph Intelligence Core
//!
//! *Le Graphe* (The Graph) - Program Dependence Graph with gravity-based traversal

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Program Dependence Graph implementation.
pub mod pdg;
/// Gravity-based graph traversal algorithms.
pub mod traversal;
/// Graph node embedding and vector representation.
pub mod embedding;
/// Extraction logic for building PDGs from signatures.
pub mod extraction;
/// Multi-project graph integration and cross-referencing.
pub mod cross_project;

pub use pdg::{ProgramDependenceGraph, Node, Edge};
pub use traversal::{GravityTraversal, TraversalConfig};
pub use embedding::NodeEmbedding;
pub use extraction::extract_pdg_from_signatures;
pub use cross_project::{CrossProjectPDG, ExternalNodeRef, MergeError};

/// Graph library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
