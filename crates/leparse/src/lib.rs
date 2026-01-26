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

/// Parallel parsing implementation.
pub mod parallel;

/// Language-specific parsers.
pub mod languages;

/// Python language implementation.
pub mod python;

/// JavaScript and TypeScript language implementation.
pub mod javascript;

/// Go language implementation.
pub mod go;

/// Rust language implementation.
pub mod rust;

/// Java language implementation.
pub mod java;

/// C++ language implementation.
pub mod cpp;

/// C# language implementation.
pub mod csharp;

/// Ruby language implementation.
pub mod ruby;

/// PHP language implementation.
pub mod php;

// Swift language implementation - disabled due to tree-sitter version incompatibility
// pub mod swift;

// Kotlin language implementation - disabled due to tree-sitter version incompatibility
// pub mod kotlin;

// Dart language implementation - disabled due to parsing issues
// pub mod dart;

/// Lua language implementation.
pub mod lua;

/// Scala language implementation.
pub mod scala;

#[cfg(test)]
mod debug_go;

#[cfg(test)]
mod debug_go_returns;

#[cfg(test)]
mod debug_go_params;

#[cfg(test)]
mod debug_rust;

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

#[cfg(test)]
mod debug_csharp_ast;


#[cfg(test)]
mod debug_lua_ast;

