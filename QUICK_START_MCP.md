# Quick Start MCP (Rust)

## Start MCP server

```bash
leindex mcp
```

Or HTTP mode:

```bash
leindex serve --host 127.0.0.1 --port 47268
```

## Core MCP tools

- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`
- `leindex_phase_analysis` (alias: `phase_analysis`)

## Minimal assistant request for 5-phase

```json
{
  "phase": "all",
  "path": "/path/to/project",
  "mode": "balanced"
}
```

## Install via cargo

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```
