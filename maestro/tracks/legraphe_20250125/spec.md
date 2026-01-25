# Specification: legraphe - Graph Intelligence Core

**Track ID:** `legraphe_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

### Vision

`legraphe` (French for "The Graph") is the Graph Intelligence Core of the LeIndex Rust Renaissance. It builds Program Dependence Graphs (PDG) from AST nodes and implements gravity-based traversal for intelligent context expansion.

### The "Why"

**Current State:**
- Python-based graph construction with memory overhead
- BFS-based traversal lacks semantic relevance
- Limited impact analysis capabilities

**Target State:**
- Pure Rust PDG engine using petgraph
- Gravity-based traversal for semantic relevance
- Bitmask-based reachability for fast impact analysis
- Node embeddings for semantic entry points

### Key Principles

1. **Unified PDG** - Call Graph + Data Flow + Inheritance in one structure
2. **Gravity Traversal** - Relevance-based expansion, not distance-based
3. **Efficient Storage** - u32 indices, stable graph structure
4. **Semantic Reach** - Node embeddings for semantic search integration

---

## Functional Requirements

### FR-1 Program Dependence Graph (PDG)

- Unified dependency graph merging: Call Graph + Data Flow Graph + Inheritance Graph
- `petgraph::StableGraph` using `u32` indices for nodes
- Persistent storage in SQLite for instant retrieval
- Graph serialization/deserialization

### FR-2 Gravity-Based Traversal

- Relevance formula: `Relevance(N) = (SemanticScore(N) * Complexity(N)) / (Distance(Entry, N)^2)`
- Priority-weighted expansion using binary heap
- Token-budget aware context building
- Flow-aware hotspot detection (high eigenvector centrality)

### FR-3 Node Embeddings

- Individual embeddings for each function/class node
- CodeRankEmbed (nomic-ai) model integration via PyO3
- 768-dimensional vectors stored in BLOB columns
- Embedding caching layer

### FR-4 Impact Analysis

- Bitmask-based reachability queries O(V+E)
- Forward and backward impact tracing
- Cross-project dependency resolution

---

## Non-Functional Requirements

### Performance Targets

- **Graph Construction:** Fast PDG building from AST nodes
- **Traversal Speed:** Gravity traversal produces more relevant results than BFS
- **Memory Efficiency:** Efficient graph storage using u32 indices

### Quality Requirements

- **Test Coverage:** >95% for all graph operations
- **Validation:** Python validation tests for PDG correctness
- **Code Quality:** Pass clippy with no warnings

---

## Acceptance Criteria

**AC-1 PDG Construction**
- [ ] PDG builds correctly for complex codebases
- [ ] Call graph, data flow, and inheritance merged properly
- [ ] Graph serialization working

**AC-2 Gravity Traversal**
- [ ] Gravity traversal produces more relevant results than BFS
- [ ] Token-budget aware context building working
- [ ] Hotspot detection working

**AC-3 Node Embeddings**
- [ ] Node embeddings generate and store successfully
- [ ] Caching layer working correctly
- [ ] Integration with search layer working

**AC-4 Impact Analysis**
- [ ] Impact analysis returns correct symbol dependencies
- [ ] Bitmask-based queries efficient
- [ ] Forward/backward tracing working

---

## Dependencies

### Internal Dependencies
- `leparse_20250125` - Requires AST node types and CodeIntelligence trait

### External Rust Crates
- `petgraph` (graph structures)
- `pyo3` (Python bindings for embeddings)
- `serde` (serialization)
- `blake3` (hashing)

---

## Out of Scope

- **No Visualization** - Graph visualization can be built on top
- **No Distributed Graph** - Single-machine architecture only
- **No Graph Database** - SQLite-based storage only
