# Implementation Plan: lepasserelle - Bridge & Integration

**Track ID:** `lepasserelle_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Bridge & Integration layer for LeIndex Rust Renaissance. It creates PyO3 FFI bindings and the unified MCP tool.

---

## Phase 1: PyO3 Module Setup

### Objective
Create Python module with PyO3 bindings.

- [ ] **Task 1.1: Create leindex_rust Python module**
  - [ ] Set up PyO3 module structure
  - [ ] Configure build with maturin
  - [ ] Add module initialization
  - [ ] Test module imports correctly

- [ ] **Task 1.2: Expose RustAnalyzer class**
  - [ ] Create RustAnalyzer struct
  - [ ] Expose via PyO3 wrapper
  - [ ] Add constructor, methods
  - [ ] Write Python usage tests

- [ ] **Task 1.3: Expose build_weighted_context function**
  - [ ] Create function signature
  - [ ] Handle Python types conversion
  - [ ] Add error handling
  - [ ] Write function tests

- [ ] **Task 1.4: Add Python-friendly error handling**
  - [ ] Define Rust error types with thiserror
  - [ ] Convert to Python exceptions
  - [ ] Add error context to messages
  - [ ] Write error handling tests

- [ ] **Task 1.5: Write FFI contract tests**
  - [ ] Test type conversions
  - [ ] Test error propagation
  - [ ] Test memory safety
  - [ ] Validate FFI contracts

- [ ] **Task: Maestro - Phase 1 Verification**

---

## Phase 2: Zero-Copy Data Transfer

### Objective
Implement zero-copy data transfer across FFI boundary.

- [ ] **Task 2.1: Implement mmap for source files**
  - [ ] Create mmap wrapper for file access
  - [ ] Handle file mapping across FFI
  - [ ] Add lifetime safety
  - [ ] Write tests for mmap correctness

- [ ] **Task 2.2: Create shared memory buffers**
  - [ ] Implement shared buffer allocation
  - [ ] Handle buffer sharing
  - [ ] Add buffer pooling
  - [ ] Write tests for shared buffers

- [ ] **Task 2.3: Add zero-copy embedding transfer**
  - [ ] Transfer embeddings without copying
  - [ ] Use byte slices for vector data
  - [ ] Handle alignment requirements
  - [ ] Write tests for zero-copy

- [ ] **Task 2.4: Optimize FFI boundary crossings**
  - [ ] Minimize crossings
  - [ ] Batch operations where possible
  - [ ] Profile FFI overhead
  - [ ] Write performance tests

- [ ] **Task 2.5: Benchmark transfer overhead**
  - [ ] Measure copy vs zero-copy
  - [ ] Profile FFI call overhead
  - [ ] Document performance characteristics
  - [ ] Optimize hot paths

- [ ] **Task: Maestro - Phase 2 Verification**

---

## Phase 3: Unified MCP Tool

### Objective
Implement unified MCP tool for deep code analysis.

- [ ] **Task 3.1: Implement leindex_deep_analyze tool**
  - [ ] Create MCP tool definition
  - [ ] Add tool parameters (query, budget, project_path)
  - [ ] Implement tool handler
  - [ ] Write tool tests

- [ ] **Task 3.2: Add semantic search entry point**
  - [ ] Integrate with lerecherche for semantic search
  - [ ] Handle query → embedding → search pipeline
  - [ ] Return top-K results
  - [ ] Write integration tests

- [ ] **Task 3.3: Integrate Rust graph expansion**
  - [ ] Call Rust gravity traversal from MCP
  - [ ] Handle context expansion
  - [ ] Manage token budget
  - [ ] Write expansion tests

- [ ] **Task 3.4: Create LLM-ready summary format**
  - [ ] Format results for LLM consumption
  - [ ] Add relevance highlighting
  - [ ] Include metadata (complexity, centrality)
  - [ ] Write format tests

- [ ] **Task 3.5: Write MCP tool tests**
  - [ ] Test end-to-end workflows
  - [ ] Test error handling
  - [ ] Test edge cases
  - [ ] Validate output format

- [ ] **Task: Maestro - Phase 3 Verification**

---

## Phase 4: Memory Management

### Objective
Implement memory-aware spilling and resource management.

- [ ] **Task 4.1: Implement RSS monitoring**
  - [ ] Add psutil bindings via PyO3
  - [ ] Monitor RSS usage
  - [ ] Check thresholds periodically
  - [ ] Write monitoring tests

- [ ] **Task 4.2: Add 90% threshold spilling logic**
  - [ ] Implement threshold check
  - [ ] Trigger spill on threshold exceeded
  - [ ] Add cooldown to prevent thrashing
  - [ ] Write spilling tests

- [ ] **Task 4.3: Create PDG cache clearing**
  - [ ] Clear PDG for non-active projects
  - [ ] Implement LRU eviction
  - [ ] Track active projects
  - [ ] Write cache tests

- [ ] **Task 4.4: Implement DuckDB cache spilling**
  - [ ] Spill DuckDB cache to disk
  - [ ] Clear in-memory tables
  - [ ] Reload on demand
  - [ ] Write spilling tests

- [ ] **Task 4.5: Add Python gc coordination**
  - [ ] Trigger gc.collect() on spill
  - [ ] Coordinate with Python memory manager
  - [ ] Add debug logging
  - [ ] Write coordination tests

- [ ] **Task: Maestro - Phase 4 Verification**

---

## Phase 5: Error Handling and Logging

### Objective
Implement comprehensive error handling and logging.

- [ ] **Task 5.1: Create Rust error types with thiserror**
  - [ ] Define error categories
  - [ ] Add error context
  - [ ] Implement error display
  - [ ] Write error tests

- [ ] **Task 5.2: Convert to Python exceptions**
  - [ ] Map Rust errors to Python exceptions
  - [ ] Preserve error context
  - [ ] Add error messages
  - [ ] Write conversion tests

- [ ] **Task 5.3: Add structured logging**
  - [ ] Integrate tracing crate
  - [ ] Add log levels (error, warn, info, debug, trace)
  - [ ] Log FFI crossings
  - [ ] Write logging tests

- [ ] **Task 5.4: Implement debug/trace modes**
  - [ ] Add debug mode toggle
  - [ ] Add trace mode toggle
  - [ ] Log FFI details in debug mode
  - [ ] Write mode tests

- [ ] **Task 5.5: Document error scenarios**
  - [ ] Document all error types
  - [ ] Add recovery guidance
  - [ ] Create troubleshooting guide
  - [ ] Write documentation tests

- [ ] **Task: Maestro - Phase 5 Verification**

---

## Phase 6: Documentation and Examples

### Objective
Complete documentation and usage examples.

- [ ] **Task 6.1: Write API documentation**
  - [ ] Document RustAnalyzer class
  - [ ] Document build_weighted_context
  - [ ] Document all exposed functions
  - [ ] Add docstrings

- [ ] **Task 6.2: Create usage examples**
  - [ ] Write basic usage example
  - [ ] Write advanced usage example
  - [ ] Write MCP tool usage example
  - [ ] Test all examples

- [ ] **Task 6.3: Add migration guide from Python**
  - [ ] Document changes from Python
  - [ ] Add migration checklist
  - [ ] Show before/after examples
  - [ ] Validate migration steps

- [ ] **Task 6.4: Document performance characteristics**
  - [ ] Document FFI overhead
  - [ ] Document memory usage
  - [ ] Add benchmark results
  - [ ] Document tuning options

- [ ] **Task 6.5: Create troubleshooting guide**
  - [ ] Add common issues
  - [ ] Add debugging steps
  - [ ] Add recovery procedures
  - [ ] Validate troubleshooting steps

- [ ] **Task: Maestro - Phase 6 Verification**

---

## Success Criteria

The track is complete when:

1. **PyO3 bindings working** - Python can call Rust code seamlessly
2. **MCP tool functional** - `leindex_deep_analyze` working end-to-end
3. **Memory management working** - Spilling activates at 90% RSS
4. **Zero-copy verified** - No unnecessary copies across FFI
5. **Error handling complete** - All errors handled and logged
6. **Documentation complete** - All APIs documented with examples

---

## Notes

- **Depends on lerecherche and lestockage:** Requires both search and storage
- **Final sub-track:** Integrates all previous work
- **Reference Code Only:** Code in `Rust_ref(DO_NOT_DIRECTLY_USE)` is for reference only
