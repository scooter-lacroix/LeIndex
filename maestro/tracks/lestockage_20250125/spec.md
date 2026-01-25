# Specification: lestockage - Persistent Storage Layer

**Track ID:** `lestockage_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

### Vision

`lestockage` (French for "The Storage") is the Persistent Storage Layer of the LeIndex Rust Renaissance. It extends the SQLite schema for code intelligence data and implements Salsa-based incremental computation for fast re-indexing.

### The "Why"

**Current State:**
- File-level invalidation (change one line → reindex entire file)
- No cross-project intelligence
- Limited analytics capabilities
- No incremental computation

**Target State:**
- Node-level incrementalism (change one function → reindex only that function)
- Extended SQLite schema with intel_nodes and intel_edges
- Cross-project symbol resolution
- DuckDB analytics for graph metrics

### Key Principles

1. **Node-Level Incrementalism** - Salsa-based hashing at symbol level
2. **Extended Schema** - intel_nodes, intel_edges, analysis_cache tables
3. **Cross-Project Intelligence** - Global symbol table
4. **Analytics Ready** - DuckDB integration for graph metrics

---

## Functional Requirements

### FR-1 Extended SQLite Schema

- `intel_nodes` table: stores all code intelligence nodes
- `intel_edges` table: stores PDG relationships
- `analysis_cache` table: stores computed analysis results
- Foreign key constraints for referential integrity
- Indexed columns for query performance

### FR-2 Node Persistence

- CRUD operations for `intel_nodes`
- Embedding BLOB storage/retrieval
- Batch insert optimization
- Efficient bulk operations

### FR-3 Edge Persistence

- CRUD operations for `intel_edges`
- Edge type filtering (call, inheritance, data_dependency)
- Graph reconstruction queries
- Bulk edge operations

### FR-4 Salsa Incremental Computation

- Node-level BLAKE3 hashing for AST sub-trees
- Query-based invalidation (only affected nodes re-computed)
- Symbol-level incrementalism (not file-level)
- Change detection at granularity of individual symbols

### FR-5 Cross-Project Intelligence

- Global symbol table using LeIndex Tier 1 metadata
- Resolve function calls across repository boundaries
- External project references with lazy loading
- Cross-project dependency tracking

### FR-6 DuckDB Analytics

- Graph metrics queries (centrality, complexity distribution)
- Hotspot detection analytics
- Codebase evolution tracking
- Analytics export functions

---

## Non-Functional Requirements

### Performance Targets

- **Incremental Rebuild:** Only changed symbols re-computed
- **Query Performance:** Fast graph queries via SQLite indexes
- **Cross-Project:** Efficient symbol resolution across projects

### Quality Requirements

- **Test Coverage:** >95% for all storage operations
- **Validation:** Schema validation tests
- **Code Quality:** Pass clippy with no warnings

---

## Acceptance Criteria

**AC-1 SQLite Schema**
- [ ] SQLite schema supports all required queries
- [ ] Schema validation tests passing
- [ ] Foreign key constraints working

**AC-2 Node/Edge Persistence**
- [ ] CRUD operations working for nodes and edges
- [ ] Batch operations optimized
- [ ] Graph reconstruction working

**AC-3 Salsa Incrementalism**
- [ ] Salsa incrementalism only re-computes changed nodes
- [ ] Node-level hashing working
- [ ] Query-based invalidation working

**AC-4 Cross-Project Resolution**
- [ ] Cross-project resolution works across multiple repos
- [ ] Global symbol table working
- [ ] External references loading correctly

**AC-5 DuckDB Analytics**
- [ ] DuckDB analytics return correct graph metrics
- [ ] Hotspot detection working
- [ ] Evolution tracking working

---

## Dependencies

### Internal Dependencies
- `legraphe_20250125` - Requires PDG structures for storage

### External Rust Crates
- `rusqlite` (SQLite bindings)
- `duckdb-rs` (DuckDB for analytics)
- `blake3` (hashing)
- `serde` (serialization)

---

## Out of Scope

- **No Distributed Storage** - Single-machine SQLite only
- **No Sharding** - No horizontal scaling
- **No Alternative Backends** - SQLite only (users can add others)
