# Implementation Plan: leparse - Core Parsing Engine

**Track ID:** `leparse_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Core Parsing Engine for LeIndex Rust Renaissance. It provides zero-copy AST extraction with multi-language support using tree-sitter.

---

## Phase 1: Tree-Sitter Integration

### Objective
Set up tree-sitter infrastructure and language grammars.

- [x] **Task 1.1: Add tree-sitter dependencies**
  - [x] Add tree-sitter to `Cargo.toml`
  - [x] Add language grammar crates for all 17+ languages
  - [x] Configure build scripts for grammar compilation
  - [x] Verify tree-sitter compiles successfully

- [x] **Task 1.2: Create LanguageConfig structures**
  - [x] Define `LanguageConfig` struct per language
  - [x] Implement language detection (file extension based)
  - [x] Create language registry for runtime lookup
  - [x] Write tests for language detection

- [x] **Task 1.3: Implement lazy-loaded grammar loading**
  - [x] Create grammar cache with lazy initialization
  - [x] Implement thread-safe grammar storage
  - [x] Add memory-efficient grammar pooling
  - [x] Write tests for grammar loading correctness

- [x] **Task: Maestro - Phase 1 Verification** (Tzar Review: ✅ PASS)
  - Fixed: Duplicate unsafe FFI declarations removed (now uses centralized loading)
  - Fixed: Unified language registry (LanguageId.config() delegates to LanguageConfig)
  - Fixed: Dead code paths removed (extract_function_definitions, extract_class_definitions)
  - Fixed: Recursion bug in extract_all_definitions (now handles nested classes/functions)
  - Fixed: MSRV violation (replaced std::sync::LazyLock with once_cell::sync::Lazy)

---

## Phase 2: AST Node Types

### Objective
Define zero-copy AST node types and structures.

- [x] **Task 2.1: Define core AST types**
  - [x] Create `AstNode` struct with byte-slice references
  - [x] Implement `SignatureInfo` for function/class signatures
  - [x] Define `FunctionElement`, `ClassElement`, `ModuleElement`
  - [x] Add documentation for all types

- [x] **Task 2.2: Implement zero-copy node references**
  - [x] Use `&[u8]` for source text references
  - [x] Implement lifetime-safe AST node borrowing
  - [x] Add tests verifying zero-copy properties
  - [x] Benchmark memory usage vs string-based approach

- [x] **Task 2.3: Implement docstring extraction**
  - [x] Extract docstrings from AST nodes
  - [x] Implement semantic summarization (basic)
  - [x] Add docstring storage in node metadata
  - [x] Write tests for docstring extraction

- [x] **Task: Maestro - Phase 2 Verification** (Tzar Review: ✅ PASS)
  - Fixed: Type duplication removed (Visibility, Parameter now imported from traits.rs)
  - Fixed: Added bounds checking to all text() methods (returns Result instead of panicking)
  - Fixed: ZeroCopyText trait moved from tests to src/ast.rs
  - Fixed: Import struct now includes byte_range for zero-copy
  - Fixed: NodeMetadata uses byte ranges (name_range, docstring_range) instead of owned strings
  - Updated: Tests use new zero-copy API (get_text(), get_name(), get_docstring())

---

## Phase 3: CodeIntelligence Trait

### Objective
Implement the trait-based extractor pattern.

- [x] **Task 3.1: Define CodeIntelligence trait**
  - [x] Create trait definition with required methods
  - [x] Add documentation for trait methods
  - [x] Define associated types for trait
  - [x] Write trait documentation examples

- [x] **Task 3.2: Implement Python language support**
  - [x] Implement `CodeIntelligence` for Python
  - [x] Add `get_signatures()` extraction
  - [x] Add `compute_cfg()` control flow graph generation
  - [x] Add `extract_complexity()` metrics calculation

- [ ] **Task 3.3: Implement JavaScript/TypeScript support**
  - [ ] Implement `CodeIntelligence` for JavaScript
  - [ ] Implement `CodeIntelligence` for TypeScript
  - [ ] Handle TS-specific syntax (types, interfaces)
  - [ ] Write tests for JS/TS extraction

- [ ] **Task 3.4: Implement Go language support**
  - [ ] Implement `CodeIntelligence` for Go
  - [ ] Handle Go-specific syntax (interfaces, goroutines)
  - [ ] Write tests for Go extraction

- [ ] **Task 3.5: Implement Rust language support**
  - [ ] Implement `CodeIntelligence` for Rust
  - [ ] Handle Rust-specific syntax (traits, lifetimes)
  - [ ] Write tests for Rust extraction

- [ ] **Task 3.6: Implement remaining 13+ languages**
  - [ ] Implement `CodeIntelligence` for Java
  - [ ] Implement for C++
  - [ ] Implement for C#
  - [ ] Implement for Ruby, PHP, Swift, Kotlin, Dart, Lua, Scala, Elixir, Haskell
  - [ ] Create language-agnostic test suite

- [ ] **Task: Maestro - Phase 3 Verification**

---

## Phase 4: Parallel Parsing

### Objective
Implement parallel file parsing with rayon.

- [ ] **Task 4.1: Implement parallel parsing pipeline**
  - [ ] Create parallel file iterator with rayon
  - [ ] Implement thread-safe AST node pooling
  - [ ] Add error handling for parse failures
  - [ ] Write tests for parallel correctness

- [ ] **Task 4.2: Optimize for large workloads**
  - [ ] Benchmark against Python baseline
  - [ ] Tune rayon thread pool settings
  - [ ] Add adaptive batch sizing
  - [ ] Optimize for 50K+ file workloads

- [ ] **Task 4.3: Python validation tests**
  - [ ] Compare AST extraction vs Python baseline
  - [ ] Validate accuracy on sample codebases
  - [ ] Document any accuracy differences
  - [ ] Create regression test suite

- [ ] **Task: Maestro - Phase 4 Verification**

---

## Success Criteria

The track is complete when:

1. **All 17+ languages parseable** - Tree-sitter grammars working for all target languages
2. **Zero-copy verified** - No unnecessary allocations in hot paths
3. **Accuracy validated** - AST extraction matches Python baseline
4. **Parallel processing working** - Rayon-based parsing with correct thread safety
5. **Tests passing** - >95% coverage, all tests green
6. **Code quality** - No clippy warnings, well-documented

---

## Notes

- **Reference Code Only:** Code in `Rust_ref(DO_NOT_DIRECTLY_USE)` is for reference only
- **Greenfield:** Write all parsing code from scratch using tree-sitter patterns
- **Python Validation:** New Rust tests validated against Python behavior
