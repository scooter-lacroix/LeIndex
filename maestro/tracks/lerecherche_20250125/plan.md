# Implementation Plan: lerecherche - Search & Analysis Fusion

**Track ID:** `lerecherche_20250125`
**Track Type:** Standard Track
**Status:** IN PROGRESS (Source-Code-Verified: 2025-01-25)
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Search & Analysis Fusion layer for LeIndex Rust Renaissance. It provides node-level semantic search with vector-AST synergy.

**Source-Code-Verified Status:** ~25% COMPLETE ⚠️ TEXT SEARCH ONLY, SEMANTIC SEARCH MISSING

**Test Results:** 6/6 tests passing ✅
**Code State:** Text search works, semantic search is placeholder

---

## Phase 1: Search Engine Structure ✅ COMPLETE

### Objective
Set up search engine data structures.

- [x] **Task 1.1: Define search types** ✅ COMPLETE
  - [x] `SearchEngine` with node indexing
  - [x] `SearchQuery` with query text, top_k, token_budget, semantic flag
  - [x] `SearchResult` with rank, scores, context
  - [x] `NodeInfo` with metadata for indexing
  - **File:** `src/search.rs` (325 lines)

- [x] **Task 1.2: Implement node indexing** ✅ COMPLETE
  - [x] `index_nodes()` - Build in-memory node index
  - [x] Stores node_id, file_path, symbol_name, content, byte_range, complexity
  - [x] Supports optional embeddings
  - **File:** `src/search.rs` lines 90-93

---

## Phase 2: Text Search ✅ COMPLETE

### Objective
Implement text-based search functionality.

- [x] **Task 2.1: Implement text matching** ✅ COMPLETE
  - [x] `calculate_text_score()` - Substring matching
  - [x] Case-insensitive search
  - [x] Token overlap calculation
  - [x] Returns score 0.0-1.0
  - **File:** `src/search.rs` lines 155-173

- [x] **Task 2.2: Implement search execution** ✅ COMPLETE
  - [x] `search()` - Main search method
  - [x] Filters by text match or semantic flag
  - [x] Combines text, semantic, and structural scores
  - [x] Sorts by overall score and returns top-K
  - **File:** `src/search.rs` lines 96-153

- [x] **Task 2.3: Result ranking** ✅ COMPLETE
  - [x] Rank by overall score
  - [x] Assign sequential ranks after sorting
  - [x] Filter out zero-score results
  - **File:** `src/search.rs` lines 136-152

---

## Phase 3: Hybrid Scoring ✅ COMPLETE

### Objective
Combine semantic + structural signals for better relevance.

- [x] **Task 3.1: Implement hybrid scoring algorithm** ✅ COMPLETE
  - [x] `HybridScorer` with configurable weights
  - [x] `score()` - Weighted combination: `semantic*0.5 + structural*0.3 + text*0.2`
  - [x] Result clamped to 0.0-1.0
  - **File:** `src/ranking.rs` (191 lines)

- [x] **Task 3.2: Implement adaptive ranking** ✅ COMPLETE
  - [x] `rerank()` - Boost scores based on query type
  - [x] QueryType: Semantic, Structural, Text
  - [x] 1.2x boost for matching query type
  - **File:** `src/ranking.rs` lines 89-130

- [x] **Task 3.3: Define score types** ✅ COMPLETE
  - [x] `Score` with semantic, structural, text_match, overall
  - [x] `ScoreResult` with node_id, score, query_type
  - **File:** `src/ranking.rs` lines 6-163

---

## Phase 4: Semantic Processing ✅ COMPLETE

### Objective
Implement PDG context expansion.

- [x] **Task 4.1: Implement semantic processor** ✅ COMPLETE
  - [x] `SemanticProcessor` struct
  - [x] `process_entry()` - Expand context from semantic entry
  - [x] Integrates with legraphe for PDG access
  - **File:** `src/semantic.rs` (140 lines)

- [x] **Task 4.2: Implement PDG context expansion** ✅ COMPLETE
  - [x] Uses `GravityTraversal` for expansion
  - [x] Respects token budget
  - [x] Formats LLM-ready context with file/symbol annotations
  - **File:** `src/semantic.rs` lines 18-55

- [x] **Task 4.3: Define semantic types** ✅ COMPLETE
  - [x] `SemanticEntry` with node_id, relevance, entry_type
  - [x] `EntryType` enum (Function, Class, Module)
  - **File:** `src/search.rs` lines 193-228

---

## Phase 5: Vector Search Backend ❌ CRITICAL MISSING PIECE

### Objective
Integrate vector search infrastructure for semantic search.

- [ ] **Task 5.1: Integrate HNSW/DiskANN backend** ❌ NOT STARTED
  - [ ] Add hnsw-rs or similar vector index
  - [ ] Configure HNSW parameters for performance
  - [ ] Implement vector index initialization
  - [ ] Write tests for index creation
  - **Status:** CRITICAL - No vector index exists

- [ ] **Task 5.2: Implement node-level indexing** ❌ NOT STARTED
  - [ ] Create indexing pipeline for node embeddings
  - [ ] Add batch embedding generation
  - [ ] Implement incremental index updates
  - [ ] Write tests for indexing correctness
  - **Status:** CRITICAL - No embedding generation pipeline

