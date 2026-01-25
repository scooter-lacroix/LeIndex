# Implementation Plan: LeIndex Rust Renaissance - Master Track

**Track ID:** `leindex_rust_refactor_20250125`
**Track Type:** Master Track (orchestrate-capable)
**Status:** In Progress (Phase 0)
**Created:** 2025-01-25
**Last Updated:** 2025-01-25

---

## Overview

This Master Track implements a complete Python-to-Rust rewrite of LeIndex, transforming it into a Deep Code Intelligence Engine. The implementation is organized as **5 orchestrate-capable sub-tracks** that can be executed via `/maestro:orchestrate`.

**Execution Strategy:**
1. Execute this Master Track plan to create all sub-tracks
2. Use `/maestro:orchestrate` to execute sub-tracks in dependency order
3. Each sub-track has its own `spec.md` and `plan.md`

---

## Phase 0: Foundation & Setup

### Objective
Prepare the project infrastructure for Rust integration while maintaining Python compatibility during transition.

- [x] **Task 0.1: Create Rust workspace structure** ✅
  - [x] Create `Cargo.toml` workspace configuration
  - [x] Set up crate structure: `leparse`, `legraphe`, `lerecherche`, `lestockage`, `lepasserelle`
  - [x] Configure workspace-level dependencies
  - [x] Set up dev-dependencies (test frameworks, linting tools)

- [ ] **Task 0.2: Configure Python-Rust build pipeline**
  - [ ] Set up `maturin` or `setuptools-rust` for PyO3 bindings
  - [ ] Update `pyproject.toml` for Rust extension building
  - [ ] Configure build scripts for grammar generation
  - [ ] Add CI/CD configuration for Rust compilation

- [ ] **Task 0.3: Establish testing infrastructure**
  - [ ] Set up Rust test framework (`cargo test`, `rstest`)
  - [ ] Create Python validation test harness
  - [ ] Configure integration test framework
  - [ ] Set up benchmarking infrastructure (`criterion`)

- [x] **Task 0.4: Create Sub-Track Specifications** ✅
  - [x] Generate `leparse` sub-track spec and plan
  - [x] Generate `legraphe` sub-track spec and plan
  - [x] Generate `lerecherche` sub-track spec and plan
  - [x] Generate `lestockage` sub-track spec and plan
  - [x] Generate `lepasserelle` sub-track spec and plan

- [ ] **Task: Maestro - User Manual Verification 'Phase 0: Foundation & Setup'**
  - (Protocol in workflow.md)

---

## Phase 1: Core Parsing Engine (`leparse_20250125`)

### Objective
Build zero-copy AST extraction with multi-language support using tree-sitter.

- [x] **Task 1.1: Implement tree-sitter integration** ✅
  - [x] Add tree-sitter dependencies for all target languages
  - [x] Create `LanguageConfig` struct per language
  - [x] Implement lazy-loaded grammar loading system
  - [x] Write tests for grammar loading correctness
  - **Completed:** 2025-01-25, Commit: 961c0e0

- [x] **Task 1.2: Define `CodeIntelligence` trait + Python Implementation** ✅
  - [x] Create trait definition with required methods
  - [x] Implement for Python language
  - [x] Add `get_signatures()` extraction with parameters, types, docstrings
  - [x] Add `compute_cfg()` control flow graph generation
  - [x] Add `extract_complexity()` metrics calculation
  - **Completed:** 2025-01-25, Commit: 015ccfe

- [x] **Task 1.3: Implement zero-copy AST node types** ✅
  - [x] Create `AstNode` struct with byte-slice references
  - [x] Implement `SignatureInfo` extraction
  - [x] Implement `FunctionElement`, `ClassElement`, `ModuleElement`
  - [x] Add docstring extraction with semantic summarization
  - [x] Write zero-copy verification tests
  - **Completed:** 2025-01-25, Commit: a02ba49

