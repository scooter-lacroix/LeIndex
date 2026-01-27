# LeIndex Rust Architecture

Detailed architecture documentation for LeIndex v0.1.0 pure Rust implementation.

---

## Overview

LeIndex is organized as a Cargo workspace with 5 crates, each responsible for a specific aspect of the system.

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────┐
│           LeIndex Rust Architecture                      │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌─────────┐ │
│  │   MCP    │  │   CLI    │  │  lepass │  │ lestock │ │
│  │  Server  │  │   Tool   │  │  erille │  │   age   │ │
│  │  (axum)  │  │  (clap)  │  │         │  │(SQLite) │ │
│  └────┬─────┘  └────┬─────┘  └────┬────┘  └────┬────┘ │
│       │             │            │            │      │
│  ┌────▼─────────────▼────────────▼────────────▼───┐  │
│  │              lepasserelle crate                 │  │
│  │         (orchestration & config)               │  │
│  └────┬──────────────┬──────────────┬────────────┘  │
│       │              │              │               │
│  ┌────▼───┐   ┌─────▼─────┐   ┌────▼────┐  ┌─────▼──┐│
│  │leparse │   │ legraphe  │   │ lerech  │  │ Turso  ││
│  │Parsing │   │    PDG    │   │  HNSW   │  │Vectors ││
│  │(tree-  │   │  (petgraph│   │(hnsw_rs)│  │(libsql)││
│  │ sitter) │   │   embed)  │   │ IN-MEM  │  │Future ││
│  └────────┘   └───────────┘   └─────────┘  └─────────┘│
│                                                         │
│  Vector Search: HNSW (in-memory, temporary)            │
│  Unified Storage: Turso/libsql (vectors + metadata)    │
│  Current State: Turso configured but not implemented   │
└─────────────────────────────────────────────────────────┘
```

---

## Crate Details

### 1. leparse - AST Extraction

**Purpose:** Zero-copy AST extraction using Tree-sitter

**Key Responsibilities:**
- Language detection
- Tree-sitter parsing initialization
- AST node extraction and traversal
- Syntax highlighting queries

**Supported Languages:**
- Python, Rust, JavaScript/TypeScript, Go, C/C++, Java, Ruby, PHP
- Swift, Kotlin, Dart (temporarily disabled due to tree-sitter conflicts)

**Key Types:**
```rust
pub struct Parser {
    language: Language,
    parser: tree_sitter::Parser,
}

pub struct AstNode {
    pub kind: String,
    pub text: String,
    pub range: Range,
    pub children: Vec<AstNode>,
}
```

**Dependencies:**
- `tree-sitter` - Parser runtime
- `tree-sitter-python`, `tree-sitter-rust`, etc. - Language grammars
- `rayon` - Parallel parsing

---

### 2. legraphe - Program Dependence Graph

**Purpose:** PDG construction and analysis using petgraph

**Key Responsibilities:**
- Control Flow Graph (CFG) construction
- Data Flow Analysis (DFA)
- Dependency tracking
- Symbol resolution

**Key Types:**
```rust
pub struct Pdg {
    pub graph: DiGraph<Node, Edge>,
    pub entry_points: Vec<NodeId>,
}

pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub location: Location,
}

pub enum NodeKind {
    Function,
    Variable,
    Parameter,
    Call,
}
```

**Dependencies:**
- `petgraph` - Graph data structures
- `rayon` - Parallel graph traversal
- `leparse` - AST input

---

### 3. lerecherche - Vector Search

**Purpose:** HNSW-based semantic similarity search

**Key Responsibilities:**
- Vector embedding storage (in-memory)
- HNSW index construction
- Approximate nearest neighbor search
- Similarity scoring

**Current State:**
- In-memory HNSW via `hnsw_rs`
- Temporary implementation
- Will be replaced by Turso/libsql with vec0 extension

**Key Types:**
```rust
pub struct VectorIndex {
    pub hnsw: Hnsw<f32, DistCosine>,
    pub dimension: usize,
}

