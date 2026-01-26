# Implementation Plan: lestockage - Persistent Storage Layer

**Track ID:** `lestockage_20250125`
**Track Type:** Standard Track
**Status:** IN PROGRESS (Source-Code-Verified: 2025-01-25)
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Persistent Storage Layer for LeIndex Rust Renaissance. It extends the SQLite schema and implements Salsa-based incremental computation.

**Source-Code-Verified Status:** ~35% COMPLETE ⚠️ CRUD COMPLETE, MISSING PDG INTEGRATION

**Test Results:** 8/8 tests passing ✅
**Code State:** CRUD operations complete, missing PDG persistence bridge

---

## Phase 1: SQLite Schema ✅ COMPLETE

### Objective
Create and extend SQLite schema for code intelligence.

- [x] **Task 1.1: Create intel_nodes table** ✅ COMPLETE
  - [x] Columns: id, project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding BLOB, timestamps
  - [x] Primary key on id
  - **File:** `src/schema.rs` lines 66-82

- [x] **Task 1.2: Create intel_edges table** ✅ COMPLETE
  - [x] Columns: caller_id, callee_id, edge_type, metadata
  - [x] Composite primary key (caller_id, callee_id, edge_type)
  - [x] FOREIGN KEY constraints to intel_nodes
  - **File:** `src/schema.rs` lines 84-96

- [x] **Task 1.3: Create analysis_cache table** ✅ COMPLETE
  - [x] Columns: node_hash (PK), cfg_data BLOB, complexity_metrics BLOB, timestamp
  - [x] For incremental computation caching
  - **File:** `src/schema.rs` lines 98-107

- [x] **Task 1.4: Add indexing** ✅ COMPLETE
  - [x] idx_nodes_project on project_id
  - [x] idx_nodes_file on file_path
  - [x] idx_nodes_symbol on symbol_name
  - [x] idx_nodes_hash on content_hash
  - **File:** `src/schema.rs` lines 109-125

- [x] **Task 1.5: Configure storage** ✅ COMPLETE
  - [x] WAL mode for better concurrency
  - [x] Cache size configuration
  - [x] `StorageConfig` struct
  - **File:** `src/schema.rs` lines 8-62

---

## Phase 2: Node Persistence ✅ COMPLETE

### Objective
Implement CRUD operations for code intelligence nodes.

- [x] **Task 2.1: Create node insertion** ✅ COMPLETE
  - [x] `insert()` - Insert single node record
  - [x] Returns auto-generated node ID
  - [x] Handles embedding BLOB storage
  - **File:** `src/nodes.rs` (244 lines) lines 66-85

- [x] **Task 2.2: Create node retrieval** ✅ COMPLETE
  - [x] `get()` - Get node by ID
  - [x] `find_by_hash()` - Find by content hash
  - [x] `get_by_file()` - Get all nodes in file
  - **File:** `src/nodes.rs` lines 117-186

- [x] **Task 2.3: Implement batch insert** ✅ COMPLETE
  - [x] `batch_insert()` - Bulk insert with transaction
  - [x] Atomic operation for all nodes
  - [x] Returns vector of IDs
  - **File:** `src/nodes.rs` lines 88-114

---

## Phase 3: Edge Persistence ✅ COMPLETE

### Objective
Implement CRUD operations for PDG edges.

- [x] **Task 3.1: Create edge insertion** ✅ COMPLETE
  - [x] `insert()` - Insert single edge
  - [x] Upsert support (ON CONFLICT DO UPDATE)
  - [x] Handles metadata JSON serialization
  - **File:** `src/edges.rs` (234 lines) lines 65-79

- [x] **Task 3.2: Create edge retrieval** ✅ COMPLETE
  - [x] `get_by_caller()` - Outgoing edges from node
  - [x] `get_by_callee()` - Incoming edges to node
  - [x] `get_by_type()` - Filter by edge type
  - **File:** `src/edges.rs` lines 105-174

- [x] **Task 3.3: Implement batch operations** ✅ COMPLETE
  - [x] `batch_insert()` - Bulk insert with transaction
  - [x] Atomic operation for all edges
  - **File:** `src/edges.rs` lines 82-102

---

## Phase 4: Salsa Incremental Computation ✅ COMPLETE

### Objective
Implement node-level incremental recomputation.

- [x] **Task 4.1: Implement BLAKE3 hashing** ✅ COMPLETE
  - [x] `NodeHash` wrapper around BLAKE3
  - [x] 64-character hex encoding
  - [x] `from_str()` validation
  - **File:** `src/salsa.rs` (188 lines) lines 8-32

