# LeIndex Follow-up: Tzar of Excellence Remediation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Optimize ONNX daemon initialization to be inherently fast and never hang, add missing test coverage for parser fixtures and index-job lifecycle guarantees, document the MCP start/poll migration, convert `JobStatus` from string to a type-safe enum, and add dead-worker detection to inference IPC.

**Architecture:** The ONNX embed daemon startup must be optimized to eliminate every unnecessary operation. The investigation found six concrete bottlenecks: (1) Python subprocess spawns for ORT discovery that can be replaced with direct filesystem path checks, (2) redundant config file parsing twice per startup, (3) `WorkerConfigEnv` re-read on every poll iteration, (4) health probe reconnect-per-poll overhead, (5) unbuffered reads on the data path, and (6) MIGraphX cold compilation with no warmup step. No arbitrary timeouts are added; every fix either removes unnecessary work, caches results, or pre-computes expensive operations. The existing `DAEMON_READY_MAX_WAIT` stays as a safety net but should never be the reason indexing succeeds.

**Tech Stack:** Rust 1.75, Tokio, ONNX Runtime, MIGraphX, Unix sockets, std::process::Command.

## Execution status (working tree, 2026-07-23)

The Tzar of Excellence review of the performance overhaul plan identified four critical defects that were fixed inline (CLI index hang, missing catch_unwind, stale warning false positives, no auto-incremental indexing). This follow-up plan addresses the remaining gaps and the ONNX daemon initialization optimization. The investigation traced the full call chain from `EmbeddingClient::new()` through `spawn_or_connect_daemon()` to `WorkerRuntime::new()` -> `init_onnx()` -> `discover_and_init()` -> `build_session()`, identifying every operation that adds latency.

---

## Non-negotiable decisions

1. **No arbitrary timeouts.** Timeouts mask issues and can prematurely terminate operations. Every fix must either eliminate unnecessary work, cache results, or pre-compute expensive operations so they complete as fast as possible. The existing `DAEMON_HEALTH_WAIT` (250ms, dead-peer detection) and `DAEMON_READY_MAX_WAIT` (120s, last-resort safety net) remain unchanged.
2. **No environment variable changes.** The user's ONNX/MIGraphX setup is correctly configured. If the daemon is slow, it is a code defect.
3. **Optimize the hot path, don't widen the budget.** If Python discovery takes 400ms, the fix is to not spawn Python, not to give it 30s. If MIGraphX compilation takes 3 minutes on first run, the fix is to pre-compile during setup, not to wait longer.
4. **Cache once, reuse forever (within process).** Config parsing, worker binary resolution, and ORT path discovery results are stable for a process lifetime. Use `OnceLock` or struct-level caching.
5. **Test coverage is a contract.** Missing fixtures and lifecycle tests are not optional polish; they verify the core architectural decisions of the overhaul.
6. **No new dependencies.** Use existing crates only.

## Evidence and root-cause hierarchy

