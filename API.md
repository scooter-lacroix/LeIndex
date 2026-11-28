# LeIndex API Reference (v2.2.0)

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

`tools/list` advertises five routers. The `mode` / `action` argument selects the
operation; every other argument is forwarded unchanged to the branch.

- `leindex_explore` — `mode`: `search` (default), `find`, `structural`,
  `symbol_lookup`, `read_file`, `read_symbol`, `project_map`, `file_summary`,
  `context`
- `leindex_analyze` — `mode`: `deep` (default), `impact`, `diagnostics`, `phase`
- `leindex_edit` — `action` (required): `preview`, `apply`, `rename`, `write`
- `leindex_manage` — `action`: `index` (default), `state`, `projects`, `evict`,
  `lease`, `cleanup`, `run`, `how_to_test`
- `leindex_git` — `action`: `status`, `diff`, `log`, `show`, `blame`, `branch`,
  `remote`, `add`, `commit`, `checkout`, `stash`, `tag`, `merge`, `rebase`,
  `push`, `reset`, `force` (mutations are plan-gated: a bare call returns a plan
  plus a `preview_token` bound to HEAD and the index; re-send it to apply, or pass
  `confirm: true` for the safe subset — `add`, `commit`, `push` without force)

The original per-operation names (`leindex_search`, `leindex_deep_analyze`,
`leindex_edit_apply`, …) remain callable but are not advertised;
`LEINDEX_MCP_LEGACY_TOOLS=1` advertises them. `LEINDEX_MCP_SCHEMA=oneof` switches
`tools/list` to `oneOf` + `discriminator` schemas. Every branch also accepts
`tier` (`l0` identity card · `l1` overview · `l2` full detail).

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
