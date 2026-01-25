# Specification: leparse - Core Parsing Engine

**Track ID:** `leparse_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

### Vision

`leparse` (French for "The Parsing") is the Core Parsing Engine of the LeIndex Rust Renaissance. It provides zero-copy AST extraction with multi-language support using tree-sitter, forming the foundation for all deep code intelligence capabilities.

### The "Why"

**Current State:**
- Python-based parsing with good accuracy but high memory overhead
- ~400 bytes per node due to Python object overhead
- Limited to a subset of programming languages

**Target State:**
- Pure Rust parser with zero-copy AST architecture
- ~32 bytes per node (10x memory reduction)
- 17+ languages supported through tree-sitter
- Lazy-loaded grammars for minimal initial footprint

### Key Principles

1. **Zero-Copy Architecture** - AST nodes are byte-slice references, no String allocations
2. **Trait-Based Design** - `CodeIntelligence` trait for language-agnostic extraction
3. **Lazy Loading** - Grammars loaded on-demand to minimize memory
4. **Parallel Processing** - Rayon-based parallel parsing for large codebases

---

## Functional Requirements

### FR-1 Multi-Language AST Extraction

- Support for 17+ languages: Python, JavaScript, TypeScript, Go, Rust, Java, C++, C#, Ruby, PHP, Swift, Kotlin, Dart, Lua, Scala, Elixir, Haskell
- Tree-sitter based parsing with per-language `LanguageConfig`
- Lazy-loaded grammar loading to minimize initial memory footprint

### FR-2 Zero-Copy Architecture

- AST nodes represented as byte-slice references into source buffers
- No intermediate String allocations during parsing
- Direct memory mapping where possible

### FR-3 Trait-Based Extractor Pattern

```rust
pub trait CodeIntelligence {
    fn get_signatures(&self, source: &[u8]) -> Vec<SignatureInfo>;
    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Graph<Block, Edge>;
    fn extract_complexity(&self, node: &Node) -> ComplexityMetrics;
}
```

### FR-4 Symbol Identification

- Function signatures with parameters and return types
- Class definitions with methods and inheritance
- Module-level imports and dependencies
- Docstring extraction with semantic summarization

---

## Non-Functional Requirements

### Performance Targets

- **Parsing Speed:** Match or beat Python baseline for 50K files
- **Memory Efficiency:** Zero-copy wherever possible, ~32 bytes per node
- **Parallel Processing:** Rayon-based parallel file parsing

### Quality Requirements

- **Test Coverage:** >95% for all parsing logic
- **Validation:** Python validation tests for accuracy
- **Code Quality:** Pass clippy with no warnings

---

## Acceptance Criteria

**AC-1 Multi-Language Support**
- [ ] All 17+ languages parseable without errors
- [ ] Grammars lazy-load correctly
- [ ] Language-agnostic test suite passing

**AC-2 Zero-Copy Architecture**
- [ ] Zero-copy architecture verified (no unnecessary allocations)
- [ ] AST extraction matches Python baseline for accuracy
- [ ] Memory usage reduced by 10x vs Python

**AC-3 Trait Implementation**
- [ ] `CodeIntelligence` trait implemented for all languages
- [ ] All trait methods produce correct results

---

## Dependencies

### Internal Dependencies
- None (first sub-track in dependency chain)

### External Rust Crates
- `tree-sitter` (parsing infrastructure)
- `tree-sitter-python`, `tree-sitter-javascript`, etc. (language grammars)
- `rayon` (parallel processing)
- `serde` (serialization)

---

## Out of Scope

- **No LSP Integration** - LSP can be built on top by others
- **No Custom Parsers** - All parsing done via tree-sitter
- **No Syntax Highlighting** - Focus on AST extraction only