- [x] **Task 4.2: Create incremental cache** ✅ COMPLETE
  - [x] `IncrementalCache` with storage backend
  - [x] `is_cached()` - Check if computation cached
  - [x] `get()` - Retrieve cached computation
  - [x] `put()` - Store computation result
  - [x] `invalidate_before()` - Time-based invalidation
  - **File:** `src/salsa.rs` lines 35-104

- [x] **Task 4.3: Implement query invalidation** ✅ COMPLETE
  - [x] `QueryInvalidation` system
  - [x] `invalidate_node()` - Remove from cache
  - [x] `get_affected_nodes()` - Get nodes affected by file change
  - **File:** `src/salsa.rs` lines 120-153

---

## Phase 5: Analytics ✅ COMPLETE (SQLite-based)

### Objective
Implement graph metrics and analytics.

- [x] **Task 5.1: Node type counts** ✅ COMPLETE
  - [x] `count_nodes_by_type()` - Group by node_type
  - [x] Returns `NodeTypeCount` vector
  - **File:** `src/analytics.rs` (155 lines) lines 18-32

- [x] **Task 5.2: Complexity distribution** ✅ COMPLETE
  - [x] `complexity_distribution()` - Bucket by complexity
  - [x] Buckets: simple, moderate, complex, very_complex
  - **File:** `src/analytics.rs` lines 34-58

- [x] **Task 5.3: Edge type counts** ✅ COMPLETE
  - [x] `count_edges_by_type()` - Group by edge_type
  - [x] Returns `EdgeTypeCount` vector
  - **File:** `src/analytics.rs` lines 60-74

- [x] **Task 5.4: Hotspot detection** ✅ COMPLETE
  - [x] `get_hotspots()` - High complexity + high fan-out
  - [x] Configurable threshold
  - [x] Returns `Hotspot` vector with metrics
  - **File:** `src/analytics.rs` lines 76-104

---

## Phase 6: PDG Persistence Bridge ❌ CRITICAL MISSING PIECE

### Objective
Implement PDG save/load from storage.

- [ ] **Task 6.1: Implement PDG persistence** ❌ NOT STARTED
  - [ ] `save_pdg()` - Save `ProgramDependenceGraph` to storage
  - [ ] Extract nodes from PDG and store in intel_nodes
  - [ ] Extract edges from PDG and store in intel_edges
  - [ ] Preserve all metadata
  - **Status:** CRITICAL - No code to persist PDG from legraphe

- [ ] **Task 6.2: Implement PDG loading** ❌ NOT STARTED
  - [ ] `load_pdg()` - Reconstruct `ProgramDependenceGraph` from storage
  - [ ] Load nodes and rebuild PDG structure
  - [ ] Load edges and reconnect nodes
  - [ ] Rebuild symbol_index and file_index
  - **Status:** CRITICAL - No code to load PDG from storage

- [ ] **Task 6.3: Graph reconstruction queries** ❌ NOT STARTED
  - [ ] Query to get all nodes for a project
  - [ ] Query to get all edges for nodes
  - [ ] Efficient JOIN queries for graph reconstruction
  - **Status:** CRITICAL - Needed for PDG loading

---

## Phase 7: Cross-Project Resolution ❌ NOT IMPLEMENTED

### Objective
Implement global symbol resolution across projects.

- [ ] **Task 7.1: Create global symbol table** ❌ NOT STARTED
  - [ ] Design global_symbols table
  - [ ] Add symbol uniqueness constraints
  - [ ] Add cross-project references
  - [ ] Write migration script

- [ ] **Task 7.2: Implement cross-project resolution** ❌ NOT STARTED
  - [ ] Resolve symbols across project boundaries
  - [ ] Handle symbol name conflicts
  - [ ] Track external dependencies
  - [ ] Write tests for resolution

---

## Phase 8: DuckDB Integration ❌ NOT IMPLEMENTED

### Objective
Integrate DuckDB for advanced analytics.

- [ ] **Task 8.1: Add DuckDB dependency** ❌ NOT STARTED
  - [ ] Add duckdb crate to dependencies
  - [ ] Set up DuckDB connection
  - [ ] Configure SQLite-DuckDB hybrid

- [ ] **Task 8.2: Implement analytics queries** ❌ NOT STARTED
  - [ ] Port analytics queries to DuckDB
  - [ ] Leverage DuckDB columnar performance
  - [ ] Test performance improvement

---

## Success Criteria

The track is complete when:

