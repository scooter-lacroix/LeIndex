# LeIndex Dashboard (v1.5.0)

LeIndex dashboard is a Bun + React UI for operational visibility over indexed codebases.

## Highlights

- codebase inventory with per-project counts
- dependency graph cardinality snapshot
- cache telemetry and estimated hit-rate view
- external dependency counters
- websocket event stream (`/ws/events`)
- backend health and status indicators

## Runtime

- Bun (required)
- React + TypeScript

## Scripts

```bash
bun install
bun run dev
bun run build
bun run start
bun run typecheck
```

## Backend Requirements

Dashboard expects `leserve` endpoints on `http://127.0.0.1:47269`:

- `GET /api/health`
- `GET /api/dashboard/overview`
- `GET /api/codebases`
- `GET /api/codebases/:id`
- `GET /api/codebases/:id/graph`
- `GET /api/codebases/:id/files`
- `GET /api/search`
- `GET /ws/events`

## Start via CLI

```bash
leindex dashboard
```

Path resolution order in CLI:

1. `./dashboard`
2. parent traversal (dev convenience)
3. `LEINDEX_DASHBOARD_DIR`
4. `~/.leindex/dashboard`

## Build Artifacts

Production files are emitted to `dashboard/dist/`.

The installer also attempts to prebuild these assets when Bun is available.
