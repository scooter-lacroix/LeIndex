# Hyper-Analytical Implementation Plan: LeIndex + LLM-TLDR Fusion (Rust Rewrite)

This document outlines the architectural specification for rewriting `llm-tldr` in Rust and integrating it as the "Deep Intelligence" layer of `LeIndex`.

---

## 1. Core Objectives
1.  **Native Performance**: Replace Python AST/Tree-Sitter overhead with zero-copy Rust implementation.
2.  **Context Density**: Improve token efficiency by 20% over current Python implementation via "Gravity-based Traversal."
3.  **Search-Analysis Fusion**: Enable "Semantic Entry Points" where vector search results automatically trigger deep call-graph summarization.
4.  **Cross-Project Intelligence**: Leverage LeIndex v2.0 Global Index to resolve function calls across repository boundaries.

---

## 2. Phase 1: The Rust Rewrite (`tldr-rs`)

### 2.1 zero-Copy AST Extraction
Instead of the current Python approach in `tldr/ast_extractor.py` and `tldr/hybrid_extractor.py` which creates heavy Python objects for every node, `tldr-rs` will use direct memory mapping.

**Rust Snippet (Trait-based Extractor):**
```rust
use tree_sitter::{Parser, Language};

pub trait CodeIntelligence {
    fn get_signatures(&self, source: &[u8]) -> Vec<SignatureInfo>;
    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Graph<Block, Edge>;
}

pub struct RustExtractor;
impl CodeIntelligence for RustExtractor {
    fn get_signatures(&self, source: &[u8]) -> Vec<SignatureInfo> {
        let mut parser = Parser::new();
        parser.set_language(tree_sitter_rust::language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        
        // Use tree-sitter Queries for high-performance extraction
        let query = tree_sitter::Query::new(
            tree_sitter_rust::language(),
            "(function_item name: (identifier) @name) @func"
        ).unwrap();
        
        // Extract without creating intermediate Strings where possible
        // Referencing slices of &[u8]
    }
}
```

### 2.2 PDG Engine (Petgraph Integration)
Replace `tldr/pdg_extractor.py` with a highly optimized graph structure using the `petgraph` crate.

**Architectural Change**:
- Current: Python dictionary-based adjacency lists.
- New: `petgraph::StableGraph` using `u32` indices for nodes.
- Memory Impact: Reduction from ~400 bytes per node (Python) to ~32 bytes (Rust).

---

## 3. Phase 2: Accuracy & Efficiency Enhancements

### 3.1 Gravity-based Traversal Algorithm
Instead of BFS (current `tldr/api.py:get_relevant_context`), we implement a priority-weighted expansion.

**The Formula**:
`Relevance(N) = (SemanticScore(N) * Complexity(N)) / (Distance(Entry, N)^2)`

**Example Implementation (Rust):**
```rust
fn expand_context(entry_node: NodeId, budget: usize) -> Vec<NodeId> {
    let mut pq = BinaryHeap::new();
    pq.push(WeightedNode { id: entry_node, weight: 1.0 });
    
    let mut context = Vec::new();
    let mut current_tokens = 0;
    
    while let Some(node) = pq.pop() {
        if current_tokens + node.estimate_tokens() > budget { break; }
        
        context.push(node.id);
        current_tokens += node.estimate_tokens();
        
        // Push neighbors with decayed gravity
        for neighbor in graph.neighbors(node.id) {
            let weight = calculate_gravity(neighbor, node.weight);
            pq.push(WeightedNode { id: neighbor, weight });
        }
    }
    context
}
```

### 3.2 Incremental "Salsa" Persistence
Integrating with LeIndex's `FileChangeTracker` (`src/leindex/file_change_tracker.py`).

- **Logic**: Use the `hashlib` logic in LeIndex to check if a file changed.
- **Rust Implementation**: If `hash(file_content)` matches the database entry, `tldr-rs` skips re-parsing and reloads the PDG from the SQLite `code_intelligence` table.

---

## 4. Phase 3: LeIndex-First Integration

