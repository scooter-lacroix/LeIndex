# Dashboard Single-Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `leindex dashboard` launch its REST/WebSocket backend and Bun UI together, then provide safe project browsing and global or project-scoped search.

**Architecture:** The dashboard command owns a `LeIndexServer` task on the REST port (47269), waits for its health endpoint, and then runs Bun. The REST service aggregates discovered project databases; focused handlers add a project filter to search and a canonical-path source-read endpoint. The current lightweight React entrypoint stays active and gains a scope selector, file tree/source pane, and reconnect action.

**Tech Stack:** Rust, Tokio, Axum, rusqlite, Bun, React 18, TypeScript, Bun test.

---

## File structure

- `src/cli/cli.rs` — supervise dashboard API and Bun lifecycle; preserve MCP-only `serve`.
- `src/server/server.rs` — expose serving through a pre-bound listener for race-free dashboard startup.
- `src/server/handlers.rs` — project-filtered symbol search and path-contained source reads.
- `src/server/responses.rs` — wire response for source reads and project identity in search results.
- `dashboard/src/lib/api.ts` — typed query construction and source-read client calls.
- `dashboard/src/types/index.ts` — frontend contracts for scoped results and source content.
- `dashboard/src/components/DashboardFileTree.tsx` — focused recursive file browser used by the active app.
- `dashboard/src/app.tsx` / `dashboard/src/styles.css` — scope picker, selected-file view, socket retry, empty states.
- `dashboard/src/lib/api_test.ts` — Bun tests for API URL and query behavior.
- `dashboard/package.json` — `bun test` script only; no new dependency.
- `README.md` / `dashboard/README.md` — document the single launch command and interaction model.

### Task 1: Make the REST server launchable by the dashboard command

**Files:**
- Modify: `src/server/server.rs:83-111`
- Modify: `src/cli/cli.rs:1785-1879`
- Test: `src/server/server.rs` (`#[cfg(test)] mod tests`)
- Test: `src/cli/cli.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing server readiness test**

Add a test that binds an ephemeral listener, starts a `LeIndexServer` with a temporary database, and verifies `/api/health` responds successfully:

```rust
#[tokio::test]
async fn test_server_serves_health_on_prebound_listener() {
    let dir = tempfile::tempdir().unwrap();
    let config = ServerConfig {
        db_path: dir.path().join("dashboard.db").display().to_string(),
        ..ServerConfig::default()
    };
    let server = LeIndexServer::new(config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { server.serve_listener(listener).await });
    let response = reqwest::get(format!("http://{address}/api/health")).await.unwrap();
    assert!(response.status().is_success());
    task.abort();
}
```

- [ ] **Step 2: Run the test and verify RED**

Run: `cargo test test_server_serves_health_on_prebound_listener -- --exact`

Expected: compile failure because `serve_listener` does not exist.

- [ ] **Step 3: Add the pre-bound listener API**

Refactor `start` to bind and delegate, so both paths construct identical state/router behavior:

```rust
pub async fn start(&self) -> Result<(), ApiError> {
    let listener = tokio::net::TcpListener::bind(self.socket_addr()?).await
        .map_err(|error| ApiError::internal(format!("Failed to bind: {error}")))?;
    self.serve_listener(listener).await
}

pub async fn serve_listener(&self, listener: tokio::net::TcpListener) -> Result<(), ApiError> {
    let state = AppState::new_from_arc(Arc::clone(&self.storage), self.config.clone());
    axum::serve(listener, create_router().with_state(state))
        .await
        .map_err(|error| ApiError::internal(format!("Server error: {error}")) )
}
```

- [ ] **Step 4: Run the server test and verify GREEN**

Run: `cargo test test_server_serves_health_on_prebound_listener -- --exact`

Expected: PASS.

- [ ] **Step 5: Write failing dashboard command tests**

Add pure helper tests for the fixed REST endpoint and dashboard database placement:

```rust
#[test]
fn test_dashboard_api_url_uses_rest_default_port() {
    assert_eq!(dashboard_api_url(), "http://127.0.0.1:47269");
}

#[test]
fn test_dashboard_database_lives_under_leindex_home() {
    let path = dashboard_database_path(Path::new("/tmp/leindex-home"));
    assert_eq!(path, PathBuf::from("/tmp/leindex-home").join("dashboard.db"));
}
```

- [ ] **Step 6: Run the CLI tests and verify RED**

Run: `cargo test test_dashboard_ --lib -- --nocapture`

Expected: compile failure because the dashboard API helpers do not exist.

- [ ] **Step 7: Implement the dashboard supervisor**

In `cmd_dashboard_impl`, use `ServerConfig` and `LeIndexServer` directly; do not invoke `Commands::Serve` or MCP server code. Add helpers with these contracts:

```rust
const DASHBOARD_API_PORT: u16 = crate::server::config::DEFAULT_PORT;

