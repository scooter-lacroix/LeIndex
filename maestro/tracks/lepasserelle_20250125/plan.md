# Implementation Plan: lepasserelle - Bridge & Integration

**Track ID:** `lepasserelle_20250125`
**Track Type:** Standard Track
**Status:** OPTIONAL (Source-Code-Verified: 2025-01-25)
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

**IMPORTANT:** This track is **OPTIONAL** for a 100% pure Rust implementation. The `lepasserelle` crate provides PyO3 FFI bindings and MCP tool integration **ONLY IF** Python interoperability is required. For a pure Rust implementation, this crate can be **skipped entirely**.

This track implements the Bridge & Integration layer for LeIndex Rust Renaissance. It creates PyO3 FFI bindings for Python-Rust interop and implements the unified MCP tool.

**Source-Code-Verified Status:** ~15% COMPLETE ⚠️ MOSTLY PLACEHOLDERS

**Test Results:** PyO3 linker error (expected without Python interpreter)
**Code State:** Structure exists, most functions return placeholder data

---

## Phase 1: PyO3 Module Setup ⚠️ PLACEHOLDER

### Objective
Create Python module with Rust bindings.

- [x] **Task 1.1: Create PyO3 module structure** ✅ COMPLETE
  - [x] Create `leindex_rust` Python module
  - [x] Configure `python-bindings` feature in Cargo.toml
  - [x] Set up PyO3 with extension-module
  - **File:** `src/lib.rs` (34 lines)

- [ ] **Task 1.2: Expose RustAnalyzer class** ⚠️ PLACEHOLDER
  - [x] `RustAnalyzer` PyClass structure exists
  - [ ] `initialize()` - Only sets flag (line 37-40)
  - [ ] `parse_file()` - Returns fake JSON (line 43-55)
  - [ ] `build_context()` - Returns formatted string (line 58-74)
  - [ ] `get_node()` - Returns fake data (line 77-86)
  - **File:** `src/bridge.rs` (194 lines)
  - **Status:** All methods return placeholder data

---

## Phase 2: Zero-Copy Data Transfer ❌ NOT IMPLEMENTED

### Objective
Implement efficient data transfer across FFI boundary.

- [ ] **Task 2.1: Implement mmap for source files** ❌ NOT STARTED
  - [ ] Use mmap for passing large source files
  - [ ] Avoid copying across FFI boundary
  - [ ] Memory-mapped file handling

- [ ] **Task 2.2: Create shared memory buffers** ❌ NOT STARTED
  - [ ] Zero-copy embedding transfer
  - [ ] Shared memory for large data structures
  - [ ] Memory-aware buffer management

- [ ] **Task 2.3: Optimize FFI boundary crossings** ❌ NOT STARTED
  - [ ] Batch operations across FFI
  - [ ] Minimize serialization overhead
  - [ ] Benchmark transfer overhead

---

## Phase 3: Unified MCP Tool ⚠️ PLACEHOLDER

### Objective
Implement `leindex_deep_analyze` MCP tool.

- [x] **Task 3.1: Create MCP tool structure** ✅ COMPLETE
  - [x] `LeIndexDeepAnalyze` struct exists
  - [x] `McpRequest`, `McpResponse` types
  - [x] `AnalysisResult`, `EntryPoint` types
  - **File:** `src/mcp.rs` (218 lines)

- [ ] **Task 3.2: Implement semantic search** ⚠️ PLACEHOLDER
  - [ ] `semantic_search()` - Returns single placeholder entry (line 65-73)
  - [ ] Should use lerecherche for actual search
  - [ ] Currently returns hardcoded data

- [ ] **Task 3.3: Implement context expansion** ⚠️ PLACEHOLDER
  - [ ] `expand_context()` - Returns formatted comment string (line 76-83)
  - [ ] Should use legraphe for actual expansion
  - [ ] Currently returns placeholder text

- [x] **Task 3.4: LLM-ready formatting** ✅ COMPLETE
  - [x] `McpResponse::to_llm_string()` - Formats for LLM consumption
  - [x] Includes query, context, entry points, tokens used
  - **File:** `src/mcp.rs` lines 165-175

---

## Phase 4: Memory Management ⚠️ PARTIAL

