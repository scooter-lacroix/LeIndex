//! LeIndex - Unified semantic code search engine
//!
//! This crate provides a complete code indexing and search solution
//! with the following modules:
//!
//! - `parse`: Code parsing and AST generation
//! - `graph`: Dependency graph construction
//! - `storage`: Persistent storage layer
//! - `search`: Vector search engine with INT8 quantization
//! - `phase`: Multi-phase indexing pipeline
//! - `cli`: Command-line interface
//! - `global`: Global operations
//! - `server`: HTTP API server
//! - `edit`: Code editing utilities
//! - `validation`: Index validation tools
//!
//! ## Installation
//!
//! ```bash
//! cargo install leindex
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! use leindex::search::SearchEngine;
//!
//! // Initialize search
//! let engine = SearchEngine::new();
//! ```

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

// Core modules (base of dependency DAG)
#[cfg(feature = "parse")]
pub mod parse;

#[cfg(feature = "graph")]
pub mod graph;

#[cfg(feature = "storage")]
pub mod storage;

#[cfg(feature = "search")]
pub mod search;

// Extended modules
#[cfg(feature = "phase")]
pub mod phase;

#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "global")]
pub mod global;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "edit")]
pub mod edit;

#[cfg(feature = "validation")]
pub mod validation;

// Re-exports for backward compatibility
// Users can use either `leindex::parse` or `leindex::leparse`

#[cfg(feature = "parse")]
#[doc(hidden)]
pub use parse as leparse;

#[cfg(feature = "graph")]
#[doc(hidden)]
pub use graph as legraphe;

#[cfg(feature = "storage")]
#[doc(hidden)]
pub use storage as lestockage;

#[cfg(feature = "search")]
#[doc(hidden)]
pub use search as lerecherche;

#[cfg(feature = "phase")]
#[doc(hidden)]
pub use phase as lephase;

#[cfg(feature = "cli")]
#[doc(hidden)]
pub use cli as lepasserelle;

#[cfg(feature = "global")]
#[doc(hidden)]
pub use global as leglobal;

#[cfg(feature = "server")]
#[doc(hidden)]
pub use server as leserve;

#[cfg(feature = "edit")]
#[doc(hidden)]
pub use edit as leedit;

#[cfg(feature = "validation")]
#[doc(hidden)]
pub use validation as levalidation;

// Public API re-exports for convenience

#[cfg(feature = "search")]
pub use search::SearchEngine;

#[cfg(feature = "cli")]
pub use cli::Cli;
