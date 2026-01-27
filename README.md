# LeIndex

<div align="center">

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![MCP Server](https://img.shields.io/badge/MCP-Server-blue?style=for-the-badge)](https://modelcontextprotocol.io)
[![License](https://img.shields.io/badge/License-MIT-yellow?style=for-the-badge)](LICENSE)
[![Version](https://img.shields.io/badge/Version-0.1.0-blue?style=for-the-badge)](CHANGELOG.md)

**Pure Rust Code Search and Analysis Engine**

*Lightning-fast semantic code search with zero-copy parsing, PDG analysis, and intelligent memory management.*

</div>

---

## What is LeIndex?

**LeIndex** is a **pure Rust** implementation of an intelligent code search and analysis engine. It combines fast parsing, semantic understanding, and efficient storage to help you navigate and understand large codebases.

### Key Features

- **Zero-Copy AST Extraction** - Tree-sitter based parsing with 11+ language support
- **Program Dependence Graph (PDG)** - Advanced code relationship analysis via petgraph
- **HNSW Vector Search** - In-memory semantic similarity search (temporary implementation)
- **MCP Server** - First-class Model Context Protocol support for AI assistants
- **Memory Efficient** - Smart cache management with automatic spilling
- **Project Configuration** - TOML-based per-project settings

---

## Architecture

LeIndex consists of 5 Rust crates:

| Crate | Purpose | Key Technologies |
|-------|---------|------------------|
| **leparse** | AST extraction | Tree-sitter (11+ languages) |
| **legraphe** | PDG analysis | petgraph, rayon |
| **lerecherche** | Vector search | hnsw_rs (in-memory HNSW) |
| **lestockage** | Storage layer | libsql/Turso (planned) |
| **lepasserelle** | CLI & MCP | axum, clap |

### Current Architecture State

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

### Language Support

- ✅ Python
- ✅ Rust
- ✅ JavaScript/TypeScript
- ✅ Go
- ✅ C/C++
- ✅ Java
- ✅ Ruby
- ✅ PHP
- ✅ Swift (temporarily disabled - tree-sitter version conflict)
- ✅ Kotlin (temporarily disabled - tree-sitter version conflict)
- ✅ Dart (temporarily disabled - tree-sitter version conflict)

---

## Quick Start

### Prerequisites

- **Rust 1.75+** - Install from [rustup.rs](https://rustup.rs/)
- **Cargo** - Comes with Rust

### Installation

#### One-Line Installer (Recommended)

**Linux/macOS:**
```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

Or with wget:
```bash
wget -qO- https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

#### From Source

```bash
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
./install.sh
```

Or manually:
```bash
cargo build --release --bins
```

The binary will be at `target/release/leindex`.

### Verification

```bash
leindex --version
# Output: LeIndex 0.1.0
```

### Basic Usage

```bash
# Index a project
leindex index /path/to/project

# Search semantically
leindex search "authentication logic"

# Run diagnostics
leindex diagnostics
```

---

## MCP Server Integration

LeIndex includes a built-in MCP server for AI assistant integration.

### Configuration

Add to your MCP client configuration (e.g., Claude Code, Cursor):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {}
    }
  }
}
```

### Available MCP Tools

- `leindex_index` - Index projects
- `leindex_search` - Semantic search
- `leindex_deep_analyze` - Deep code analysis
- `leindex_context` - Context expansion
- `leindex_diagnostics` - System health checks

---

## Configuration

LeIndex uses TOML configuration files. Place `leindex.toml` in your project root:

```toml
# Memory settings
[memory]
total_budget_mb = 3072          # 3GB total budget
soft_limit_percent = 0.80       # 80% = cleanup triggered
hard_limit_percent = 0.93       # 93% = spill to disk
emergency_percent = 0.98        # 98% = emergency eviction

# File filtering
[file_filtering]
max_file_size = 1073741824      # 1GB per file
exclude_patterns = [
    "**/node_modules/**",
    "**/.git/**",
    "**/target/**",
    "**/build/**"
]

# Parser settings
[parsing]
batch_size = 100                # Files per batch
parallel_parsers = 4            # Concurrent parsing jobs
```

---

## Development

### Project Structure

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

### Building

```bash
# Build all crates
cargo build --release

# Build specific crate
cargo build -p lepasserelle --release

# Run tests
cargo test --workspace
```

### Running Examples

```bash
# Index current directory
cargo run --release -- index .

# Search indexed code
cargo run --release -- search "database connection"
```

---

## Migration from Python v2.0.2

LeIndex has been completely rewritten in Rust. See [MIGRATION.md](MIGRATION.md) for:
- Breaking changes
- Configuration differences
- Data migration notes

---

## Documentation

- [Installation Guide](INSTALLATION_RUST.md) - Detailed setup instructions
- [Architecture](ARCHITECTURE.md) - System design and internals
- [Migration Guide](MIGRATION.md) - Python to Rust migration
- [MCP Compatibility](MCP_COMPATIBILITY.md) - MCP server details
- [Contributing](CONTRIBUTING.md) - Development guidelines

---

## Performance

### Benchmarks (Preliminary)

| Metric | Value |
|--------|-------|
| **Indexing Speed** | ~10K files/min (est.) |
| **Search Latency (p50)** | ~50ms (est.) |
| **Memory Usage** | <3GB typical |
| **Max Scalability** | 100K+ files |

*Note: Official benchmarks pending. Python v2.0.2 achieved ~10K files/min indexing speed. Rust implementation aims to match or exceed this.*

---

## Technology Stack

| Component | Technology | Purpose |
|-----------|------------|---------|
| **Parsing** | Tree-sitter | Zero-copy AST extraction |
| **Graph Analysis** | petgraph | PDG construction and traversal |
| **Vector Search** | hnsw_rs | HNSW approximate nearest neighbors |
| **CLI** | clap | Command-line argument parsing |
| **MCP Server** | axum | HTTP-based MCP protocol |
| **Async** | tokio | Future async operations |
| **Logging** | tracing | Structured logging |
| **Serialization** | serde | Efficient data encoding |

---

## Roadmap

### v0.2.0 (Planned)
- [ ] Complete Turso/libsql integration
- [ ] Persistent vector storage (vec0 extension)
- [ ] Metadata persistence

### v0.3.0 (Planned)
- [ ] Re-enable Swift/Kotlin/Dart parsers
- [ ] Cross-project search
- [ ] Advanced memory management

---

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

Areas where help is especially appreciated:
- Additional language parsers
- Performance optimizations
- Documentation improvements
- Bug fixes

---

## License

MIT OR Apache-2.0 - see [LICENSE](LICENSE) for details.

---

## Acknowledgments

LeIndex is built on amazing open-source projects:

- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/) - Incremental parsing system
- [petgraph](https://github.com/petgraph/petgraph) - Graph data structures
- [hnsw_rs](https://github.com/jorgecarleitao/hnsw_rs) - HNSW algorithm
- [axum](https://github.com/tokio-rs/axum) - Web framework
- [Model Context Protocol](https://modelcontextprotocol.io) - AI integration

---

## Support

- **GitHub Issues:** [https://github.com/scooter-lacroix/leindex/issues](https://github.com/scooter-lacroix/leindex/issues)
- **Documentation:** [https://github.com/scooter-lacroix/leindex](https://github.com/scooter-lacroix/leindex)
- **Star us on GitHub** - It helps more people discover LeIndex! ⭐

---

<div align="center">

**Built with ❤️ and Rust for developers who love their code**

*⭐ Star us on GitHub — it makes us smile!*

**Ready to search smarter?** [Install LeIndex now](#quick-start) 🚀

</div>
