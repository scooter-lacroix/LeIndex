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

- [ ] **Task 1.1: Add tree-sitter dependencies**
  - [ ] Add tree-sitter to `Cargo.toml`
  - [ ] Add language grammar crates for all 17+ languages
  - [ ] Configure build scripts for grammar compilation
  - [ ] Verify tree-sitter compiles successfully

- [ ] **Task 1.2: Create LanguageConfig structures**
  - [ ] Define `LanguageConfig` struct per language
  - [ ] Implement language detection (file extension based)
  - [ ] Create language registry for runtime lookup
  - [ ] Write tests for language detection

- [ ] **Task 1.3: Implement lazy-loaded grammar loading**
  - [ ] Create grammar cache with lazy initialization
  - [ ] Implement thread-safe grammar storage
  - [ ] Add memory-efficient grammar pooling
  - [ ] Write tests for grammar loading correctness

- [ ] **Task: Maestro - Phase 1 Verification**

---

## Phase 2: AST Node Types

### Objective
Define zero-copy AST node types and structures.

- [ ] **Task 2.1: Define core AST types**
  - [ ] Create `AstNode` struct with byte-slice references
  - [ ] Implement `SignatureInfo` for function/class signatures
  - [ ] Define `FunctionElement`, `ClassElement`, `ModuleElement`
  - [ ] Add documentation for all types

- [ ] **Task 2.2: Implement zero-copy node references**
  - [ ] Use `&[u8]` for source text references
  - [ ] Implement lifetime-safe AST node borrowing
  - [ ] Add tests verifying zero-copy properties
  - [ ] Benchmark memory usage vs string-based approach

- [ ] **Task 2.3: Implement docstring extraction**
  - [ ] Extract docstrings from AST nodes
  - [ ] Implement semantic summarization (basic)
  - [ ] Add docstring storage in node metadata
  - [ ] Write tests for docstring extraction

- [ ] **Task: Maestro - Phase 2 Verification**

---

## Phase 3: CodeIntelligence Trait

### Objective
Implement the trait-based extractor pattern.

- [ ] **Task 3.1: Define CodeIntelligence trait**
  - [ ] Create trait definition with required methods
  - [ ] Add documentation for trait methods
  - [ ] Define associated types for trait
  - [ ] Write trait documentation examples

- [ ] **Task 3.2: Implement Python language support**
  - [ ] Implement `CodeIntelligence` for Python
  - [ ] Add `get_signatures()` extraction
  - [ ] Add `compute_cfg()` control flow graph generation
  - [ ] Add `extract_complexity()` metrics calculation

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