- [ ] **Task 1.4: Multi-language support expansion** ⏳ Partial
  - [ ] Implement `CodeIntelligence` for JavaScript/TypeScript
  - [ ] Implement for Go language
  - [ ] Implement for Rust language
  - [ ] Implement for remaining 13+ languages
  - [x] Language-agnostic test suite created
  - **Status:** Python complete, 13+ languages remaining

- [ ] **Task 1.5: Parallel parsing with rayon**
  - [ ] Implement parallel file parsing pipeline
  - [ ] Add thread-safe AST node pooling
  - [ ] Benchmark vs Python baseline
  - [ ] Optimize for 50K+ file workloads

- [ ] **Task: Maestro - User Manual Verification 'Phase 1: Core Parsing Engine'**
  - (Protocol in workflow.md)

**Sub-Track Status:** Phase 1-3 complete, 32/32 tests passing. Remaining: multi-language expansion and parallel processing.

---

## Phase 2: Graph Intelligence Core (`legraphe`)

### Objective
Build the Program Dependence Graph (PDG) engine with gravity-based traversal.

- [ ] **Task 2.1: Define graph data structures**
  - [ ] Create `petgraph::StableGraph` wrapper types
  - [ ] Define `Node` and `Edge` types with metadata
  - [ ] Implement `u32` node indexing
  - [ ] Add graph serialization/deserialization

- [ ] **Task 2.2: Implement PDG construction**
  - [ ] Build Call Graph extraction from AST
  - [ ] Build Data Flow Graph extraction
  - [ ] Build Inheritance Graph extraction
  - [ ] Merge into unified PDG
  - [ ] Write tests for graph correctness

- [ ] **Task 2.3: Gravity-based traversal algorithm**
  - [ ] Implement relevance scoring formula
  - [ ] Create priority queue based expansion
  - [ ] Add token-budget aware context building
  - [ ] Implement flow-aware hotspot detection
  - [ ] Benchmark vs BFS approach

- [ ] **Task 2.4: Node embedding generation**
  - [ ] Integrate CodeRankEmbed model via PyO3
  - [ ] Implement per-function/class embedding
  - [ ] Store 768-dim vectors efficiently
  - [ ] Add embedding caching layer
  - [ ] Write tests for embedding quality

- [ ] **Task 2.5: Impact analysis queries**
  - [ ] Implement bitmask-based reachability
  - [ ] Add forward impact tracing
  - [ ] Add backward impact tracing
  - [ ] Create analysis query API
  - [ ] Validate against Python baseline

- [ ] **Task: Maestro - User Manual Verification 'Phase 2: Graph Intelligence Core'**
  - (Protocol in workflow.md)

---

## Phase 3: Search & Analysis Fusion (`lerecherche`)

### Objective
Implement node-level semantic search with vector-AST synergy.

- [ ] **Task 3.1: LEANN backend integration**
  - [ ] Integrate HNSW/DiskANN for node embeddings
  - [ ] Implement node-level indexing pipeline
  - [ ] Add batch embedding generation
  - [ ] Optimize for sub-10ms lookups
  - [ ] Write performance tests

- [ ] **Task 3.2: Semantic entry point implementation**
  - [ ] Create query → vector search pipeline
  - [ ] Implement symbol ID → PDG expansion
  - [ ] Add context summarization
  - [ ] Create unified search API
  - [ ] Write integration tests

- [ ] **Task 3.3: Hybrid scoring algorithm**
  - [ ] Combine semantic + structural signals
  - [ ] Implement adaptive ranking
  - [ ] Add context-aware highlighting
  - [ ] Tune scoring weights
  - [ ] Validate relevance improvements

- [ ] **Task 3.4: Natural language query processing**
  - [ ] Implement query understanding
  - [ ] Add semantic search across patterns
  - [ ] Support complexity + centrality queries
  - [ ] Create query examples and tests
  - [ ] Benchmark query latency (<100ms P95)