### Objective
Implement RSS monitoring and cache spilling.

- [x] **Task 4.1: Implement RSS monitoring** ✅ COMPLETE
  - [x] `MemoryManager` with process access
  - [x] `get_rss_bytes()` - Get current RSS memory
  - [x] `get_total_memory()` - Get system memory
  - [x] `is_threshold_exceeded()` - Check 90% threshold
  - **File:** `src/memory.rs` (202 lines) lines 48-70

- [ ] **Task 4.2: Implement cache spilling** ⚠️ PLACEHOLDER
  - [ ] `spill_cache()` - Returns fake result (line 73-85)
  - [ ] Should clear PDG cache from legraphe
  - [ ] Should track memory freed

- [ ] **Task 4.3: DuckDB cache spilling** ⚠️ EMPTY
  - [ ] `spill_to_duckdb()` - Empty implementation (line 88-91)
  - [ ] Should spill analytics cache to DuckDB

- [ ] **Task 4.4: Python GC coordination** ⚠️ EMPTY
  - [ ] `trigger_python_gc()` - Empty implementation (line 94-97)
  - [ ] Should trigger Python garbage collection

---

## Phase 5: Error Handling ⚠️ BASIC

### Objective
Implement error handling and logging.

- [x] **Task 5.1: Create Rust error types** ✅ COMPLETE
  - [x] `BridgeError` with thiserror (init, parse, IO, serialization)
  - [x] `McpError` with thiserror (search, context, project, query)
  - [x] `MemoryError` with thiserror (process, memory_info, spill)
  - **Files:** `src/bridge.rs`, `src/mcp.rs`, `src/memory.rs`

- [x] **Task 5.2: Convert to Python exceptions** ✅ COMPLETE
  - [x] `From<BridgeError> for PyErr` implementation
  - [x] Automatic conversion with error messages
  - **File:** `src/bridge.rs` lines 170-174

- [ ] **Task 5.3: Add structured logging** ❌ NOT STARTED
  - [ ] No tracing integration yet
  - [ ] No FFI crossing logs
  - [ ] No debug/trace modes

---

## Phase 6: Documentation ❌ NOT STARTED

### Objective
Complete documentation and usage examples.

- [ ] **Task 6.1: Write API documentation** ❌ NOT STARTED
  - [ ] Document RustAnalyzer class
  - [ ] Document build_weighted_context
  - [ ] Document all exposed functions

- [ ] **Task 6.2: Create usage examples** ❌ NOT STARTED
  - [ ] Write basic usage example
  - [ ] Write advanced usage example
  - [ ] Write MCP tool usage example

- [ ] **Task 6.3: Add migration guide from Python** ❌ NOT STARTED
  - [ ] Document changes from Python
  - [ ] Add migration checklist
  - [ ] Show before/after examples

---

## Success Criteria

The track is complete when:

1. **⚠️ PyO3 bindings working** - Python can call Rust **PLACEHOLDER**
2. **❌ Zero-copy transfer working** - mmap implemented **NOT STARTED**
3. **❌ MCP tool functional** - Actual search/expansion **PLACEHOLDER**
4. **✅ RSS monitoring working** - Memory tracking **ACHIEVED**
5. **❌ Cache spilling working** - Actual spilling **NOT STARTED**
6. **❌ Documentation complete** - API docs **NOT STARTED**

---

## Files Implemented

| File | Lines | Purpose | Status |
|------|-------|---------|--------|
| `src/lib.rs` | 34 | Module declarations, Python module | ✅ COMPLETE (partial) |
| `src/bridge.rs` | 194 | PyO3 bindings, RustAnalyzer | ⚠️ PLACEHOLDER |
| `src/mcp.rs` | 218 | MCP tool structures | ⚠️ PLACEHOLDER |
| `src/memory.rs` | 202 | Memory manager, RSS monitoring | ⚠️ PARTIAL |

**Total:** ~648 lines of code (mostly placeholders)

---

## What Works vs What's Missing

