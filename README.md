# LeIndex

<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="600"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![MCP Server](https://img.shields.io/badge/MCP-Server-blue?style=for-the-badge)](https://modelcontextprotocol.io)
[![Tests](https://img.shields.io/badge/Tests-339%2F339-passing-success?style=for-the-badge)](https://github.com/scooter-lacroix/leindex)
[![License](https://img.shields.io/badge/License-MIT-yellow?style=for-the-badge)](LICENSE)
[![Version](https://img.shields.io/badge/Version-0.1.0-blue?style=for-the-badge)](CHANGELOG.md)

**Pure Rust Code Search and Analysis Engine**

*Lightning-fast semantic code search with zero-copy parsing, PDG analysis, gravity-based traversal, and intelligent memory management.*

</div>

---

## What is LeIndex?

**LeIndex** is a **pure Rust** implementation of an intelligent code search and analysis engine. It combines zero-copy parsing, semantic understanding, and efficient storage to help you navigate and understand large codebases.

### Key Features

- **Zero-Copy AST Extraction** - Tree-sitter based parsing with 12 language support
- **Program Dependence Graph (PDG)** - Advanced code relationship analysis with gravity-based traversal
- **HNSW Vector Search** - Production-scale semantic similarity search with natural language queries
- **Natural Language Queries** - Intent-aware search (HowWorks, WhereHandled, Bottlenecks, Semantic, Text)
- **MCP Server** - First-class Model Context Protocol support for AI assistants
- **Memory Efficient** - Smart cache management with RSS monitoring, spilling, reloading, and warming
- **INT8 Quantization** - 74% memory reduction for vector storage with SIMD-optimized distance computation
- **Cross-Project Intelligence** - Global symbol table for multi-project resolution
- **Pure Rust CLI** - Five commands: index, search, analyze, diagnostics, serve

---

## Architecture

LeIndex consists of 5 production-ready Rust crates:

| Crate | Purpose | Status | Tests |
|-------|---------|--------|-------|
| **leparse** | Zero-copy AST extraction | ✅ Production Ready | 97/97 |
| **legraphe** | PDG analysis with gravity traversal | ✅ Production Ready | 38/38 |
| **lerecherche** | HNSW semantic search with NL queries | ✅ Production Ready | 87/87 |
| **lestockage** | SQLite storage + cross-project | ✅ Production Ready | 45/45 |
| **lepasserelle** | CLI & MCP server | ✅ Production Ready | 72/72 |
| **Total** | | | **339/339** |

### Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                              LeIndex v0.1.0 Architecture                                │
├─────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                         │
│             ┌──────────────────────┐  ┌──────────────────────┐                          │
│             │     CLI Commands     │  │     MCP Server       │                          │
│             │  index, search,      │  │    JSON-RPC 2.0      │                          │
│             │  analyze, diag, serve│  │   (axum HTTP)        │                          │
│             └──────────┬───────────┘  └──────────┬───────────┘                          │
│                        │                         │                                      │
│                        └────────────┬────────────┘                                      │
│                                     ▼                                                   │
│        ┌────────────────────────────────────────────────────────────────┐               │
│        │                  LeIndex Orchestration                         │               │
│        │              (lepasserelle - 675 lines)                        │               │
│        │  • Project indexing • Search • Analysis • Diagnostics          │               │
│        │  • Cache spilling/reloading/warming • Memory monitoring        │               │
│        └─────┬────────┬───────────┬───────────┬─────────────┬───────────┘               │
│              │        │           │           │             │                           │
│        ┌─────▼───┐ ┌──▼─────┐ ┌───▼────┐ ┌────▼────┐ ┌──────▼──────┐                    │
│        │ leparse │ │legraphe│ │lerech  │ │lestock  │ │   Cache     │                    │
│        │         │ │        │ │ erche  │ │ age     │ │ Management  │                    │
│        │12 langs │ │  PDG   │ │ HNSW   │ │ SQLite  │ │ RSS Monitor │                    │
│        │zero-copy│ │gravity │ │ NL Q   │ │ global  │ │ Spill/Reload│                    │
│        │ tree-   │ │traverse│ │INT8    │ │ symbols │ │ 4 Warm Strat│                    │
│        │ sitter  │ │ embed  │ │quantize│ │ PDG     │ │             │                    │
│        └─────────┘ └────────┘ └────────┘ └─────────┘ └─────────────┘                    │
│  Technologies:                                                                          │
│  • Parsing: tree-sitter (12 langs) • Rayon parallel processing                          │
│  • Graph: petgraph StableGraph • Gravity traversal w/ priority queue                    │
│  • Search: HNSW (hnsw-rs) • Cosine similarity • NL query parser                         │
│  • Storage: SQLite + BLAKE3 hashing • Vector embeddings • Cross-project global symbols  │
│  • Server: axum + tokio • JSON-RPC 2.0 protocol                                         │
│                                                                                         │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

### Language Support

| Language | Parser | Status |
|----------|--------|--------|
| Python | tree-sitter-python | ✅ Working |
| Rust | tree-sitter-rust | ✅ Working |
| JavaScript | tree-sitter-javascript | ✅ Working |
| TypeScript | tree-sitter-typescript | ✅ Working |
| Go | tree-sitter-go | ✅ Working |
| Java | tree-sitter-java | ✅ Working |
| C++ | tree-sitter-cpp | ✅ Working |
| C# | tree-sitter-c-sharp | ✅ Working |
| Ruby | tree-sitter-ruby | ✅ Working |
| PHP | tree-sitter-php | ✅ Working |
| Lua | tree-sitter-lua | ✅ Working |
| Scala | tree-sitter-scala | ✅ Working |

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

# Deep analysis with context expansion
leindex analyze "how does the database connection work"

# Run diagnostics
leindex diagnostics

# Start MCP server for AI assistant integration
leindex serve --host 127.0.0.1 --port 3000
```

---

## MCP Server Integration

LeIndex includes a built-in MCP server for AI assistant integration.

### Starting the Server

```bash
leindex serve --host 127.0.0.1 --port 3000
```

The server provides:
- `POST /mcp` - JSON-RPC 2.0 endpoint
- `GET /mcp/tools/list` - List available tools
- `GET /health` - Health check

### Configuration

Add to your MCP client configuration:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["serve", "--host", "127.0.0.1", "--port", "3000"],
      "env": {}
    }
  }
}
```

### Available MCP Tools

| Tool | Description |
|------|-------------|
| `deep_analyze` | Deep code analysis with context expansion |
| `search` | Semantic code search |
| `index` | Index projects |
| `context` | Context expansion with gravity traversal |
| `diagnostics` | System health checks |

---

## Cache Management

LeIndex includes intelligent cache management for memory efficiency.

### Cache Strategies

- **All** - Warm both PDG and vector caches
- **PDGOnly** - Warm only PDG cache
- **SearchIndexOnly** - Warm only vector search cache
- **RecentFirst** - Prioritize recently accessed data

### Memory Monitoring

- RSS monitoring with 85% threshold
- Automatic cache spilling when memory limit exceeded
- Cache reloading from storage
- Configurable warming strategies

---

## Development

### Project Structure

```
leindex/
├── crates/
│   ├── leparse/        # AST extraction (97 tests) ✅
│   ├── legraphe/       # PDG analysis (38 tests) ✅
│   ├── lerecherche/    # Vector search (87 tests) ✅
│   ├── lestockage/     # Storage layer (45 tests) ✅
│   └── lepasserelle/   # CLI & MCP server (72 tests) ✅
├── Cargo.toml          # Workspace configuration
├── install.sh          # One-line installer
└── README.md           # This file
```

### Building

```bash
# Build all crates
cargo build --release

# Run all tests
cargo test --workspace

# Run with diagnostics
RUST_LOG=debug cargo run --release -- index .
```

---

## Performance

### Benchmarks (v0.1.0)

| Metric | Target | Status |
|--------|--------|--------|
| **Indexing Speed** | <60s for 50K files | ✅ Achieved |
| **Search Latency (P95)** | <100ms | ✅ Achieved |
| **Memory Reduction** | 10x (400→32 bytes/node) | ✅ Achieved |
| **Token Efficiency** | 20% improvement | ✅ Achieved |

### Code Quality

| Metric | Value |
|--------|-------|
| **Tests** | 339/339 passing (100%) |
| **Warnings** | 0 clippy warnings |
| **Documentation** | Complete rustdoc |
| **Code Review** | Tzar-approved for lerecherche (18 issues resolved) |

---

## Technology Stack

| Component | Technology | Purpose |
|-----------|------------|---------|
| **Parsing** | tree-sitter | Zero-copy AST extraction (12 languages) |
| **Graph** | petgraph | PDG construction with StableGraph |
| **Traversal** | Custom | Gravity-based traversal with priority queue |
| **Vector Search** | hnsw-rs | HNSW approximate nearest neighbors |
| **NL Queries** | Custom | Intent classification and pattern matching |
| **CLI** | clap | Command-line argument parsing |
| **MCP Server** | axum | HTTP-based MCP protocol (JSON-RPC 2.0) |
| **Async** | tokio | Async runtime |
| **Logging** | tracing | Structured logging |
| **Serialization** | serde/bincode | Efficient data encoding |
| **Storage** | SQLite | Local persistence with WAL mode |
| **Hashing** | BLAKE3 | Incremental computation cache |

---

## Documentation

- [Installation Guide](INSTALLATION_RUST.md) - Detailed setup instructions
- [Architecture](ARCHITECTURE.md) - System design and internals
- [Migration Guide](MIGRATION.md) - Python to Rust migration
- [MCP Compatibility](MCP_COMPATIBILITY.md) - MCP server details
- [Contributing](CONTRIBUTING.md) - Development guidelines
- [Changelog](CHANGELOG.md) - Version history

---

## Roadmap

### Completed ✅

- [x] Zero-copy AST extraction with 12 languages
- [x] PDG construction with gravity-based traversal
- [x] HNSW vector index for semantic search
- [x] Natural language query processing
- [x] Cross-project symbol resolution
- [x] JSON-RPC 2.0 MCP server
- [x] Cache management (spill/reload/warm)


### v0.2.0 (Planned)

- [ ] Project configuration (TOML/JSON)
- [ ] Detailed error reporting and recovery

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
- [hnsw-rs](https://github.com/jorgecarleitao/hnsw_rs) - HNSW algorithm
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
