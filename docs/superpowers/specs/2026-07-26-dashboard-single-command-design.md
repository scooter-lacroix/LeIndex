# Dashboard single-command design

## Goal

`leindex dashboard` must start a working dashboard by itself: live REST and
WebSocket data, all discoverable indexed projects, project browsing, and
project-scoped or global search.

## Root cause

The dashboard browser client calls the REST/WebSocket service at port 47269.
`leindex dashboard` currently starts only Bun. `leindex serve` is an MCP
server on port 47500, not the dashboard REST server. Starting it cannot make
the dashboard API or event socket available.

## Launch lifecycle

`leindex dashboard` owns the REST/WebSocket server lifecycle.

1. It creates the existing `LeIndexServer` with a persistent dashboard
   database and its normal project-database discovery.
2. It binds the dashboard API on `127.0.0.1:47269` and waits for `/api/health`
   before starting Bun on the requested UI port (5173 by default).
3. It reuses a healthy LeIndex dashboard API already bound to that port;
   another process on the port is a clear startup error.
4. Bun exit, Ctrl-C, and startup failure stop the API task owned by the
   dashboard command.

`leindex serve` remains the MCP server command. Dashboard documentation names
only `leindex dashboard` as the normal launch command.

## Dashboard interactions

The existing lightweight `dashboard/src/app.tsx` remains the active UI.

- The codebase list represents every project discovered by the REST service.
- Selecting a codebase loads its graph, metrics, and expandable file tree.
- Selecting a file opens a read-only source view.
- Search includes a scope dropdown with `All indexed projects` and each
  discovered codebase.
- Search results show their project and select the project/file when opened.
- WebSocket events refresh affected dashboard data and show live connection
  state.

## API additions

- `GET /api/search` accepts an optional project identifier to restrict results.
- `GET /api/codebases/:id/file?path=...` returns UTF-8 source only after
  canonicalizing the requested path and proving it is contained by the
  selected project root.

No new frontend framework or dependency is introduced.

## Failure behavior

- Dashboard startup fails before opening the UI when the REST service is not
  healthy.
- API/socket loss is visible in the UI with a retry action.
- A healthy but empty index explains that no indexed projects were discovered.
- File access outside a selected project returns a client error and never
  exposes arbitrary local files.

## Verification

- Rust tests cover API startup/reuse decisions, project-scoped search, and
  path-safe source reads.
- Dashboard checks cover the API client scope/query behavior, TypeScript type
  checking, and Bun production build.
- Repository completion gates remain `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace`.