- [ ] **Task 3.5: Search API and interface**
  - [ ] Design search request/response types
  - [ ] Implement streaming responses
  - [ ] Add query result caching
  - [ ] Create search analytics
  - [ ] Document API usage

- [ ] **Task: Maestro - User Manual Verification 'Phase 3: Search & Analysis Fusion'**
  - (Protocol in workflow.md)

---

## Phase 4: Persistent Storage Layer (`lestockage`)

### Objective
Implement extended SQLite schema with Salsa incremental computation.

- [ ] **Task 4.1: Extend SQLite schema**
  - [ ] Create migration for `intel_nodes` table
  - [ ] Create migration for `intel_edges` table
  - [ ] Create migration for `analysis_cache` table
  - [ ] Add foreign key constraints
  - [ ] Write schema validation tests

- [ ] **Task 4.2: Implement node persistence**
  - [ ] Create CRUD operations for `intel_nodes`
  - [ ] Add embedding BLOB storage/retrieval
  - [ ] Implement batch insert optimization
  - [ ] Add indexing for query performance
  - [ ] Write persistence tests

- [ ] **Task 4.3: Implement edge persistence**
  - [ ] Create CRUD operations for `intel_edges`
  - [ ] Add edge type filtering
  - [ ] Implement bulk edge operations
  - [ ] Add graph reconstruction queries
  - [ ] Write edge validation tests

- [ ] **Task 4.4: Salsa incremental computation**
  - [ ] Implement node-level BLAKE3 hashing
  - [ ] Create query-based invalidation system
  - [ ] Add incremental re-computation logic
  - [ ] Implement symbol-level change detection
  - [ ] Benchmark incremental vs full rebuild

- [ ] **Task 4.5: Cross-project intelligence**
  - [ ] Create global symbol table
  - [ ] Implement cross-project resolution
  - [ ] Add external project references
  - [ ] Implement lazy loading for external symbols
  - [ ] Write cross-project tests

- [ ] **Task 4.6: DuckDB analytics integration**
  - [ ] Create graph metrics queries
  - [ ] Implement hotspot detection analytics
  - [ ] Add codebase evolution tracking
  - [ ] Create analytics export functions
  - [ ] Document analytics API

- [ ] **Task: Maestro - User Manual Verification 'Phase 4: Persistent Storage Layer'**
  - (Protocol in workflow.md)

---

## Phase 5: Bridge & Integration (`lepasserelle`)

### Objective
Create PyO3 FFI bindings and unified MCP tool.

- [ ] **Task 5.1: PyO3 module setup**
  - [ ] Create `leindex_rust` Python module
  - [ ] Expose `RustAnalyzer` class
  - [ ] Expose `build_weighted_context` function
  - [ ] Add Python-friendly error handling
  - [ ] Write FFI contract tests

- [ ] **Task 5.2: Zero-copy data transfer**
  - [ ] Implement mmap for source files
  - [ ] Create shared memory buffers
  - [ ] Add zero-copy embedding transfer
  - [ ] Optimize FFI boundary crossings
  - [ ] Benchmark transfer overhead

- [ ] **Task 5.3: Unified MCP tool**
  - [ ] Implement `leindex_deep_analyze` tool
  - [ ] Add semantic search entry point
  - [ ] Integrate Rust graph expansion
  - [ ] Create LLM-ready summary format
  - [ ] Write MCP tool tests

- [ ] **Task 5.4: Memory management**
  - [ ] Implement RSS monitoring
  - [ ] Add 90% threshold spilling logic
  - [ ] Create PDG cache clearing
  - [ ] Implement DuckDB cache spilling
  - [ ] Add Python gc coordination

- [ ] **Task 5.5: Error handling and logging**
  - [ ] Create Rust error types with `thiserror`
  - [ ] Convert to Python exceptions
  - [ ] Add structured logging
  - [ ] Implement debug/trace modes
  - [ ] Document error scenarios

- [ ] **Task 5.6: Documentation and examples**
  - [ ] Write API documentation
  - [ ] Create usage examples
  - [ ] Add migration guide from Python
  - [ ] Document performance characteristics
  - [ ] Create troubleshooting guide