pub struct SearchResult {
    pub file_path: String,
    pub line_number: usize,
    pub score: f32,
}
```

**Dependencies:**
- `hnsw_rs` - HNSW algorithm
- `rayon` - Parallel search

**Future (Turso/libsql):**
```toml
# Planned dependencies
libsql = "0.5"
vec0-extension = "0.1"
```

---

### 4. lestockage - Storage Layer

**Purpose:** Unified storage for metadata and vectors (planned)

**Current State:**
- Configuration stub only
- Will implement Turso/libsql integration

**Planned Features:**
- SQLite/Libsql metadata storage
- vec0 extension for vector storage
- F32_BLOB columns for vectors
- Remote Turso database support

**Key Types (Planned):**
```rust
pub struct Storage {
    pub conn: Connection,
    pub remote: Option<TursoClient>,
}

pub struct Document {
    pub id: String,
    pub file_path: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub metadata: serde_json::Value,
}
```

**Dependencies (Planned):**
- `rusqlite` - SQLite bindings
- `libsql` - Libsql client
- `vec0` - Vector extension

---

### 5. lepasserelle - CLI & MCP Server

**Purpose:** Orchestration, CLI, and MCP server

**Key Responsibilities:**
- Command-line interface (clap)
- MCP server implementation (axum)
- Configuration management
- Orchestration of all crates

**CLI Commands:**
```bash
leindex index <path>       # Index a project
leindex search <query>     # Search indexed code
leindex diagnostics        # System health check
leindex mcp                # Start MCP server
```

**MCP Server:**
- Transport: stdio (default), HTTP (optional)
- Protocol: MCP 1.0
- Tools: leindex_index, lesearch_search, leindex_deep_analyze, leindex_context, leindex_diagnostics

**Key Types:**
```rust
pub struct Config {
    pub memory: MemoryConfig,
    pub file_filtering: FileFilteringConfig,
    pub parsing: ParsingConfig,
}

pub struct MemoryManager {
    pub budget_mb: usize,
    pub current_mb: usize,
}
```

**Dependencies:**
- `clap` - CLI argument parsing
- `axum` - HTTP server for MCP
- `serde` - Configuration serialization
- `toml` - Config file format
- All other crates (leparse, legraphe, lerecherche, lestockage)

---

## Data Flow

### Indexing Pipeline

```
┌──────────────┐
│   Files      │
└──────┬───────┘
       │
       ▼
┌──────────────┐     ┌──────────────┐
│   leparse    │────▶│    AST       │
│  (Parser)    │     │  (Nodes)     │
└──────┬───────┘     └──────┬───────┘
       │                     │
       ▼                     ▼
┌──────────────┐     ┌──────────────┐
│  legraphe    │◀────│  Symbols     │
│    (PDG)     │     │ (Extracted)  │
└──────┬───────┘     └──────────────┘
       │
       ▼
┌──────────────┐     ┌──────────────┐
│ lerecherche  │◀────│ Embeddings   │
│   (HNSW)     │     │ (Generated)  │
└──────┬───────┘     └──────────────┘
       │
       ▼
┌──────────────┐
│   Indexed    │
│   Memory     │
└──────────────┘
```

### Search Pipeline

```
┌──────────────┐
│   Query      │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│  Embedding   │
│  (Generate)  │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ lerecherche  │
│   (HNSW)     │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│  Results     │
│  (Ranked)    │
└──────────────┘
```

---

## Memory Management

### Hierarchical Configuration

```
Global Config (memory)
├── total_budget_mb: 3072
├── soft_limit_percent: 0.80
├── hard_limit_percent: 0.93
└── emergency_percent: 0.98

