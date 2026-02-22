<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)
[![Tests](https://img.shields.io/badge/Tests-339%2F339-brightgreen?style=flat-square)](https://github.com/scooter-lacroix/leindex)

**AI-powered code search and indexing with MCP integration**

*Lightning-fast semantic search • Zero-copy parsing • PDG analysis • 12 languages*

</div>

---

## Features

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

| Crate | Purpose | Status |
|-------|---------|--------|
| **leparse** | Zero-copy AST extraction | Parsing 12 languages (Python, Rust, JS, TS, Go, Java, C++, C#, Ruby, PHP, Lua, Scala) |
| **legraphe** | PDG analysis with gravity traversal | Context based exploration |
| **lerecherche** | HNSW semantic search with NL queries | Semantic similarity search |
| **lestockage** | SQLite storage + cross-project | RSS monitoring, automatic spilling, and warming strategies |
| **lepasserelle** | CLI & MCP server orchestration | MCP server tools with configurable token outputs, CLI fallback/alternative |

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

### Installation

```bash
# One-line installer (Linux/macOS)
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash

# Or via cargo
cargo install leindex

# Verify
leindex --version
```

### Basic Usage

```bash
# Index a project
leindex index /path/to/project

# Search semantically
leindex search "authentication flow"

# Deep analysis
leindex analyze "how does error handling work"

# 5-phase additive analysis
leindex phase /path/to/project

# System diagnostics
leindex diagnostics
```

## CLI Commands

| Command | Description |
|---------|-------------|
| `index` | Index a project for search and analysis |
| `search` | Semantic code search with NL queries |
| `analyze` | Deep analysis with context expansion |
| `phase` | 5-phase additive analysis workflow |
| `diagnostics` | System health and index statistics |
| `serve` | Start MCP HTTP server (axum) |
| `mcp` | MCP stdio mode for AI tool integration |

## MCP Integration

### Claude Code

Add to `~/.claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"]
    }
  }
}
```

### Cursor / Other MCP Clients

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["serve", "--host", "127.0.0.1", "--port", "3000"]
    }
  }
}
```

### Available MCP Tools

| Tool | Description |
|------|-------------|
| `deep_analyze` | Deep code analysis with PDG traversal |
| `search` | Semantic code search |
| `index` | Index projects for analysis |
| `context` | Context expansion around nodes |
| `diagnostics` | System health checks |
| `phase_analysis` | 5-phase additive analysis |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    lepasserelle (CLI/MCP)                   │
│              index • search • analyze • serve               │
└───────────┬─────────────┬─────────────┬─────────────┬───────┘
            │             │             │             |
     ┌──────▼──────┐┌─────▼─────┐┌──────▼──────┐┌─────▼─────┐
     │   leparse   ││ legraphe  ││ lerecherche ││ lestockage│
     │             ││           ││             ││           │
     │ tree-sitter ││    PDG    ││    HNSW     ││  SQLite   │
     │ 12 langs    ││  gravity  ││   vectors   ││  storage  │
     │ zero-copy   ││ traversal ││   search    ││  global   │
     └─────────────┘└───────────┘└─────────────┘└───────────┘
```

**Crates:**
- **leparse** — Zero-copy AST extraction with tree-sitter
- **legraphe** — PDG construction with gravity-based traversal
- **lerecherche** — HNSW semantic search with NL query parser
- **lestockage** — SQLite persistence with cross-project symbols
- **lepasserelle** — CLI & MCP server orchestration

## Development

```bash
# Build
cargo build --release

# Test all crates
cargo test --workspace

# Run with debug logging
RUST_LOG=debug cargo run --release -- index .

# Format and lint
cargo fmt && cargo clippy
```

### Project Structure

```
leindex/
├── crates/
│   ├── leparse/        # AST extraction
│   ├── legraphe/       # PDG analysis
│   ├── lerecherche/    # Vector search
│   ├── lestockage/     # Storage layer
│   └── lepasserelle/   # CLI & MCP
├── Cargo.toml
└── install.sh
```

## Documentation

- [Architecture Guide](ARCHITECTURE.md) — System design internals
- [API Reference](API.md) — Detailed API documentation
- [MCP Compatibility](MCP_COMPATIBILITY.md) — MCP server details
- [Contributing](CONTRIBUTING.md) — Development guidelines

## Performance

| Metric | Target | Status |
|--------|--------|--------|
| Indexing (50K files) | <60s | ✅ |
| Search P95 latency | <100ms | ✅ |
| Memory per node | 32 bytes | ✅ |
| Tests | 339/339 | ✅ 100% |

## Roadmap

### Completed ✅

- [x] Zero-copy AST extraction with 12 languages
- [x] PDG construction with gravity-based traversal
- [x] HNSW vector index for semantic search
- [x] Natural language query processing
- [x] Cross-project symbol resolution
- [x] JSON-RPC 2.0 MCP server
- [x] Cache management (spill/reload/warm)
- [x] INT8 quantization for vector storage


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

MIT OR Apache-2.0 — see [LICENSE](LICENSE) for details.

---

<div align="center">

**Built with Rust for developers who love their code**

[Install Now](#quick-start) • [Documentation](#documentation) • [Contribute](CONTRIBUTING.md)

</div>
