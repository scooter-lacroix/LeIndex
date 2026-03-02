# LeIndex Architecture (v1.5.0)

## Overview

LeIndex is a Rust workspace for code intelligence with:

- parsing and symbol extraction
- program-dependency graph traversal
- semantic/structural retrieval
- storage and diagnostics
- MCP server + HTTP/WebSocket API
- dashboard observability

The design target is low-latency analysis with low resource usage, including multi-project operation in a single process.

## Workspace Crates

- `leparse`: tree-sitter parsing + signatures
- `legraphe`: dependency graph modeling and traversal
- `lerecherche`: retrieval, scoring, embeddings/vector internals
- `lestockage`: SQLite-backed persistence and schema
- `lephase`: additive multi-phase analysis
- `lepasserelle`: CLI, MCP request handling, tool execution
- `leglobal`: project discovery and registry helpers
- `leserve`: HTTP/WebSocket server for dashboard/API clients
- `leedit`: edit preview/apply primitives
- `levalidation`: validation and safety checks

## Runtime Surfaces

### CLI (`leindex`)

- `index`
- `search`
- `analyze`
- `phase`
- `diagnostics`
- `serve`
- `mcp`
- `dashboard`

### MCP

LeIndex exposes 16 MCP tools for indexing, retrieval, context, edits, and impact analysis.

### HTTP/WebSocket

`leserve` exposes dashboard-facing APIs such as:

- `GET /api/health`
- `GET /api/dashboard/overview`
- `GET /api/codebases`
- `GET /api/codebases/:id`
- `GET /api/codebases/:id/graph`
- `GET /api/codebases/:id/files`
- `GET /api/search`
- `GET /ws/events`

## Concurrency Model

LePasserelle uses a project registry model:

- one process can handle multiple projects
- per-project locking enables parallel read workloads
- indexing rebuilds in blocking tasks then performs brief in-memory swap
- SQLite busy-timeout is configured to reduce transient lock failures

## Data Flow

1. Parse source files into signatures.
2. Build/update dependency graph and symbol relationships.
3. Persist index artifacts in storage.
4. Serve read/analysis/edit-preview requests via CLI, MCP, and HTTP.
5. Emit telemetry for diagnostics and dashboard metrics.

## Dashboard Integration

Dashboard assets live under `dashboard/` and are served in development via Bun.

`leindex dashboard` resolves dashboard path in this order:

1. `./dashboard` from current directory
2. parent traversal (dev convenience)
3. `LEINDEX_DASHBOARD_DIR`
4. `~/.leindex/dashboard`

## Packaging Notes

- `cargo install leindex` installs CLI/MCP binaries.
- Dashboard assets are distributed via repository installs/installer, not embedded in the crate artifact.
