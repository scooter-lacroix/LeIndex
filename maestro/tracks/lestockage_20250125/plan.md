# Implementation Plan: lestockage - Persistent Storage Layer

**Track ID:** `lestockage_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Persistent Storage Layer for LeIndex Rust Renaissance. It extends the SQLite schema and implements Salsa-based incremental computation.

---

## Phase 1: Extended SQLite Schema

### Objective
Create and extend SQLite schema for code intelligence.

- [ ] **Task 1.1: Create migration for intel_nodes table**
  - [ ] Design intel_nodes schema
  - [ ] Add id, project_id, file_path, symbol_name columns
  - [ ] Add node_type, signature, complexity columns
  - [ ] Add content_hash, embedding BLOB columns
  - [ ] Write migration script

- [ ] **Task 1.2: Create migration for intel_edges table**
  - [ ] Design intel_edges schema
  - [ ] Add caller_id, callee_id columns
  - [ ] Add edge_type column (call, inheritance, data_dependency)
  - [ ] Add composite primary key
  - [ ] Write migration script

- [ ] **Task 1.3: Create migration for analysis_cache table**
  - [ ] Design analysis_cache schema
  - [ ] Add node_hash primary key
  - [ ] Add cfg_data, complexity_metrics BLOB columns
  - [ ] Add timestamp column
  - [ ] Write migration script

- [ ] **Task 1.4: Add foreign key constraints**
  - [ ] Link intel_nodes to projects table
  - [ ] Link intel_edges to intel_nodes
  - [ ] Add cascade delete rules
  - [ ] Write constraint validation tests

- [ ] **Task 1.5: Add indexing for query performance**
  - [ ] Index on symbol_name for lookups
  - [ ] Index on project_id for filtering
  - [ ] Index on content_hash for incrementalism
  - [ ] Write query performance tests

- [ ] **Task 1.6: Write schema validation tests**
  - [ ] Test migration correctness
  - [ ] Test foreign key constraints
  - [ ] Test index effectiveness
  - [ ] Validate schema with real data

- [ ] **Task: Maestro - Phase 1 Verification**

---

## Phase 2: Node Persistence

### Objective
Implement CRUD operations for code intelligence nodes.

- [ ] **Task 2.1: Create node insertion operations**
  - [ ] Implement insert_node function
  - [ ] Handle embedding BLOB storage
  - [ ] Add upsert support
  - [ ] Write tests for insertion

- [ ] **Task 2.2: Create node retrieval operations**
  - [ ] Implement get_node_by_id
  - [ ] Implement get_nodes_by_symbol
  - [ ] Implement get_nodes_by_project
  - [ ] Write tests for retrieval

- [ ] **Task 2.3: Create node update/delete operations**
  - [ ] Implement update_node
  - [ ] Implement delete_node
  - [ ] Handle cascade deletes
  - [ ] Write tests for updates/deletes

- [ ] **Task 2.4: Implement batch insert optimization**
  - [ ] Add batch_insert_nodes
  - [ ] Use transaction for bulk operations
  - [ ] Optimize for large batches
  - [ ] Write performance tests

- [ ] **Task 2.5: Write persistence tests**
  - [ ] Test CRUD operations
  - [ ] Test embedding storage/retrieval
  - [ ] Test batch operations
  - [ ] Test error handling

- [ ] **Task: Maestro - Phase 2 Verification**

---

## Phase 3: Edge Persistence

### Objective
Implement CRUD operations for PDG edges.

- [ ] **Task 3.1: Create edge insertion operations**
  - [ ] Implement insert_edge function
  - [ ] Handle edge_type enumeration
  - [ ] Add upsert support
  - [ ] Write tests for insertion

- [ ] **Task 3.2: Create edge retrieval operations**
  - [ ] Implement get_edges_by_node
  - [ ] Implement get_edges_by_type
  - [ ] Implement get_call_graph
  - [ ] Write tests for retrieval

- [ ] **Task 3.3: Add edge type filtering**
  - [ ] Filter by call edges
  - [ ] Filter by inheritance edges
  - [ ] Filter by data_dependency edges
  - [ ] Write tests for filtering

- [ ] **Task 3.4: Implement bulk edge operations**
  - [ ] Add batch_insert_edges
  - [ ] Add batch_delete_edges
  - [ ] Optimize for bulk operations
  - [ ] Write performance tests

- [ ] **Task 3.5: Add graph reconstruction queries**
  - [ ] Reconstruct PDG from storage
  - [ ] Load subgraphs by project
  - [ ] Load subgraphs by symbol
  - [ ] Write tests for reconstruction

- [ ] **Task 3.6: Write edge validation tests**
  - [ ] Test edge CRUD operations
  - [ ] Test edge type filtering
  - [ ] Test graph reconstruction
  - [ ] Test bulk operations

- [ ] **Task: Maestro - Phase 3 Verification**

---

## Phase 4: Salsa Incremental Computation

### Objective
Implement node-level incremental recomputation.

- [ ] **Task 4.1: Implement node-level BLAKE3 hashing**
  - [ ] Hash AST sub-trees at symbol level
  - [ ] Hash function signatures
  - [ ] Hash class definitions
  - [ ] Write tests for hashing

- [ ] **Task 4.2: Create query-based invalidation system**
  - [ ] Track queries that depend on nodes
  - [ ] Invalidate dependent queries on change
  - [ ] Implement incremental invalidation
  - [ ] Write tests for invalidation

- [ ] **Task 4.3: Add incremental re-computation logic**
  - [ ] Detect changed nodes via hash comparison
  - [ ] Re-compute only changed nodes
  - [ ] Cache unchanged results
  - [ ] Write tests for incrementalism

- [ ] **Task 4.4: Implement symbol-level change detection**
  - [ ] Compare hashes at symbol granularity
  - [ ] Detect added/removed symbols
  - [ ] Detect modified symbols
  - [ ] Write tests for change detection

- [ ] **Task 4.5: Benchmark incremental vs full rebuild**
  - [ ] Measure speedup for single-symbol changes
  - [ ] Measure speedup for multi-symbol changes
  - [ ] Compare memory usage
  - [ ] Document performance characteristics

- [ ] **Task: Maestro - Phase 4 Verification**

---

## Phase 5: Cross-Project Intelligence

### Objective
Implement global symbol resolution across projects.

- [ ] **Task 5.1: Create global symbol table**
  - [ ] Design global_symbols table
  - [ ] Add symbol uniqueness constraints
  - [ ] Add cross-project references
  - [ ] Write migration script

- [ ] **Task 5.2: Implement cross-project resolution**
  - [ ] Resolve symbols across project boundaries
  - [ ] Handle symbol name conflicts
  - [ ] Track external dependencies
  - [ ] Write tests for resolution

- [ ] **Task 5.3: Add external project references**
  - [ ] Store references to external projects
  - [ ] Track external symbol locations
  - [ ] Handle lazy loading
  - [ ] Write tests for external refs

- [ ] **Task 5.4: Implement lazy loading for external symbols**
  - [ ] Load external symbols on demand
  - [ ] Cache frequently accessed symbols
  - [ ] Invalidate cache on changes
  - [ ] Write tests for lazy loading

- [ ] **Task 5.5: Write cross-project tests**
  - [ ] Test multi-project resolution
  - [ ] Test circular dependencies
  - [ ] Test external symbol loading
  - [ ] Test cache invalidation

- [ ] **Task: Maestro - Phase 5 Verification**

---

## Phase 6: DuckDB Analytics

### Objective
Integrate DuckDB for graph analytics.

- [ ] **Task 6.1: Create graph metrics queries**
  - [ ] Query centrality metrics
  - [ ] Query complexity distribution
  - [ ] Query graph density
  - [ ] Write tests for metrics

- [ ] **Task 6.2: Implement hotspot detection analytics**
  - [ ] Detect high-centrality nodes
  - [ ] Detect high-complexity nodes
  - [ ] Combine centrality + complexity
  - [ ] Write tests for detection

- [ ] **Task 6.3: Add codebase evolution tracking**
  - [ ] Track changes over time
  - [ ] Compare snapshots
  - [ ] Detect growth patterns
  - [ ] Write tests for evolution

- [ ] **Task 6.4: Create analytics export functions**
  - [ ] Export metrics to CSV
  - [ ] Export metrics to JSON
  - [ ] Generate analytics reports
  - [ ] Document analytics API

- [ ] **Task 6.5: Document analytics API**
  - [ ] Write API documentation
  - [ ] Create usage examples
  - [ ] Add query reference
  - [ ] Document performance characteristics

- [ ] **Task: Maestro - Phase 6 Verification**

---

## Success Criteria

The track is complete when:

1. **SQLite schema working** - All tables created and indexed
2. **Node/edge persistence working** - CRUD operations functional
3. **Salsa incrementalism working** - Only changed nodes re-computed
4. **Cross-project resolution working** - Global symbol table functional
5. **DuckDB analytics working** - Graph metrics computed correctly
6. **Tests passing** - >95% coverage, all tests green

---

## Notes

- **Depends on legraphe:** Requires PDG structures for storage
- **Can run parallel with lerecherche:** Both depend on legraphe
- **Reference Code Only:** Code in `Rust_ref(DO_NOT_DIRECTLY_USE)` is for reference only
