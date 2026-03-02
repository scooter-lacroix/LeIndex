<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)

**LeIndex v1.5.0**

AI-powered code indexing, semantic search, dependency analysis, MCP tooling, and dashboard observability.

</div>

---

## What Is LeIndex?

LeIndex is a Rust workspace for code intelligence. It provides:

- Fast indexing with tree-sitter parsing.
- PDG-based structural analysis and context expansion.
- Semantic + structural retrieval for code understanding.
- 16 MCP tools for read, analysis, and safe code-edit workflows.
- HTTP/WebSocket server (`leserve`) and a frontend dashboard.
- Multi-project support with low-resource operation.

## Install

### Option A: One-line installer (recommended)

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

This installs:

- `leindex` binary to `/usr/local/bin`
- dashboard assets to `~/.leindex/dashboard`
- optional dashboard production build (if Bun is available)

### Option B: crates.io

```bash
cargo install leindex
```

Note: crates.io installs the CLI/MCP binaries. Dashboard assets are distributed from repository installs (or manual clone) rather than bundled in the crate artifact.

## Quick Start

```bash
# index
leindex index /path/to/project

# semantic search
leindex search "authentication flow"

# deep analysis
leindex analyze "how authorization is enforced"

# 5-phase additive analysis
leindex phase --all --path /path/to/project

# diagnostics
leindex diagnostics

# MCP stdio mode (for Claude/Codex/Cursor integrations)
leindex mcp

# HTTP MCP server
leindex serve --host 127.0.0.1 --port 47268

# Dashboard (dev server)
leindex dashboard
```

## Dashboard

LeIndex ships with a Bun + React dashboard focused on operational visibility:

- codebase inventory and per-project metrics
- graph volume and dependency telemetry
- cache temperature/hit-rate snapshot
- external dependency counters
- live events over WebSocket

Local dashboard paths used by the CLI:

1. `./dashboard` (repo root)
2. parent-directory traversal (dev convenience)
3. `LEINDEX_DASHBOARD_DIR` env override
4. `~/.leindex/dashboard` (installer default)

## Workspace Layout

LeIndex v1.5.0 workspace crates:

- `leparse`: language parsing and signature extraction
- `legraphe`: graph construction and traversal
- `lerecherche`: retrieval / scoring / vector search internals
- `lestockage`: SQLite persistence + storage primitives
- `lephase`: additive phase analysis pipeline
- `lepasserelle`: CLI + MCP protocol handlers
- `leglobal`: cross-project discovery/registry support
- `leserve`: HTTP/WebSocket API server
- `leedit`: edit-preview/apply support
- `levalidation`: validation and guardrails

## MCP Tools (16)

- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`
- `leindex_phase_analysis`
- `phase_analysis` (alias)
- `leindex_file_summary`
- `leindex_symbol_lookup`
- `leindex_project_map`
- `leindex_grep_symbols`
- `leindex_read_symbol`
- `leindex_edit_preview`
- `leindex_edit_apply`
- `leindex_rename_symbol`
- `leindex_impact_analysis`

## Development

```bash
cargo build --workspace
cargo test --workspace

cd dashboard
bun install
bun run build
```

## Docs

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [API.md](API.md)
- [docs/MCP.md](docs/MCP.md)
- [dashboard/README.md](dashboard/README.md)

## License

MIT OR Apache-2.0
