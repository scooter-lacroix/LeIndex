# Implementation Plan: lerecherche - Search & Analysis Fusion

**Track ID:** `lerecherche_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Search & Analysis Fusion layer for LeIndex Rust Renaissance. It provides node-level semantic search with vector-AST synergy.

---

## Phase 1: Vector Search Backend

### Objective
Integrate vector search infrastructure for node embeddings.

- [ ] **Task 1.1: Integrate HNSW/DiskANN backend**
  - [ ] Add hnsw-rs or LEANN Rust backend
  - [ ] Configure HNSW parameters for performance
  - [ ] Implement vector index initialization
  - [ ] Write tests for index creation

- [ ] **Task 1.2: Implement node-level indexing**
  - [ ] Create indexing pipeline for node embeddings
  - [ ] Add batch embedding generation
  - [ ] Implement incremental index updates
  - [ ] Write tests for indexing correctness

- [ ] **Task 1.3: Optimize for sub-10ms lookups**
  - [ ] Tune HNSW parameters for speed
  - [ ] Add query caching
  - [ ] Implement parallel query processing
  - [ ] Benchmark query latency

- [ ] **Task 1.4: Write performance tests**
  - [ ] Test lookup latency (<10ms target)
  - [ ] Test index build time
  - [ ] Test memory usage
  - [ ] Create performance regression tests

- [ ] **Task: Maestro - Phase 1 Verification**

---

## Phase 2: Semantic Entry Points

### Objective
Implement query → vector search → PDG expansion pipeline.

- [ ] **Task 2.1: Implement query → vector search**
  - [ ] Embed user queries using same model
  - [ ] Implement top-K vector search
  - [ ] Return symbol IDs (not file paths)
  - [ ] Write tests for search accuracy

- [ ] **Task 2.2: Implement symbol ID → PDG expansion**
  - [ ] Integrate with legraphe for PDG access
  - [ ] Implement gravity-based expansion from entry nodes
  - [ ] Build context from expanded graph
  - [ ] Write tests for expansion correctness

- [ ] **Task 2.3: Implement context summarization**
  - [ ] Summarize expanded context for LLM
  - [ ] Create LLM-ready output format
  - [ ] Add relevance highlighting
  - [ ] Write tests for summarization

- [ ] **Task 2.4: Create unified search API**
  - [ ] Design search request/response types
  - [ ] Implement streaming responses
  - [ ] Add query result caching
  - [ ] Write integration tests

- [ ] **Task: Maestro - Phase 2 Verification**

---

## Phase 3: Hybrid Scoring

### Objective
Combine semantic + structural signals for better relevance.

- [ ] **Task 3.1: Implement hybrid scoring algorithm**
  - [ ] Combine semantic similarity (LEANN) with structural relevance (graph)
  - [ ] Add complexity score from AST
  - [ ] Add centrality score from PDG
  - [ ] Write tests for scoring correctness

- [ ] **Task 3.2: Implement adaptive ranking**
  - [ ] Detect query type (semantic vs structural)
  - [ ] Adjust scoring weights based on query
  - [ ] Add ranking feedback mechanism
  - [ ] Write tests for adaptive behavior

- [ ] **Task 3.3: Add context-aware highlighting**
  - [ ] Highlight relevant code sections
  - [ ] Annotate with match reasons
  - [ ] Add confidence scores
  - [ ] Write tests for highlighting

- [ ] **Task 3.4: Tune scoring weights**
  - [ ] Create tuning dataset
  - [ ] Run weight optimization
  - [ ] Validate relevance improvements
  - [ ] Document optimal settings

- [ ] **Task: Maestro - Phase 3 Verification**

---

## Phase 4: Natural Language Queries

### Objective
Support natural language queries for code search.

- [ ] **Task 4.1: Implement query understanding**
  - [ ] Parse natural language queries
  - [ ] Extract search intent
  - [ ] Detect query patterns
  - [ ] Write tests for query parsing

- [ ] **Task 4.2: Implement semantic search across patterns**
  - [ ] "Show me how X works" → function search
  - [ ] "Where is X handled?" → pattern search
  - [ ] "What are bottlenecks?" → complexity search
  - [ ] Write query pattern tests

- [ ] **Task 4.3: Support complexity + centrality queries**
  - [ ] Add complexity-based ranking
  - [ ] Add centrality-based ranking
  - [ ] Combine with semantic scores
  - [ ] Write tests for combined queries

- [ ] **Task 4.4: Create query examples and tests**
  - [ ] Document query patterns
  - [ ] Create example queries
  - [ ] Add query validation tests
  - [ ] Benchmark query latency (<100ms P95)

- [ ] **Task: Maestro - Phase 4 Verification**

---

## Phase 5: Search API and Interface

### Objective
Complete search API with analytics.

- [ ] **Task 5.1: Design search request/response types**
  - [ ] Define SearchRequest struct
  - [ ] Define SearchResponse struct
  - [ ] Add pagination support
  - [ ] Add filtering options

- [ ] **Task 5.2: Implement streaming responses**
  - [ ] Add async response streaming
  - [ ] Implement chunked results
  - [ ] Add progress indication
  - [ ] Write tests for streaming

- [ ] **Task 5.3: Add query result caching**
  - [ ] Implement LRU cache for results
  - [ ] Add cache invalidation
  - [ ] Tune cache size
  - [ ] Write tests for caching

- [ ] **Task 5.4: Create search analytics**
  - [ ] Track query patterns
  - [ ] Measure result relevance
  - [ ] Monitor performance metrics
  - [ ] Export analytics data

- [ ] **Task 5.5: Document API usage**
  - [ ] Write API documentation
  - [ ] Create usage examples
  - [ ] Add troubleshooting guide
  - [ ] Document performance characteristics

- [ ] **Task: Maestro - Phase 5 Verification**

---

## Success Criteria

The track is complete when:

1. **Node-level search working** - Every function/class searchable via embeddings
2. **Semantic entry points working** - Query → Vector → PDG → Context pipeline
3. **Hybrid scoring implemented** - Semantic + structural signals combined
4. **Natural language queries supported** - Ask questions, get code
5. **Performance targets met** - <100ms P95 search latency
6. **Tests passing** - >95% coverage, all tests green

---

## Notes

- **Depends on legraphe:** Requires PDG for context expansion
- **Can run parallel with lestockage:** Both depend on legraphe
- **Reference Code Only:** Code in `Rust_ref(DO_NOT_DIRECTLY_USE)` is for reference only