### 4.1 Schema Migration (SQLite)
Extend `src/leindex/storage/sqlite_storage.py` to support deep analysis metadata.

```sql
-- New table for Function-level Intelligence
CREATE TABLE intel_nodes (
    id INTEGER PRIMARY KEY,
    project_id TEXT,
    file_path TEXT,
    symbol_name TEXT,
    node_type TEXT, -- 'function', 'class', 'method'
    signature TEXT,
    complexity INTEGER,
    content_hash TEXT,
    embedding BLOB, -- Node-level embedding for semantic entry points
    FOREIGN KEY(project_id) REFERENCES projects(id)
);

CREATE TABLE intel_edges (
    caller_id INTEGER,
    callee_id INTEGER,
    edge_type TEXT, -- 'call', 'inheritance', 'data_dependency'
    PRIMARY KEY(caller_id, callee_id, edge_type)
);
```

### 4.2 Semantic Entry Points
Modify `src/leindex/core_engine/engine.py` to support `ask` or `analyze` queries that start with semantic search.

**Pipeline Flow**:
1.  **Query**: "Show me how the embedding batching works."
2.  **Search**: `LEANNVectorBackend` (LeIndex) finds node-level embedding for `generate_embeddings_batch` in `leann_backend.py`.
3.  **Entry**: This node is fed as the `entry_point` to `tldr-rs`.
4.  **Expand**: `tldr-rs` builds the context around `generate_embeddings_batch` (e.g., calling `_encode_batch`, `_encode_batch_gpu`, etc.).

### 4.3 Unified MCP Tool: `leindex_deep_tldr`
Merge `tldr/mcp_server.py` logic into `src/leindex/server.py`.

```python
async def leindex_deep_tldr(ctx: Context, query: str, budget: int = 2000):
    # 1. Semantic search for entry point
    search_results = await core_engine.search(query, top_k=3)
    
    # 2. Trigger Rust analyzer
    context = rust_analyzer.build_weighted_context(
        entry_nodes=[r.node_id for r in search_results],
        token_limit=budget
    )
    
    # 3. Return LLM-ready summary
    return context.to_llm_string()
```

---

## 5. Phase 4: Resource Management

### 5.1 Memory-Aware Spilling
Integrate with `src/leindex/memory/tracker.py`.

- **Action**: When `MemoryTracker` reports high RSS usage (e.g., > 90%), `tldr-rs` will:
    1.  Clear the `petgraph` cache for non-active projects.
    2.  Spill the `analysis_cache` (JSON/MsgPack) to the DuckDB analytics layer.
    3.  Force a `gc.collect()` on the Python side.

### 5.2 Shared Memory Buffers
Use `mmap` to pass large source files between LeIndex (Python) and the Analysis Engine (Rust) to avoid `O(N)` string copies across the FFI boundary.

---

## 6. Implementation Checklist
- [ ] Implement `tldr-rs` core in Rust with `PyO3` bindings.
- [ ] Migrate `tree-sitter` grammars to Rust crate dependencies.
- [ ] Implement `Gravity-based Traversal` in `petgraph`.
- [ ] Update LeIndex SQLite DAL to include `intel_nodes` and `intel_edges`.
- [ ] Modify `LEANNVectorBackend` to support node-level indexing.
- [ ] Expose `leindex_deep_tldr` as an MCP tool in `server.py`.
- [ ] Benchmark against 100K+ files for memory/speed regression.

---

## 7. Comparative Analysis: Current LeIndex Core vs. Proposed Plan

This section provides an exhaustive comparison between the existing LeIndex core (as found in `src/leindex/` and `maestro/leindex/rust/`) and the proposed integration plan.

### 7.1. Extraction Engine (AST/CFG/DFG)
| Feature | Current LeIndex (Python) | Current Maestro Rust (Migration) | Proposed Plan (Rust TLDR) |
| :--- | :--- | :--- | :--- |
| **Parsing Logic** | Python `ast` module (`ast_extractor.py`) | N/A (Handles SQL migrations only) | Native Rust `tree-sitter` (via `tldr-rs`) |
| **Performance** | O(N) object creation per AST node | N/A | Zero-copy byte-slice references into buffers |
| **Language Support** | Primarily Python; some TS/Go via regex | N/A | Trait-based polymorphic extractors for 17+ languages |
| **Concurrency** | Limited by Python GIL | N/A | Thread-pool based parallel scanning via `rayon` |

