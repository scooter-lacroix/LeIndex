# Implementation Plan: lepasserelle - Integration & API Layer

**Track ID:** `lepasserelle_20250125`
**Track Type:** Standard Track
**Status:** PENDING (Source-Code-Verified: 2025-01-26)
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the **Integration & API Layer** for LeIndex Rust Renaissance. It brings together all core crates (leparse, legraphe, lerecherche, lestockage) into a cohesive system with CLI and MCP server interfaces.

**IMPORTANT:** This is a **100% Pure Rust implementation**. All PyO3/Python bindings from the prototype are being removed and replaced with native Rust implementations.

**Source-Code-Verified Status:** ~25% COMPLETE ⚠️ IN PROGRESS

**Current State:** PyO3 dependencies removed, pure Rust foundation established. Ready for Phase 2/4 implementation.

---

## Phase 1: Remove PyO3, Create Pure Rust Foundation ✅ COMPLETE (e2c6243)

### Objective
Remove all Python dependencies and establish pure Rust architecture.

- [x] **Task 1.1: Remove PyO3 dependencies** ✅ COMPLETE (e2c6243)
  - [x] Remove `python-bindings` feature from Cargo.toml
  - [x] Remove `pyo3` dependency entirely
  - [x] Change crate-type from `["cdylib", "rlib"]` to `["rlib"]`
  - [x] Delete `src/bridge.rs` (Python FFI layer)
  - **Files:** `Cargo.toml`, `src/lib.rs`, `src/bridge.rs`

- [x] **Task 1.2: Create pure Rust module structure** ✅ COMPLETE (e2c6243)
  - [x] Basic module structure exists (mcp.rs, memory.rs)
  - [x] Updated lib.rs exports for non-Python API
  - [x] Removed all Python-related exports
  - **File:** `src/lib.rs`

- [x] **Task 1.3: Add MCP server dependencies** ✅ COMPLETE (e2c6243)
  - [x] `tokio` already in workspace dependencies
  - [x] MCP server dependencies added as comments (will enable in Phase 2)
  - [x] CLI dependencies added as comments (will enable in Phase 3)
  - **File:** `Cargo.toml`

---

## Phase 2: Pure Rust MCP Server ❌ NOT STARTED

### Objective
Implement native Rust MCP server (no Python).

- [ ] **Task 2.1: Implement MCP JSON-RPC server** ❌ NOT STARTED
  - [ ] Create `McpServer` struct with axum/warp
  - [ ] Implement JSON-RPC 2.0 handler
  - [ ] Add CORS and error handling middleware
  - [ ] Support SSE (Server-Sent Events) for streaming
  - **File:** `src/mcp/server.rs` (new file)

- [ ] **Task 2.2: Implement MCP tool handlers** ❌ NOT STARTED
  - [ ] `leindex_deep_analyze` - Main analysis tool
  - [ ] `leindex_search` - Semantic search tool
  - [ ] `leindex_context` - Graph expansion tool
  - [ ] `leindex_index` - Project indexing tool
  - [ ] `leindex_diagnostics` - System diagnostics tool
  - **File:** `src/mcp/handlers.rs` (new file)

- [ ] **Task 2.3: Wire up actual crate integration** ❌ NOT STARTED
  - [ ] Call `lerecherche::SearchEngine` for semantic search
  - [ ] Call `legraphe::GravityTraversal` for context expansion
  - [ ] Call `leparse::ParallelParser` for parsing
  - [ ] Call `lestockage::*` for persistence
  - **File:** `src/integration.rs` (new file)

---

## Phase 3: CLI Interface ❌ NOT STARTED

### Objective
Create command-line interface for LeIndex.

- [ ] **Task 3.1: Implement CLI structure with clap** ❌ NOT STARTED
  - [ ] `leindex index <path>` - Index a project
  - [ ] `leindex search <query>` - Search code
  - [ ] `leindex analyze <query>` - Deep analysis
  - [ ] `leindex diagnostics` - System status
  - [ ] `leindex serve` - Start MCP server
  - **File:** `src/cli.rs` (new file, ~300 lines)

- [ ] **Task 3.2: Implement index command** ❌ NOT STARTED
  - [ ] Parse project files with leparse
  - [ ] Build PDG with legraphe
  - [ ] Index with lerecherche
  - [ ] Persist to lestockage
  - [ ] Show progress and statistics
  - **File:** `src/cli/index.rs` (new file)

- [ ] **Task 3.3: Implement search command** ❌ NOT STARTED
  - [ ] Load project from lestockage
  - [ ] Execute search via lerecherche
  - [ ] Display results with formatting
  - [ ] Support JSON output mode
  - **File:** `src/cli/search.rs` (new file)

---

## Phase 4: Integration Layer ❌ NOT STARTED

### Objective
Create unified API that brings all crates together.

- [ ] **Task 4.1: Implement LeIndex orchestration** ❌ NOT STARTED
  - [ ] `LeIndex` struct with project management
  - [ ] `index_project()` - Full pipeline (parse → graph → index → store)
  - [ ] `search()` - Unified search interface
  - [ ] `analyze()` - Deep analysis with PDG expansion
  - [ ] `get_diagnostics()` - Project statistics
  - **File:** `src/leindex.rs` (new file, ~400 lines)