| Priority | Evidence | Root cause | Primary code |
|---|---|---|---|
| P0 | Daemon startup spawns Python interpreter to discover ORT library path, adding 200-600ms when the pip fallback is reached | `python_one_line()` at `ort_discovery.rs:252` spawns `python3`/`python` to run `import onnxruntime.capi` which loads the entire ORT C extension just to get a directory path | `crates/leindex-embed/src/ort_discovery.rs:252-293` |
| P0 | Daemon recompiles MIGraphX kernels on first run of each version; cold compilation can take minutes | No warmup/pre-compilation step; `commit_from_file()` at `runtime.rs:630` triggers full GPU kernel compilation | `crates/leindex-embed/src/runtime.rs:483-635` |
| P1 | Config file (`leindex.toml`) is parsed twice per worker startup: once for `ort_dylib_path`, once for `model_name` | `read_config_ort_path()` at discovery and `RuntimeConfig::from_env()` both call `LeIndexConfig::load()` independently | `crates/leindex-embed/src/ort_discovery.rs:323`, `crates/leindex-embed/src/runtime.rs:206` |
| P1 | `WorkerConfigEnv` is re-read from `leindex.toml` on every `availability()` poll call during daemon readiness wait | `read_worker_config_env_from_config()` called in `availability()` at `client.rs:778`, polled every 25ms | `src/search/onnx/client.rs:778` |
| P1 | Health probe opens a new Unix socket connection every 25ms poll, doing full connect/write/read/close cycle | `wait_for_daemon_ready()` at `client.rs:1274` calls `probe_daemon_health_unbounded()` each iteration | `src/search/onnx/client.rs:1274-1313` |
| P2 | Parser fixture coverage gaps; the `Askpass`, nested CLI, and dataflow fixtures described in the overhaul plan were never created | Plan was checked off without creating the files | `tests/fixtures/rust/` (missing directory) |
| P2 | No disconnect survival, coalescing, or concurrent first-load test; ownership guarantees are untested | Tests were planned in Task 6 Step 1 but never written | `tests/index_job_recovery.rs` |
| P2 | Inference response data path uses raw `UnixStream::read` without buffering; large embedding responses cause many small syscalls | `read_frame()` at `client.rs:43` operates on unbuffered `R: Read` | `src/search/onnx/client.rs:43-75` |
| P3 | `docs/MIGRATION.md` covers only v1->v2; the 1.9.0 MCP index start/poll change is undocumented | Migration doc was not updated for the new async job model | `docs/MIGRATION.md` |
| P3 | `IndexJobSnapshot.status` is `String` instead of `JobStatus` enum; status transitions rely on string matching | Design deviation from the plan's typed enum | `src/cli/index_job.rs:24` |

## Target request paths after fixes

```text
Worker startup (optimized):
  worker_main: bind socket -> write "initializing" state
  -> spawn init thread
  -> init thread: resolve ORT path
     -> check OnceLock cache (process-local, hit = 0ms)
     -> check ORT_DYLIB_PATH env (hit = 0ms)
     -> check config ort_dylib_path (hit = 0ms, config from OnceLock)
     -> scan ~/.local/lib/pythonX.YY/site-packages/onnxruntime/capi/ (hit = stat, ~1ms)
     -> ONLY if all above miss: spawn Python (last resort, cached to file for next time)
  -> init thread: build session
     -> load model protobuf
     -> MIGraphX: check cache at ORT_MIGRAPHX_MODEL_CACHE_PATH (warm = fast, cold = compile)
     -> register EP
  -> init thread: set Ready or Failed

Client readiness (optimized):
  ensure_worker: resolve config (OnceLock, read once)
  -> check daemon availability
     -> read status file (fast, ~0.1ms)
     -> check PID liveness (kill(0), ~0.01ms)
     -> if status=ready and PID alive: return Ready WITHOUT socket probe
     -> if status=initializing: open one persistent health socket
     -> poll health on same socket (no reconnect), 25ms sleep between polls

Inference IPC (optimized):
  send_and_receive: send request frame
  -> reader thread uses BufReader-wrapped UnixStream
  -> response arrives in fewer, larger reads
  -> detect dead worker via read error (not arbitrary timeout)
```

---

### Task 1: Optimize ONNX daemon initialization for inherent speed

**Files:**
- Modify: `crates/leindex-embed/src/ort_discovery.rs`
- Modify: `crates/leindex-embed/src/runtime.rs`
- Modify: `crates/leindex-embed/src/config.rs`
- Modify: `crates/leindex-embed/src/worker_main.rs`
- Modify: `src/search/onnx/client.rs`
- Test: `tests/onnx_worker_fallback.rs`

- [ ] **Step 1: Write failing performance characterization tests**

Add tests that measure startup phases and assert they complete within bounds reflecting optimized code (not arbitrary limits):

```rust
#[test]
fn ort_discovery_caches_config_load() {
    // First call loads from disk
    let start = Instant::now();
    let _config1 = LeIndexConfig::load_cached();
    let first_load = start.elapsed();

    // Second call must hit cache
    let start = Instant::now();
    let _config2 = LeIndexConfig::load_cached();
    let cached_load = start.elapsed();

    assert!(cached_load < Duration::from_millis(1),
        "cached config load must be <1ms, got {:?}", cached_load);
}

#[test]
fn pip_discovery_checks_filesystem_before_python() {
    // When ORT_DYLIB_PATH is set, Python must never be spawned
    std::env::set_var("ORT_DYLIB_PATH", "/usr/lib/libonnxruntime.so");
    let not_a_real_python = std::process::Command::new("false")
        .env("LEINDEX_TEST_BLOCK_PYTHON", "1");
    // Discovery should skip Python entirely and return the env path
    let result = discover_candidates();
    assert!(result.iter().any(|p| p.to_str().unwrap().contains("libonnxruntime")));
}
```