- [ ] **Task 5.3: Implement vector search** ❌ PLACEHOLDER
  - [ ] `semantic_search()` - Currently returns empty Vec (line 176-183)
  - [ ] Embed user queries using same model
  - [ ] Return top-K symbol IDs from vector search
  - [ ] **File:** `src/search.rs` lines 176-183

- [ ] **Task 5.4: Implement embedding model integration** ❌ NOT STARTED
  - [ ] Integrate CodeRankEmbed or similar model
  - [ ] Implement per-function embedding generation
  - [ ] Implement per-class embedding generation
  - [ ] Generate 768-dim vectors
  - **Status:** CRITICAL - No embedding model integrated

---

## Phase 6: Natural Language Queries ❌ NOT IMPLEMENTED

### Objective
Support natural language queries for code search.

- [ ] **Task 6.1: Implement query understanding** ❌ NOT STARTED
  - [ ] Parse natural language queries
  - [ ] Extract search intent
  - [ ] Detect query patterns
  - [ ] Write tests for query parsing

- [ ] **Task 6.2: Implement semantic search across patterns** ❌ NOT STARTED
  - [ ] "Show me how X works" → function search
  - [ ] "Where is X handled?" → pattern search
  - [ ] "What are bottlenecks?" → complexity search
  - [ ] Write query pattern tests

- [ ] **Task 6.3: Support complexity + centrality queries** ❌ NOT STARTED
  - [ ] Add complexity-based ranking
  - [ ] Add centrality-based ranking
  - [ ] Combine with semantic scores
  - [ ] Write tests for combined queries

---

## Success Criteria

The track is complete when:

1. **✅ Text search working** - Substring and token search (ACHIEVED)
2. **✅ Hybrid scoring implemented** - Semantic + structural signals combined (ACHIEVED)
3. **✅ Context expansion working** - PDG-based gravity traversal (ACHIEVED)
4. **❌ Vector search working** - HNSW/DiskANN backend **NOT IMPLEMENTED**
5. **❌ Embedding generation working** - Per-function/class embeddings **NOT IMPLEMENTED**
6. **❌ Natural language queries supported** - Ask questions, get code **NOT IMPLEMENTED**

---

## Files Implemented

| File | Lines | Purpose | Status |
|------|-------|---------|--------|
| `src/lib.rs` | 20 | Module declarations, exports | ✅ COMPLETE |
| `src/search.rs` | 325 | SearchEngine, text search | ✅ COMPLETE (partial) |
| `src/semantic.rs` | 140 | PDG context expansion | ✅ COMPLETE |
| `src/ranking.rs` | 191 | Hybrid scoring | ✅ COMPLETE |

**Total:** ~676 lines of production Rust code

---

## What Works vs What's Missing

```
┌─────────────────────────────────────────────────────────────────────┐
│                        lerecherche STATUS                           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  ✅ COMPLETE (Working):                                              │
│  ├── SearchEngine with node indexing                                │
│  ├── Text search with substring/token matching                       │
│  ├── HybridScorer with configurable weights                          │
│  ├── Adaptive ranking by query type                                 │
│  ├── SemanticProcessor with PDG integration                         │
│  ├── expand_context() with gravity traversal                        │
│  └── LLM-ready context formatting                                    │
│                                                                       │
│  ❌ MISSING (Critical Integration):                                  │
│  ├── No HNSW/DiskANN vector index                                   │
│  ├── semantic_search() returns empty Vec                            │
│  ├── No embedding model integrated (CodeRankEmbed)                   │
│  ├── No embedding generation pipeline                                │
│  ├── No natural language query processing                           │
│  └── No semantic pattern matching                                    │
│                                                                       │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Implementation Plan for Remaining Work

### Task 5.1-5.4: Vector Search Backend

**Objective:** Add semantic search capability

**Implementation Strategy:**

1. **Add HNSW dependency**
   ```toml
   [dependencies]
   hnsw = "0.1"  # or similar crate
   ```

2. **Create `src/vector.rs` module**
   ```rust
   pub struct VectorIndex {
       index: Hnsw,
       dimension: usize,
   }

   impl VectorIndex {
       pub fn new(dimension: usize) -> Self { ... }
       pub fn insert(&mut self, id: String, vector: Vec<f32>) { ... }
       pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> { ... }
   }
   ```

3. **Update `SearchEngine`**
   - Add `VectorIndex` field
   - Implement actual `semantic_search()` with vector lookup
   - Generate embeddings during `index_nodes()`

4. **Integration Tests**
   - Test vector search accuracy
   - Benchmark search latency (<100ms P95 target)

---

## Next Steps

**IMMEDIATE PRIORITY:** Implement vector search backend (Task 5.1-5.4)
- Currently only text search works
- Need HNSW/DiskANN for actual semantic search
- Need embedding model integration

**SECONDARY PRIORITY:** Natural language queries (Task 6.1-6.3)
- Depends on vector search being functional

---

## Status: TEXT SEARCH ONLY, SEMANTIC SEARCH MISSING ⚠️

Text search, hybrid scoring, and PDG context expansion are fully implemented. The critical missing piece is the vector search backend for actual semantic search functionality.
