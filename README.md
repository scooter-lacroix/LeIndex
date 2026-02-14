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

- **Zero-Copy AST** — Tree-sitter parsing for 12 languages (Python, Rust, JS, TS, Go, Java, C++, C#, Ruby, PHP, Lua, Scala)
- **Semantic Search** — HNSW vector search with natural language queries
- **PDG Analysis** — Program Dependence Graph with gravity-based traversal
- **MCP Server** — First-class Model Context Protocol for AI assistants
- **Smart Caching** — RSS monitoring, automatic spilling, and warming strategies
- **Cross-Project** — Global symbol table for multi-project resolution

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
│                    lepasserelle (CLI/MCP)                    │
│              index • search • analyze • serve                │
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

## License

MIT OR Apache-2.0 — see [LICENSE](LICENSE) for details.

---

<div align="center">

**Built with Rust for developers who love their code**

[Install Now](#quick-start) • [Documentation](#documentation) • [Contribute](CONTRIBUTING.md)

</div>