**Difference Analysis**: 
The existing `maestro/leindex/rust/` is a **migration framework** for moving SQLite/DuckDB/Tantivy to Turso/libsql. It does **not** contain code analysis logic. The proposed plan fills this vacuum by introducing a high-performance analysis engine that actually implements the 5-layer TLDR stack in Rust, whereas the current "Rust core" only manages database state.

### 7.2. Graph Representation (Call Graph / PDG)
| Feature | Current LeIndex (Python) | Proposed Plan (Rust TLDR) |
| :--- | :--- | :--- |
| **Storage Structure** | Python dictionaries/adjacency lists | `petgraph::StableGraph` using `u32` indices |
| **Memory Overhead** | ~400 bytes/node | ~32 bytes/node |
| **Traversal Algorithm** | Standard BFS/DFS | Gravity-based Weighted priority expansion |
| **Impact Analysis** | Basic cross-file lookups | Bitmask-based Reachability (O(V+E)) |

**Difference Analysis**:
LeIndex currently lacks a persistent Program Dependence Graph (PDG). It reconstructs call graphs on-the-fly or stores them in temporary caches. The proposed plan introduces a unified, persistent PDG stored in SQLite/DuckDB, allowing for instant impact analysis across projects without re-parsing.

### 7.3. Storage Layer & Schema
| Component | Current LeIndex | Proposed Plan |
| :--- | :--- | :--- |
| **Database** | SQLite + DuckDB (v2.0) | Extended SQLite + DuckDB + Salsa KV |
| **FTS** | Tantivy (via Python wrapper) | Native Rust Tantivy integration |
| **Schema Metadata** | `projects`, `files`, `versions`, `diffs` | Adds `intel_nodes`, `intel_edges`, `analysis_cache` |
| **Caching** | `file_content_cache.json.tmp` | Embedded `sled` or `rocksdb` for incremental Salsa queries |

**Difference Analysis**:
The current storage handles file metadata and search indices. The proposed plan upgrades the schema to store **semantic symbols** (nodes) and **logical relationships** (edges) as first-class citizens. This allows LeIndex to search for *functions* directly rather than just lines of text.

### 7.4. Search & Analysis Fusion
| Feature | Current LeIndex | Proposed Plan |
| :--- | :--- | :--- |
| **Search Result** | File path + Context lines | Function Node + Gravity-expanded graph context |
| **Entry Points** | File-based | Semantic Node-based (Vector-AST synergy) |
| **MCP Strategy** | `search_content`, `manage_project` | `leindex_deep_analyze` (Combined Search + Summary) |

**Difference Analysis**:
The proposed plan bridges the gap between "Finding" and "Understanding." Current LeIndex tells you where a keyword is; the proposed plan uses `tldr-rs` to explain the *logic* surrounding that keyword by following its dependencies immediately after retrieval.

### 7.5. Incremental Computation
| Component | Current LeIndex | Proposed Plan |
| :--- | :--- | :--- |
| **Change Detection** | `FileChangeTracker` (Python/hashlib) | Salsa-based Incremental Graph (Rust) |
| **Update Strategy** | Re-index changed files | Re-trace affected graph paths only |

**Difference Analysis**:
LeIndex's current incremental indexing is file-based. If a file changes, the whole file is re-indexed. The proposed plan’s Salsa integration allows for **symbol-based incrementalism**: if `func_a` changes but `func_b` in the same file doesn't, only `func_a`'s node in the PDG is invalidated.

---

## 7. Comparative Analysis: Existing Maestro Rust Core vs. Proposed Plan

This section provides a fine-grained, line-level comparison between the current Maestro Rust core (implemented in `maestro/leindex/rust/src/`) and the proposed integration plan.

