# Implementation Plan: lestockage - Persistent Storage Layer

**Track ID:** `lestockage_20250125`
**Track Type:** Standard Track
**Status:** CORE COMPLETE (Source-Code-Verified: 2025-01-26)
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

This track implements the Persistent Storage Layer for LeIndex Rust Renaissance. It extends the SQLite schema and implements Salsa-based incremental computation.

**Source-Code-Verified Status:** ~50% COMPLETE ⚠️ CORE STORAGE COMPLETE, CROSS-PROJECT + HNSW/TURSO MISSING

**Test Results:** 17/17 tests passing ✅
**Code State:** CRUD operations, BLAKE3 hashing, incremental caching, analytics, and PDG persistence all working. **CRITICAL: Cross-project resolution (Phase 7) and HNSW/Turso vector store (Phase 8) are REQUIRED and NOT IMPLEMENTED.**

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

## Phase 6: PDG Persistence Bridge ✅ COMPLETE

### Objective
Implement PDG save/load from storage.

- [x] **Task 6.1: Implement PDG persistence** ✅ COMPLETE
  - [x] `save_pdg()` - Save `ProgramDependenceGraph` to storage
  - [x] Extract nodes from PDG and store in intel_nodes
  - [x] Extract edges from PDG and store in intel_edges
  - [x] Preserve all metadata including embeddings
  - **File:** `src/pdg_store.rs` (640 lines)
  - **Tests:** 9 comprehensive tests passing

- [x] **Task 6.2: Implement PDG loading** ✅ COMPLETE
  - [x] `load_pdg()` - Reconstruct `ProgramDependenceGraph` from storage
  - [x] Load nodes and rebuild PDG structure
  - [x] Load edges and reconnect nodes
  - [x] Rebuild symbol_index and file_index
  - **File:** `src/pdg_store.rs` lines 220-342

- [x] **Task 6.3: Graph reconstruction queries** ✅ COMPLETE
  - [x] Query to get all nodes for a project
  - [x] Query to get all edges for nodes using JOIN
  - [x] Efficient graph reconstruction with node_id mapping
  - **File:** `src/pdg_store.rs` lines 246-342

- [x] **Task 6.4: Helper functions** ✅ COMPLETE
  - [x] `pdg_exists()` - Check if PDG exists for project
  - [x] `delete_pdg()` - Delete PDG with cascade to edges
  - [x] Type conversion functions for Node/Edge types
  - **File:** `src/pdg_store.rs` lines 344-386

**Additional Changes:**
- **legraphe/src/pdg.rs** - Added public iteration methods:
  - `node_indices()` - Iterate over all node indices
  - `edge_indices()` - Iterate over all edge indices
  - `edge_endpoints()` - Get edge endpoints
- **lestockage/src/lib.rs** - Added exports for pdg_store module

---

## Phase 7: Cross-Project Resolution ❌ REQUIRED - CRITICAL MISSING PIECE

### Objective
Implement global symbol resolution across projects.

**CRITICAL:** This phase is **REQUIRED** for production use. Cross-project symbol resolution enables tracking function calls across repository boundaries and handling external dependencies properly.

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

## Phase 8: HNSW/Turso Vector Store Integration ❌ REQUIRED - PRODUCTION SCALE

### Objective
Integrate HNSW/Turso vector store for production-scale semantic search.

**CRITICAL:** This phase is **REQUIRED** for production-scale deployments. Current brute-force cosine similarity search is optimal for <100K embeddings, but production use requires HNSW/Turso for efficient vector search at scale.

- [ ] **Task 8.1: Add Turso dependency** ❌ NOT STARTED
  - [ ] Add libsql/turso crate to dependencies
  - [ ] Set up Turso connection with vector extension
  - [ ] Configure SQLite-Turso hybrid for existing storage

- [ ] **Task 8.2: Implement HNSW vector indexing** ❌ NOT STARTED
  - [ ] Create HNSW index for embeddings
  - [ ] Port vector search queries to Turso with HNSW
  - [ ] Implement efficient approximate nearest neighbor search
  - [ ] Test search performance improvement vs brute-force

