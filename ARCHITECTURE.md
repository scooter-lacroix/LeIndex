# LeIndex Architecture (v1.5.1)

## Overview

LeIndex is a unified Rust crate for code intelligence with:

- parsing and symbol extraction
- program-dependency graph traversal
- semantic/structural retrieval
- storage and diagnostics
- MCP server + HTTP/WebSocket API
- dashboard observability

The design target is low-latency analysis with low resource usage, including multi-project operation in a single process.

## Unified Modules

- `parse`: tree-sitter parsing + signatures
- `graph`: dependency graph modeling and traversal
- `search`: retrieval, scoring, embeddings/vector internals
- `storage`: SQLite-backed persistence and schema
- `phase`: additive multi-phase analysis
- `cli`: CLI, MCP request handling, tool execution
- `global`: project discovery and registry helpers
- `server`: HTTP/WebSocket server for dashboard/API clients
- `edit`: edit preview/apply primitives
- `validation`: validation and safety checks

Hidden compatibility aliases for the legacy crate names still exist for migration, but the canonical module paths are the unified `leindex::*` names above.

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

`leindex::server` exposes dashboard-facing APIs such as:

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

## Removed Components Documentation

### `leserve` Binary (Removed in v1.5.1)

The `leserve` binary was a standalone HTTP/WebSocket server for serving the dashboard without requiring Bun. It was removed in v1.5.1 when the crate was unified into a single `leindex` binary.

**Functionality:**
- Compiled Rust HTTP server using Axum
- Served dashboard static files directly without Bun dependency
- WebSocket support for real-time events
- SQLite-backed API endpoints for dashboard data

**Original Binary Location:** `src/bin/leserve.rs`

**Key Components:**
```rust
// Entry point pattern
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = leindex::server::config::ServerConfig::from_env();
    let server = leindex::server::LeIndexServer::new(config)?;
    server.start().await?;
    Ok(())
}
```

**Configuration:**
- Host: from env or default `127.0.0.1`
- Port: from env `LEINDEX_PORT` or default `8080`
- DB Path: configured via `ServerConfig`

**API Endpoints Provided:**
- `GET /api/health` - Health check
- `GET /api/dashboard/overview` - Dashboard overview data
- `GET /api/codebases` - List indexed codebases
- `GET /api/codebases/:id` - Get codebase details
- `GET /api/codebases/:id/graph` - Get codebase graph data
- `GET /api/codebases/:id/files` - List files in codebase
- `GET /api/search` - Search endpoint
- `GET /ws/events` - WebSocket for real-time events

**Why It Was Useful:**
- Zero runtime dependencies (no Bun required)
- Single binary deployment
- Faster startup than Bun-based dev server
- Better for production deployments

**Current Status:**
- Functionality replaced by `leindex dashboard` command (requires Bun)
- `leindex serve` command provides MCP HTTP server only, not dashboard serving
- To restore: Add `leindex serve-dashboard` subcommand using `LeIndexServer` or restore `leserve` binary

**Reimplementation Path:**
1. Option A: Add new subcommand `leindex serve-dashboard` using existing `LeIndexServer` in library
2. Option B: Restore `leserve` binary entry point in `Cargo.toml` and `src/bin/leserve.rs`
3. Option C: Merge `LeIndexServer` functionality into `leindex serve` with `--dashboard` flag

### `leedit` Binary (Removed in v1.5.1)

The `leedit` binary was a stub for code editing utilities. It was removed as it contained no implemented functionality.

**Original State:**
- Only printed "not yet implemented" for all commands
- Commands planned: `format`, `lint`
- No actual editing logic was implemented

**Current Status:**
- Editing functionality exists as MCP tools within `leindex` binary:
  - `leindex_edit_preview`
  - `leindex_edit_apply`
  - `leindex_rename_symbol`
- No need for standalone binary - all editing through MCP

**If Restored:**
- Would implement local CLI editing without MCP
- Commands: `leedit format <file>`, `leedit lint <file>`
- Currently not needed as editing is MCP-first
