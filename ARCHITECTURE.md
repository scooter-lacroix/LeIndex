# LeIndex Architecture (v1.9.5)

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

LeIndex exposes 20 MCP tools for indexing, retrieval, context, edits, and impact analysis.

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

LeIndex uses a project registry model:

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

## Core Retrieval and Hybrid Search Architecture

LeIndex has one node-level search corpus. Parsing creates PDG nodes for files,
symbols, methods, and other code units; both TF-IDF and Qwen3 vectors use those
same stable node identifiers. This preserves graph reachability and symbol
precision while adding semantic recall. It does not maintain an independent
file/chunk semantic sidecar.

Indexing writes three search artifacts under each project:

- `embeddings.bin`: memory-mapped TF-IDF rows
- `neural_embeddings.bin`: memory-mapped Qwen3 rows
- `search_snapshot.bin`: PDG fingerprint, row metadata, and hydration state

When the PDG fingerprint matches, CLI and MCP processes hydrate the snapshot
and mmap files directly. They do not run refresh, deduplication, or index
maintenance before every query. Changed files update affected nodes through
incremental indexing; a full compatibility rebuild is reserved for missing,
stale, or incompatible artifacts.

Candidate generation combines lexical, neural, and structural scores. TF-IDF
and graph results are always available. When ONNX is enabled, semantic requests
start and await the configured worker through its explicit lifecycle, so a
healthy cold provider contributes neural scores; terminal failure preserves
the TF-IDF/PDG core.

## Embedding Worker

`leindex-embed` owns tokenization, ONNX Runtime, model memory, and provider
state. On Unix, clients connect to a local socket keyed by provider, model, and
inference shape,
allowing short-lived CLI commands and the MCP server to share one resident
session. Resident socket workers exit after ten idle minutes. Setup smoke tests
and non-Unix platforms retain direct process IPC with a one-minute idle limit.

The Qwen3 runtime contract is:

- CPU/CUDA dynamic batches, default maximum 32
- MIGraphX stable batches of 8 with padded inputs excluded from results
- 128-token stable input shape by default
- `input_ids`, `attention_mask`, and `position_ids`
- last-unpadded-token pooling and L2 normalization
- output order identical to request order

MIGraphX uses graph optimization level 3 and a model/version/shape-specific
compiled-program cache under `$LEINDEX_HOME/cache/migraphx`. Setup warms the selected provider
and rejects a GPU configuration when the active provider is CPU. CUDA and CPU
use the same model graph with their corresponding ONNX Runtime package.

## Model And Runtime Distribution

Release artifacts contain the main binary, embedding worker, and platform
ONNX Runtime libraries. They never contain model weights, tokenizer data, or
model configuration. `leindex setup` uses Hugging Face CLI to stage, install,
and checksum the validated dynamic Qwen3 export under
`$LEINDEX_HOME/models`.

Provider choice is runtime configuration:

- CPU: `onnxruntime`
- NVIDIA: `onnxruntime-gpu` with CUDA
- AMD: `onnxruntime-migraphx` with ROCm/MIGraphX

This keeps crates.io, npm, PyPI, and GitHub bundles small and prevents a stale
or provider-incompatible model from being embedded in a release.

## Dashboard Integration

Dashboard assets live under `dashboard/` and are served in development via Bun.

`leindex dashboard` resolves dashboard path in this order:

1. `./dashboard` from current directory
2. parent traversal (dev convenience)
3. `LEINDEX_DASHBOARD_DIR`
4. `~/.leindex/dashboard`

## Packaging Notes

- `cargo install leindex` installs CLI/MCP and embedding-worker binaries.
- GitHub and npm bundles include ONNX Runtime libraries but no model files.
- `leindex setup` installs the model and records provider configuration.
- Dashboard assets are distributed via repository installs/installer, not embedded in the crate artifact.

## Removed Components Documentation

### `leserve` Binary (Removed in v1.5.2)

The `leserve` binary was a standalone HTTP/WebSocket server for serving the dashboard without requiring Bun. It was removed in v1.5.2 when the crate was unified into a single `leindex` binary.

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

### `leedit` Binary (Removed in v1.5.2)

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