Run: `cargo test -p leindex-embed ort_discovery --all-features`

Expected: FAIL because `load_cached()` and filesystem-first discovery don't exist.

- [ ] **Step 2: Cache LeIndexConfig in a process-local OnceLock**

In `crates/leindex-embed/src/config.rs`, add:

```rust
use std::sync::OnceLock;

static CONFIG_CACHE: OnceLock<LeIndexConfig> = OnceLock::new();

impl LeIndexConfig {
    pub fn load_cached() -> &'static LeIndexConfig {
        CONFIG_CACHE.get_or_init(|| LeIndexConfig::load().unwrap_or_default())
    }
}
```

Update ALL callers of `LeIndexConfig::load()` in the embed crate to use `load_cached()`:
- `src/ort_discovery.rs:read_config_ort_path()` -> use `LeIndexConfig::load_cached()`
- `src/runtime.rs:RuntimeConfig::from_env()` -> use `LeIndexConfig::load_cached()`

This eliminates the double parse: the first call reads and parses the TOML; all subsequent calls return the static reference.

- [ ] **Step 3: Replace Python subprocess with direct filesystem path scanning**

In `crates/leindex-embed/src/ort_discovery.rs`, add a filesystem-first discovery function that checks common pip site-packages paths without spawning Python:

```rust
fn discover_pip_lib_filesystem() -> Option<PathBuf> {
    // Check user-local pip installs: ~/.local/lib/pythonX.YY/site-packages/onnxruntime/capi/
    if let Some(home) = std::env::var_os("HOME") {
        let local_lib = PathBuf::from(&home).join(".local/lib");
        if let Ok(entries) = std::fs::read_dir(&local_lib) {
            for entry in entries.filter_map(|e| e.ok()) {
                let py_dir = entry.path().join("site-packages/onnxruntime/capi");
                if let Some(lib) = find_ort_lib_in_dir(&py_dir) {
                    return Some(lib);
                }
            }
        }
    }

    // Check system site-packages
    for prefix in ["/usr/lib", "/usr/local/lib"] {
        let lib_dir = PathBuf::from(prefix);
        if let Ok(entries) = std::fs::read_dir(&lib_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                if name.to_str().map_or(false, |s| s.starts_with("python")) {
                    let capi = entry.path().join("site-packages/onnxruntime/capi");
                    if let Some(lib) = find_ort_lib_in_dir(&capi) {
                        return Some(lib);
                    }
                }
            }
        }
    }
    None
}

fn find_ort_lib_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    // Prefer the exact version, fall back to any libonnxruntime*.so
    let mut best: Option<PathBuf> = None;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name_str = name.to_str()?;
        if name_str.starts_with("libonnxruntime.so") {
            // Prefer .so over .so.X.Y.Z
            if name_str == "libonnxruntime.so" {
                return Some(entry.path());
            }
            best = Some(entry.path());
        }
    }
    best
}
```

Update `discover_pip_lib()` (line 281) to try `discover_pip_lib_filesystem()` FIRST. Only fall back to `python_one_line()` if the filesystem scan finds nothing and the filesystem result has not been cached as "not found." If `python_one_line()` succeeds, write the result to `~/.leindex/cache/ort_pip_path` so subsequent startups skip Python entirely.

- [ ] **Step 4: Cache WorkerConfigEnv in EmbeddingClient struct**

In `src/search/onnx/client.rs`, change `EmbeddingClient` to cache the config at construction:

```rust
pub struct EmbeddingClient {
    // ... existing fields ...
    cached_config: OnceLock<WorkerConfigEnv>,
}
```

In `EmbeddingClient::new()`, initialize the `OnceLock`. In `read_worker_config_env_from_config()`, use `self.cached_config.get_or_init(|| ...)` so it only reads the file once per client instance. This eliminates repeated file reads during `availability()` polling.