fn dashboard_api_url() -> String {
    format!("http://127.0.0.1:{DASHBOARD_API_PORT}")
}

fn dashboard_database_path(home: &Path) -> PathBuf {
    home.join("dashboard.db")
}
```

Resolve the data directory from `LEINDEX_HOME`, otherwise `dirs::home_dir()?.join(".leindex")`; create it. Bind `127.0.0.1:47269`. On `AddrInUse`, request `/api/health` and reuse only a response whose JSON service is `leindex`; otherwise return an error that names the occupied port. For an owned listener, construct `LeIndexServer` with `db_path` set to `dashboard_database_path`, spawn `serve_listener`, and poll `/api/health` before launching Bun.

Run `bun install --frozen-lockfile` before Bun, then execute `bun run start` for `--prod` or `bun run dev` otherwise, with `DASHBOARD_PORT` set to the CLI port. Retain the spawned task handle and abort it when Bun exits. Print the UI and API URLs once readiness succeeds.

- [ ] **Step 8: Run focused Rust tests and format**

Run:

```bash
cargo fmt --all
cargo test test_dashboard_ --lib -- --nocapture
cargo test test_server_serves_health_on_prebound_listener -- --exact
```

Expected: PASS.

- [ ] **Step 9: Commit the lifecycle change**

```bash
git add src/cli/cli.rs src/server/server.rs
git commit -m "feat: launch dashboard API with UI"
```

### Task 2: Add scoped search and path-safe source reads

**Files:**
- Modify: `src/server/handlers.rs:26-40,696-775,889-909`
- Modify: `src/server/responses.rs` (search and source response structs)
- Test: `src/server/handlers.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing handler tests**

Create a temporary project root and verify the path resolver accepts a contained file and rejects traversal:

```rust
#[test]
fn test_resolve_project_file_rejects_parent_traversal() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join("project");
    let outside = workspace.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.rs"), "fn secret() {}\n").unwrap();

    let error = resolve_project_file(&root, Path::new("../outside/secret.rs")).unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}
```

Add an in-memory storage fixture with two `global_symbols` rows and assert `search_symbols(..., Some("project-a"))` returns only the project-a row.

- [ ] **Step 2: Run handler tests and verify RED**

Run: `cargo test test_resolve_project_file_rejects_parent_traversal --lib -- --exact`

Expected: compile failure because `resolve_project_file` does not exist.

- [ ] **Step 3: Extend search and source contracts**

Add `project_id: Option<String>` to `SearchQuery`. Extend `SearchResultResponse` with `project_id` and `project_name`. Add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContentResponse {
    pub path: String,
    pub content: String,
}
```

Factor symbol SQL into `search_symbols(conn, query, project_id, limit)`. Its `WHERE` clause must include `AND (?2 IS NULL OR project_id = ?2)` and its `LIMIT` parameter must be `?3`; it must return the matching project identity. Preserve unscoped behavior when `project_id` is absent.

- [ ] **Step 4: Implement contained file reading**

Add these focused functions to `handlers.rs`:

```rust
fn resolve_project_file(root: &Path, requested: &Path) -> ApiResult<PathBuf> {
    let root = root.canonicalize().map_err(|error| ApiError::not_found(error.to_string()))?;
    let candidate = root.join(requested).canonicalize()
        .map_err(|error| ApiError::not_found(error.to_string()))?;
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return Err(ApiError::bad_request("file must be inside the selected project"));
    }
    Ok(candidate)
}
```

`get_file_content(Path(id), Query(FileQuery { path }), State(state))` must obtain the project root from `project_metadata`, call `resolve_project_file`, read with `std::fs::read_to_string`, and return `FileContentResponse`. Invalid UTF-8 returns a client error without including file contents.

Register it as `GET /api/codebases/:id/file`; retain the existing `/files` tree route.

- [ ] **Step 5: Run handler tests and verify GREEN**

Run: `cargo test 'test_resolve_project_file_rejects_parent_traversal|test_search_symbols_filters_project' --lib -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit the API contract**

```bash
git add src/server/handlers.rs src/server/responses.rs
git commit -m "feat: add scoped dashboard search and source reads"
```

### Task 3: Connect the active dashboard to browse and scoped-search APIs

**Files:**
- Create: `dashboard/src/components/DashboardFileTree.tsx`
- Create: `dashboard/src/lib/api_test.ts`
- Modify: `dashboard/src/lib/api.ts`
- Modify: `dashboard/src/types/index.ts`
- Modify: `dashboard/src/app.tsx`
- Modify: `dashboard/src/styles.css`
- Modify: `dashboard/package.json`

- [ ] **Step 1: Write failing Bun tests for scoped request URLs**

