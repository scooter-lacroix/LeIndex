# Implementation Plan: LeIndex Rust Renaissance - Master Track

**Track ID:** `leindex_rust_refactor_20250125`
**Track Type:** Master Track (orchestrate-capable)
**Status:** In Progress (Source-Code-Verified Assessment: 2025-01-25)
**Created:** 2025-01-25
**Last Updated:** 2025-01-25 (Source Code Verification - Accurate State)

---

## Overview

This Master Track implements a **complete pure Rust** rewrite of LeIndex, transforming it into a Deep Code Intelligence Engine. The implementation is organized as **4 core sub-tracks** (plus optional PyO3 bridge) that can be executed via `/maestro:orchestrate`.

**IMPORTANT:** This is a **100% pure Rust implementation**. The `lepasserelle` PyO3 bridge is optional and only needed if Python interop is required.

**Execution Strategy:**
1. Execute this Master Track plan to create all sub-tracks
2. Use `/maestro:orchestrate` to execute sub-tracks in dependency order
3. Each sub-track has its own `spec.md` and `plan.md`

---

## Phase 0: Foundation & Setup

### Objective
Prepare the project infrastructure for pure Rust implementation.

- [x] **Task 0.1: Create Rust workspace structure** ✅ COMPLETE
  - [x] Create `Cargo.toml` workspace configuration
  - [x] Set up crate structure: `leparse`, `legraphe`, `lerecherche`, `lestockage`, `lepasserelle`
  - [x] Configure workspace-level dependencies
  - [x] Set up dev-dependencies (test frameworks, linting tools)
  - **Status:** COMPLETE - All 5 crates compile successfully
  - **Verified:** 2025-01-25

- [x] **Task 0.3: Establish testing infrastructure** ✅ COMPLETE
  - [x] Set up Rust test framework (`cargo test`, `rstest`)
  - [x] Tests running for all crates (120 tests passing total)
  - **Status:** COMPLETE - Basic testing infrastructure working

- [x] **Task 0.4: Create Sub-Track Specifications** ✅ COMPLETE
  - [x] Generate `leparse` sub-track spec and plan
  - [x] Generate `legraphe` sub-track spec and plan
  - [x] Generate `lerecherche` sub-track spec and plan
  - [x] Generate `lestockage` sub-track spec and plan
  - [x] Generate `lepasserelle` sub-track spec and plan (optional)
  - **Status:** COMPLETE

**Phase 0 Status:** 100% COMPLETE ✅

---

## Phase 1: Core Parsing Engine (`leparse_20250125`)

### Objective
Build zero-copy AST extraction with multi-language support using tree-sitter.

### Source-Code-Verified Status: **~90% COMPLETE** ✅