- [ ] **Step 5: Optimize health probe to reuse connection during readiness polling**

In `src/search/onnx/client.rs`, modify `wait_for_daemon_ready()` to open ONE persistent connection and poll on it instead of reconnecting every 25ms:

```rust
fn wait_for_daemon_ready(socket_path: &Path) -> Result<(), ClientError> {
    let started = Instant::now();
    let mut stream: Option<UnixStream> = None;
    loop {
        if started.elapsed() >= DAEMON_READY_MAX_WAIT {
            return Err(ClientError::DaemonStuck);
        }
        // Reuse connection or open new one
        if stream.is_none() {
            stream = Some(UnixStream::connect(socket_path)?);
        }
        let s = stream.as_mut().unwrap();
        match probe_daemon_health_on_stream(s) {
            Ok(health) => match health.state {
                WorkerState::Ready => return Ok(()),
                WorkerState::Initializing => {
                    thread::sleep(DAEMON_READINESS_POLL);
                    continue;
                }
                WorkerState::Failed => return Err(...),
            },
            Err(_) => {
                // Connection lost, try to reconnect next iteration
                stream.take();
            }
        }
    }
}
```

This eliminates the connect/disconnect overhead per poll iteration. Each poll is just write+read on an existing socket.

- [ ] **Step 6: Short-circuit daemon reuse when status file says Ready and PID is alive**

In `src/search/onnx/client.rs`, method `availability()`, when the status file already says "ready" and the PID is alive, return `WorkerAvailability::Ready` immediately without a socket health probe:

```rust
fn availability(&self) -> WorkerAvailability {
    // ... existing status/PID checks ...
    
    // Fast path: status file says ready and PID is alive
    if let Some(state) = &status_state {
        if state.state == "ready" && daemon_pid_alive {
            return WorkerAvailability::Ready;
        }
    }
    
    // Fall through to socket probe for initializing/unknown states
    // ...
}
```

- [ ] **Step 7: Add BufReader to inference data path**

In `src/search/onnx/client.rs`, wrap the reader thread's `UnixStream` in `BufReader` to reduce syscall count for large embedding responses:

```rust
// In socket_worker_handle or the reader thread setup:
let reader = std::io::BufReader::with_capacity(BUF_CAPACITY, stream);
// Where BUF_CAPACITY is sized for the largest expected response
// (e.g., 1024-dim * 32 batch * 4 bytes = 128KB)
const READ_BUF_CAPACITY: usize = 128 * 1024;
```

Apply to both the client-side reader thread and the worker-side handler.

- [ ] **Step 8: Pre-compile MIGraphX model during leindex setup**

In `src/cli/setup.rs` (or wherever the setup/install process lives), after model files are verified, add an optional warmup step that triggers MIGraphX compilation at setup time. This stores compiled kernels in the cache before any indexing run needs them. See `src/cli/leindex/setup.rs` or equivalent for the setup flow.

The warmup should:
1. Spawn the embed worker with a dummy one-line input
2. Wait for it to reach Ready
3. Send a single embed request with a dummy string
4. Wait for the response (triggers compilation, populates cache)
5. Shut down the worker gracefully
6. Log "MIGraphX warm compilation complete"

This is an opt-in step (add a `--warmup` flag to `leindex setup`) but recommend it in the setup output. Alternatively, auto-run on first index if the cache is cold.

- [ ] **Step 9: Verify optimized startup is fast**

Run: `cargo test -p leindex-embed --all-features`

Expected: PASS including caching tests.

Run: `cargo test --test onnx_worker_fallback --all-features`

Expected: PASS.

Run: `timeout 30 target/release/leindex index tests/fixtures/memcheck/small_repo`