- [ ] **Task: Maestro - User Manual Verification 'Phase 5: Bridge & Integration'**
  - (Protocol in workflow.md)

---

## Phase 6: Validation & Performance Testing

### Objective
Comprehensive testing, benchmarking, and validation against Python baseline.

- [ ] **Task 6.1: Performance benchmarking**
  - [ ] Benchmark indexing speed (50K files target <60s)
  - [ ] Benchmark memory usage (10x reduction target)
  - [ ] Benchmark search latency (<100ms P95 target)
  - [ ] Benchmark analysis speed vs BFS
  - [ ] Create performance regression tests

- [ ] **Task 6.2: Accuracy validation**
  - [ ] Compare AST extraction vs Python baseline
  - [ ] Compare PDG construction vs Python baseline
  - [ ] Compare search results vs Python baseline
  - [ ] Validate token efficiency (20% improvement)
  - [ ] Document any accuracy differences

- [ ] **Task 6.3: Integration testing**
  - [ ] Test end-to-end workflows
  - [ ] Test MCP tool functionality
  - [ ] Test cross-project resolution
  - [ ] Test incremental computation
  - [ ] Test memory spilling scenarios

- [ ] **Task 6.4: Code quality validation**
  - [ ] Verify >95% test coverage
  - [ ] Run clippy with no warnings
  - [ ] Validate all unsafe code blocks
  - [ ] Review error handling completeness
  - [ ] Check documentation coverage

- [ ] **Task 6.5: Production readiness**
  - [ ] Test on 100K+ file codebases
  - [ ] Validate resource limits
  - [ ] Test graceful degradation
  - [ ] Verify error recovery
  - [ ] Create runbook for operations

- [ ] **Task: Maestro - User Manual Verification 'Phase 6: Validation & Performance Testing'**
  - (Protocol in workflow.md)

---

## Orchestration Guide

### Sub-Track Execution Order

Use `/maestro:orchestrate` with the following dependency order:

```
1. leparse (must complete first)
2. legraphe (depends on leparse)
3. lerecherche (depends on legraphe)
4. lestockage (depends on legraphe)
5. lepasserelle (depends on lerecherche, lestockage)
```

### Parallel Execution Opportunities

- `lerecherche` and `lestockage` can execute in parallel after `legraphe` completes
- Each sub-track's internal phases execute sequentially

### Checkpoint Strategy

- Each sub-track completion creates a checkpoint
- Phase completion verification applies within each sub-track
- Master Track completes when all sub-tracks are done

---

## Success Criteria

The Master Track is complete when:

1. **All Sub-Tracks Complete** - All 5 sub-tracks marked as completed
2. **Performance Targets Met** - All NFRs validated through benchmarks
3. **Quality Gates Passed** - Tests passing, coverage >95%, no clippy warnings
4. **MCP Tool Functional** - `leindex_deep_analyze` working end-to-end
5. **Documentation Complete** - All APIs documented, examples provided
6. **Production Ready** - Validated on 100K+ file codebases

---

## Notes

- **Reference Code Only:** Code in `Rust_ref(DO_NOT_DIRECTLY_USE)` is for reference only - all Rust code must be written from scratch
- **No TLDR Branding:** All components use LeIndex-themed French-inspired naming
- **Greenfield Approach:** Complete re-architecture, not a port
- **Python Validation:** New Rust tests validated against Python behavior for correctness

## Progress Log

### 2025-01-25
- **Task 0.1 Completed**: Rust workspace structure created and verified
  - All 5 crates (leparse, legraphe, lerecherche, lestockage, lepasserelle) compiling successfully
  - Workspace dependencies configured (tree-sitter, petgraph, pyo3, serde, rusqlite, psutil, chrono, etc.)
  - Core traits and types defined in each crate
  - Ready to proceed to Task 0.2 (Python-Rust build pipeline)