- [ ] **Task 4.2: Implement project configuration** ❌ NOT STARTED
  - [ ] `ProjectConfig` with TOML/JSON support
  - [ ] Language filtering (which languages to parse)
  - [ ] Path exclusions (.git, node_modules, etc.)
  - [ ] Token budget settings
  - [ ] Storage backend selection
  - **File:** `src/config.rs` (new file, ~200 lines)

- [ ] **Task 4.3: Implement error recovery** ❌ NOT STARTED
  - [ ] Graceful handling of parse failures
  - [ ] Partial indexing (continue on error)
  - [ ] Corruption detection and recovery
  - [ ] Detailed error reporting
  - **File:** `src/errors.rs` (new file)

---

## Phase 5: Memory Management (Port from Prototype) ⚠️ PARTIAL

### Objective
Memory-aware operations with RSS monitoring and cache spilling.

- [x] **Task 5.1: RSS monitoring** ✅ COMPLETE (from prototype)
  - [x] `MemoryManager` with process access
  - [x] `get_rss_bytes()` - Current RSS memory
  - [x] `get_total_memory()` - System memory
  - [x] `is_threshold_exceeded()` - 90% threshold check
  - **File:** `src/memory.rs` (keep existing code)

- [ ] **Task 5.2: Implement cache spilling** ❌ NOT STARTED
  - [ ] `spill_pdg_cache()` - Unload PDG from memory
  - [ ] `spill_vector_cache()` - Unload HNSW index
  - [ ] Track memory freed
  - [ ] Automatic spill on threshold
  - **File:** `src/memory.rs` (extend existing)

- [ ] **Task 5.3: Implement cache reloading** ❌ NOT STARTED
  - [ ] Reload PDG from lestockage
  - [ ] Reload HNSW index from disk
  - [ ] Lazy loading strategy
  - **File:** `src/memory.rs` (extend existing)

---

## Phase 6: Testing & Documentation ❌ NOT STARTED

### Objective
Comprehensive testing and documentation.

- [ ] **Task 6.1: Integration tests** ❌ NOT STARTED
  - [ ] Test full indexing pipeline
  - [ ] Test search functionality
  - [ ] Test MCP server endpoints
  - [ ] Test CLI commands
  - [ ] Test error handling
  - **File:** `tests/integration.rs` (new file)

- [ ] **Task 6.2: Unit tests** ❌ NOT STARTED
  - [ ] Test LeIndex orchestration
  - [ ] Test configuration loading
  - [ ] Test memory management
  - [ ] Test error handling
  - **Files:** Unit tests in each module

- [ ] **Task 6.3: Documentation** ❌ NOT STARTED
  - [ ] API documentation with rustdoc
  - [ ] CLI usage examples
  - [ ] MCP server protocol docs
  - [ ] Architecture overview
  - [ ] Migration guide from Python prototype

---

## Success Criteria

The track is complete when:

1. **✅ PyO3 completely removed** - Zero Python dependencies
2. **✅ Pure Rust MCP server** - Native JSON-RPC server working
3. **✅ CLI interface functional** - All commands working
4. **✅ Integration complete** - All crates wired together
5. **✅ Memory management working** - RSS monitoring + cache spilling
6. **✅ Tests passing** - Integration + unit tests
7. **✅ Documentation complete** - API docs + usage examples

---

## Implementation Priority

**Phase 1 is CRITICAL** - Must remove all Python code first.

**Phase 2 & 4 are HIGH PRIORITY** - Core integration functionality.

**Phase 3 is MEDIUM** - CLI is important but can be incremental.

**Phase 5 is LOW** - Memory management is nice-to-have for MVP.

**Phase 6 is ONGOING** - Tests and docs should be developed alongside.

---

## Files to Delete

- `src/bridge.rs` - Entire file (Python FFI, no longer needed)
- Python bindings from `src/lib.rs`

---

## Files to Create

| File | Purpose | Est. Lines |
|------|---------|------------|
| `src/mcp/server.rs` | MCP JSON-RPC server | ~300 |
| `src/mcp/handlers.rs` | MCP tool handlers | ~400 |
| `src/integration.rs` | Crate integration layer | ~300 |
| `src/cli.rs` | CLI structure | ~300 |
| `src/cli/index.rs` | Index command | ~200 |
| `src/cli/search.rs` | Search command | ~150 |
| `src/leindex.rs` | Main orchestration | ~400 |
| `src/config.rs` | Configuration | ~200 |
| `src/errors.rs` | Error handling | ~150 |
| `tests/integration.rs` | Integration tests | ~500 |

**Total New Code:** ~2,900 lines of pure Rust

---

## Next Steps

1. **Start with Phase 1** - Remove PyO3 and reorganize module structure
2. **Move to Phase 4** - Implement core LeIndex orchestration
3. **Add Phase 2** - MCP server on top of orchestration
4. **Complete with Phase 3** - CLI interface

---

## Status: READY TO START ⚠️

This track requires a complete rewrite of the existing prototype code to remove Python dependencies and implement pure Rust integration. The foundation exists but needs significant restructuring.