Expected: Completes in seconds (not minutes), prints "Indexing complete!", exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/leindex-embed/src/ort_discovery.rs crates/leindex-embed/src/runtime.rs crates/leindex-embed/src/config.rs crates/leindex-embed/src/worker_main.rs src/search/onnx/client.rs tests/onnx_worker_fallback.rs
git commit -m "perf: optimize ONNX daemon initialization startup (Python-free discovery, config caching, connection reuse, buffer/reader)"
```

---

### Task 2: Add realistic Rust parser test fixtures

**Files:**
- Create: `tests/fixtures/rust/askpass.rs`
- Create: `tests/fixtures/rust/nested_cli.rs`
- Create: `tests/fixtures/rust/flows.rs`
- Create: `tests/parser_fixtures.rs`

- [ ] **Step 1: Write failing parser fixture tests**

Create `tests/parser_fixtures.rs` with tests that parse each fixture and assert specific symbol inventory, kinds, qualified names, and byte ranges:

```rust
// askpass.rs assertions
let symbols = parse_rust_file("tests/fixtures/rust/askpass.rs");
assert!(symbols.iter().any(|s| s.name == "Askpass" && s.kind == "struct"));
assert!(symbols.iter().any(|s| s.name == "new" && s.qualified_name == "Askpass::new" && s.is_method));
assert!(symbols.iter().any(|s| s.name == "path" && s.qualified_name == "Askpass::path" && s.is_method));

// nested_cli.rs assertions
assert!(symbols.iter().any(|s| s.name == "Cli" && s.kind == "struct"));
assert!(symbols.iter().any(|s| s.name == "Commands" && s.kind == "enum"));
assert!(symbols.iter().any(|s| s.name == "Install" && s.qualified_name == "Commands::Install" && s.kind == "enum_variant"));
assert!(symbols.iter().any(|s| s.name == "Config" && s.kind == "struct" && s.qualified_name == "config::Config"));