- [ ] **Task 8.3: Vector migration bridge** ❌ NOT STARTED
  - [ ] Migrate existing embeddings from SQLite BLOB to Turso vectors
  - [ ] Update save_pdg() to use Turso for embedding storage
  - [ ] Update load_pdg() to retrieve from Turso vector store
  - [ ] Maintain backward compatibility during migration

---

## Success Criteria

The track is complete when:

1. **✅ SQLite schema working** - All tables created and indexed (ACHIEVED)
2. **✅ Node/edge persistence working** - CRUD operations functional (ACHIEVED)
3. **✅ Salsa incrementalism working** - BLAKE3 hashing and cache (ACHIEVED)
4. **✅ PDG persistence working** - Save/load PDG (ACHIEVED)
5. **❌ Cross-project resolution working** - Global symbol table **REQUIRED AND MISSING**
6. **❌ HNSW/Turso vector store working** - Production-scale vector search **REQUIRED AND MISSING**

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
| `src/pdg_store.rs` | 640 | PDG persistence bridge (NEW) | ✅ COMPLETE |

**Total:** ~1,656 lines of production Rust code

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
│  ├── Analytics with counts, distribution, hotspots                 │
│  └── PDG persistence bridge (save_pdg, load_pdg, pdg_exists)      │
│                                                                       │
│  ❌ REQUIRED (Missing Critical Pieces):                               │
│  ├── Cross-project symbol resolution                                  │
│  │   - Global symbol table for multi-project analysis              │
│  │   - Cross-project symbol resolution                              │
│  │   - External dependency tracking                                 │
│  │                                                               │
│  ├── HNSW/Turso vector store                                          │
│  │   - Production-scale vector search (>100K embeddings)          │
│  │   - Turso database with HNSW extension                          │
│  │   - Vector migration bridge from SQLite BLOBs                   │
│  │                                                               │
│  └── Advanced caching strategies (Optional)                         │
│                                                                       │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Implementation Plan for Remaining Work

**REQUIRED:** Cross-Project Resolution (Task 7.1-7.2) and HNSW/Turso Vector Store (Task 8.1-8.3)

Core storage functionality is complete. Remaining tasks are **required for production**:

### Task 7.1-7.2: Cross-Project Resolution
- Global symbol table for multi-project analysis
- Cross-project symbol resolution
- External dependency tracking

### Task 8.1-8.3: HNSW/Turso Vector Store
- Production-scale vector search for large embeddings
- Turso database with HNSW extension
- Vector migration bridge from SQLite BLOBs
- Efficient approximate nearest neighbor search

---

## Next Steps

**TRACK REQUIRES COMPLETION** ⚠️

Core storage functionality is fully implemented:
- SQLite schema with proper indexing
- Full CRUD operations for nodes and edges
- BLAKE3 hashing for incremental computation
- PDG persistence bridge (save/load/delete)
- Analytics queries for code insights

**CRITICAL MISSING PIECES (REQUIRED):**
- **Cross-project symbol resolution (Task 7.1-7.2)** - MUST BE IMPLEMENTED
  - Global symbol table for multi-project analysis
  - Cross-project symbol resolution
  - External dependency tracking

- **HNSW/Turso vector store (Task 8.1-8.3)** - MUST BE IMPLEMENTED
  - Production-scale vector search (>100K embeddings)
  - Turso database with HNSW extension
  - Vector migration bridge

---

## Status: CORE STORAGE COMPLETE, CROSS-PROJECT + HNSW/TURSO REQUIRED ⚠️

SQLite schema, CRUD operations, BLAKE3 hashing, incremental caching, analytics, and PDG persistence are all fully implemented (17/17 tests passing). **Cross-project resolution (Phase 7) and HNSW/Turso vector store (Phase 8) are REQUIRED for production use.**