```
┌─────────────────────────────────────────────────────────────────────┐
│                       lepasserelle STATUS                           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  ✅ COMPLETE (Working):                                              │
│  ├── PyO3 module structure configured                               │
│  ├── RustAnalyzer PyClass structure                                │
│  ├── MCP tool structures (LeIndexDeepAnalyze, etc.)                │
│  ├── Memory manager with RSS monitoring                            │
│  └── LLM-ready response formatting                                   │
│                                                                       │
│  ⚠️ PARTIAL (Structure exists, needs implementation):                 │
│  ├── RSS monitoring works (get_rss_bytes, is_threshold_exceeded)     │
│  └── LLM formatting works (to_llm_string)                           │
│                                                                       │
│  ❌ MISSING (All integration with actual Rust crates):               │
│  ├── initialize() - Sets flag only                                  │
│  ├── parse_file() - Returns fake JSON                             │
│  ├── build_context() - Returns formatted string                    │
│  ├── get_node() - Returns fake data                                │
│  ├── semantic_search() - Returns placeholder                       │
│  ├── expand_context() - Returns placeholder                        │
│  ├── spill_cache() - Returns fake result                          │
│  ├── spill_to_duckdb() - Empty                                    │
│  ├── trigger_python_gc() - Empty                                  │
│  └── No structured logging                                           │
│                                                                       │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Implementation Plan for Remaining Work

### Task 1.2: Complete RustAnalyzer Implementation

**Objective:** Make RustAnalyzer actually work with leparse

**Implementation Strategy:**

1. **Update `src/bridge.rs` methods**
   ```rust
   pub fn parse_file(&self, file_path: &str) -> PyResult<String> {
       // Use leparse to parse the file
       let source = std::fs::read(file_path)?;
       let lang_parser = leparse::languages::parser_for_language(/* ... */)?;
       let signatures = lang_parser.get_signatures(&source)?;

       // Convert to JSON
       serde_json::to_string(&signatures).map_err(Into::into)
   }
   ```

2. **Implement `build_context()`**
   - Use legraphe for PDG expansion
   - Use GravityTraversal for context building
   - Return actual expanded context

3. **Integration Tests**
   - Test with real Python code
   - Validate AST extraction
   - Test context expansion

### Task 3.2-3.3: Complete MCP Tool Implementation

**Objective:** Make MCP tool actually work

**Implementation Strategy:**

1. **Update `src/mcp.rs` methods**
   ```rust
   async fn semantic_search(&self, query: &str, top_k: usize) -> Result<Vec<EntryPoint>, Error> {
       // Use lerecherche for actual semantic search
       // Return actual entry points from search
   }

   async fn expand_context(&self, entry_points: &[EntryPoint]) -> Result<String, Error> {
       // Use legraphe for actual PDG expansion
       // Use GravityTraversal for context building
       // Return actual expanded context
   }
   ```

2. **Integration with legraphe/lerecherche**
   - Add dependencies on legraphe and lerecherche
   - Implement actual search and expansion
   - Test end-to-end workflow

---

## Decision: Pure Rust vs Python Interop

### For 100% Pure Rust Implementation:

**RECOMMENDATION:** Skip this track entirely

**Reasons:**
1. All core functionality can be implemented in pure Rust
2. PyO3 adds complexity and dependency on Python
3. MCP tool can be implemented as native Rust CLI
4. No Python ecosystem required

**Alternative:** Implement native Rust CLI tool
- Replace MCP tool with native Rust CLI
- Use same leparse/legraphe/lerecherche/lestockage
- No FFI overhead

### For Python Integration Required:

**IMPLEMENTATION PATH:**
1. Complete RustAnalyzer integration with leparse
2. Complete MCP tool integration with legraphe/lerecherche
3. Implement actual cache spilling with legraphe
4. Add zero-copy data transfer with mmap

---

## Status: OPTIONAL - PLACEHOLDER IMPLEMENTATION ⚠️

The `lepasserelle` crate exists but is mostly placeholder code. The RSS monitoring works, but all integration with the core Rust crates (leparse, legraphe, lerecherche, lestockage) is missing.

**For a 100% pure Rust implementation, this crate can be SKIPPED ENTIRELY.**

---

## Notes

- **Optional for Pure Rust:** This crate is only needed if Python interoperability is a requirement
- **Placeholder Status:** Most methods return fake data and need actual implementation
- **PyO3 Linker Error:** Expected when building without Python interpreter
- **Memory Monitoring Works:** RSS monitoring functions are implemented and working
