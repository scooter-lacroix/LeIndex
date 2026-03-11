# LeIndex API Reference (v1.5.1)

## CLI

Primary binary: `leindex`

### Commands

- `leindex index <path> [--force] [--progress]`
- `leindex search <query> [--top-k <n>] [--project <path>]`
- `leindex analyze <query> [--tokens <n>] [--project <path>]`
- `leindex phase --phase <1..5> | --all [--path <path>] [options]`
- `leindex diagnostics [--project <path>]`
- `leindex serve [--host <ip>] [--port <u16>]`
- `leindex mcp [--stdio]`
- `leindex dashboard [--port <u16>] [--prod]`

## MCP JSON-RPC

Transport:

- stdio (`leindex mcp`)
- HTTP (`leindex serve`)

### Methods

- `initialize`
- `tools/list`
- `tools/call`
- `notifications/initialized`

### Tool Names

- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`
- `leindex_phase_analysis`
- `phase_analysis`
- `leindex_file_summary`
- `leindex_symbol_lookup`
- `leindex_project_map`
- `leindex_grep_symbols`
- `leindex_read_symbol`
- `leindex_edit_preview`
- `leindex_edit_apply`
- `leindex_rename_symbol`
- `leindex_impact_analysis`

### Request/Response Notes

- Request `id` is optional for notifications.
- Non-notification calls should include an `id`.
- Boolean-like tool args accept both JSON boolean and string forms where applicable.

## HTTP + WebSocket (`leindex serve`)

Base default: `http://127.0.0.1:47269`

### Endpoints

- `GET /api/health`
- `GET /api/dashboard/overview`
- `GET /api/codebases`
- `GET /api/codebases/:id`
- `POST /api/codebases/:id`
- `GET /api/codebases/:id/graph`
- `GET /api/codebases/:id/files`
- `GET /api/search`
- `GET /ws`
- `GET /ws/events`

## Rust Integration

Install from crates.io:

```bash
cargo install leindex
```

The unified crate version is defined in the root `Cargo.toml` under `[package].version`.

## Dashboard Integration

Dashboard consumes the `leindex serve` APIs and WebSocket events.

Dashboard path lookup used by `leindex dashboard`:

1. `./dashboard`
2. parent traversal (up to 5 levels)
3. `LEINDEX_DASHBOARD_DIR`
4. `~/.leindex/dashboard`