Export the URL builder from `api.ts` and test both global and selected-project requests:

```ts
import { expect, test } from "bun:test";
import { searchPath } from "./api";

test("searchPath omits project_id for all projects", () => {
  expect(searchPath("auth", 12)).toBe("/api/search?q=auth&limit=12");
});

test("searchPath includes project_id for a selected project", () => {
  expect(searchPath("auth", 12, "project-a")).toBe(
    "/api/search?q=auth&limit=12&project_id=project-a",
  );
});
```

- [ ] **Step 2: Run the Bun test and verify RED**

Run: `cd dashboard && bun test src/lib/api_test.ts`

Expected: failure because `searchPath` is not exported.

- [ ] **Step 3: Implement typed client and file tree**

Add the corresponding frontend types:

```ts
export interface FileContentResponse { path: string; content: string; }

export interface SearchResultResponse {
  project_id: string;
  project_name: string;
  // retain existing rank, node, file, symbol, language, score, context, byte_range fields
}
```

Use `URLSearchParams` in `searchPath(query, limit, projectId?)`, omitting `project_id` when `projectId` is absent. Add `api.getFileContent(id, path)` using `path` as a query parameter.

Create `DashboardFileTree` with props `nodes: FileNode[]`, `selectedPath?: string`, and `onSelect(path: string)`. Render directories recursively and call `onSelect` only for file nodes. It owns no network state.

- [ ] **Step 4: Wire the active app**

In `app.tsx` add `searchScope`, `selectedFile`, `selectedFileContent`, and `socketAttempt` state. Render a `<select>` before the search button with an empty value labeled `All indexed projects` and one option per `codebase.id`. Pass `searchScope || undefined` to `api.search`.

Render `DashboardFileTree` from the loaded `FileTreeResponse`; selecting a file calls `api.getFileContent(selectedCodebaseId, path)` and displays the returned text in a read-only `<pre>`. Clicking a search result sets its `project_id`, then opens its file content. Show `project_name` in result metadata.

Make the WebSocket effect depend on `socketAttempt`; add a Retry button that increments it and reruns `loadDashboard`. On event receipt, rerun `loadDashboard` so project metrics do not go stale. When `loadState === "ready" && codebases.length === 0`, render `No indexed projects discovered. Run leindex index <path> and refresh.`

- [ ] **Step 5: Add minimal styling and test script**

Add CSS for the scope select, file-tree indentation/selection, source pane overflow, clickable search results, and retry action. Add:

```json
"test": "bun test"
```

to `dashboard/package.json` scripts.

- [ ] **Step 6: Run dashboard checks and verify GREEN**

Run: `cd dashboard && bun test && bun run typecheck && bun run build`

Expected: all three commands exit 0.

- [ ] **Step 7: Commit the dashboard interaction change**

```bash
git add dashboard/package.json dashboard/src/app.tsx dashboard/src/components/DashboardFileTree.tsx dashboard/src/lib/api.ts dashboard/src/lib/api_test.ts dashboard/src/styles.css dashboard/src/types/index.ts
git commit -m "feat: browse indexed projects from dashboard"
```

### Task 4: Align public guidance and run end-to-end validation

**Files:**
- Modify: `README.md`
- Modify: `dashboard/README.md`

- [ ] **Step 1: Update dashboard launch documentation**

Replace instructions that require `leindex serve` with:

```bash
leindex dashboard
```

Document `leindex dashboard --prod` as the production UI mode and explain that the command starts the REST/WebSocket backend automatically. Describe the search scope selector and source inspection behavior.

- [ ] **Step 2: Verify the installed-flow behavior manually**

Run: `leindex dashboard`

Expected: output reports a healthy API at `http://127.0.0.1:47269` and UI at `http://127.0.0.1:5173`; the browser reports Socket `live`, lists discovered projects, supports scope selection, and opens a selected file.

- [ ] **Step 3: Run all repository gates**

Run:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd dashboard && bun install --frozen-lockfile && bun test && bun run typecheck && bun run build
```

Expected: zero warnings and every command exits 0.

- [ ] **Step 4: Commit documentation and final verification**

```bash
git add README.md dashboard/README.md
git commit -m "docs: document unified dashboard launch"
```

## Plan self-review

- Spec coverage: Task 1 owns one-command API/UI lifecycle and health behavior; Task 2 adds scoped search and path-safe source reads; Task 3 provides project browsing, source interaction, global/project scope selection, socket retry, and empty state; Task 4 updates guidance and runs the full gates.
- Placeholder scan: all file paths, test names, endpoints, data contracts, commands, and lifecycle outcomes are explicit.
- Type consistency: Rust uses `project_id`, `FileContentResponse`, `resolve_project_file`, and `serve_listener` consistently; TypeScript uses the same `project_id` and `FileContentResponse` contract.
