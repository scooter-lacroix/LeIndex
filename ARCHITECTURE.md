# LeIndex Architecture

Pure Rust implementation of an intelligent code search and analysis engine.

---

## Table of Contents

- [Overview](#overview)
- [Workspace Structure](#workspace-structure)
- [Crate Architecture](#crate-architecture)
- [Data Flow](#data-flow)
- [Storage Design](#storage-design)
- [MCP Server](#mcp-server)
- [Performance](#performance)

---

## Overview

LeIndex v0.1.0 is a **pure Rust** codebase indexing and analysis system with 5 specialized crates:

```
┌─────────────────────────────────────────────────────────┐
│           LeIndex Rust Architecture                     │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌─────────┐   │
│  │   MCP    │  │   CLI    │  │  lepass │  │ lestock │   │
│  │  Server  │  │   Tool   │  │  erille │  │   age   │   │
│  │  (axum)  │  │  (clap)  │  │         │  │(SQLite) │   │
│  └────┬─────┘  └────┬─────┘  └────┬────┘  └────┬────┘   │
│       │             │             │            │        │
│  ┌────▼─────────────▼─────────────▼────────────▼────┐   │
│  │              lepasserelle crate                  │   │
│  │         (orchestration & config)                 │   │
│  └────┬──────────────┬──────────────┬─────────────┬─┘   │
│       │              │              │             │     │
│  ┌────▼───┐    ┌─────▼─────┐   ┌────▼────┐  ┌─────▼───┐ │
│  │leparse │    │ legraphe  │   │ lerech  │  │ Turso   │ │
│  │Parsing │    │    PDG    │   │  HNSW   │  │Vectors  │ │
│  │(tree-  │    │  (petgraph│   │(hnsw_rs)│  │(libsql) │ │
│  │ sitter)│    │   embed)  │   │ IN-MEM  │  │Future   │ │
│  └────────┘    └───────────┘   └─────────┘  └─────────┘ │
│                                                         │
│  Vector Search: HNSW (in-memory, temporary)             │
│  Unified Storage: Turso/libsql (vectors + metadata)     │
│  Current State: Turso configured but not implemented    │
└─────────────────────────────────────────────────────────┘
```

---

## Workspace Structure

### Root Configuration

```toml
[workspace]
members = [
    "crates/leparse",
    "crates/legraphe",
    "crates/lerecherche",
    "crates/lestockage",
    "crates/lepasserelle",
]
```

### Directory Layout

```
leindex/
├── crates/
│   ├── leparse/        # AST extraction (Tree-sitter)
│   ├── legraphe/       # PDG analysis (petgraph)
│   ├── lerecherche/    # Vector search (HNSW)
│   ├── lestockage/     # Storage layer (Turso/libsql)
│   └── lepasserelle/   # CLI & MCP server
├── Cargo.toml          # Workspace configuration
├── install.sh          # Linux/Unix installer
├── install_macos.sh    # macOS installer
└── install.ps1         # Windows PowerShell installer
```

---

## Crate Architecture

### 1. leparse - AST Extraction

**Purpose:** Zero-copy AST extraction using Tree-sitter

**Key Responsibilities:**
- Language detection
- Tree-sitter parser initialization
- AST node extraction and traversal
- Syntax highlighting queries

**Supported Languages:**
- ✅ Python, Rust, JavaScript/TypeScript, Go, C/C++, Java, Ruby, PHP
- ⚠️ Swift, Kotlin, Dart (temporarily disabled)

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

**Dependencies:**
- `clap` - CLI argument parsing
- `axum` - HTTP server for MCP
- `serde` - Configuration serialization
- `toml` - Config file format
- All other crates

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

## Storage Design

### Current State (Temporary)

**In-Memory HNSW:**
- Vectors stored in `hnsw_rs` structure
- No persistence between runs
- Fast search but requires re-indexing

### Target Architecture (Planned)

**Turso/libsql Unified Storage:**

```sql
-- Files table
CREATE TABLE files (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    language TEXT,
    indexed_at INTEGER
);

-- Vectors table (using vec0 extension)
CREATE TABLE vectors (
    id TEXT PRIMARY KEY REFERENCES files(id),
    embedding F32_BLOB(768),  -- CodeRankEmbed dimension
    metadata JSON
);

-- Symbols table
CREATE TABLE symbols (
    id TEXT PRIMARY KEY,
    file_id TEXT REFERENCES files(id),
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    line_start INTEGER,
    line_end INTEGER
);
```

**Benefits:**
- Single unified storage for vectors AND metadata
- Remote Turso database support
- Persistent across restarts
- vec0 extension for efficient vector operations

---

## MCP Server

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│                   MCP Server                            │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌──────────────┐    ┌──────────────┐                 │
│  │   Transport  │    │   Protocol   │                 │
│  │  (stdio/HTTP)│    │    (MCP 1.0) │                 │
│  └──────┬───────┘    └──────┬───────┘                 │
│         │                   │                          │
│         └────────┬──────────┘                          │
│                  ▼                                     │
│         ┌──────────────────┐                          │
│         │  Tool Registry   │                          │
│         └────────┬─────────┘                          │
│                  │                                     │
│    ┌─────────────┼─────────────┐                      │
│    ▼             ▼             ▼                      │
│ ┌────────┐  ┌────────┐  ┌────────┐                    │
│ │ index  │  │ search │  │ analyze│                    │
│ │  tool  │  │  tool  │  │  tool  │                    │
│ └────┬───┘  └────┬───┘  └────┬───┘                    │
│      │          │           │                         │
│      └──────────┼───────────┘                         │
│                 ▼                                     │
│         ┌──────────────┐                              │
│         │ lepasserelle │                              │
│         │   (crates)   │                              │
│         └──────────────┘                              │
└─────────────────────────────────────────────────────────┘
```

### Available Tools

| Tool | Description | Crate |
|------|-------------|-------|
| `leindex_index` | Index projects | lepasserelle + leparse |
| `leindex_search` | Semantic search | lepasserelle + lerecherche |
| `leindex_deep_analyze` | Deep PDG analysis | lepasserelle + legraphe |
| `leindex_context` | Context expansion | lepasserelle + leparse |
| `leindex_diagnostics` | System health | lepasserelle |

---

## Performance

### Design Principles

1. **Zero-Copy Parsing** - Tree-sitter enables direct references into source buffer
2. **Parallel Processing** - Rayon provides work-stealing parallelism
3. **Cache Efficiency** - Spatial locality in data structures
4. **Memory Safety** - Rust guarantees memory safety without GC overhead

### Expected Performance

| Metric | Target | Status |
|--------|--------|--------|
| **Indexing Speed** | ~10K files/min | 🎯 Target |
| **Search Latency (p50)** | ~50ms | 🎯 Target |
| **Memory Usage** | <3GB | 🎯 Target |
| **Max Scalability** | 100K+ files | 🎯 Target |

### Optimizations

**Parallel Processing:**
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

**Zero-Copy Parsing:**
```rust
// Tree-sitter zero-copy AST
let node = parser.parse(source, None)?;
let root = node.root_node();

// Direct references, no allocation
let text = &source[node.byte_range()];
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

## Implementation Status

| Component | Status | Notes |
|-----------|--------|-------|
| **leparse** | ✅ Complete | 11+ languages |
| **legraphe** | ✅ Complete | CFG, DFA |
| **lerecherche** | ⚠️ Temporary | HNSW in-memory |
| **lestockage** | ❌ Stub | Planned |
| **lepasserelle** | ✅ Complete | CLI, MCP |

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

## Contributing

See architecture-specific contribution guidelines:

- [leparse](crates/leparse/README.md) - Add language parsers
- [legraphe](crates/legraphe/README.md) - Improve PDG algorithms
- [lerecherche](crates/lerecherche/README.md) - Optimize HNSW
- [lestockage](crates/lestockage/README.md) - Implement Turso/libsql
- [lepasserelle](crates/lepasserelle/README.md) - Enhance CLI/MCP

---

**Built with ❤️ and Rust**

*LeIndex v0.1.0*
*Last Updated: 2025-01-26*
