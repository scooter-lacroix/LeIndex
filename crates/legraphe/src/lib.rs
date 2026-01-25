// legraphe - Graph Intelligence Core
//
// *Le Graphe* (The Graph) - Program Dependence Graph with gravity-based traversal

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod pdg;
pub mod traversal;
pub mod embedding;

pub use pdg::{ProgramDependenceGraph, Node, Edge};
pub use traversal::{GravityTraversal, TraversalConfig};
pub use embedding::NodeEmbedding;

/// Graph library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
