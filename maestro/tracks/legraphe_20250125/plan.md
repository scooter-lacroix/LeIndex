# Implementation Plan: legraphe - Graph Intelligence Core

**Track ID:** `legraphe_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Graph Intelligence Core for LeIndex Rust Renaissance. It builds Program Dependence Graphs (PDG) from AST nodes and implements gravity-based traversal.

---

## Phase 1: Graph Data Structures

### Objective
Define and implement graph data structures.

- [ ] **Task 1.1: Create graph wrapper types**
  - [ ] Wrap `petgraph::StableGraph` for PDG
  - [ ] Define `Node` and `Edge` types with metadata
  - [ ] Implement `u32` node indexing
  - [ ] Add documentation for graph types

- [ ] **Task 1.2: Implement graph serialization**
  - [ ] Add serialization support for nodes
  - [ ] Add serialization support for edges
  - [ ] Implement graph reconstruction from serialized form
  - [ ] Write tests for serialization correctness

- [ ] **Task 1.3: Define PDG metadata structures**
  - [ ] Define `NodeMetadata` (symbol info, complexity, etc.)
  - [ ] Define `EdgeMetadata` (edge types, weights)
  - [ ] Implement metadata builders
  - [ ] Write tests for metadata structures

- [ ] **Task: Maestro - Phase 1 Verification**

---

## Phase 2: PDG Construction

### Objective
Build PDG from AST nodes.

- [ ] **Task 2.1: Implement Call Graph extraction**
  - [ ] Extract function calls from AST
  - [ ] Build caller-callee relationships
  - [ ] Handle method calls and virtual dispatch
  - [ ] Write tests for call graph correctness

- [ ] **Task 2.2: Implement Data Flow Graph extraction**
  - [ ] Extract variable definitions and uses
  - [ ] Build data dependency edges
  - [ ] Handle function parameters and returns
  - [ ] Write tests for data flow correctness

- [ ] **Task 2.3: Implement Inheritance Graph extraction**
  - [ ] Extract class inheritance relationships
  - [ ] Build class hierarchy edges
  - [ ] Handle interface implementations
  - [ ] Write tests for inheritance correctness

- [ ] **Task 2.4: Merge into unified PDG**
  - [ ] Combine Call, Data Flow, and Inheritance graphs
  - [ ] Handle edge type conflicts
  - [ ] Optimize graph storage
  - [ ] Write integration tests

- [ ] **Task: Maestro - Phase 2 Verification**

---

## Phase 3: Gravity-Based Traversal

### Objective
Implement semantic relevance-based traversal.

- [ ] **Task 3.1: Implement relevance scoring**
  - [ ] Implement relevance formula
  - [ ] Add semantic score integration (placeholder for embeddings)
  - [ ] Add complexity metrics integration
  - [ ] Write tests for scoring correctness

- [ ] **Task 3.2: Implement priority queue expansion**
  - [ ] Create binary heap for prioritized traversal
  - [ ] Implement token-budget aware expansion
  - [ ] Add early termination logic
  - [ ] Write tests for traversal correctness

- [ ] **Task 3.3: Implement flow-aware hotspot detection**
  - [ ] Calculate eigenvector centrality
  - [ ] Identify high-centrality nodes
  - [ ] Prioritize hotspots in traversal
  - [ ] Write tests for hotspot detection

- [ ] **Task 3.4: Benchmark vs BFS**
  - [ ] Compare relevance results vs BFS
  - [ ] Measure performance differences
  - [ ] Document improvement metrics
  - [ ] Tune scoring parameters

- [ ] **Task: Maestro - Phase 3 Verification**

---

## Phase 4: Node Embeddings

### Objective
Integrate node embeddings for semantic search.

- [ ] **Task 4.1: Integrate CodeRankEmbed model**
  - [ ] Add PyO3 bindings for Python embedding model
  - [ ] Implement per-function embedding generation
  - [ ] Implement per-class embedding generation
  - [ ] Write tests for embedding generation

- [ ] **Task 4.2: Implement embedding storage**
  - [ ] Store 768-dim vectors efficiently
  - [ ] Add embedding BLOB columns to nodes
  - [ ] Implement embedding serialization
  - [ ] Write tests for embedding storage

- [ ] **Task 4.3: Implement embedding caching**
  - [ ] Create in-memory cache for embeddings
  - [ ] Add cache invalidation logic
  - [ ] Implement cache warming
  - [ ] Write tests for caching behavior

- [ ] **Task: Maestro - Phase 4 Verification**

---

## Phase 5: Impact Analysis

### Objective
Implement fast impact analysis queries.

- [ ] **Task 5.1: Implement bitmask-based reachability**
  - [ ] Add bitmask storage per node
  - [ ] Implement forward reachability
  - [ ] Implement backward reachability
  - [ ] Write tests for reachability correctness

- [ ] **Task 5.2: Implement forward impact tracing**
  - [ ] Trace "what breaks if I change this"
  - [ ] Return impacted symbols list
  - [ ] Add impact distance calculation
  - [ ] Write tests for forward tracing

- [ ] **Task 5.3: Implement backward impact tracing**
  - [ ] Trace "where did this come from"
  - [ ] Return dependency chain
  - [ ] Add circular dependency detection
  - [ ] Write tests for backward tracing

- [ ] **Task 5.4: Validate against Python baseline**
  - [ ] Compare PDG vs Python implementation
  - [ ] Validate impact analysis accuracy
  - [ ] Document any differences
  - [ ] Create regression test suite

- [ ] **Task: Maestro - Phase 5 Verification**

---

## Success Criteria

The track is complete when:

1. **PDG builds correctly** - Call, Data Flow, and Inheritance graphs merged properly
2. **Gravity traversal working** - More relevant results than BFS
3. **Node embeddings integrated** - Embeddings generate and store correctly
4. **Impact analysis working** - Fast reachability queries
5. **Tests passing** - >95% coverage, all tests green
6. **Python validated** - Matches baseline accuracy

---

## Notes

- **Depends on leparse:** Requires AST node types from leparse
- **Reference Code Only:** Code in `Rust_ref(DO_NOT_DIRECTLY_USE)` is for reference only
- **Greenfield:** Write all graph code from scratch using petgraph patterns