**Test Results:** 97/97 tests passing ✅
**Code Quality:** Tzar review PASSED (Phases 1-3)
**Supported Languages:** 12 (Python, JavaScript, TypeScript, Go, Rust, Java, C++, C#, Ruby, PHP, Lua, Scala)

### Implementation Status (Source Verified):

- [x] **Task 1.1: Tree-sitter integration** ✅ COMPLETE
  - [x] Lazy-loaded thread-safe grammar cache (`grammar.rs`)
  - [x] Language detection by extension
  - [x] 17 language grammars configured (12 working, 3 disabled, 2 not attempted)

- [x] **Task 1.2: Zero-copy AST types** ✅ COMPLETE
  - [x] `AstNode` with lifetime-safe byte-slice references (`ast.rs`)
  - [x] `SignatureInfo`, `FunctionElement`, `ClassElement`, `ModuleElement`
  - [x] Zero-copy verified through tests

- [x] **Task 1.3: CodeIntelligence trait** ✅ COMPLETE
  - [x] Trait definition (`traits.rs`)
  - [x] `get_signatures()` - full implementation with parameters, types, docstrings
  - [x] `compute_cfg()` - control flow graph generation
  - [x] `extract_complexity()` - cyclomatic complexity calculation

- [x] **Task 1.4: Multi-language support** ✅ COMPLETE (12/12 implemented)
  - [x] Python (`python.rs`) - FULLY IMPLEMENTED
  - [x] JavaScript/TypeScript (`javascript.rs`) - FULLY IMPLEMENTED
  - [x] Go (`go.rs`) - FULLY IMPLEMENTED
  - [x] Rust (`rust.rs`) - FULLY IMPLEMENTED
  - [x] Java (`java.rs`) - FULLY IMPLEMENTED
  - [x] C++ (`cpp.rs`) - FULLY IMPLEMENTED
  - [x] C# (`csharp.rs`) - FULLY IMPLEMENTED
  - [x] Ruby (`ruby.rs`) - FULLY IMPLEMENTED
  - [x] PHP (`php.rs`) - FULLY IMPLEMENTED
  - [x] Lua (`lua.rs`) - FULLY IMPLEMENTED
  - [x] Scala (`scala.rs`) - FULLY IMPLEMENTED
  - [~] Swift - DISABLED (tree-sitter v15 vs v13-14 incompatibility)
  - [~] Kotlin - DISABLED (tree-sitter version incompatibility)
  - [~] Dart - DISABLED (parsing issues)
  - [ ] Elixir - NOT ATTEMPTED
  - [ ] Haskell - NOT ATTEMPTED

- [x] **Task 1.5: Parallel parsing with rayon** ✅ **COMPLETE**
  - [x] `ParallelParser` with rayon parallel iteration (`parallel.rs` - 322 lines)
  - [x] Thread-local parser pooling (THREAD_PARSER)
  - [x] `ParsingResult` and `ParsingStats` structures
  - [x] Error handling for individual file failures
  - [x] 3 passing tests (multiple files, error handling, statistics)

**What's MISSING from leparse:**
- Optional: Swift, Kotlin, Dart, Elixir, Haskell support
- Optional: 50K+ file benchmarking

**Sub-Track Status:** PRODUCTION READY ✅
- All core functionality complete
- 97 tests passing
- No stubs, no placeholders in core code
- Ready for integration with other crates

---

## Phase 2: Graph Intelligence Core (`legraphe_20250125`)

### Objective
Build the Program Dependence Graph (PDG) engine with gravity-based traversal.

### Source-Code-Verified Status: **~30% COMPLETE** ⚠️

**Test Results:** 9/9 tests passing ✅
**Code State:** Data structures and algorithms COMPLETE, missing AST→PDG extraction

### Implementation Status (Source Verified):

- [x] **Task 2.1: Graph data structures** ✅ COMPLETE
  - [x] `ProgramDependenceGraph` with `StableGraph` wrapper (`pdg.rs` - 363 lines)
  - [x] `Node` and `Edge` types with metadata
  - [x] `add_node()`, `add_edge()`, `get_node()`, `get_edge()`
  - [x] `find_by_symbol()`, `nodes_in_file()`, `neighbors()`
  - [x] Node/edge counting and neighbor access

- [x] **Task 2.2: PDG construction helpers** ✅ COMPLETE
  - [x] `add_call_graph_edges()` - bulk call edge insertion
  - [x] `add_data_flow_edges()` - bulk data flow edge insertion
  - [x] `add_inheritance_edges()` - bulk inheritance edge insertion

- [x] **Task 2.3: Impact analysis** ✅ COMPLETE
  - [x] `get_forward_impact()` - forward reachability using DFS
  - [x] `get_backward_impact()` - backward reachability using reversed graph

- [x] **Task 2.4: Gravity-based traversal** ✅ COMPLETE
  - [x] `GravityTraversal` with priority queue (`traversal.rs` - 205 lines)
  - [x] `TraversalConfig` with token budget, decay, weights
  - [x] `expand_context()` - priority-weighted expansion
  - [x] Relevance scoring: `(semantic * weight + complexity) / distance^decay`

- [x] **Task 2.5: Node embeddings** ✅ COMPLETE
  - [x] `NodeEmbedding` with 768-dim vectors (`embedding.rs` - 143 lines)
  - [x] Cosine similarity calculation
  - [x] `EmbeddingCache` with FIFO eviction and `find_similar()`

- [ ] **Task 2.6: AST → PDG extraction** ❌ **CRITICAL MISSING PIECE**
  - [ ] Extract call graphs from `SignatureInfo` (leparse output)
  - [ ] Extract data flow graphs from AST
  - [ ] Extract inheritance graphs from class definitions
  - [ ] Build unified PDG from parsed code
  - [ ] **This is the bridge between leparse and legraphe**

- [ ] **Task 2.7: Graph serialization** ❌ PLACEHOLDER
  - [ ] `serialize()` - currently returns error "not yet implemented"
  - [ ] `deserialize()` - currently returns error "not yet implemented"
  - [ ] StableGraph doesn't support serde directly, needs custom implementation

**Sub-Track Status:** FOUNDATIONS COMPLETE, MISSING EXTRACTION ❌
- **What Works:** All PDG data structures, algorithms, and traversal
- **What's Missing:** The code that transforms `SignatureInfo` into a `ProgramDependenceGraph`
- **Critical Need:** AST→PDG extraction module to bridge leparse output

---

## Phase 3: Search & Analysis Fusion (`lerecherche_20250125`)

### Objective
Implement node-level semantic search with vector-AST synergy.

### Source-Code-Verified Status: **~25% COMPLETE** ⚠️

**Test Results:** 6/6 tests passing ✅
**Code State:** Text search works, semantic search is placeholder

### Implementation Status (Source Verified):

- [x] **Task 3.1: Search engine structure** ✅ COMPLETE
  - [x] `SearchEngine` with node indexing (`search.rs` - 325 lines)
  - [x] `SearchQuery`, `SearchResult`, `NodeInfo` structures
  - [x] `index_nodes()` - builds in-memory node index
  - [x] `search()` - text search with substring matching

- [x] **Task 3.2: Text search** ✅ COMPLETE
  - [x] `calculate_text_score()` - substring and token overlap
  - [x] Case-insensitive matching
  - [x] Top-K result limiting
  - [x] Result ranking by score

- [x] **Task 3.3: Hybrid scoring** ✅ COMPLETE
  - [x] `HybridScorer` with configurable weights (`ranking.rs` - 191 lines)
  - [x] `Score` struct with semantic, structural, text_match components
  - [x] `rerank()` - adaptive ranking by query type
  - [x] Weighted combination: `overall = semantic*0.5 + structural*0.3 + text*0.2`

- [x] **Task 3.4: Semantic processor** ✅ COMPLETE
  - [x] `SemanticProcessor` with PDG integration (`semantic.rs` - 140 lines)
  - [x] `process_entry()` - expands context using gravity traversal
  - [x] Formats LLM-ready context with file/symbol annotations

- [ ] **Task 3.5: Vector search backend** ❌ **CRITICAL MISSING PIECE**
  - [ ] No HNSW/DiskANN integration
  - [ ] `semantic_search()` returns empty Vec (line 176-183)
  - [ ] No embedding model integration (CodeRankEmbed or similar)
  - [ ] No vector index for node embeddings

- [ ] **Task 3.6: Natural language queries** ❌ NOT IMPLEMENTED
  - [ ] No query understanding/parsing
  - [ ] No semantic pattern matching

**Sub-Track Status:** TEXT SEARCH ONLY, SEMANTIC SEARCH MISSING ❌
- **What Works:** Full text search, hybrid scoring, PDG context expansion
- **What's Missing:** Vector search backend, embedding model, semantic queries
- **Critical Need:** Vector search integration (HNSW/DiskANN) + embedding generation

---

## Phase 4: Persistent Storage Layer (`lestockage_20250125`)

### Objective
Implement extended SQLite schema with Salsa incremental computation.

### Source-Code-Verified Status: **~35% COMPLETE** ⚠️

**Test Results:** 8/8 tests passing ✅
**Code State:** CRUD operations complete, missing PDG integration

### Implementation Status (Source Verified):

- [x] **Task 4.1: SQLite schema** ✅ COMPLETE
  - [x] `Storage` with WAL mode and cache config (`schema.rs` - 171 lines)
  - [x] `intel_nodes` table (id, project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding BLOB)
  - [x] `intel_edges` table (caller_id, callee_id, edge_type, metadata, FOREIGN KEYs)
  - [x] `analysis_cache` table (node_hash, cfg_data, complexity_metrics, timestamp)
  - [x] Indexes on project_id, file_path, symbol_name, content_hash

- [x] **Task 4.2: Node persistence** ✅ COMPLETE
  - [x] `NodeStore` with full CRUD (`nodes.rs` - 244 lines)
  - [x] `insert()`, `get()`, `batch_insert()`, `find_by_hash()`, `get_by_file()`
  - [x] `NodeRecord` with all required fields
  - [x] Embedding BLOB storage

- [x] **Task 4.3: Edge persistence** ✅ COMPLETE
  - [x] `EdgeStore` with full CRUD (`edges.rs` - 234 lines)
  - [x] `insert()`, `batch_insert()`, `get_by_caller()`, `get_by_callee()`, `get_by_type()`
  - [x] `EdgeRecord` with metadata
  - [x] Upsert support (ON CONFLICT DO UPDATE)

- [x] **Task 4.4: Salsa incremental computation** ✅ COMPLETE
  - [x] BLAKE3 hashing (`salsa.rs` - 188 lines)
  - [x] `NodeHash` with hex encoding
  - [x] `IncrementalCache` with `is_cached()`, `get()`, `put()`
  - [x] `QueryInvalidation` with `invalidate_node()`, `get_affected_nodes()`

- [x] **Task 4.5: Analytics** ✅ COMPLETE (SQLite-based)
  - [x] `Analytics` with metric queries (`analytics.rs` - 155 lines)
  - [x] `count_nodes_by_type()`, `complexity_distribution()`, `count_edges_by_type()`
  - [x] `get_hotspots()` - high complexity + high fan-out

- [ ] **Task 4.6: PDG persistence bridge** ❌ **CRITICAL MISSING PIECE**
  - [ ] No code to persist `ProgramDependenceGraph` from legraphe
  - [ ] No graph reconstruction from storage
  - [ ] No `load_pdg()` or `save_pdg()` functions

- [ ] **Task 4.7: Cross-project resolution** ❌ NOT IMPLEMENTED
  - [ ] No global symbol table
  - [ ] No cross-project symbol resolution

- [ ] **Task 4.8: DuckDB integration** ❌ NOT IMPLEMENTED
  - [ ] Analytics use SQLite, not DuckDB
  - [ ] No DuckDB-specific optimizations

**Sub-Track Status:** STORAGE PRIMITIVES COMPLETE, MISSING INTEGRATION ❌
- **What Works:** All CRUD operations, BLAKE3 hashing, analytics queries
- **What's Missing:** PDG persistence bridge, cross-project resolution, DuckDB
- **Critical Need:** Code to save/load `ProgramDependenceGraph` to/from storage

---

## Phase 5: Bridge & Integration (`lepasserelle_20250125`) - OPTIONAL

### Objective
PyO3 FFI bindings and unified MCP tool (OPTIONAL for pure Rust goal).

### Source-Code-Verified Status: **~15% COMPLETE** (Mostly Placeholders)

**Test Results:** PyO3 linker error (expected without Python interpreter)
**Code State:** Structure exists, most functions return placeholders

### Implementation Status (Source Verified):

- [ ] **Task 5.1: PyO3 bindings** ⚠️ PLACEHOLDER
  - [x] `RustAnalyzer` PyClass structure (`bridge.rs` - 194 lines)
  - [ ] `initialize()` - sets flag only (line 37-40)
  - [ ] `parse_file()` - returns fake JSON (line 43-55)
  - [ ] `build_context()` - returns formatted string (line 58-74)
  - [ ] `get_node()` - returns fake data (line 77-86)

- [ ] **Task 5.2: MCP tool** ⚠️ PLACEHOLDER
  - [x] `LeIndexDeepAnalyze` structure (`mcp.rs` - 218 lines)
  - [ ] `semantic_search()` - returns single placeholder entry (line 65-73)
  - [ ] `expand_context()` - returns formatted comment string (line 76-83)
  - [x] `McpResponse` with `to_llm_string()` formatting

- [ ] **Task 5.3: Memory management** ⚠️ PARTIAL
  - [x] `MemoryManager` with RSS monitoring (`memory.rs` - 202 lines)
  - [x] `get_rss_bytes()`, `get_total_memory()`, `is_threshold_exceeded()` - WORKING
  - [ ] `spill_cache()` - placeholder with fake result (line 73-85)
  - [ ] `spill_to_duckdb()` - empty (line 88-91)
  - [ ] `trigger_python_gc()` - empty (line 94-97)

**Sub-Track Status:** OPTIONAL - PyO3 BRIDGE FOR PYTHON INTEROP ❌
- **What Works:** PyO3 class structure, RSS monitoring
- **What's Missing:** All actual integration with leparse/legraphe/lerecherche
- **Note:** If goal is 100% pure Rust, this crate is **optional and may be obsolete**

---

## Phase 6: Integration & Validation

### Objective
End-to-end integration testing and validation.

### Status: **NOT STARTED** (0%)

**Prerequisites:**
- AST→PDG extraction (legraphe Task 2.6)
- Vector search backend (lerecherche Task 3.5)
- PDG persistence bridge (lestockage Task 4.6)

- [ ] **Task 6.1: Integration testing** ⏳ BLOCKED
  - [ ] Test leparse → legraphe pipeline (needs Task 2.6)
  - [ ] Test legraphe → lerecherche pipeline (needs Task 3.5)
  - [ ] Test legraphe → lestockage pipeline (needs Task 4.6)
  - [ ] End-to-end workflow testing

- [ ] **Task 6.2: Performance benchmarking** ⏳ BLOCKED
  - [ ] Indexing speed (50K files target)
  - [ ] Memory usage vs Python baseline
  - [ ] Search latency (<100ms P95 target)
  - [ ] Gravity traversal vs BFS comparison

- [ ] **Task 6.3: Code quality validation** ⏳ BLOCKED
  - [ ] Achieve >95% test coverage
  - [ ] Run clippy with no warnings
  - [ ] Validate all unsafe code blocks
  - [ ] Complete documentation

---

## Critical Dependencies & Missing Integration

```
┌─────────────────────────────────────────────────────────────────────┐
│                    ACTUAL INTEGRATION STATE                         │
├─────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  leparse (97 tests ✅ PRODUCTION READY)                              │
│     │                                                                │
│     │  ❌ MISSING: AST → PDG Extraction (legraphe Task 2.6)           │
│     │                                                                │
│     ▼                                                                │
│  legraphe (9 tests ✅ STRUCTURES COMPLETE)                            │
│     │                                                                │
│     │  ✅ PDG data structures, traversal, embeddings implemented       │
│     │  ❌ No code to populate PDG from leparse output                 │
│     │                                                                │
│     ├──────────────────────────────────────────────────────────┐     │
│     │                                                          │     │
│     │  ❌ MISSING: Vector search (lerecherche Task 3.5)         │     │
│     │                                                          │     │
│     ▼                                                          │     │
│  lerecherche (6 tests ✅ TEXT SEARCH ONLY)                       │     │
│     │                                                          │     │
│     │  ✅ Text search, hybrid scoring, context expansion       │     │
│     │  ❌ No vector/semantic search                             │     │
│     │                                                          │     │
│     │  ❌ MISSING: PDG persistence (lestockage Task 4.6)        │     │
│     │                                                          │     │
│     ▼                                                          │     │
│  lestockage (8 tests ✅ CRUD COMPLETE)                           │     │
│     │                                                          │     │
│     │  ✅ Node/edge storage, BLAKE3 hashing, analytics         │     │
│     │  ❌ No PDG save/load from storage                         │     │
│     │                                                          │     │
│  lepasserelle (OPTIONAL - PyO3 bridge, mostly placeholders)    │     │
│                                                                       │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Orchestration Guide

### Sub-Track Execution Order

Use `/maestro:orchestrate` with the following dependency order:

```
1. ✅ leparse - COMPLETE (can skip or finalize)
2. ❌ legraphe - FOCUS HERE (implement AST→PDG extraction)
3. ❌ lerecherche - BLOCKED (depends on legraphe + needs vector search)
4. ❌ lestockage - BLOCKED (needs PDG persistence bridge)
5. ⏸️ lepasserelle - OPTIONAL (skip for pure Rust goal)
```

### Recommended Execution Path

```
┌─────────────────────────────────────────────────────────────────────┐
│                         SEQUENTIAL WORKFLOW                          │
├─────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  STEP 1: legraphe - AST → PDG Extraction                             │
│  ├── Task 2.6: Implement extract_pdg_from_signatures()               │
│  │   ├── Extract call graphs from SignatureInfo                      │
│  │   ├── Extract data flow from AST nodes                            │
│  │   ├── Extract inheritance from class hierarchies                  │
│  │   └── Build unified ProgramDependenceGraph                       │
│  ├── Task 2.7: Implement serialize/deserialize                      │
│  └── Integration tests with leparse output                           │
│                                                                       │
│  STEP 2: lerecherche - Vector Search Backend                          │
│  ├── Task 3.5: Implement vector search                               │
│  │   ├── Integrate HNSW or similar vector index                      │
│  │   ├── Implement embedding generation (or placeholder)             │
│  │   └── Implement semantic_search() with actual vector lookup       │
│  └── Integration tests with legraphe PDG                             │
│                                                                       │
│  STEP 3: lestockage - PDG Persistence Bridge                          │
│  ├── Task 4.6: Implement PDG persistence                             │
│  │   ├── Implement save_pdg() to store nodes/edges                   │
│  │   ├── Implement load_pdg() to reconstruct from storage           │
│  │   └── Graph reconstruction queries                                │
│  └── Integration tests with legraphe PDG                             │
│                                                                       │
│  STEP 4: Integration & Validation                                    │
│  ├── End-to-end workflow testing                                     │
│  ├── Performance benchmarking                                        │
│  └── Code quality validation                                         │
│                                                                       │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Success Criteria

The Master Track is complete when:

1. **✅ leparse** - Production-ready for 12 languages (ACHIEVED)
2. **❌ legraphe** - PDG builds from actual code (needs AST→PDG extraction)
3. **❌ lerecherche** - Semantic search functional (needs vector backend)
4. **❌ lestockage** - PDG persistence working (needs save/load bridge)
5. **⏸️ lepasserelle** - Optional for pure Rust goal
6. **❌ Integration** - End-to-end pipeline tested

---

## Progress Log

### 2025-01-25 (Source Code Verification - Accurate State)
**Master Track Status:** In Progress (~35% overall)

**leparse:** 90% COMPLETE ✅ PRODUCTION READY
- 97/97 tests passing
- 12 languages fully implemented
- Parallel parsing complete
- Zero-copy architecture verified
- Tzar review PASSED

**legraphe:** 30% COMPLETE ⚠️ FOUNDATIONS ONLY
- 9/9 tests passing
- PDG structures, traversal, embeddings implemented
- **CRITICAL MISSING:** AST→PDG extraction (Task 2.6)
- **CRITICAL MISSING:** Serialization/deserialization (Task 2.7)

**lerecherche:** 25% COMPLETE ⚠️ TEXT SEARCH ONLY
- 6/6 tests passing
- Text search, hybrid scoring, context expansion working
- **CRITICAL MISSING:** Vector search backend (Task 3.5)
- **CRITICAL MISSING:** Embedding model integration

**lestockage:** 35% COMPLETE ⚠️ CRUD ONLY
- 8/8 tests passing
- Node/edge storage, BLAKE3 hashing, analytics complete
- **CRITICAL MISSING:** PDG persistence bridge (Task 4.6)
- MISSING: Cross-project resolution, DuckDB integration

**lepasserelle:** 15% COMPLETE ⏸️ OPTIONAL
- PyO3 bindings structure exists
- Most functions return placeholders
- RSS monitoring works
- **OPTIONAL:** Only needed if Python interop required

**Overall Assessment:**
- **leparse is production-ready** and can be used as-is
- **Integration layers between crates are missing**
- ~40-50% implementation work remains
- Clear sequential path forward: legraphe → lerecherche → lestockage

---

## Next Steps

**IMMEDIATE PRIORITY:** Implement AST → PDG extraction in legraphe (Task 2.6)
- This is the critical bridge between leparse output and legraphe PDG
- Once complete, enables legraphe → lerecherche → lestockage integration

**SECONDARY PRIORITY:** Vector search backend in lerecherche (Task 3.5)
- Enables actual semantic search (currently text-only)

**TERTIARY PRIORITY:** PDG persistence bridge in lestockage (Task 4.6)
- Enables saving/loading PDGs to/from database