Project Config (projects.*.memory)
├── max_loaded_files: 1000
└── max_cached_queries: 500
```

### Memory Actions

| Threshold | Action |
|-----------|--------|
| 80% (soft) | Trigger garbage collection |
| 93% (hard) | Spill cached data to disk |
| 98% (emergency) | Evict low-priority data |

### Cache Spilling

```
In-Memory Cache
├── L1: Hot data (frequently accessed)
├── L2: Warm data (recently accessed)
└── Spill: Cold data (moved to disk)
```

---

## Configuration

### File Locations

```
~/.leindex/
├── config/
│   └── global.toml           # Global configuration
├── projects/
│   ├── project-a.toml        # Project-specific overrides
│   └── project-b.toml
├── data/
│   ├── index/                # Indexed data
│   └── cache/                # Spilled cache
├── logs/
│   └── leindex.log
└── bin/
    └── leindex               # Binary
```

### Configuration Precedence

1. Command-line arguments
2. Project-specific TOML
3. Global TOML
4. Default values

---

## Concurrency Model

### Parallel Processing

```rust
// File scanning (rayon)
files.par_iter()
    .map(|file| parse_file(file))
    .collect()

// Graph traversal (rayon)
graph.par_nodes()
    .map(|node| analyze_node(node))
    .collect()
```

### Async Operations (Future)

```rust
// Planned async operations
async fn index_project(path: PathBuf) -> Result<()> {
    let files = discover_files(path).await?;
    parse_files_parallel(files).await?;
    build_index().await?;
    Ok(())
}
```

---

## Technology Rationale

| Technology | Reason |
|------------|--------|
| **Rust** | Performance, memory safety, zero-cost abstractions |
| **Tree-sitter** | Incremental parsing, zero-copy, excellent language support |
| **petgraph** | Flexible graph algorithms, Rust-native |
| **hnsw_rs** | Fast approximate nearest neighbors, pure Rust |
| **axum** | Modern async web framework, excellent ergonomics |
| **clap** | Best-in-class CLI argument parsing |
| **tokio** | Industry-standard async runtime |
| **Turso/libsql** | Edge SQLite, vector support (planned) |

---

## Implementation Status

| Component | Status | Notes |
|-----------|--------|-------|
| **leparse** | ✅ Complete | 11+ languages, Swift/Kotlin/Dart disabled |
| **legraphe** | ✅ Complete | CFG, DFA, dependency tracking |
| **lerecherche** | ⚠️ Temporary | HNSW in-memory, Turso planned |
| **lestockage** | ❌ Stub | Configuration only, implementation planned |
| **lepasserelle** | ✅ Complete | CLI, MCP server, orchestration |

---

## Future Enhancements

### v0.2.0 - Turso/libsql Integration

- [ ] Implement lestockage with libsql
- [ ] Add vec0 extension support
- [ ] F32_BLOB columns for vectors
- [ ] Remote Turso database support
- [ ] Persistent metadata storage

### v0.3.0 - Advanced Features

- [ ] Re-enable Swift/Kotlin/Dart parsers
- [ ] Cross-project search
- [ ] Global index dashboard
- [ ] Advanced memory management
- [ ] Full-text search (Tantivy)

---

## Performance Considerations

### Zero-Copy Parsing

Tree-sitter enables zero-copy AST traversal:
- No string allocation for node text
- Direct references into source buffer
- Minimal memory overhead

### Parallel Processing

Rayon enables work-stealing parallelism:
- Automatic thread pool management
- Load balancing across cores
- No manual thread spawning

### Cache-Friendly Design

- Spatial locality in data structures
- Sequential access patterns
- Minimal pointer chasing

---

## Contributing

See architecture-specific contribution guidelines:

1. **leparse**: Add language parsers
2. **legraphe**: Improve PDG algorithms
3. **lerecherche**: Optimize HNSW parameters
4. **lestockage**: Implement Turso/libsql
5. **lepasserelle**: Enhance CLI/MCP features

---

*Last Updated: 2025-01-26*
*LeIndex v0.1.0*