1. **✅ SQLite schema working** - All tables created and indexed (ACHIEVED)
2. **✅ Node/edge persistence working** - CRUD operations functional (ACHIEVED)
3. **✅ Salsa incrementalism working** - BLAKE3 hashing and cache (ACHIEVED)
4. **❌ PDG persistence working** - Save/load PDG **NOT IMPLEMENTED**
5. **❌ Cross-project resolution working** - Global symbol table **NOT IMPLEMENTED**
6. **❌ DuckDB analytics working** - Columnar analytics **NOT IMPLEMENTED**

---

## Files Implemented

| File | Lines | Purpose | Status |
|------|-------|---------|--------|
| `src/lib.rs` | 24 | Module declarations, exports | ✅ COMPLETE |
| `src/schema.rs` | 171 | SQLite schema, storage config | ✅ COMPLETE |
| `src/nodes.rs` | 244 | Node CRUD operations | ✅ COMPLETE |
| `src/edges.rs` | 234 | Edge CRUD operations | ✅ COMPLETE |
| `src/salsa.rs` | 188 | BLAKE3 hashing, incremental cache | ✅ COMPLETE |
| `src/analytics.rs` | 155 | Analytics queries | ✅ COMPLETE |

**Total:** ~1,016 lines of production Rust code

---

## What Works vs What's Missing

```
┌─────────────────────────────────────────────────────────────────────┐
│                        lestockage STATUS                            │
├─────────────────────────────────────────────────────────────────────┤
│                                                                       │
│  ✅ COMPLETE (Working):                                              │
│  ├── SQLite schema with intel_nodes, intel_edges, analysis_cache   │
│  ├── NodeStore with full CRUD (insert, get, batch, find_by_hash)   │
│  ├── EdgeStore with full CRUD (insert, get_by_caller/callee/type)  │
│  ├── BLAKE3 hashing with NodeHash wrapper                           │
│  ├── IncrementalCache with is_cached, get, put, invalidate          │
│  ├── QueryInvalidation with get_affected_nodes                     │
│  └── Analytics with counts, distribution, hotspots                 │
│                                                                       │
│  ❌ MISSING (Critical Integration):                                  │
│  ├── No save_pdg() - Can't persist ProgramDependenceGraph          │
│  ├── No load_pdg() - Can't reconstruct PDG from storage             │
│  ├── No graph reconstruction queries                                  │
│  ├── No cross-project symbol resolution                              │
│  └── No DuckDB integration (analytics use SQLite)                   │
│                                                                       │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Implementation Plan for Remaining Work

### Task 6.1-6.3: PDG Persistence Bridge

**Objective:** Save and load PDG from storage

**Implementation Strategy:**

1. **Create `src/pdg_store.rs` module**
   ```rust
   use legraphe::pdg::ProgramDependenceGraph;

   pub fn save_pdg(storage: &mut Storage, pdg: &ProgramDependenceGraph) -> Result<(), Error> {
       // Extract all nodes and save to intel_nodes
       // Extract all edges and save to intel_edges
   }

   pub fn load_pdg(storage: &Storage, project_id: &str) -> Result<ProgramDependenceGraph, Error> {
       // Load all nodes for project
       // Load all edges for nodes
       // Reconstruct PDG with symbol_index and file_index
   }
   ```

2. **Integration with legraphe**
   - Add dependency on legraphe crate
   - Implement conversion between `Node`/`Edge` and storage records
   - Handle embedding BLOB serialization

3. **Graph Reconstruction**
   - Use JOIN queries to efficiently load nodes with edges
   - Rebuild StableGraph structure
   - Rebuild indexes for symbol and file lookup

4. **Integration Tests**
   - Test PDG round-trip (save → load → verify)
   - Test with multi-file projects
   - Test with complex graph structures

---

## Next Steps

**IMMEDIATE PRIORITY:** Implement PDG persistence bridge (Task 6.1-6.3)
- Currently no way to save/load PDG from storage
- This is the bridge between legraphe and lestockage
- Critical for persistent code intelligence

**SECONDARY PRIORITY:** Cross-project resolution (Task 7.1-7.2)
- Enables global symbol table
- Allows cross-project analysis

**TERTIARY PRIORITY:** DuckDB integration (Task 8.1-8.2)
- Performance improvement for analytics
- Can be deferred, SQLite analytics work

---

## Status: STORAGE PRIMITIVES COMPLETE, MISSING INTEGRATION ⚠️

All CRUD operations, BLAKE3 hashing, incremental caching, and analytics are fully implemented. The critical missing piece is the PDG persistence bridge that saves/loads `ProgramDependenceGraph` from storage.
