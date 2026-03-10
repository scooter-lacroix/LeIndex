//! leglobal - Global Project Registry
//!
//! *Le Global* (The Global) - Persistent project discovery and registry

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Global project registry using local SQLite
pub mod registry;

/// Project discovery and scanning
pub mod discovery;

/// Background sync for keeping registry up to date
pub mod sync;

/// Tool discovery and execution utilities
pub mod tools;

/// Default database path for the registry
pub const DEFAULT_DB_PATH: &str = ".leindex/global_registry.db";

/// Initial backoff delay in seconds for sync retries
pub const INITIAL_BACKOFF_SECS: u64 = 1;
/// Maximum backoff delay in seconds for sync retries (5 minutes)
pub const MAX_BACKOFF_SECS: u64 = 300;

pub use registry::{GlobalRegistry, GlobalRegistryError, ProjectInfo, Result as RegistryResult};