### 7.1. AST Extraction & Language Support
| Feature | Current Rust Core (`multi_lang_ast.rs`) | Proposed Plan Enhancement |
| :--- | :--- | :--- |
| **Parsing Engine** | Native `tree-sitter` with per-language `LanguageConfig`. | Addition of **Lazy-Loaded Grammars** to further reduce initial memory footprint. |
| **Node Association** | Uses `is_inside_class` logic to link methods to classes. | **Unified Symbol ID System**: Every AST node gets a unique, project-wide UUID for persistent tracking. |
| **Docstring Handling** | Truncates to fixed characters (e.g., 100 chars). | **Semantic Summarization**: Uses a small local LLM or heuristic summarizer to extract "Intent" rather than raw text. |

### 7.2. Traversal & Relevance Logic (The "Gravity" Gap)
| Component | Current Implementation (`five_phase.rs`) | Proposed "Gravity" Implementation |
| :--- | :--- | :--- |
| **Selection Logic** | Static `file_priority` (e.g., `main.rs` = 100). | **Dynamic Gravity Algorithm**: Relevance decays as a function of call-graph distance from the search-hit node. |
| **Context Expansion** | Collects top-N files and truncates based on char count. | **Greedy Graph Expansion**: Starts at entry node and adds neighbors to budget using a Priority Queue (PQ) of `(Relevance/TokenCost)`. |
| **Hotspot Detection** | CC >= 10 in `phase5_optimization_report`. | **Flow-Aware Hotspots**: Identifies high-complexity nodes that are also "Central" in the call graph (High Eigenvector Centrality). |

### 7.3. Graph Persistence & Cross-Project Resolution
| Component | Current State | Proposed Plan |
| :--- | :--- | :--- |
| **Graph Lifecycle** | Ephemeral; built per-analysis session. | **Persistent PDG**: Stored in SQLite `intel_edges` table for sub-millisecond retrieval. |
| **Symbol Resolution** | Limited to current file or project-local imports. | **Global Symbol Resolution**: Uses LeIndex v2.0 Tier 1 metadata to follow calls into external indexed projects. |
| **Logical Edges** | Basic call relationships. | **Unified Dependency Graph**: Merges Call Graph, DFG (data flow), and Inheritance into a single queryable structure. |

### 7.4. Search-Analysis Fusion (Vector-AST Synergy)
| Feature | Current LeIndex | Proposed "Deep Intelligence" |
| :--- | :--- | :--- |
| **Vector Indexing** | Chunks files into segments. | **Node-Level Embeddings**: Every `FunctionElement` and `ClassElement` is embedded individually. |
| **Search Result** | "File X contains 'auth' at line Y". | "Function 'auth_handler' in File X is the primary match; here is its logic flow...". |
| **Entry Point** | User-provided path or file scan. | **Semantic Entry Point**: Vector search returns the `symbol_id`, which directly anchors the Rust `PDG` expansion. |

### 7.5. Incrementalism (Salsa Implementation)
| Component | Current Logic | Proposed Salsa Logic |
| :--- | :--- | :--- |
| **Change Detection** | File-based hashing in `FileChangeTracker.py`. | **Node-based Hashing**: Rust core computes BLAKE3 hashes for individual AST sub-trees. |
| **Re-computation** | Re-analyzes the entire file if the hash changes. | **Query-based Invalidation**: Only invalidates the specific `SalsaQuery` for the affected node and its direct logical dependents. |

### 7.6. Detailed Schema Differences (SQLite)
The current SQLite schema (`sqlite_storage.py`) focuses on file-level metadata. The proposed plan introduces a **Logical Layer**:

```sql
-- CURRENT SCHEMA (Incomplete for logic)
-- files (path, type, size, hash)
-- versions (id, file_path, content)

-- PROPOSED ADDITIONS
-- intel_nodes: Stores AST elements (functions/classes) linked to vector embeddings.
-- intel_edges: Stores logical flow (calls/depends_on) between node IDs.
-- analysis_cache: Stores the result of expensive CFG/DFG computations indexed by node_hash.
```

---

**End of Plan.**
