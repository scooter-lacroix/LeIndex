// leparse - Core Parsing Engine
//
// *Le Parse* (The Parsing) - Zero-copy AST extraction with multi-language support

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Core parsing traits and types for the LeIndex parsing engine.
pub mod traits;

/// Lazy-loaded grammar cache for memory-efficient parsing.
pub mod grammar;

/// AST node types and implementations.
pub mod ast;

/// Tests for AST zero-copy properties.
#[cfg(test)]
mod ast_tests;

/// Language-specific parsers.
pub mod languages;

/// Python language implementation.
pub mod python;

/// JavaScript and TypeScript language implementation.
pub mod javascript;

/// Re-exports of commonly used types.
pub mod prelude;

/// Test suite for leparse.
#[cfg(test)]
mod tests;

/// Library initialization.
pub fn init() {
    // Initialize logging if not already set up
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