// flows.rs assertions
assert!(symbols.iter().any(|s| s.name == "run_installation" && s.kind == "function"));
assert!(symbols.iter().any(|s| s.name == "RegistryRecord" && s.kind == "struct"));
assert!(symbols.iter().any(|s| s.name == "verify_registry" && s.kind == "function"));
assert!(symbols.iter().any(|s| s.name == "detect_arch" && s.kind == "function"));
```

Run: `cargo test --test parser_fixtures --all-features -- --nocapture`

Expected: FAIL because fixtures do not exist.

- [ ] **Step 2: Create `askpass.rs` fixture**

Create `tests/fixtures/rust/askpass.rs` with a realistic `Askpass` struct, `new`/`path` methods, credential handling, and a standalone `main` function.

- [ ] **Step 3: Create `nested_cli.rs` fixture**

Create `tests/fixtures/rust/nested_cli.rs` with nested modules, impl blocks, Clap-style enum variants, associated functions without `self`, and a `mod config` with `pub struct Config`.

- [ ] **Step 4: Create `flows.rs` fixture**

Create `tests/fixtures/rust/flows.rs` with `RegistryRecord`, `run_installation` calling `verify_registry`/`detect_arch`/`execute_command`, and cross-function data dependencies.

- [ ] **Step 5: Verify parser fixture tests pass**

Run: `cargo test --test parser_fixtures --all-features -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add tests/fixtures/rust/askpass.rs tests/fixtures/rust/nested_cli.rs tests/fixtures/rust/flows.rs tests/parser_fixtures.rs
git commit -m "test: add realistic Rust parser fixtures"
```

---

### Task 3: Add index-job lifecycle tests (disconnect, coalescing, concurrent first-load)

**Files:**
- Modify: `tests/index_job_recovery.rs`
- Modify: `src/cli/registry.rs` (test hooks, if needed)

- [ ] **Step 1: Write disconnect survival test**

Start an index job, drop the snapshot (simulating MCP disconnect), then poll and assert the job completes independently:

```rust
#[tokio::test]
async fn disconnect_survival_job_continues_to_completion() {
    let dir = tempfile::tempdir().unwrap();
    init_git_fixture(&dir).await;
    let registry = Arc::new(ProjectRegistry::new());
    let snapshot = registry.start_index_job(
        Some(dir.path().to_str().unwrap()), false, false
    ).await.unwrap();
    drop(snapshot);  // Simulate disconnect

    // Poll for completion
    for _ in 0..120 {
        let status = registry.index_status(&dir.path()).await;
        if status.status != "running" { break; }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(registry.index_status(&dir.path()).await.status, "complete");
}
```

- [ ] **Step 2: Write concurrent request coalescing test**

Start indexing, issue a second `start_index_job` for the same project, assert same `job_id`:

```rust
#[tokio::test]
async fn concurrent_requests_coalesce_into_single_job() {
    let dir = tempfile::tempdir().unwrap();
    init_git_fixture_with_files(&dir, 5).await;
    let registry = Arc::new(ProjectRegistry::new());
    let snap1 = registry.start_index_job(
        Some(dir.path().to_str().unwrap()), false, false
    ).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let snap2 = registry.start_index_job(
        Some(dir.path().to_str().unwrap()), false, false
    ).await.unwrap();
    assert_eq!(snap1.job_id, snap2.job_id);
}
```

- [ ] **Step 3: Write concurrent first-load test (20 callers)**

Spawn 20 concurrent `get_or_create` callers, assert project creation occurs exactly once:

```rust
#[tokio::test]
async fn concurrent_first_load_creates_project_once() {
    let dir = tempfile::tempdir().unwrap();
    init_git_fixture(&dir).await;
    let registry = Arc::new(ProjectRegistry::new());
    let mut handles = Vec::new();
    for _ in 0..20 {
        let reg = registry.clone();
        let path = dir.path().to_path_buf();
        handles.push(tokio::spawn(async move {
            reg.get_or_create(path.to_str().unwrap()).await
        }));
    }
    let results = futures::future::join_all(handles).await;
    for r in &results { assert!(r.is_ok()); }
    // Verify single DB instance
    let storage_path = dir.path().join(".leindex");
    assert!(storage_path.exists());
}
```

- [ ] **Step 4: Write panic recovery test**

Inject a panic during indexing, assert job transitions to `failed` (not stuck in `running`):

```rust
#[tokio::test]
async fn panic_during_index_sets_failed_status() {
    let dir = tempfile::tempdir().unwrap();
    init_git_fixture(&dir).await;
    let registry = Arc::new(ProjectRegistry::new());
    std::env::set_var("LEINDEX_INJECT_PANIC", "1");
    let snap = registry.start_index_job(
        Some(dir.path().to_str().unwrap()), false, false
    ).await.unwrap();
    for _ in 0..20 {
        let status = registry.index_status(&dir.path()).await;
        if status.status == "failed" { break; }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    std::env::remove_var("LEINDEX_INJECT_PANIC");
    let final_status = registry.index_status(&dir.path()).await;
    assert_eq!(final_status.status, "failed");
    assert!(final_status.last_error.unwrap_or("").contains("panic"));
}
```

- [ ] **Step 5: Verify lifecycle tests pass**

Run: `cargo test --test index_job_recovery --all-features -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add tests/index_job_recovery.rs src/cli/registry.rs
git commit -m "test: add index-job lifecycle tests (disconnect, coalescing, concurrency, panic)"
```

---

### Task 4: Document MCP start/poll migration

**Files:**
- Modify: `docs/MIGRATION.md`

- [ ] **Step 1: Add the 1.9.0 migration section**

Add a new section to `docs/MIGRATION.md` documenting the MCP `index` tool change from request-blocking to start/poll, with before/after JSON examples, the `wait=true` backward compat behavior, and the structured `_meta.freshness` badge replacing free-text stale warnings.

- [ ] **Step 2: Commit**

```bash
git add docs/MIGRATION.md
git commit -m "docs: document MCP index start/poll migration in 1.9.0"
```

---

### Task 5: Convert JobStatus from String to type-safe enum

**Files:**
- Modify: `src/cli/index_job.rs`
- Modify: `src/cli/registry.rs`
- Modify: `src/cli/mcp/index_handler.rs`
- Modify: `tests/index_job_recovery.rs`
- Modify: `tests/cli_mcp_stdio_e2e.rs`

- [ ] **Step 1: Define the JobStatus enum**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus { Running, Complete, Failed }
```

Change `IndexJobSnapshot.status` from `String` to `JobStatus`.

- [ ] **Step 2: Update all status comparisons**

Replace all string literal comparisons with enum match variants throughout registry, handlers, and tests.

- [ ] **Step 3: Verify compile and tests**

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Expected: PASS.

Run: `cargo test --test index_job_recovery --test cli_mcp_stdio_e2e --all-features`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/cli/index_job.rs src/cli/registry.rs src/cli/mcp/index_handler.rs tests/index_job_recovery.rs tests/cli_mcp_stdio_e2e.rs
git commit -m "refactor: convert JobStatus from String to type-safe enum"
```

---

### Task 6: Add dead-worker detection to inference IPC

**Files:**
- Modify: `src/search/onnx/client.rs`
- Modify: `tests/onnx_worker_fallback.rs`

- [ ] **Step 1: Write failing dead-worker test**

Create a mock worker that accepts requests but never responds (dies after accepting). Assert the client detects the dead worker via the read error on the response channel, not via an arbitrary timeout.

```rust
#[test]
fn dead_worker_detected_via_read_error() {
    let mock = spawn_worker_that_dies_after_accept();
    let client = EmbeddingClient::with_daemon_socket(&mock.socket_path);
    let result = client.embed_attempt("test");
    assert!(result.is_err());
    // Error should indicate the worker died, not an arbitrary timeout
    assert!(matches!(result.unwrap_err(), ClientError::WorkerDied { .. }));
}
```

- [ ] **Step 2: Detect dead worker via read error propagation**

When the reader thread detects EOF (the worker process closed the socket), it should propagate the error through the response channel immediately. The `rx.recv()` in `send_and_receive` will then return `RecvError` (not timeout), which maps to `WorkerDied`.

If the worker process dies but the socket FD remains open (zombie descriptor), the read will eventually return EOF when the kernel reclaims the socket. No arbitrary timeout is needed for this path.

For the case where the worker is alive but genuinely unresponsive (not dead), the reader thread's socket read will eventually fail with `EPIPE` or `ECONNRESET` when the worker's buffer overflows or the OS kills the process.

- [ ] **Step 3: Verify dead-worker detection**

Run: `cargo test --test onnx_worker_fallback dead_worker --all-features`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/search/onnx/client.rs tests/onnx_worker_fallback.rs
git commit -m "fix: detect dead worker via read error propagation"
```

---

## Notepad recommendation traceability

| Recommendation | Implemented by | Proof |
|---|---|---|
| Eliminate daemon slowness root causes (not mask with timeout) | Task 1 | Python-free discovery, OnceLock config caching, connection reuse, MIGraphX warmup |
| Parser fixture coverage | Task 2 | `askpass.rs`, `nested_cli.rs`, `flows.rs` with kind/qualification/method assertions |
| Index-job ownership guarantees are testable | Task 3 | Disconnect survival, coalescing, concurrent first-load, panic recovery tests |
| MCP migration documentation | Task 4 | `docs/MIGRATION.md` documents start/poll change |
| Type-safe job status | Task 5 | `JobStatus` enum replaces string comparisons |
| Dead-worker inference detection | Task 6 | `WorkerDied` via read error, not arbitrary timeout |

## Explicitly rejected designs

- **Adding arbitrary timeouts to `python_one_line()`:** Masks the real issue; Python should not be spawned at all when filesystem inspection works.
- **Adding arbitrary init watchdog to daemon:** Masks issues; the init path should be fast enough to not need one.
- **Increasing `DAEMON_READY_MAX_WAIT`:** Hides the root cause; startup should be inherently fast.
- **Changing any environment variables:** The user's configuration is correct; code must adapt.
- **Using `tokio::time::timeout` around indexing work:** Violates non-negotiable decision #1 from the original plan.

## Final stop condition

The follow-up is complete only when:
1. All workspace tests pass including new parser fixture tests and index-job lifecycle tests.
2. ONNX daemon startup completes in seconds, not minutes, on a system with warm MIGraphX cache.
3. Python subprocess is never spawned for ORT discovery when the library can be found via env var, config, or filesystem scan.
4. Config file is parsed once per process lifetime, not per poll iteration.
5. Health polling reuses a single connection instead of reconnecting each iteration.
6. Inference IPC detects dead workers via read errors, not arbitrary timeouts.
7. `docs/MIGRATION.md` documents the 1.9.0 MCP start/poll change.
8. `IndexJobSnapshot.status` is `JobStatus`, not `String`.
9. `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --all-features` all pass.
