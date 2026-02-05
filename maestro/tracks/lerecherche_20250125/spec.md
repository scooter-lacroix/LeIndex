# Specification: lerecherche - Search & Analysis Fusion

**Track ID:** `lerecherche_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

### Vision

`lerecherche` (French for "The Search") is the Search & Analysis Fusion layer of the LeIndex Rust Renaissance. It implements node-level semantic search with vector-AST synergy, bridging semantic vector search with deep graph-based context expansion.

### The "Why"

**Current State:**
- Text-based search only
- Limited semantic understanding
- No intelligent context expansion
- Search and analysis are separate

**Target State:**
- Node-level semantic search (every function/class searchable)
- Semantic entry points → graph expansion pipeline
- Vector-AST synergy for deep understanding
- Unified search和分析 experience

### Key Principles

1. **Node-Level Search** - Every function/class has its own embedding
2. **Semantic Entry Points** - Vector search finds entry → graph expands context
3. **Hybrid Scoring** - Combines semantic + structural signals
4. **Natural Language Queries** - Ask questions, get code answers

---

## Functional Requirements

### FR-1 Node-Level Vector Search

- LEANN backend (HNSW/DiskANN) for node embeddings
- Find functions by semantic meaning, not just text
- Top-K results return symbol IDs, not file paths
- Sub-10ms lookups for individual nodes

### FR-2 Semantic Entry Points

- Pipeline: Query → Vector Search → Symbol ID → PDG Expansion → Context
- Automatic deep analysis triggered by search results
- Gravity-based context expansion from entry nodes
- LLM-ready summary format

### FR-3 Hybrid Scoring

- Combines semantic similarity (LEANN) with structural relevance (graph position)
- Adaptive ranking based on query type
- Context-aware result highlighting
- Tunable scoring weights

### FR-4 Natural Language Queries

- "Show me how embedding batching works" → Finds function → Expands call graph
- "Where is authentication logic handled?" → Semantic search across all auth patterns
- "What are the performance bottlenecks?" → Complexity + centrality analysis
- Support for complexity + centrality queries

---

## Non-Functional Requirements

### Performance Targets

- **Search Latency:** <100ms P95 for semantic queries
- **Node Lookup:** <10ms for node-level lookups
- **Indexing:** Batch embedding generation for large codebases

### Quality Requirements

- **Test Coverage:** >95% for all search operations
- **Validation:** Compare relevance vs baseline
- **Code Quality:** Pass clippy with no warnings

---

## Acceptance Criteria

**AC-1 Node-Level Search**
- [ ] Node-level search finds functions semantically
- [ ] Sub-10ms lookups achieved
- [ ] Top-K results return symbol IDs

**AC-2 Semantic Entry Points**
- [ ] Semantic entry points trigger correct graph expansion
- [ ] Pipeline: Query → Vector Search → PDG Expansion → Context
- [ ] LLM-ready summaries generated

**AC-3 Hybrid Scoring**
- [ ] Hybrid scoring improves relevance over text-only
- [ ] Adaptive ranking working for different query types
- [ ] Scoring weights tunable

**AC-4 Natural Language Queries**
- [ ] Natural language queries return relevant code
- [ ] Complexity + centrality queries working
- [ ] Query latency <100ms P95

---

## Dependencies

### Internal Dependencies
- `legraphe_20250125` - Requires PDG for context expansion

### External Rust Crates
- `hnsw-rs` or LEANN Rust backend (vector search)
- `pyo3` (Python bindings for embedding model)
- `serde` (serialization)

---

## Out of Scope

- **No Web UI** - CLI/MCP only
- **No Federated Search** - Single-machine architecture
- **No Custom Model Training** - Use existing CodeRankEmbed
