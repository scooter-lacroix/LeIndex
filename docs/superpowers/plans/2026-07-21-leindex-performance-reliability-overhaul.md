# LeIndex Performance and Reliability Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make live LeIndex operations predictably fast while preserving TF-IDF, neural, and PDG quality, replacing request-coupled cancellation with owned resumable indexing, and implementing every improvement recorded in `.omx/notepad.md`.

**Architecture:** Separate live filesystem/git/catalog reads from heavyweight resident state; route exact queries without neural query embedding; make indexing a single owned per-project job that publishes the mandatory TF-IDF/lexical and PDG layers before actively evaluating configured neural vectors; and make the embedding daemon observable before model initialization. PDG and TF-IDF are core result layers, not addons: every applicable symbol/file/status/context result exposes their status and uses resident PDG relations plus TF-IDF retrieval, while ONNX-enabled semantic paths use the neural companion whenever the provider reaches Ready. Reuse the existing SQLite schema, mmap format, registry slots, Tree-sitter parsers, Tokio primitives, and tracing stack—no new dependency, actor framework, or remote service.

**Tech Stack:** Rust 1.75, Tokio, Axum/MCP JSON-RPC, rusqlite/WAL, Tree-sitter, petgraph, memmap2, ONNX Runtime worker over Unix sockets, Git porcelain v2, Criterion, serde/bincode.

## Execution status (working tree, 2026-07-22)

The working tree contains the first implementation pass for the causal
fast paths and core enrichment contract: phase counters/timings, live Git and
catalog reads, deterministic exact routing, health snapshots, owned index jobs,
state-driven neural startup/fallback, mmap residency, Rust symbol repair, hybrid
chunks, session-scoped freshness advisories, and PDG flow edges (including
cross-file argument/state propagation) are implemented and covered by tests.

This continuation adds atomic scan/parse/PDG/lexical/neural checkpoint artifacts,
validated scan/parse/PDG/lexical/neural reuse after restart, persisted job
snapshots, generation snapshots with a validated `CURRENT` selector/rollback, a deterministic
20k-file/20k-ignored-file JSON performance gate (with small env overrides for
smoke runs), and one bounded PDG module node per Git submodule commit. The
lexical/PDG generation now publishes before neural work; durable artifact errors
are terminal and the registry poller mirrors persisted phase health. The six named
phase methods are now the live indexing path: each phase owns its checkpoint and
failure seam, and publication is explicit between core lexical/PDG work and neural
enrichment. Parse resume now verifies per-file BLAKE3/path identity, PDG resume
verifies its scan-hash metadata, and core snapshots exclude stale neural rows. The
default 20k benchmark run now passes; owned-job neural status reads the active
generation rather than a stale mutable-root file. The
parse checkpoint writer now groups per-file records into deterministic two-hex
hash buckets, preserving per-file hash/path validation while reducing durable
sync/rename work from one operation per source file to at most 256 bucket
artifacts. Owned-job polling also reloads the resident handle from `CURRENT` at
the core publication seam, so registry-backed tools see PDG/TF-IDF before neural
enrichment completes. The synchronous registry indexing path uses the same
publication monitor and performs one last `CURRENT`-generation refresh on a
post-core neural/task failure, so a failed request cannot hide a durable core
generation from resident readers; the failure is recorded as `last_failure`/
`last_failure_phase` without downgrading the published core health status.
Stale catalog symbol reads now suppress persisted PDG
relations and report `pdg_status="stale"`; dependency requests hydrate only a
fresh active generation and report `partial` on hydration failure. The
20k/20k two-sample smoke also passes
after fixing three measured cliffs: catalog misses no longer `git grep` the
unchanged tree when the generation `HEAD` tree matches, live candidate paths
honor the shared skip-directory policy (including `.leindex`), and catalog
connections reuse a bounded serialized SQLite page cache. Report
`target/leindex-performance-20k-smoke-final3.json`: exact-symbol p95 22.984 ms,
scoped exact-text p95 13.826 ms, Git-status warm/cold p95 15.319/14.563 ms,
TF-IDF semantic p95 74.434 ms, exact RSS delta +233 KiB, and zero exact
hydration/PDG/neural requests. Do not
“solve” those by reintroducing request timeouts or by weakening the PDG/TF-IDF
core path.
`cargo test --workspace --all-features -- --test-threads=1` passes the full
workspace, including all ten memcheck
integration tests (two product tests remain ignored). The memcheck
schema/phase tests use an empty temporary baseline directory so normal host RSS
variance cannot turn report-shape checks into false regressions; dedicated
diff-logic tests and direct release memcheck still enforce committed baselines
and absolute ceilings. CPU-tagged evidence from the earlier verification pass
is discarded; provider validation inherits the configured/default provider and
must not force CPU to hide a ROCm/MIGraphX problem. The implementation still
reports neural readiness and falls back to core TF-IDF/PDG independently.
`target/leindex-performance.json` records 20,000 tracked plus 20,000 ignored
files, 100 warm/10 cold samples, exact-symbol p95 25.061 ms, exact-text p95
16.593 ms, Git-status warm/cold p95 15.600/15.424 ms, and semantic-hybrid p95
202.713 ms. Exact/live paths keep hydration, PDG, and neural deltas at zero;
configured semantic work records exactly one neural request and combines it
with TF-IDF/PDG scoring. The latest default run remains within those limits
and adds watcher publication plus current-generation hydration regression
tests. A lexical fault-injection
integration test now proves `CURRENT` advances to a usable PDG+TF-IDF
generation before restart and resumes into the next generation. The memcheck
worker-active request now uses the registered `leindex.search` tool and forces
the semantic route, so it exercises the configured hybrid path rather than a
TF-IDF-only fallback. The cold auto worker is resolved from the active Cargo
profile, started, and awaited through `Initializing -> Ready` before the
request returns; the latest provider-default release baseline records
`neural_ms=12,267`, worker RSS `4,928,636 KiB`, and combined RSS
`4,949,500 KiB`. The direct release memcheck then passed all nine phases under
the provider-aware 8 GiB ceiling (latest direct peak combined RSS
`3,154,808 KiB`). The ignored cold-start integration test also returns a
non-zero 384-dimensional neural vector and reaches `ready`. The
provider-default release probe (`cargo test --release -p leindex-embed
--test migraphx_dynamic --features onnx`) reports
`is_migraphx_compiled_in()=true` and successfully builds a session with the
MIGraphX execution provider; no CPU provider override is used.
The cold-daemon transport race is also closed: a socket accepted during
`Initializing` is discarded before inference, readiness is polled through an
unbounded health response, and a fresh blocking socket is opened only after
`Ready`; this removes the former first-request `worker is ready` retry and the
250 ms cold-health `EAGAIN` degradation without adding a model/inference
timeout. A fresh release `auto` proof now returns the same TF-IDF/PDG-enriched
semantic results with `neural_ms=8,523` and no retry/fallback warning.
The forced release `auto` index proof also produced a separate
`.leindex/neural_embeddings.bin` (1,161,824 bytes for 274 indexed nodes)
alongside the 881,248-byte TF-IDF mmap; the worker startup report again said
`provider=migraphx status=available`.
The reduced benchmark smoke report uses
`LEINDEX_PERF_SOURCE_FILES`, `LEINDEX_PERF_IGNORED_FILES`, and sample-count
overrides; CI defaults remain 20,000/20,000 and 100/10.

---

## Non-negotiable decisions

1. **A timeout is not a fix.** Remove the MCP request timeout around correctness-critical indexing and persistence. `max_latency_ms` is a response/context-expansion budget only: it may stop optional formatting or bounded PDG traversal and return `partial`, but it must not skip configured neural startup/inference, abandon or cancel an index build, database transaction, repair, or snapshot publication.
2. **Live facts stay live.** `git_status`, `read_file`, and exact text reads operate on the filesystem/Git without loading `LeIndex`, PDG, search snapshots, TF-IDF vectors, or neural vectors.
3. **Exact means exact.** Exact identifier and symbol queries do not call neural or TF-IDF query embedding. A deterministic query classifier chooses exact, lexical, semantic, or deep-PDG work before any expensive operation starts.
4. **One project, one index job.** The registry owns indexing beyond the lifetime of any MCP request. Concurrent starts coalesce, disconnects do not orphan work, and completion swaps the in-memory generation exactly once.
5. **Resume only from reusable artifacts.** A phase is checkpointed only after its output is atomically persisted and can be loaded after process restart. A progress marker without a reusable artifact does not count as resumability.
6. **Publish useful work early.** Scan/parse/PDG/TF-IDF publication is the core searchable generation and must be usable while the configured neural worker initializes. The same owned job then actively evaluates neural vectors and publishes the hybrid generation when the provider reaches `Ready`; an explicit `Failed`/`Absent` state preserves the core TF-IDF/PDG generation and remains observable rather than silently pretending neural work ran.
7. **Do not rewrite working Rust recursion.** Nested `mod` traversal already exists. Fix impl qualification, explicit symbol kinds, enum variants, canonical path use, and stale-generation publication; prove the result with realistic fixtures.
8. **No hard-coded Rusty Stack vocabulary.** Request-scoped task context supplies domain terms and exact identifiers. It is bounded, scored, and never persisted by default.
9. **Git is the ignore oracle in Git worktrees.** Enumerate tracked and non-ignored untracked source files using Git; do not descend into submodules, nested repositories, ignored build trees, or generated output unless explicitly included. Retain the existing WalkDir path for non-Git projects.
10. **Release as 1.9.0.** The MCP index response and metadata contracts gain fields and MCP indexing becomes start/poll instead of request-blocking. Preserve CLI blocking behavior. This is a documented minor release with all version surfaces updated together.

## Evidence and root-cause hierarchy

The implementation worker must preserve these findings as regression tests rather than treating the observed latency as a generic “large repository” problem.

| Priority | Evidence | Root cause | Primary code |
|---|---|---|---|
| P0 | A two-source-file fixture took 1,200.192 seconds: one 600-second neural wait, one retry, then a second 600-second wait before TF-IDF fallback; the worker logged only “starting” and had no socket FD | `WorkerRuntime::new` loads ORT/model before `UnixListener::bind`; the spawning client holds the daemon flock and worker mutex while polling for the absent socket | `crates/leindex-embed/src/worker_main.rs`, `crates/leindex-embed/src/runtime.rs`, `src/search/onnx/client.rs` |
| P0 | The MCP index request ended at its cap while `spawn_blocking` continued and later persisted data | Request timeout drops the future before registry swap; blocking work is not cancelled, so disk and memory generations diverge and retries race | `src/cli/mcp/server.rs`, `src/cli/registry.rs`, `src/cli/mcp/index_handler.rs` |
| P0 | Rusty Stack `git_status` took 27.582s and diagnostics 23.563s while native Git was effectively instant; RSS reached about 8 GB | `get_or_create` eagerly loads the entire PDG/search state before handlers that do not need it | `src/cli/registry.rs`, `src/cli/leindex/indexing.rs` |
| P0 | All same-project operations queued; `git_status` held the guard across Git subprocesses and repeated graph traversals | `ProjectRwLock` is a Tokio `Mutex<LeIndex>` because rusqlite makes the aggregate `!Sync`; “read” and “write” guards are the same exclusive lock | `src/cli/registry.rs`, `src/cli/mcp/git_status_handler.rs` |
| P0 | `grep_symbols(mode=exact)` logged a 250 ms neural fallback before returning | The handler calls `index.search()` before branching on `mode` | `src/cli/mcp/grep_symbols_handler.rs`, `src/cli/leindex/query.rs` |
| P1 | Snapshot hydration expanded mmap entries into owned `Vec<f32>` values and rebuilt a heap vector index | The mmap persistence path exists, but `restore_from_search_snapshot` calls `entries()` and duplicates every vector on heap; current residency tests do not assert the engine dropped the heap mirror | `src/search/search.rs`, `src/search/vector.rs`, `tests/search_residency.rs` |
| P1 | 16,424 files, 338,065 PDG nodes, 422,363 edges, and a 1.446 GB DB included ignored build trees under `Fork/llama.cpp` build directories | Generic WalkDir scanning ignores Git’s authoritative exclude/submodule boundaries | `src/cli/index_builder.rs`, `src/cli/index_freshness.rs` |
| P1 | `read_symbol` missed obvious symbols with a relative `file_path`; `file_summary` validated a canonical path then queried with the raw path | Indexed lookups use inconsistent path keys | `src/cli/mcp/helpers.rs`, `src/cli/mcp/read_symbol_handler.rs`, `src/cli/mcp/file_summary_handler.rs` |
| P1 | After the two-file build finally completed, the same 2,577-line `main.rs` exposed 179 symbols in 40 ms and `Askpass` read in 17 ms, but `Askpass` was typed as `function` | The earlier one-symbol result was stale/incompletely published, not failed nested recursion; current Rust gaps are node classification, impl type qualification, associated methods without `self`, and enum variants | `src/parse/rust.rs`, `src/graph/extraction.rs`, registry publication |
| P1 | Every response repeats a warning and `is_stale_fast` can stat thousands of files or walk manifests | Freshness is recomputed as one Boolean on the response hot path instead of using persisted generation health | `src/cli/index_freshness.rs`, `src/cli/mcp/helpers.rs` |
| P1 | The first 20k/20k run reported exact-symbol p95 1,381 ms (RSS +160 MiB) while exact text was 407 ms; the exact pattern was absent from the indexed catalog | A catalog miss fell back to `git grep` over the entire unchanged tree; the benchmark then exposed that the fallback also treated LeIndex's own untracked `.leindex` artifacts as live source candidates | `src/cli/mcp/grep_symbols_handler.rs`, `src/cli/git.rs`, `benches/mcp_tool_latency.rs` |
| P1 | After Git candidate narrowing, exact misses still measured about 526 ms on 20k files | Each request reopened or rebuilt the read-only SQLite catalog/page cache; a bounded per-path serialized connection pool removes the repeated open cost without sharing `rusqlite::Connection` across async tasks | `src/storage/catalog.rs` |
| P2 | A 20k-file index spent several minutes in parse/checkpoint persistence before request measurements began | Per-file durable writes multiplied sync/rename overhead; deterministic two-hex parsed-artifact buckets now preserve atomic hash/path validation while reducing the phase to at most 256 durable artifacts | `src/cli/index_job.rs`, `src/cli/leindex/indexing.rs` |
| P2 | PDG-aware Git status calls `forward_impact` for every node of every changed file | Duplicate roots repeated graph traversal and held the project mutex; one multi-source BFS now shares a visited set | `src/cli/mcp/git_status_handler.rs`, `src/graph/pdg.rs` |

## Target request paths

```text
git-status/read-file/text-search(exact)
  -> canonical project root
  -> live Git/filesystem operation
  -> structured freshness badge from small index-state.json
  -> response (never hydrate PDG/search/neural)

read-symbol/file-summary/grep-symbols(exact)
  -> canonical project/file path
  -> read-only SQLite catalog query
  -> live Tree-sitter fallback on catalog miss/stale file
  -> resident PDG relations/status by default, within the enrichment budget (never hydrate on the hot path)
  -> response (never start neural worker)

search(semantic)/deep-analyze/context
  -> loaded immutable query generation
  -> mandatory TF-IDF candidate retrieval immediately
  -> start/await configured neural worker through Initializing -> Ready/Failed
  -> neural scoring when Ready; explicit Failed/Absent leaves the core result intact
  -> PDG expansion for deep/context operations and resident relation enrichment for symbol/file/status results
  -> response with component status/timings

leindex.index (MCP)
  -> registry-owned single-flight job
  -> immediate running/current response
  -> atomic reusable phase checkpoints
  -> publish PDG+lexical generation
  -> actively evaluate configured neural vectors after core TF-IDF+PDG publication
  -> publish hybrid generation, or publish core with explicit neural Failed/Absent status

leindex index (CLI)
  -> same registry/job implementation
  -> waits and renders progress until complete/failure
```

## Public contracts to implement exactly

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    Fresh,
    Partial,
    Stale,
    NotLoaded,
    Initializing,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexPhase {
    Scan,
    Parse,
    Pdg,
    Lexical,
    Neural,
    Persist,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexHealth {
    pub generation: u64,
    pub phase: IndexPhase,
    pub status: ComponentStatus,
    pub head_oid: Option<String>,
    pub tree_oid: Option<String>,
    pub indexed_file_count: usize,
    pub dirty_file_count: usize,
    pub changed_unindexed_count: usize,
    pub indexed_at_unix_ms: Option<u64>,
    pub last_failure_phase: Option<IndexPhase>,
    pub last_failure: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WorkBudget {
    pub max_latency_ms: u64,
    pub allow_partial: bool,
}

impl WorkBudget {
    pub fn elapsed(self, started: std::time::Instant) -> bool {
        self.allow_partial && started.elapsed().as_millis() >= self.max_latency_ms as u128
    }
}
```

`WorkBudget::elapsed` is checked only between optional enrichment units. It does not wrap a future, kill a process, stop a database transaction, or abandon index state. The notepad’s proposed `pdg_status="timeout"` is deliberately represented as `pdg_status="partial"` plus elapsed timing: the response budget ended enrichment cleanly; no operation was timed out or interrupted.

The MCP index result is additive and stable:

```json
{
  "job_id": "project-hash-42",
  "status": "running",
  "phase": "parse",
  "generation": 42,
  "completed_units": 183,
  "total_units": 920,
  "published": {
    "pdg": false,
    "lexical": false,
    "neural": false
  },
  "last_error": null
}
```

`force_reindex=true` starts a new generation only when no job is active. A repeated call returns the same `job_id`. `wait=true` is accepted for backward compatibility but is documented for CLI/interactive use; MCP schema defaults it to `false`.

## File responsibility map

**Create**

- `src/cli/live_project.rs` — canonical project/file resolution, read-only index-state loading, and live operations that must not instantiate `LeIndex`.
- `src/cli/git.rs` — one porcelain-v2 parser/runner used by status, scanning, freshness, and benchmark fixtures.
- `src/cli/index_job.rs` — `IndexJobState`, owned task lifecycle, atomic progress persistence, checkpoint validation, and status snapshots.
- `src/cli/mcp/request_meta.rs` — `WorkBudget`, component status, phase timings, and session/generation stale-warning policy.
- `src/storage/catalog.rs` — bounded, read-only point queries against existing `intel_nodes`/`intel_edges` tables using a per-generation serialized connection pool; no connection crosses async tasks without the pool mutex.
- `src/search/query_route.rs` — deterministic query classification and exact-identifier boosts.
- `tests/fixtures/rust/askpass.rs` — realistic `Askpass`, `new`, and `path` parser fixture.
- `tests/fixtures/rust/nested_cli.rs` — nested modules, impl blocks, and Clap-style enum variants.
- `tests/fixtures/rust/flows.rs` — sudo, registry/verification, Arch detection, and command-channel dataflow fixture.
- `tests/mcp_fast_paths.rs` — deterministic assertions that fast routes do not hydrate/search/embed.
- `tests/index_job_recovery.rs` — single-flight, disconnect, fault-injection, checkpoint, and publication tests.
- `tests/git_porcelain.rs` — NUL-safe status, rename, conflict, and submodule parsing tests.
- `benches/mcp_tool_latency.rs` — cold/warm/status/exact/concurrent latency and RSS benchmark.

**Modify**

- `src/cli/mod.rs` — export the new focused modules.
- `src/cli/registry.rs` — separate lightweight project identity from loaded `LeIndex`; add creation single-flight and owned index jobs using existing per-project slots.
- `src/cli/leindex/mod.rs` — expose storage-path resolution without eager storage/search initialization and use `IndexHealth`.
- `src/cli/leindex/indexing.rs` — split phases, checkpoint reusable outputs, publish lexical before neural, and report progress.
- `src/cli/index_builder.rs` — Git-aware scan, hybrid symbol/comment chunks, batch checkpoints, and zero-copy mmap restoration inputs.
- `src/cli/index_freshness.rs` — replace response-time full scans with generation health plus live Git deltas.
- `src/cli/mcp/server.rs` — remove tool-call timeout cancellation; add timing/session metadata.
- `src/cli/mcp/index_handler.rs` — start/poll owned job and expose `wait` only as an explicit option.
- `src/cli/mcp/git_status_handler.rs` — run live status first; enrich from an already-loaded PDG only.
- `src/cli/mcp/grep_symbols_handler.rs` — route before search; exact catalog path invokes zero embeddings.
- `src/cli/mcp/read_symbol_handler.rs` — canonical catalog lookup, live parser fallback, one-read source/line cache, `symbol_index_miss`.
- `src/cli/mcp/file_summary_handler.rs` — canonical path key and live parser fallback.
- `src/cli/mcp/helpers.rs` — central path resolver and structured `_meta`; remove repeated free-text stale warning.
- `src/cli/mcp/context_handler.rs`, `src/cli/mcp/deep_analyze_handler.rs` — optional task context, budgeted expansion, component status.
- `src/cli/leindex/query.rs` — state-driven neural startup/use, routed search, request-scoped context, hybrid-cache provenance, and no detached timeout threads.
- `src/search/search.rs`, `src/search/vector.rs` — actual mmap-backed vector variant and bounded delta overlay; no heap reconstruction on load.
- `src/search/onnx/client.rs` — daemon state handshake, state-driven readiness, blocking inference reconnect, spawn-lock release, PID ownership, and reaping.
- `crates/leindex-embed/src/worker_main.rs`, `crates/leindex-embed/src/runtime.rs`, `crates/leindex-embed/src/protocol.rs` — bind before initialization and expose initializing/ready/failed state.
- `src/parse/traits.rs`, `src/parse/rust.rs`, `src/graph/extraction.rs`, `src/graph/pdg.rs`, `src/storage/nodes.rs`, `src/storage/edges.rs`, `src/storage/pdg_store.rs`, `src/storage/schema.rs` — explicit symbol kinds, qualified Rust anchors, new flow edges, compatible migrations.
- `tests/search_residency.rs`, `tests/storage_reader_pool.rs`, `tests/cli_mcp_stdio_e2e.rs`, `tests/onnx_worker_fallback.rs` — turn existing claims into end-to-end invariants.
- `Cargo.toml` — add the new benchmark entry and bump both crate versions to 1.9.0; do not add dependencies.
- `crates/leindex-embed/Cargo.toml`, `install.sh`, `install.ps1`, `install_macos.sh`, `package.json`, `dashboard/package.json`, `pi/package.json`, `packages/npm-leindex-mcp/package.json`, `packages/pypi-leindex/pyproject.toml`, `packages/pypi-leindex/src/leindex/__init__.py`, `packages/npm-leindex-mcp/test.js` — release parity.
- `README.md`, `packages/pypi-leindex/README.md`, `packages/npm-leindex-mcp/README.md`, `INSTALLATION.md`, `INSTALLATION_RUST.md`, `RELEASE_NOTES.md`, `docs/MCP.md`, `docs/CLI.md`, `docs/MIGRATION.md`, `docs/AGENT_GUIDANCE.md`, `docs/NEURAL_SETUP.md`, `docs/PERFORMANCE_BENCHMARKS.md` — aligned behavior, health fields, config examples, and performance evidence.

---

### Task 1: Lock in causal regressions and phase timings

**Files:**
- Create: `tests/mcp_fast_paths.rs`
- Create: `benches/mcp_tool_latency.rs`
- Create: `src/cli/mcp/request_meta.rs`
- Modify: `src/cli/mcp/mod.rs`
- Modify: `src/cli/mcp/server.rs`
- Modify: `src/cli/mcp/handlers.rs`
- Modify: `Cargo.toml`

- [x] **Step 1: Write failing fast-path dependency tests**

Define test-only counters in `src/cli/mcp/request_meta.rs`; production increments are relaxed atomics and add effectively zero overhead:

```rust
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub static PROJECT_HYDRATIONS: AtomicU64 = AtomicU64::new(0);
pub static PDG_LOADS: AtomicU64 = AtomicU64::new(0);
pub static NEURAL_REQUESTS: AtomicU64 = AtomicU64::new(0);

pub fn reset_path_counters() {
    PROJECT_HYDRATIONS.store(0, Ordering::Relaxed);
    PDG_LOADS.store(0, Ordering::Relaxed);
    NEURAL_REQUESTS.store(0, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WorkBudget {
    pub max_latency_ms: u64,
    pub allow_partial: bool,
}

impl WorkBudget {
    pub fn elapsed(self, started: Instant) -> bool {
        self.allow_partial && started.elapsed().as_millis() >= self.max_latency_ms as u128
    }
}

#[derive(Debug, Default, Serialize)]
pub struct PhaseTimings {
    pub lock_wait_ms: u64,
    pub hydrate_ms: u64,
    pub scan_ms: u64,
    pub parse_ms: u64,
    pub git_ms: u64,
    pub catalog_ms: u64,
    pub pdg_ms: u64,
    pub lexical_ms: u64,
    pub neural_ms: u64,
    pub cache_read_ms: u64,
    pub cache_write_ms: u64,
    pub persist_ms: u64,
    pub handler_ms: u64,
    pub transport_queue_ms: u64,
    pub total_ms: u64,
}
```

In `tests/mcp_fast_paths.rs`, construct a Git tempdir with one Rust file and call handlers directly. Add these exact assertions:

```rust
assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
assert_eq!(PDG_LOADS.load(Ordering::Relaxed), 0);
assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);
```

Cover `git-status`, `read-file`, `text-search` with an exact pattern, and `grep-symbols` with `mode="exact"`. The tests must initially fail because the current handlers call `ProjectRegistry::get_or_create` and exact symbol grep calls `index.search`.

- [x] **Step 2: Run the tests and preserve the failure evidence**

Run: `cargo test --test mcp_fast_paths --all-features -- --nocapture`

Expected: FAIL; at least `PROJECT_HYDRATIONS` and `NEURAL_REQUESTS` are non-zero after instrumentation is wired into `create_and_insert`, `load_from_storage_inner`, `ensure_pdg_loaded`, and `EmbeddingClient::embed_attempt`.

- [x] **Step 3: Add monotonic phase timing without changing behavior**

Create a small local span helper in `request_meta.rs`:

```rust
pub fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}
```

At the current MCP dispatch boundary, record total/handler/transport-queue duration and attach `_meta.timings`. At each existing hot boundary, record lock wait, hydrate, scan, parse, Git, catalog, PDG, lexical, neural, cache read/write, and persist durations. Do not create a generic tracing framework; use `Instant` and the existing `tracing` macros.

- [x] **Step 4: Add the latency benchmark skeleton with real fixtures**

Register this benchmark in `Cargo.toml`:

```toml
[[bench]]
name = "mcp_tool_latency"
harness = false
required-features = ["cli"]
```

Benchmark cold and warm `git-status`, exact grep, file summary, semantic search, and status while indexing. Use Criterion’s existing dev dependency, `tempfile`, and Git CLI. Record RSS before/after via the existing `memory_report` helpers. Do not assert wall-clock limits inside unit tests; benchmarks print p50/p95 and CI later compares a checked-in threshold table.

- [x] **Step 5: Verify instrumentation tests and commit**

Run: `cargo test --test mcp_fast_paths --all-features -- --nocapture`

Expected: FAIL only on the intended non-zero dependency counters; no compilation failures.

Run: `cargo bench --bench mcp_tool_latency --no-run --all-features`

Expected: benchmark executable builds.

```bash
git add Cargo.toml src/cli/mcp/mod.rs src/cli/mcp/server.rs src/cli/mcp/handlers.rs src/cli/mcp/request_meta.rs tests/mcp_fast_paths.rs benches/mcp_tool_latency.rs
git commit -m "test: capture MCP latency root causes"
```

---

### Task 2: Centralize canonical paths and read-only catalog lookups

**Files:**
- Create: `src/cli/live_project.rs`
- Create: `src/storage/catalog.rs`
- Modify: `src/cli/mod.rs`
- Modify: `src/storage/mod.rs`
- Modify: `src/storage/schema.rs`
- Modify: `src/cli/leindex/mod.rs`
- Modify: `src/cli/mcp/helpers.rs`
- Modify: `src/cli/mcp/read_symbol_handler.rs`
- Modify: `src/cli/mcp/file_summary_handler.rs`
- Test: `tests/mcp_fast_paths.rs`

- [x] **Step 1: Write failing canonical-path tests**

Add a fixture indexed under an absolute canonical path, then query it three ways: absolute, project-relative, and a path containing `src/../src`. Assert all resolve to the same stored key and return the same symbol. Add a boundary test showing `../outside.rs` is rejected.

```rust
assert_eq!(absolute["symbol"], "Askpass");
assert_eq!(relative["symbol"], "Askpass");
assert_eq!(normalized["symbol"], "Askpass");
assert!(outside_error.message.contains("outside the project boundary"));
```

Run: `cargo test --test mcp_fast_paths canonical_path --all-features -- --nocapture`

Expected: FAIL because `read_symbol` passes the raw hint to `nodes_in_file` and `file_summary` discards the canonical value.

- [x] **Step 2: Implement the one canonical resolver used by every handler**

Create `src/cli/live_project.rs`:

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LiveProject {
    root: PathBuf,
    storage: PathBuf,
}

impl LiveProject {
    pub fn resolve(raw: &str) -> std::io::Result<Self> {
        let root = Path::new(raw).canonicalize()?;
        let storage = crate::cli::leindex::resolve_existing_storage_path(&root)
            .unwrap_or_else(|| root.join(".leindex"));
        Ok(Self { root, storage })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn storage(&self) -> &Path {
        &self.storage
    }

    pub fn file(&self, raw: &str) -> std::io::Result<PathBuf> {
        let candidate = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            self.root.join(raw)
        };
        let canonical = candidate.canonicalize()?;
        if canonical.starts_with(&self.root) {
            Ok(canonical)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{} is outside {}", canonical.display(), self.root.display()),
            ))
        }
    }
}
```

Refactor `LeIndex::resolve_storage_path` into a public(crate) pair: `resolve_existing_storage_path` performs no writes and checks the in-project, `LEINDEX_HOME`, XDG, and temp candidates in existing precedence; `resolve_storage_path` calls it first, then creates the first writable location. This preserves fallback storage compatibility without constructing `LeIndex`.

- [x] **Step 3: Reuse the existing SQLite catalog instead of hydrating PDG**

Create `src/storage/catalog.rs`. Keep a bounded process-local pool keyed by immutable active-generation DB path. Each value is `Arc<Mutex<rusqlite::Connection>>`; lookup closures run on `spawn_blocking`, lock only for the short SQL operation, and never expose a connection to async callers. Evict an arbitrary oldest key once the 16-path bound is reached. This preserves SQLite page-cache reuse without an unbounded global cache or cross-thread `rusqlite` access. Query existing indexed columns and add only the missing composite indexes in the schema migration:

```sql
CREATE INDEX IF NOT EXISTS idx_nodes_project_file_name
ON intel_nodes(project_id, file_path, symbol_name COLLATE NOCASE);

CREATE INDEX IF NOT EXISTS idx_nodes_project_qualified
ON intel_nodes(project_id, qualified_name COLLATE NOCASE);
```

Use this exact record:

```rust
#[derive(Debug, Clone)]
pub struct CatalogSymbol {
    pub node_id: String,
    pub symbol_name: String,
    pub qualified_name: String,
    pub file_path: PathBuf,
    pub language: String,
    pub node_type: String,
    pub complexity: u32,
    pub byte_range: (usize, usize),
}
```

`CatalogReader::open(db_path, canonical_project_path)` first reads `project_metadata.unique_project_id` by canonical path, then exposes `find_symbol(symbol, file)` and `symbols_in_file(file)` with parameterized SQL. Exact name/qualified name matches precede case-insensitive matches. Return at most 200 candidate rows so a corrupt or ambiguous catalog cannot cause unbounded response work.

For identifier-shaped exact misses, do not run a leading-wildcard catalog query or
scan the unchanged tree. Prove that the active generation is complete and its
stored `tree_oid` equals `git rev-parse HEAD^{tree}`; then inspect only live
edits. If proof fails (old generation, missing health, non-Git project), retain
the full Git-authoritative candidate path:

```rust
let candidates = if catalog_tree_current {
    git::changed_source_candidates(root)? // diff HEAD + non-ignored untracked
} else {
    git::source_candidates(root, pattern)? // git grep + untracked fallback
};
```

Both helpers reject nested repositories, gitlinks, and every `SKIP_DIRS`
component (especially `.leindex`) before the parser sees a path. The first
20k-file run is a regression fixture for this exact negative-query invariant.

Set `Storage::SCHEMA_VERSION` to 3 and add `migrate_v2_to_v3` that creates both composite indexes idempotently. Schema v3 is also the compatibility boundary for Task 12’s new persisted edge strings; an older 1.8.x binary must reject a v3 generation rather than silently dropping edges.

- [x] **Step 4: Switch symbol/file handlers to canonical catalog-first reads**

Resolve `LiveProject` before touching `ProjectRegistry`. Use the read-only catalog when the DB exists. Read each source file once, compute all line ranges from the cached bytes, and reuse that buffer for source/doc comments. Acquire a resident PDG snapshot for caller/callee enrichment by default; an explicit `include_dependencies=true` request may hydrate the active immutable generation, while stale source reads return `pdg_status="stale"` instead of inheriting old relations.

Set response metadata exactly:

```json
{
  "symbol_index_miss": false,
  "source_freshness": "live",
  "pdg_status": "not_loaded"
}
```

- [x] **Step 5: Run focused and storage tests**

Run: `cargo test --test mcp_fast_paths canonical_path --all-features -- --nocapture`

Expected: PASS for all three path forms and the outside-root rejection.

Run: `cargo test --all-features storage::schema`

Expected: PASS.

Run: `cargo test --all-features storage::pdg_store`

Expected: PASS, including migration from the previous schema and indexed catalog queries.

- [ ] **Step 6: Commit**

```bash
git add src/cli/live_project.rs src/cli/mod.rs src/storage/catalog.rs src/storage/mod.rs src/storage/schema.rs src/cli/leindex/mod.rs src/cli/mcp/helpers.rs src/cli/mcp/read_symbol_handler.rs src/cli/mcp/file_summary_handler.rs tests/mcp_fast_paths.rs
git commit -m "fix: route exact symbol reads through catalog"
```

---

### Task 3: Route exact queries before semantic work

**Files:**
- Create: `src/search/query_route.rs`
- Modify: `src/search/mod.rs`
- Modify: `src/cli/mcp/grep_symbols_handler.rs`
- Modify: `src/cli/leindex/query.rs`
- Modify: `src/cli/mcp/read_symbol_handler.rs`
- Test: `tests/mcp_fast_paths.rs`

- [x] **Step 1: Write failing routing tests**

Test quoted identifiers, Rust paths, snake/camel identifiers, natural-language questions, and explicit deep analysis:

```rust
assert_eq!(classify("Askpass::new", RequestedMode::Auto), QueryRoute::ExactSymbol);
assert_eq!(classify("run_installation", RequestedMode::Auto), QueryRoute::ExactSymbol);
assert_eq!(classify("registry_record", RequestedMode::Exact), QueryRoute::ExactText);
assert_eq!(classify("how are sudo credentials propagated", RequestedMode::Auto), QueryRoute::Semantic);
assert_eq!(classify("sudo credential flow", RequestedMode::Deep), QueryRoute::DeepPdg);
```

Add a mock neural backend that increments `NEURAL_REQUESTS`; exact grep and exact read must leave it at zero.

Run: `cargo test --test mcp_fast_paths exact_route --all-features -- --nocapture`

Expected: FAIL because `grep_symbols` calls `index.search` before checking `mode`.

- [x] **Step 2: Implement a deterministic classifier**

Create `src/search/query_route.rs` without a model or configurable rule engine:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedMode { Auto, Exact, Semantic, Deep }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryRoute { ExactSymbol, ExactText, Semantic, DeepPdg }

pub fn classify(query: &str, requested: RequestedMode) -> QueryRoute {
    match requested {
        RequestedMode::Exact => return QueryRoute::ExactText,
        RequestedMode::Semantic => return QueryRoute::Semantic,
        RequestedMode::Deep => return QueryRoute::DeepPdg,
        RequestedMode::Auto => {}
    }
    let q = query.trim_matches(|c| c == '`' || c == '"' || c == '\'');
    let identifier = !q.is_empty()
        && q.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.'))
        && !q.chars().any(char::is_whitespace);
    if identifier { QueryRoute::ExactSymbol } else { QueryRoute::Semantic }
}
```

- [x] **Step 3: Move the mode branch ahead of search**

In `grep_symbols_handler.rs`, compute `QueryRoute` before obtaining semantic state. Exact mode calls `CatalogReader`; semantic mode calls the search engine; deep mode remains exclusive to deep-analyze/context. Remove the pre-branch `index.search(&pattern, candidate_limit, None)` call entirely.

For exact matches, rank in this order: case-sensitive full qualified name, case-sensitive short name, case-insensitive full name, substring. Preserve scope/type filters and pagination.

- [x] **Step 4: Replace detached query timeout threads with readiness checks**

Change `generate_query_neural_embedding` to:

```rust
pub fn generate_query_neural_embedding(&self, query: &str) -> Option<Vec<f32>> {
    let embedder = self.embedder.as_ref()?;
    embedder.embed_neural_blocking(query).and_then(Result::ok)
}
```

Delete `DEFAULT_QUERY_EMBED_TIMEOUT_MS`, `QUERY_EMBED_TIMEOUT_ENV`, `query_neural_timeout`, the spawned detached thread, and its `recv_timeout`. Worker readiness is state-driven: a configured semantic request starts the cold worker and waits for `Ready` or terminal `Failed`/`Absent`; there is no elapsed-time gate that silently suppresses neural scoring.

- [x] **Step 5: Verify route invariants and commit**

Run: `cargo test --test mcp_fast_paths exact_route --all-features -- --nocapture`

Expected: PASS; exact calls report zero neural requests and zero search hydration.

Run: `cargo test --all-features cli::mcp::grep_symbols_handler`

Expected: PASS.

Run: `cargo test --all-features cli::leindex::query`

Expected: PASS.

Run: `cargo test --all-features search::query_route`

Expected: PASS.

```bash
git add src/search/query_route.rs src/search/mod.rs src/cli/mcp/grep_symbols_handler.rs src/cli/leindex/query.rs src/cli/mcp/read_symbol_handler.rs tests/mcp_fast_paths.rs
git commit -m "perf: bypass embeddings for exact queries"
```

---

### Task 4: Make Git status a live fast path with core resident-PDG enrichment

**Files:**
- Create: `src/cli/git.rs`
- Create: `tests/git_porcelain.rs`
- Modify: `src/cli/mod.rs`
- Modify: `src/cli/mcp/git_status_handler.rs`
- Modify: `src/cli/mcp/request_meta.rs`
- Test: `tests/mcp_fast_paths.rs`

- [x] **Step 1: Write porcelain-v2 parser tests**

Use NUL-delimited byte fixtures for ordinary changes, staged+unstaged changes, rename pairs, unmerged conflicts, paths containing spaces/tabs, branch headers, and submodule states. Assert categories are distinct:

```rust
assert_eq!(status.modified, vec![PathBuf::from("src/lib.rs")]);
assert_eq!(status.staged, vec![PathBuf::from("src/main.rs")]);
assert_eq!(status.untracked, vec![PathBuf::from("notes.txt")]);
assert_eq!(status.submodules[0].path, PathBuf::from("vendor/engine"));
assert_eq!(status.renames[0].from, PathBuf::from("src/old.rs"));
assert_eq!(status.renames[0].to, PathBuf::from("src/new.rs"));
assert_eq!(status.branch.as_deref(), Some("feature/perf"));
```

Run: `cargo test --test git_porcelain --all-features -- --nocapture`

Expected: FAIL because the parser does not exist and the current handler parses line-oriented porcelain v1.

- [x] **Step 2: Implement one Git invocation and byte parser**

Run exactly:

```rust
Command::new("git")
    .args(["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all"])
    .current_dir(root)
    .output()
```

Define `GitStatus` with `modified`, `staged`, `untracked`, `conflicted`, `renames`, `submodules`, `branch`, and `head_oid`. Parse record types `#`, `1`, `2`, `u`, `?`, and `!` from bytes; record type `2` consumes the next NUL field as the original rename path. Never split paths on whitespace or `->`.

- [x] **Step 3: Return live status before touching registry state**

`GitStatusHandler::execute` resolves `LiveProject`, runs Git in `spawn_blocking`, and builds the base response without `get_or_create`. Add schema fields:

```json
{
  "max_latency_ms": {"type":"integer","default":150},
  "allow_partial": {"type":"boolean","default":true},
  "enrich_pdg": {"type":"boolean","default":true},
  "scope": {"type":"string"}
}
```

The live Git result is returned first and carries a truthful PDG component status. If `registry.try_get_loaded(project_root)` finds a resident generation, PDG enrichment is attempted by default (subject to the response budget); if no generation is resident, return `pdg_status="not_loaded"` without hydration. `not_loaded` describes current component residency, not an optional feature. Do not call `get_or_load`, `get_or_create`, `ensure_pdg_loaded`, or `load_from_storage` from the live status path.

- [x] **Step 4: Deduplicate and budget PDG enrichment**

Collect all changed-file node IDs into one `HashSet<NodeId>`. Add `ProgramDependenceGraph::forward_impact_multi_source(&HashSet<NodeId>, &TraversalConfig)` using one BFS visited set. After each changed file’s symbol list, check `budget.elapsed(started)`; if exhausted, return accumulated data with `pdg_status="partial"`. If the generation is stale, return `pdg_status="stale"`. A PDG error returns the live Git result with `pdg_status="failed"` and an error string.

Safe-to-stage candidates are advisory only. Include tracked source files inside optional `scope`; exclude untracked files, conflicted paths, deleted paths, submodules, `.leindex`, and paths outside scope. Never run `git add`.

- [x] **Step 5: Verify fast status and parsing**

Run: `cargo test --test git_porcelain --all-features -- --nocapture`

Expected: PASS for all record types.

Run: `cargo test --test mcp_fast_paths git_status --all-features -- --nocapture`

Expected: PASS with all hydration/PDG/neural counters at zero when no generation is resident.

- [ ] **Step 6: Commit**

```bash
git add src/cli/git.rs src/cli/mod.rs src/cli/mcp/git_status_handler.rs src/cli/mcp/request_meta.rs src/graph/pdg.rs tests/git_porcelain.rs tests/mcp_fast_paths.rs
git commit -m "perf: split live git status from PDG enrichment"
```

---

### Task 5: Replace broad scans and Boolean freshness with Git-aware inventory health

**Files:**
- Modify: `src/cli/git.rs`
- Modify: `src/cli/index_builder.rs`
- Modify: `src/cli/index_freshness.rs`
- Modify: `src/cli/leindex/types.rs`
- Modify: `src/cli/leindex/mod.rs`
- Modify: `src/cli/mcp/helpers.rs`
- Modify: `src/cli/mcp/diagnostics_handler.rs`
- Modify: `src/cli/config.rs`
- Test: `tests/git_porcelain.rs`
- Test: `tests/mcp_fast_paths.rs`

- [x] **Step 1: Write failing inventory-boundary tests**

Create a Git fixture containing tracked Rust files, ignored `target/` and `build-local/` trees, a non-ignored untracked source file, a nested Git repository, and a registered submodule. Assert the inventory includes tracked and non-ignored untracked source, but does not include any descendant of ignored directories, nested repositories, or gitlink mode `160000` paths.

```rust
assert!(paths.contains(&root.join("src/lib.rs")));
assert!(paths.contains(&root.join("src/new.rs")));
assert!(!paths.iter().any(|p| p.starts_with(root.join("target"))));
assert!(!paths.iter().any(|p| p.starts_with(root.join("Fork/llama.cpp/build-local"))));
assert!(!paths.iter().any(|p| p.starts_with(root.join("vendor/submodule"))));
assert!(!paths.iter().any(|p| p.starts_with(root.join("scratch/nested-repo"))));
```

Run: `cargo test --test git_porcelain inventory --all-features -- --nocapture`

Expected: FAIL because `scan_project_files` uses WalkDir and does not consult Git’s ignore or gitlink data.

- [x] **Step 2: Implement Git-aware source inventory**

For Git worktrees, obtain candidates with:

```bash
git ls-files -z --cached --others --exclude-standard
git ls-files -z --stage
```

Parse the second command into a set of mode-`160000` gitlink roots. Build a second set of nested-repository roots by checking each unique candidate ancestor for a `.git` file or directory, stopping at the project root. Filter candidates by source extension, `ProjectConfig::should_exclude`, size/count/total-byte limits, gitlink roots, and nested-repository roots. Sort canonical paths before hashing so scan checkpoints are deterministic.

For non-Git projects, retain the current WalkDir implementation and `SKIP_DIRS`. Rename the current function `scan_non_git_project_files`; `scan_project_files` chooses Git inventory when `git rev-parse --is-inside-work-tree` succeeds.

Do not add the `ignore` crate: Git already implements `.gitignore`, `.git/info/exclude`, global excludes, submodule boundaries, and nested-worktree semantics.

- [x] **Step 3: Persist structured generation health**

Add the `ComponentStatus`, `IndexPhase`, and `IndexHealth` contracts from the plan header to `src/cli/leindex/types.rs`, and re-export them from `src/cli/leindex/mod.rs` beside `IndexStats`. Persist `.leindex/index-state.json` with a temporary sibling plus rename:

```rust
pub fn save_health(path: &Path, health: &IndexHealth) -> anyhow::Result<()> {
    let target = path.join("index-state.json");
    let temp = path.join("index-state.json.next");
    let bytes = serde_json::to_vec_pretty(health)?;
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, &target)?;
    Ok(())
}
```

On Windows, remove an existing `index-state.json.previous`, rename target to previous, rename next to target, then remove previous. Never truncate the current state file in place.

Populate `head_oid` with `git rev-parse HEAD`, `tree_oid` with `git rev-parse HEAD^{tree}`, `dirty_file_count` from porcelain v2, `indexed_file_count` from SQLite, `changed_unindexed_count` by comparing dirty/untracked paths to `indexed_files`, and `indexed_at_unix_ms` from the last completed generation.

- [x] **Step 4: Make freshness a cheap snapshot plus live delta**

Replace `wrap_with_meta`’s call to `is_stale_fast` with a read of the small state JSON and the already-collected live Git status when the handler has one. `is_stale_fast` remains as a compatibility method for CLI callers, but MCP response paths no longer stat all indexed files or walk for manifests.

Return this response shape:

```json
{
  "_meta": {
    "freshness": {
      "generation": 42,
      "status": "stale",
      "head_oid": "0123456789abcdef",
      "tree_oid": "fedcba9876543210",
      "indexed_file_count": 920,
      "dirty_file_count": 4,
      "changed_unindexed_count": 1,
      "age_ms": 5500,
      "last_failure_phase": null
    }
  }
}
```

For a tool that names one or more files, add `_meta.freshness.queried_files` entries containing canonical `path`, `status`, `indexed_hash`, and `live_hash`. Exact live text/source is `live` even if semantic state is stale; PDG/search-derived fields keep the generation status. This explicitly distinguishes “usable live exact result” from “possibly outdated semantic result.”

The status is `fresh` when HEAD/tree match and no relevant source/manifest path is dirty; `stale` when live changes affect indexed inputs; `partial` while a newer generation is publishing; `failed` only when the last build failed and no newer completed generation exists.

- [x] **Step 5: Verify inventory, freshness cost, and diagnostics**

Run: `cargo test --test git_porcelain inventory --all-features -- --nocapture`

Expected: PASS.

Run: `cargo test --test mcp_fast_paths freshness --all-features -- --nocapture`

Expected: PASS; the test’s filesystem metadata counter shows no full indexed-file stat loop and no WalkDir traversal.

Run: `cargo test --all-features cli::index_freshness`

Expected: PASS.

Run: `cargo test --all-features cli::mcp::diagnostics_handler`

Expected: PASS with structured health fields.

- [ ] **Step 6: Commit**

```bash
git add src/cli/git.rs src/cli/index_builder.rs src/cli/index_freshness.rs src/cli/leindex/types.rs src/cli/leindex/mod.rs src/cli/mcp/helpers.rs src/cli/mcp/diagnostics_handler.rs src/cli/config.rs tests/git_porcelain.rs tests/mcp_fast_paths.rs
git commit -m "perf: use git-aware inventory and health snapshots"
```

---

### Task 6: Move indexing ownership from MCP requests into the registry

**Files:**
- Create: `src/cli/index_job.rs`
- Create: `tests/index_job_recovery.rs`
- Modify: `src/cli/mod.rs`
- Modify: `src/cli/registry.rs`
- Modify: `src/cli/mcp/index_handler.rs`
- Modify: `src/cli/mcp/server.rs`
- Modify: `src/cli/cli.rs`
- Modify: `tests/cli_mcp_stdio_e2e.rs`

- [x] **Step 1: Write failing ownership and coalescing tests**

Add an injectable phase hook used only by tests. Start indexing, drop the initiating request future after the scan hook, issue a second start, release the hook, and assert:

```rust
assert_eq!(first.job_id, second.job_id);
assert_eq!(build_counter.load(Ordering::SeqCst), 1);
assert_eq!(registry.index_status(&project).await.status, JobStatus::Complete);
assert_eq!(registry.loaded_generation(&project).await, Some(first.generation));
```

Add a concurrent first-load test with 20 callers and assert project creation/hydration occurs once. Add a failure test asserting the previous completed generation remains readable.

Run: `cargo test --test index_job_recovery ownership --all-features -- --nocapture`

Expected: FAIL because the current `spawn_blocking` result/swap belongs to the caller future.

- [x] **Step 2: Define the owned job state**

Create `src/cli/index_job.rs`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus { Running, Complete, Failed }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedComponents {
    pub pdg: bool,
    pub lexical: bool,
    pub neural: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexJobSnapshot {
    pub job_id: String,
    pub generation: u64,
    pub status: JobStatus,
    pub phase: crate::cli::leindex::IndexPhase,
    pub completed_units: usize,
    pub total_units: usize,
    pub published: PublishedComponents,
    pub last_error: Option<String>,
}

pub struct IndexJob {
    state: tokio::sync::watch::Sender<IndexJobSnapshot>,
}

impl IndexJob {
    pub fn snapshot(&self) -> IndexJobSnapshot {
        self.state.borrow().clone()
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<IndexJobSnapshot> {
        self.state.subscribe()
    }
}
```

The registry keeps `jobs: Mutex<HashMap<PathBuf, Arc<IndexJob>>>` and, for this task, `creating: Mutex<HashMap<PathBuf, Arc<tokio::sync::OnceCell<ProjectHandle>>>>>`. `ProjectHandle` is the existing `Arc<ProjectRwLock>` type until Task 9 replaces its internals. Reuse the existing `index_slots` lock as the single-flight phase lock; do not add a second per-project indexing mutex.

- [x] **Step 3: Spawn work from an `Arc<ProjectRegistry>` and own the final swap**

Change the start signature to:

```rust
pub async fn start_index(
    self: &Arc<Self>,
    project_path: Option<&str>,
    force: bool,
) -> Result<IndexJobSnapshot, JsonRpcError>
```

Under the jobs mutex, return the existing running job or allocate the next generation and insert its state. Then `tokio::spawn` one owner task. The task calls `spawn_blocking` for synchronous phases, updates the watch sender between phases, publishes via a registry method, records failure without deleting the previous generation, and leaves the final snapshot queryable. No request awaits the `JoinHandle`; the runtime owns it.

Use `catch_unwind(AssertUnwindSafe(|| build_index(path_for_blocking, force, progress_tx)))` inside `spawn_blocking` so a panic becomes a failed job state rather than silently losing progress. Define `build_index` in `index_job.rs` as the sole synchronous runner for the current monolithic build:

```rust
pub struct OwnedBuildResult { pub index: LeIndex, pub stats: IndexStats }

fn build_index(path: PathBuf, force: bool, progress: watch::Sender<IndexJobSnapshot>)
    -> Result<OwnedBuildResult, JsonRpcError>;
```

The owner task swaps `OwnedBuildResult.index` into the existing handle. Task 8 replaces this runner with reusable phase methods and `PublishedGeneration`; request ownership is solved here before pipeline restructuring.

- [x] **Step 4: Remove MCP tool-call cancellation**

Delete `DEFAULT_REQUEST_TIMEOUT_SECS`, `DEFAULT_INDEX_TIMEOUT_SECS`, `LONG_RUNNING_TOOL_TIMEOUTS`, `INDEX_TIMEOUT_ENV`, `index_timeout_secs`, `long_running_tool_timeout_secs`, and the `tokio::time::timeout` wrapper around `tools/call` in `src/cli/mcp/server.rs`. Transport disconnects may drop response delivery, but owned work continues and remains queryable.

Do not remove bounded socket read timeouts needed to detect a dead peer at the transport layer; Task 7 replaces neural operation waiting with state responses.

- [x] **Step 5: Change MCP index to start/poll and preserve CLI waiting**

Add `wait` and `job_id` to `IndexHandler::argument_schema`. With `wait=false` (MCP default), return `IndexJobSnapshot` immediately. With `job_id`, return that job’s current snapshot without starting work. With `wait=true`, subscribe and await `Complete` or `Failed` without a deadline.

The CLI `leindex index` calls `registry.start_index(Some(project_path.as_str()), force)` then subscribes and renders phase/progress until the watch channel reaches a terminal state. This preserves the user-facing blocking CLI contract without coupling core work to an MCP request.

- [x] **Step 6: Verify disconnect, coalescing, and server responsiveness**

Run: `cargo test --test index_job_recovery ownership --all-features -- --nocapture`

Expected: PASS; exactly one build and one generation swap.

Run: `cargo test --test cli_mcp_stdio_e2e index --all-features -- --nocapture`

Expected: PASS; the first MCP response is `running`, polling reaches `complete`, and another tool call succeeds while indexing runs.

- [ ] **Step 7: Commit**

```bash
git add src/cli/index_job.rs src/cli/mod.rs src/cli/registry.rs src/cli/mcp/index_handler.rs src/cli/mcp/server.rs src/cli/cli.rs tests/index_job_recovery.rs tests/cli_mcp_stdio_e2e.rs
git commit -m "fix: make indexing an owned single-flight job"
```

---

### Task 7: Make the neural daemon observable before model initialization

**Files:**
- Modify: `crates/leindex-embed/src/protocol.rs`
- Modify: `crates/leindex-embed/src/runtime.rs`
- Modify: `crates/leindex-embed/src/worker_main.rs`
- Modify: `src/search/onnx/client.rs`
- Modify: `src/cli/index_builder.rs`
- Modify: `tests/onnx_worker_fallback.rs`

- [x] **Step 1: Write failing startup/readiness/lifecycle tests**

Inject a model initializer blocked on a channel. Start the socket worker and assert the socket accepts a health request before releasing model initialization. Add tests for one daemon spawn under 20 concurrent clients, stale state-file cleanup when PID is dead, no socket deletion by a non-owner client, and child reaping on worker exit.

```rust
assert_eq!(health.state, WorkerState::Initializing);
assert_eq!(spawn_count.load(Ordering::SeqCst), 1);
initializer_release.send(()).unwrap();
assert_eq!(wait_until_ready(&client), WorkerState::Ready);
assert_eq!(reaped_count.load(Ordering::SeqCst), 1);
```

Define `wait_until_ready` in the test module as a health poll guarded by a test-only two-second deadline that panics with the last `HealthResponse`. This deadline prevents a broken test from hanging CI; it is not production fallback or cancellation behavior.

Run: `cargo test --test onnx_worker_fallback daemon_readiness --all-features -- --nocapture`

Expected: FAIL because the current worker binds only after `WorkerRuntime::new` completes ORT initialization.

- [x] **Step 2: Add an explicit health protocol**

Extend the existing frame protocol with `HealthRequest` and `HealthResponse` message types and these serializable values:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState { Initializing, Ready, Failed }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub state: WorkerState,
    pub phase: String,
    pub started_unix_ms: u64,
    pub provider: Option<String>,
    pub model: String,
    pub error: Option<String>,
}
```

An embed/rerank request received while initializing returns a structured `ErrorKind::Initializing`; it does not block the socket client thread waiting for the model mutex.

- [x] **Step 3: Bind the socket before initializing ORT**

In socket mode, reorder `worker_main`:

1. Parse config.
2. Remove only a socket proven stale by failed connect and dead owner PID.
3. Bind `UnixListener` and write daemon state `{pid, socket, state:"initializing"}` atomically.
4. Construct `WorkerRuntime::initializing(config)`.
5. Spawn exactly one model-initializer thread that transitions shared state to `Ready` or `Failed`.
6. Enter the accept loop immediately; health requests are served in all states.

Keep pipe mode synchronous for compatibility because its stdin/stdout startup report is already the readiness boundary.

- [x] **Step 4: Stop holding the spawn lock during model compilation**

In `EmbeddingClient`, replace the 600-second flock/poll loop with:

```rust
pub enum WorkerAvailability {
    Ready,
    Initializing(HealthResponse),
    Failed(HealthResponse),
    Absent,
}
```

Under `DaemonSpawnLock`: connect and query health; if absent and no live state owner exists, spawn one child and atomically write its PID/state file. Release the flock immediately after process spawn/state publication. A later client observes live PID `Initializing` and does not spawn another daemon.

The spawning process moves `Child` into one reaper thread that calls `wait()` and removes socket/state only when the file PID still matches the child PID. Connected clients never remove a persistent daemon socket from `kill_worker`; they only drop their stream.

- [x] **Step 5: Make readiness—not waiting—the fallback rule**

Add `EmbeddingClient::availability()` and make neural demand state-driven. Query search always obtains the mandatory TF-IDF candidates, then starts/awaits the configured worker while it is `Initializing`; it uses neural scoring once `Ready`, and returns the core result with explicit `Failed`/`Absent` status if initialization cannot succeed. Indexing publishes lexical results first, keeps the owned job alive while the worker initializes, and actively evaluates neural batches before publishing the hybrid generation; terminal failure leaves the reusable TF-IDF/PDG generation intact and records the provider/model error.

Remove the daemon startup `IPC_TIMEOUT_SECS` wait. Retain bounded frame-size validation and read failures for a connected peer, but do not express model compilation or inference correctness as elapsed-time cancellation.

- [x] **Step 6: Verify the original two-file failure cannot recur**

Run: `cargo test --test onnx_worker_fallback daemon_readiness --all-features -- --nocapture`

Expected: PASS; socket health is visible while initialization is blocked, one worker is spawned, and the child is reaped.

Run: `cargo test --all-features search::onnx`

Expected: PASS.

Run: `cargo test -p leindex-embed --all-features`

Expected: PASS.

Run the two-file fixture with neural initialization intentionally blocked, then call exact grep and Git status.

Expected: both live/exact operations return immediately with `neural_status="initializing"`; no request waits 600 seconds.

- [ ] **Step 7: Commit**

```bash
git add crates/leindex-embed/src/protocol.rs crates/leindex-embed/src/runtime.rs crates/leindex-embed/src/worker_main.rs src/search/onnx/client.rs src/cli/index_builder.rs tests/onnx_worker_fallback.rs
git commit -m "fix: expose neural readiness before model load"
```

---

### Task 8: Add reusable checkpoints and staged index publication

**Files:**
- Modify: `src/cli/index_job.rs`
- Modify: `src/cli/leindex/mod.rs`
- Modify: `src/cli/leindex/indexing.rs`
- Modify: `src/cli/index_builder.rs`
- Modify: `src/cli/registry.rs`
- Modify: `src/cli/live_project.rs`
- Modify: `src/storage/catalog.rs`
- Modify: `src/storage/pdg_store.rs`
- Modify: `src/storage/schema.rs`
- Test: `tests/index_job_recovery.rs`

- [x] **Step 1: Write phase fault-injection tests**

For each phase `Scan`, `Parse`, `Pdg`, `Lexical`, `Neural`, and `Persist`, inject a process-safe error after the phase artifact is atomically written. Restart a fresh registry, resume, and assert completed reusable phases are loaded rather than rerun. Record a counter per phase.

```rust
assert_eq!(after_resume.scan_runs, 0);
assert_eq!(after_resume.parse_runs, 0);
assert_eq!(after_resume.pdg_runs, expected_pdg_runs);
assert_eq!(registry.current_generation(&project), Some(previous_generation));
```

When failure occurs before PDG/lexical publication, the previous generation stays current. When neural fails after lexical publication, the new lexical generation stays current with `neural=false`.

Run: `cargo test --test index_job_recovery resume_each_phase --all-features -- --nocapture`

Expected: FAIL because the current build has no durable phase artifacts and rewrites active storage directly.

- [x] **Step 2: Introduce generation directories without breaking legacy indexes**

Use this layout:

```text
.leindex/
  CURRENT
  generations/
    42/
      leindex.db
      search_snapshot.bin
      embeddings.bin
      neural_embeddings.bin
      index_stats.json
      index-state.json
  jobs/
    43/
      state.json
      job-status.json
      scan.bin
      parsed/
      pdg.bin
      lexical.complete
      neural.complete
```

`CURRENT` contains one decimal generation plus newline and is updated with `CURRENT.next` then rename. If `CURRENT` does not exist, treat the legacy `.leindex` root as generation 0 and load it unchanged. A new successful build writes generation 1 and `CURRENT`; do not move or delete legacy files during the release.

Add `LiveProject::active_storage()`: read and validate `CURRENT`, reject path separators/non-decimal content, and return `storage/generations/<generation>` only when it is a completed generation; otherwise return the legacy storage root. `CatalogReader` and every query artifact loader use `active_storage()`, while job/checkpoint writers use the storage root.

Refactor every artifact helper in `index_builder.rs` to accept `storage_path: &Path` rather than deriving `.leindex` from `project_path`. This is required for staging and fallback storage correctness.

- [x] **Step 3: Define reusable phase artifacts**

- `scan.bin`: sorted `Vec<FileFingerprint { canonical_path, blake3, bytes, language }>`.
- `parsed/bucket-<hex>.bin`: deterministic two-hex buckets of successful `ParsedFileCheckpoint` records without raw source bytes; source is reread only while building that file’s PDG chunk, and resume still validates the requested path inside the bucket.
- `pdg.bin`: bincode `ProgramDependenceGraph` plus `{scan_hash, node_count, edge_count}` header.
- `lexical.complete`: JSON containing TF-IDF artifact hashes, search snapshot hash, and PDG fingerprint.
- `neural.complete`: JSON containing model identity, provider, dimension, row count, and neural mmap hash.
- `state.json`: durable checkpoint input generation, artifact hashes, and last reusable phase.
- `job-status.json`: registry-facing `IndexJobSnapshot`; kept separate so polling cannot overwrite recovery metadata.

Define the phase types in `index_job.rs` so the method signatures below are compile-complete:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPaths { pub root: PathBuf, pub generation: u64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFingerprint { pub canonical_path: PathBuf, pub blake3: String, pub bytes: u64, pub language: String }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanCheckpoint { pub input_hash: String, pub files: Vec<FileFingerprint> }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedFileCheckpoint { pub file_path: PathBuf, pub language: String, pub signatures: Vec<SignatureInfo>, pub parse_time_ms: u64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseCheckpoint {
    pub scan_hash: String,
    pub artifact_paths: Vec<PathBuf>,
    pub artifact_hashes: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdgCheckpoint { pub scan_hash: String, pub artifact_path: PathBuf, pub nodes: usize, pub edges: usize }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LexicalCheckpoint { pub pdg_hash: String, pub snapshot_path: PathBuf, pub tfidf_path: PathBuf }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeuralCheckpoint { pub lexical_hash: String, pub mmap_path: PathBuf, pub rows: usize, pub provider: String, pub model: String }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedGeneration { pub generation: u64, pub storage_path: PathBuf, pub health: IndexHealth }
```

The hash fields are BLAKE3 hex strings of the referenced checkpoint headers/artifacts. `artifact_paths` are sorted by deterministic bucket path, not completion order; `artifact_hashes` maps each source BLAKE3 to its bucket artifact hash.

Write every bucket artifact as `.next`, call `File::sync_all`, rename to final, then sync the parent directory on Unix. A phase transition is recorded only after all bucket artifacts and the phase marker succeed.

- [x] **Step 4: Split the monolithic index function into exact phase methods**

Use these signatures in `src/cli/leindex/indexing.rs`:

```rust
pub(crate) fn run_scan(&mut self, job: &JobPaths) -> Result<ScanCheckpoint>;
pub(crate) fn run_parse(&mut self, job: &JobPaths, scan: &ScanCheckpoint) -> Result<ParseCheckpoint>;
pub(crate) fn run_pdg(&mut self, job: &JobPaths, parsed: &ParseCheckpoint) -> Result<PdgCheckpoint>;
pub(crate) fn run_lexical(&mut self, job: &JobPaths, pdg: &PdgCheckpoint) -> Result<LexicalCheckpoint>;
pub(crate) fn run_neural(&mut self, job: &JobPaths, lexical: &LexicalCheckpoint) -> Result<NeuralCheckpoint>;
pub(crate) fn publish_generation(&mut self, job: &JobPaths, neural: Option<&NeuralCheckpoint>) -> Result<PublishedGeneration>;
```

The live implementation keeps the method signatures small and carries only
ephemeral cross-phase state in `LeIndex::pipeline: Option<IndexPipelineState>`:

```rust
let scan = self.run_scan(&job)?;
let parsed = self.run_parse(&job, &scan)?;
let pdg = self.run_pdg(&job, &parsed)?;
let lexical = self.run_lexical(&job, &pdg)?;
let _core = self.publish_generation(&job, None)?; // PDG + TF-IDF is current
let neural = self.run_neural(&job, &lexical)?;
let _enhanced = self.publish_generation(&job, Some(&neural))?;
```

Each phase takes the state out, performs its existing algorithm, then restores
it only after its checkpoint is valid. An error drops the ephemeral state while
durable artifacts remain restartable. `ParseCheckpoint.artifact_paths` is sorted
by bucket path and its per-file BLAKE3/path identity is checked before reuse;
`PdgCheckpoint` stores a scan-hash header in `pdg.complete`,
and the core publication seam remains before neural work. This is intentionally
an orchestration-only extraction: no phase calls neural during lexical indexing.

Move existing code without algorithm changes first. `run_parse` skips a file when its bucketed parse artifact and requested path validate. `run_pdg` loads its artifact when its header scan hash matches. `run_lexical` never invokes neural embedding. `run_neural` reads lexical chunks and appends neural rows atomically.

- [x] **Step 5: Publish lexical/PDG before neural enrichment**

After `run_lexical`, create a generation directory containing SQLite PDG, search snapshot, TF-IDF mmap, stats, and health; set `CURRENT`; and swap the registry query generation. Mark `published.pdg=true`, `published.lexical=true`, `published.neural=false`.

After successful neural completion, stage a new immutable generation containing
the already-published DB/PDG, lexical snapshot, TF-IDF mmap, stats, and the
neural mmap copied through `neural_embeddings.bin.next`; then atomically advance
`CURRENT` to that generation. Existing generation files remain immutable; no
file held by an active mmap is modified, and readers can keep using the core
generation if neural publication fails.

- [x] **Step 6: Replace destructive auto-repair with generation rollback**

In `create_and_insert`, stop calling `remove_dir_all(.leindex)` on corruption. Validate `CURRENT`; if corrupt, scan completed generation directories newest-first and select the newest usable generation. Record the bad generation in health and leave artifacts for diagnostics. Only explicit `leindex cleanup --corrupt-generations` may remove them.

- [x] **Step 7: Verify resumability and publication**

Run: `cargo test --test index_job_recovery resume_each_phase --all-features -- --nocapture`

Expected: PASS for all injected failures; counters prove completed phases are reused.

Run: `cargo test --all-features cli::leindex::indexing`

Expected: PASS.

Run: `cargo test --all-features storage::pdg_store`

Expected: PASS, including legacy generation-0 loading.

- [ ] **Step 8: Commit**

```bash
git add src/cli/index_job.rs src/cli/leindex/mod.rs src/cli/leindex/indexing.rs src/cli/index_builder.rs src/cli/registry.rs src/cli/live_project.rs src/storage/catalog.rs src/storage/pdg_store.rs src/storage/schema.rs tests/index_job_recovery.rs
git commit -m "feat: resume and publish index generations"
```

---

### Task 9: Make mmap residency real and expose immutable query generations

**Files:**
- Modify: `src/search/vector.rs`
- Modify: `src/search/search.rs`
- Modify: `src/cli/leindex/mod.rs`
- Modify: `src/cli/leindex/query.rs`
- Modify: `src/cli/registry.rs`
- Modify: `tests/search_residency.rs`
- Modify: `tests/storage_reader_pool.rs`
- Test: `tests/mcp_fast_paths.rs`

- [x] **Step 1: Write failing heap-drop and concurrency tests**

Strengthen `test_heap_mirror_dropped_after_mmap_swap`: load 10,000 768-dimension vectors from persisted artifacts into the actual `SearchEngine`, then assert its vector backend reports `mmap` and owned vector bytes are bounded to the delta overlay.

```rust
assert_eq!(engine.vector_residency().kind, "mmap");
assert_eq!(engine.vector_residency().base_rows, 10_000);
assert_eq!(engine.vector_residency().delta_rows, 0);
assert_eq!(engine.vector_residency().owned_vector_bytes, 0);
```

Add a barrier test: pause indexing, concurrently run 20 semantic reads against the previous generation, and assert all complete without acquiring the build lock.

Run: `cargo test --test search_residency heap_mirror --all-features -- --nocapture`

Expected: FAIL because `restore_from_search_snapshot` calls `MmapEmbeddingIndex::entries()` and inserts owned vectors into `BruteForce`.

- [x] **Step 2: Add the minimum mmap-backed vector variant**

Add:

```rust
pub struct MmapVectorIndex {
    base: std::sync::Arc<MmapEmbeddingIndex>,
    rows: HashMap<String, u32>,
    delta: HashMap<String, Vec<f32>>,
    tombstones: HashSet<String>,
}
```

`MmapEmbeddingIndex` exposes `node_id_by_index(u32) -> Result<&str, MmapError>` and `build_row_map() -> Result<HashMap<String,u32>, MmapError>`. `MmapVectorIndex::search` scores borrowed `embedding_slice_by_index` rows, skips tombstones, scores owned delta rows, and keeps only the best `top_k` in the same ordering contract as the current brute-force implementation. Inserts go to `delta`; removes tombstone base rows or delete delta rows. Compaction is triggered by the existing explicit persistence phase, not automatically on a read path.

Add `VectorIndexImpl::Mmap(MmapVectorIndex)` and exhaustively update `len`, `is_empty`, `dimension`, `search`, `insert`, `clear`, `remove`, `estimated_memory_bytes`, and residency reporting.

Increment `SEARCH_SNAPSHOT_VERSION` from 1 to 2 because restored nodes no longer carry owned embeddings. Keep mmap file version 1: its bytes and row layout do not change. A v1 search snapshot takes the existing rebuild path once and is persisted as v2; do not reject the whole PDG generation.

- [x] **Step 3: Restore snapshots without calling `entries()`**

`restore_from_search_snapshot` builds `NodeInfo` metadata with empty `tfidf_embedding` and `neural_embedding`, validates every snapshot node ID through the mmap row map, constructs inverted/token metadata, and installs `VectorIndexImpl::Mmap`. Neural mmap is a separate `MmapVectorIndex` used by the active hybrid scorer; do not copy it into each `NodeInfo`. It may be absent only when the configured neural phase reaches an explicit terminal failure.

Delete the current `tfidf_by_id: HashMap<String, Vec<f32>>` and `neural_by_id` hydration code. Keep `entries()` only for diagnostics/export/tests outside load hot paths.

- [x] **Step 4: Split build state from immutable query state**

Define:

```rust
pub struct QueryGeneration {
    pub generation: u64,
    pub search: crate::search::search::SearchEngine,
    pub pdg: Option<crate::graph::pdg::ProgramDependenceGraph>,
    pub embedder: Option<crate::cli::index_builder::HybridEmbedder>,
    pub health: crate::cli::leindex::IndexHealth,
}

pub struct ProjectEntry {
    pub live: crate::cli::live_project::LiveProject,
    generation: tokio::sync::RwLock<Option<Arc<QueryGeneration>>>,
}
```

`LeIndex` exists only inside indexing/CLI maintenance jobs. On publication, consume its search/PDG/embedder fields with `into_query_generation`; do not clone the graph or vectors. Handlers clone the `Arc<QueryGeneration>` under the RwLock and immediately release the lock. Search result caches use their existing bounded internal mutex; SQLite is absent from `QueryGeneration`, so the aggregate can be shared.

Move `SearchEngine`'s `search_cache` and `search_cache_bytes` into one `std::sync::Mutex<SearchCacheState>` and change read-only search entry points from `&mut self` to `&self`. Keep index mutation methods on `&mut self`. This is the only internal mutability added and allows `Arc<QueryGeneration>` to serve concurrent semantic reads without wrapping the whole generation in a mutex.

- [x] **Step 5: Add project creation single-flight**

Use the `creating` `OnceCell` map from Task 6 so concurrent cold semantic requests load one query generation, changing its cell value from `ProjectHandle` to `Arc<ProjectEntry>` in the same commit. Live/catalog handlers do not enter this path. Increment `PROJECT_HYDRATIONS` exactly once inside the cell initializer.

- [x] **Step 6: Verify residency, reader concurrency, and RSS**

Run: `cargo test --test search_residency --all-features -- --nocapture`

Expected: PASS; actual engine reports mmap base, zero owned base vector bytes, correct delta/tombstone behavior, and equal rankings.

Run: `cargo test --test storage_reader_pool --all-features -- --nocapture`

Expected: PASS; catalog connections remain local/bounded and query generations contain no rusqlite connection.

Run: `cargo test --test mcp_fast_paths concurrent_generation --all-features -- --nocapture`

Expected: PASS; live/exact tools remain independent and semantic readers share the previous immutable generation during a build.

- [ ] **Step 7: Commit**

```bash
git add src/search/vector.rs src/search/search.rs src/cli/leindex/mod.rs src/cli/leindex/query.rs src/cli/registry.rs tests/search_residency.rs tests/storage_reader_pool.rs tests/mcp_fast_paths.rs
git commit -m "perf: query mmap vectors without heap hydration"
```

---

### Task 10: Repair Rust symbol extraction and live miss fallback

**Files:**
- Create: `tests/fixtures/rust/askpass.rs`
- Create: `tests/fixtures/rust/nested_cli.rs`
- Modify: `src/parse/rust.rs`
- Modify: `src/graph/extraction.rs`
- Modify: `src/cli/mcp/read_symbol_handler.rs`
- Modify: `src/cli/mcp/file_summary_handler.rs`
- Modify: `src/storage/catalog.rs`
- Test: `tests/mcp_fast_paths.rs`

- [x] **Step 1: Add realistic failing Rust fixtures**

`tests/fixtures/rust/askpass.rs` contains a public `Askpass` struct, a private field, an `impl Askpass` with associated `new` and borrowed `path`, and a caller. `tests/fixtures/rust/nested_cli.rs` contains `sudo_creds`, `update_impl::DirectInstallerExecutor`, `uninstall_impl`, `reinstall_impl`, nested functions, and a derive-annotated CLI enum with unit/tuple/struct variants.

Assert exact qualified names and non-empty ranges:

```rust
assert_anchor("Askpass", "Askpass", NodeType::Class);
assert_anchor("new", "Askpass::new", NodeType::Method);
assert_anchor("path", "Askpass::path", NodeType::Method);
assert_anchor("DirectInstallerExecutor", "update_impl::DirectInstallerExecutor", NodeType::Class);
assert_anchor("run", "update_impl::DirectInstallerExecutor::run", NodeType::Method);
assert_anchor("Install", "Command::Install", NodeType::Variable);
assert_anchor("Remove", "Command::Remove", NodeType::Variable);
```

For every anchor, assert `byte_range.1 > byte_range.0` and source slicing produces the definition text.

Run: `cargo test parse::rust::realistic --all-features -- --nocapture`

Expected: FAIL for impl qualification, associated functions without `self`, and enum variants. Nested module recursion should already pass; preserve that proof.

- [x] **Step 2: Qualify impl methods with the implemented type**

Add a helper that reads the `type` field of `impl_item`, strips generic arguments only from the display path, and appends the type to `parent_path`:

```rust
fn impl_type_path(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let raw = node.child_by_field_name("type")?.utf8_text(source).ok()?.trim();
    let base = raw.split('<').next().unwrap_or(raw).trim();
    (!base.is_empty()).then(|| base.to_string())
}
```

When visiting an impl, call `extract_function_signature` with `impl_path`, and set `sig.is_method = true` for every impl function, including `new` without `self`. Trait impls use the implemented concrete type, not the trait name.

- [x] **Step 3: Map existing signature markers to correct node categories**

Keep the existing public `NodeType` enum to avoid a broad storage/API migration. In `signature_to_node`, map signatures deterministically:

```rust
let marker = sig.return_type.as_deref().unwrap_or("");
let node_type = if sig.is_method {
    NodeType::Method
} else if marker == "module" {
    NodeType::Module
} else if marker.starts_with("struct") || marker == "enum" || marker == "trait" {
    NodeType::Class
} else if marker == "enum_variant" {
    NodeType::Variable
} else {
    NodeType::Function
};
```

This fixes the current bug where emitted struct/enum/module signatures become `Function` nodes. Preserve `qualified_name` in `intel_nodes`; the MCP response can distinguish the original declaration through `qualified_name` and language without expanding the public node-type enum.

- [x] **Step 4: Emit enum variants as anchors**

For each `enum_variant`, emit `SignatureInfo` with short `name`, `qualified_name="<module>::<enum>::<variant>"`, `return_type=Some("enum_variant")`, `is_method=false`, doc comment, and the variant’s full byte range. Recurse through `enum_item` only for variants; do not index derive macro internals.

- [x] **Step 5: Add immediate live parser fallback on catalog miss**

Do not implement a “two miss” session counter; falling back on the first miss is simpler and strictly stronger. With a file hint, run the existing language parser on that one canonical live file and match short or qualified name. Without a file hint, use exact text search to find definition-like candidate files, cap at 20 files, then parse those files until an exact signature matches.

Return:

```json
{
  "symbol_index_miss": true,
  "resolved_by": "live_parser",
  "source_freshness": "live",
  "index_generation": 42
}
```

If the live parser also misses, return the original structured invalid-params error plus up to five exact-text candidate locations. Never silently substitute a substring-different symbol.

- [x] **Step 6: Make file summary use live symbols when catalog generation is stale**

When the file hash differs from `indexed_files.file_hash`, parse the live file and build its summary from current signatures, with PDG dependencies marked `stale` or `not_loaded`. This guarantees nested symbols are visible even before reindex finishes.

- [x] **Step 7: Verify parser, fallback, and line ranges**

Run: `cargo test parse::rust::realistic --all-features -- --nocapture`

Expected: PASS for `Askpass`, `Askpass::new`, `Askpass::path`, nested modules/functions, executor impl, and CLI variants.

Run: `cargo test --test mcp_fast_paths symbol_index_miss --all-features -- --nocapture`

Expected: PASS; a deliberately stale catalog returns live source and `symbol_index_miss=true` without neural work.

- [ ] **Step 8: Commit**

```bash
git add tests/fixtures/rust/askpass.rs tests/fixtures/rust/nested_cli.rs src/parse/rust.rs src/graph/extraction.rs src/cli/mcp/read_symbol_handler.rs src/cli/mcp/file_summary_handler.rs src/storage/catalog.rs tests/mcp_fast_paths.rs
git commit -m "fix: recover qualified Rust symbols and live misses"
```

---

### Task 11: Build non-redundant hybrid chunks and exact-biased ephemeral context

**Files:**
- Modify: `src/cli/index_builder.rs`
- Modify: `src/search/search.rs`
- Modify: `src/search/ranking.rs`
- Modify: `src/search/query_route.rs`
- Modify: `src/cli/leindex/query.rs`
- Modify: `src/cli/mcp/search_handler.rs`
- Modify: `src/cli/mcp/deep_analyze_handler.rs`
- Modify: `src/cli/mcp/context_handler.rs`
- Test: `tests/cross_area_validation_test.rs`

- [x] **Step 1: Write failing chunk and context-ranking tests**

Index a fixture with symbols, adjacent doc comments, a comment-only file, and a large generated blob. Assert one chunk per symbol range, docs included once, file-summary fallback only for a file with no extracted symbols, no chunk over 64 KiB, and no duplicate `(file, byte_range, content_hash)`.

Add a ranking test where request context includes `registry_record`, `NativeInstallerContext`, and `ARCH`; exact identifier-bearing symbols must rank above semantically related but identifier-free symbols.

Add a vocabulary-context test using all requested Rusty Stack concepts—ROCm channels, AMD/ROCm-only dependency policy, sudo askpass, pacman/yay, registry sealing, verification false positives, and native installer dispatch. Supply them through `task_context`; assert relevant anchors rank without any production hard-coded synonym table.

```rust
assert_eq!(duplicate_chunk_count(&chunks), 0);
assert!(chunks.iter().all(|c| c.text.len() <= 64 * 1024));
assert_eq!(ranked[0].symbol_name, "registry_record");
```

Run: `cargo test --test cross_area_validation_test hybrid_chunks --all-features -- --nocapture`

Expected: FAIL because current node content is built independently of explicit hybrid chunk policy and request context is not scored.

- [x] **Step 2: Define the bounded chunk contract**

```rust
pub enum ChunkKind { Symbol, Comment, FileSummary }

pub struct SearchChunk {
    pub node_id: String,
    pub file_path: String,
    pub symbol_name: Option<String>,
    pub kind: ChunkKind,
    pub byte_range: (usize, usize),
    pub text: String,
    pub exact_terms: Vec<String>,
}
```

For every PDG symbol, slice its byte range and prepend only immediately adjacent doc comments not already inside the range. Emit standalone comment chunks only for comment blocks containing at least one identifier and 40 non-whitespace characters. Emit one file-summary chunk only when a source file has zero symbol chunks. Truncate at a UTF-8 boundary to 64 KiB and record truncation in chunk metadata.

- [x] **Step 3: Add bounded request-scoped task context**

Accept one optional `task_context` string on semantic search, deep-analyze, and context. Validate at the MCP boundary: maximum 16 KiB UTF-8. Extract at most 32 unique identifier-shaped terms and at most 128 normalized lexical terms. Store them only in the in-memory request; do not write them to cache keys, SQLite, mmap files, logs, or health state.

Use:

```rust
pub struct QueryContext {
    pub exact_identifiers: Vec<String>,
    pub lexical_terms: Vec<String>,
}
```

- [x] **Step 4: Weight exact identifiers before semantic tie-breaking**

Add an `exact_identifier` component to `Score`. A case-sensitive full symbol/qualified-name match contributes `1.0`; case-insensitive full match `0.9`; token match `0.7`; no match `0.0`. For auto/semantic queries, final ordering applies exact component first when it is non-zero, then existing hybrid score. Do not add a fixed Rusty Stack synonym table.

Neural embeddings use the natural-language query plus at most eight highest-value lexical task terms. Exact identifiers remain a separate scoring feature; do not dilute them into only the embedding text.

- [x] **Step 5: Keep cache behavior privacy-safe**

When `task_context` is present, use only the bounded extracted terms in an in-memory cache key and skip disk persistence of the result. Existing disk query cache remains available for context-free queries.

- [x] **Step 6: Verify chunk counts, ranking, and fallback**

Run: `cargo test --test cross_area_validation_test hybrid_chunks --all-features -- --nocapture`

Expected: PASS; no duplicate chunks, bounded text, file fallback only when needed, and exact context terms lead.

Run: `cargo test --all-features search::ranking`

Expected: PASS.

Run: `cargo test --all-features cli::index_builder`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/cli/index_builder.rs src/search/search.rs src/search/ranking.rs src/search/query_route.rs src/cli/leindex/query.rs src/cli/mcp/search_handler.rs src/cli/mcp/deep_analyze_handler.rs src/cli/mcp/context_handler.rs tests/cross_area_validation_test.rs
git commit -m "feat: add bounded hybrid query context"
```

---

### Task 12: Extend PDG extraction for data, state, command, and submodule flows

**Files:**
- Create: `tests/fixtures/rust/flows.rs`
- Modify: `src/parse/traits.rs`
- Modify: `src/parse/rust.rs`
- Modify: `src/parse/bash.rs`
- Modify: `src/parse/c.rs`
- Modify: `src/parse/completeness.rs`
- Modify: `src/parse/cpp.rs`
- Modify: `src/parse/csharp.rs`
- Modify: `src/parse/dart.rs`
- Modify: `src/parse/go.rs`
- Modify: `src/parse/java.rs`
- Modify: `src/parse/javascript.rs`
- Modify: `src/parse/kotlin.rs`
- Modify: `src/parse/lua.rs`
- Modify: `src/parse/php.rs`
- Modify: `src/parse/python.rs`
- Modify: `src/parse/ruby.rs`
- Modify: `src/parse/scala.rs`
- Modify: `src/parse/swift.rs`
- Modify: `src/graph/pdg.rs`
- Modify: `src/graph/extraction.rs`
- Modify: `src/storage/edges.rs`
- Modify: `src/storage/pdg_store.rs`
- Modify: `src/cli/index_builder.rs`
- Test: `tests/cross_area_validation_test.rs`

- [x] **Step 1: Write the three requested end-to-end flow fixtures**

The fixture must contain these generic-but-realistic chains:

1. `Subcommands::Install` password field and match arm → `resolve_sudo_credentials` → `run_installation` → `NativeInstallerContext` → `run_native_installer` → `execute_native_command`/`run_script`.
2. two early `registry_record` writes plus installation result → verification → `final_success` → downstream registry gate → UI success/failure rendering.
3. `DistroFacade::family`/`is_arch_family` and `platform::detection::DistroFamily::Arch` → pacman/yay package-manager selection → component installer dispatch.

Also include `Command::new("sudo").arg("-S").env("SUDO_ASKPASS", askpass).stdin(password)` and one submodule declaration. Assert path sequences by edge kind, not merely node presence:

```rust
assert_path(&pdg, "cli_password", "execute_native_command", EdgeType::DataDependency);
assert_path(&pdg, "run_installation", "registry_record", EdgeType::StateTransition);
assert_edge(&pdg, "execute_native_command", "sudo", EdgeType::CommandArgument, "argv");
assert_edge(&pdg, "execute_native_command", "SUDO_ASKPASS", EdgeType::Environment, "env");
assert_edge(&pdg, "execute_native_command", "password", EdgeType::Stdin, "stdin");
```

Run: `cargo test --test cross_area_validation_test requested_flows --all-features -- --nocapture`

Expected: FAIL because current structured edges stop at call/type inference and do not preserve call arguments, state, argv/env/stdin channels.

- [x] **Step 2: Add backward-compatible structured facts to signatures**

In `src/parse/traits.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowChannel { Argument, ReturnValue, StateRead, StateWrite, CommandArgument, Environment, Stdin }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlowFact {
    pub channel: FlowChannel,
    pub source: String,
    pub target: String,
    pub position: Option<usize>,
    pub byte_range: (usize, usize),
}
```

Add `#[serde(default)] pub flow_facts: Vec<FlowFact>` to `SignatureInfo`. Update every `SignatureInfo` literal under `src/parse` with `flow_facts: vec![]`; Rust then populates real facts. This is a mechanical compile-enforced migration: run the exact `rg -l` command in the Files list and do not leave a parser constructor unmodified.

- [x] **Step 3: Extract Rust call/data facts without full type inference**

Within each function body, Tree-sitter extracts:

- call-expression callee plus identifier/scoped-identifier arguments by ordinal;
- assignment/let bindings where RHS is a call result;
- return/tail identifiers;
- method chains rooted at `Command::new`, recording `.arg`, `.args`, `.env`, `.envs`, and `.stdin` arguments;
- obvious state calls whose method name is one of `insert`, `write`, `set`, `record`, `save`, `update`, `verify`, or `render`, using receiver+argument identifiers as facts.

Do not implement alias analysis, macro expansion, or interprocedural type inference in this release. Mark inferred state facts confidence `0.65`; explicit call argument/command channel facts confidence `1.0`.

- [x] **Step 4: Add explicit edge kinds and metadata**

Extend graph and storage `EdgeType` with `StateTransition`, `CommandArgument`, `Environment`, and `Stdin`. Add `#[serde(default)] channel: Option<String>` and `#[serde(default)] position: Option<usize>` to edge metadata. Update `as_str`, `from_str_name`, graph↔storage conversions, traversal filters, serialization tests, and schema-compatible JSON metadata loading.

Fix the existing incorrect persistence mapping `PDGEdgeType::Containment => StorageEdgeType::Call`; persist it as `Containment`.

- [x] **Step 5: Correlate facts across calls**

For a call from function A to function B, match A’s argument facts by ordinal to B’s named parameters. Propagate a bounded value label through call edges and return bindings. Build state edges from producer result → verifier → recorder → renderer when the same value label is passed or returned. Build command-channel edges from the containing function to a stable external command node whose name is the command expression.

Use one work queue keyed by `(node_id, value_label)` and a visited set; cap propagation at 8 call boundaries. This prevents cycles from expanding indefinitely while covering the requested chains.

- [x] **Step 6: Summarize submodules instead of traversing them**

For each Git gitlink root, emit one `NodeType::Module` node with file path equal to the submodule root and metadata containing recorded commit OID. Add an `Import` edge only when project source imports that submodule path. Do not parse submodule descendants by default.

- [x] **Step 7: Verify flows and persistence round-trip**

Run: `cargo test --test cross_area_validation_test requested_flows --all-features -- --nocapture`

Expected: PASS for all three chains and argv/env/stdin edge assertions.

Run: `cargo test --all-features graph::extraction`

Expected: PASS.

Run: `cargo test --all-features storage::pdg_store`

Expected: PASS.

Run: `cargo test --all-features parse::rust`

Expected: PASS; new/old metadata round-trips and containment remains containment.

- [ ] **Step 8: Commit**

```bash
git add tests/fixtures/rust/flows.rs src/parse/traits.rs src/parse/rust.rs src/parse/bash.rs src/parse/c.rs src/parse/completeness.rs src/parse/cpp.rs src/parse/csharp.rs src/parse/dart.rs src/parse/go.rs src/parse/java.rs src/parse/javascript.rs src/parse/kotlin.rs src/parse/lua.rs src/parse/php.rs src/parse/python.rs src/parse/ruby.rs src/parse/scala.rs src/parse/swift.rs src/graph/pdg.rs src/graph/extraction.rs src/storage/edges.rs src/storage/pdg_store.rs src/cli/index_builder.rs tests/cross_area_validation_test.rs
git commit -m "feat: trace state and command data through PDG"
```

---

### Task 13: Enforce utilization policy, partial enrichment, and session-scoped stale badges

**Files:**
- Modify: `src/cli/mcp/request_meta.rs`
- Modify: `src/cli/mcp/server.rs`
- Modify: `src/cli/mcp/helpers.rs`
- Modify: `src/cli/mcp/search_handler.rs`
- Modify: `src/cli/mcp/grep_symbols_handler.rs`
- Modify: `src/cli/mcp/file_summary_handler.rs`
- Modify: `src/cli/mcp/context_handler.rs`
- Modify: `src/cli/mcp/deep_analyze_handler.rs`
- Modify: `src/cli/leindex/query.rs`
- Modify: `src/graph/traversal.rs`
- Modify: `tests/cli_mcp_stdio_e2e.rs`

- [x] **Step 1: Write failing policy and warning-generation tests**

In one MCP session, return two stale responses for the same project/generation and assert the first has detailed freshness plus a concise advisory while the second has only the structured badge. In a second session, assert detail appears once independently. Advance generation and assert detail appears again.

Add a deliberately wide PDG and a 1 ms optional-enrichment budget. Assert the base results are intact, status is `partial`, and no background detached traversal remains.

```rust
assert!(first["_meta"]["freshness"]["advisory"].is_string());
assert!(second["_meta"]["freshness"]["advisory"].is_null());
assert_eq!(partial["_meta"]["pdg_status"], "partial");
assert!(!partial["results"].as_array().unwrap().is_empty());
```

Run: `cargo test --test cli_mcp_stdio_e2e freshness_policy --all-features -- --nocapture`

Expected: FAIL because warnings are currently repeated free text and handlers have no common enrichment budget.

- [x] **Step 2: Add budget fields consistently**

All composite handlers accept `max_latency_ms` and `allow_partial`. Defaults:

| Tool class | Default `max_latency_ms` | Budgeted work |
|---|---:|---|
| live Git/file | 150 | optional formatting/diff only |
| exact catalog/symbol | 250 | caller/callee PDG relation enrichment when resident; no neural/TF-IDF query embedding |
| semantic search | 500 | configured neural startup/inference plus rerank/context expansion; the budget never cancels the neural request |
| context/deep-analyze | 1500 | graph expansion and source assembly |

Base live/catalog/TF-IDF results and applicable resident PDG relations are correctness work and must be published. Between PDG nodes/files/batches, check `WorkBudget::elapsed`; stop cleanly and report which core component is partial (`pdg_status="partial"`) without cancelling the owning job. Configured neural enrichment is an active phase: start/await it without elapsed cancellation, use it when `Ready`, and report terminal `Absent`/`Failed` while TF-IDF and PDG stay usable. Never spawn work that outlives the response merely to satisfy a latency budget.

- [x] **Step 3: Make tool routing policy explicit in descriptions and responses**

- Exact code review: `text-search` plus bounded file slices; no semantic or PDG work unless requested.
- Symbol lookup: catalog then immediate live parser fallback; report `symbol_index_miss`.
- Git status: tracked/staged/untracked/conflicted/submodule categorization plus advisory safe-to-stage candidates; resident PDG impact is a core enrichment layer when available and reports `not_loaded|partial|fresh|stale|failed` otherwise.
- Broad explanation: mandatory TF-IDF candidates plus configured neural scoring after the worker reaches `Ready`, then PDG expansion.
- Context: caller/callee/data/state/command edges with status per component.

Return `route`, `lexical_status`, `neural_status`, `pdg_status`, and phase timings in `_meta` so utilization is observable.

- [x] **Step 4: Suppress repeated stale prose per session and generation**

Add `freshness_advisories: DashMap<(Arc<str>, PathBuf), u64>` to `McpServer`. At the server response boundary, inspect `_meta.freshness.generation`; include the advisory only when stored generation differs, then record it. Every response retains the compact status badge. Remove advisory entries when the session cleanup removes `session_handshakes`, so the map cannot grow independently.

Do not put session state inside handlers or a process-global Boolean.

- [x] **Step 5: Verify the three context-expansion scenarios**

Using `tests/fixtures/rust/flows.rs`, call context/deep-analyze for:

- “How does a supplied sudo password reach command execution?”
- “Where is registry_record written relative to verification?”
- “How does Arch detection choose an installer?”

Assert returned context contains the complete ordered path, including the install enum field/match arm, both early registry writes, `final_success`, registry gate semantics, `DistroFacade::family/is_arch_family`, `DistroFamily::Arch`, pacman/yay selection, and installer dispatch. Require labels `data_dependency`, `state_transition`, or command channel. Run with neural unavailable to prove TF-IDF+PDG remains functional.

- [x] **Step 6: Run policy tests and commit**

Run: `cargo test --test cli_mcp_stdio_e2e freshness_policy --all-features -- --nocapture`

Expected: PASS with session/generation isolation and partial results.

Run: `cargo test --test cross_area_validation_test context_scenarios --all-features -- --nocapture`

Expected: PASS with neural disabled.

```bash
git add src/cli/mcp/request_meta.rs src/cli/mcp/server.rs src/cli/mcp/helpers.rs src/cli/mcp/search_handler.rs src/cli/mcp/grep_symbols_handler.rs src/cli/mcp/file_summary_handler.rs src/cli/mcp/context_handler.rs src/cli/mcp/deep_analyze_handler.rs src/cli/leindex/query.rs src/graph/traversal.rs tests/cli_mcp_stdio_e2e.rs tests/cross_area_validation_test.rs
git commit -m "feat: budget optional enrichment and freshness advisories"
```

---

### Task 14: Prove latency, memory, concurrency, and degradation targets

**Files:**
- Modify: `benches/mcp_tool_latency.rs`
- Modify: `tests/mcp_fast_paths.rs`
- Modify: `tests/index_job_recovery.rs`
- Modify: `tests/search_residency.rs`
- Create: `scripts/check-performance.sh`
- Create: `.github/workflows/performance-regression.yml`
- Modify: `docs/PERFORMANCE_BENCHMARKS.md`

- [x] **Step 1: Implement a deterministic 20k-file benchmark fixture**

Generate 20,000 small source files in 200 directories, commit 19,995, leave five tracked modifications and five untracked files, add ignored build trees totaling at least 20,000 extra files, and ignore the benchmark's in-project `.leindex/` artifacts. Build a completed lexical/PDG generation once during benchmark setup. Use a fixed RNG seed where content variation is needed.

Benchmark 100 warm samples and 10 cold-process samples. Capture p50, p95, max, RSS delta, hydration counter, PDG load counter, and neural request counter as JSON.

- [x] **Step 2: Enforce deterministic dependency gates in ordinary tests**

These assertions run in `cargo test` and are release-blocking:

| Operation | Hydration | PDG load | Neural request |
|---|---:|---:|---:|
| live Git status, no resident generation | 0 | 0 | 0 |
| read file / exact text | 0 | 0 | 0 |
| exact symbol/file summary | 0 | 0 | 0 |
| semantic hybrid cold start | 1 max | 0 unless PDG expansion requested | 1 state-driven neural request |
| context/deep hybrid cold start | 1 max | 1 max | 1 state-driven neural request |

Also assert one index job, one project hydration, and one generation swap under 20 concurrent callers.

- [x] **Step 3: Enforce benchmark acceptance thresholds outside unit tests**

`scripts/check-performance.sh` runs the benchmark JSON exporter and fails on:

- 20k-file clean Git status: cold p95 > 500 ms or warm p95 > 150 ms.
- exact symbol query: warm p95 > 100 ms or any neural invocation.
- exact text search over scoped file: warm p95 > 100 ms.
- TF-IDF semantic search over 300k nodes: warm p95 > 500 ms.
- concurrent index + fast status: status p95 > 2× idle status p95.
- live/exact process RSS delta > 150 MiB.
- mmap generation load owned vector bytes > delta-overlay bytes.
- daemon bind/health visibility > 500 ms when model initialization is blocked.

The script runs with `LC_ALL=C`, release binaries, a local tempdir, and five warm-up samples. It prints actual/limit pairs on failure. Neural inference throughput is reported by provider but not release-gated across heterogeneous hardware; correctness requires TF-IDF availability, a state-driven neural attempt for configured semantic work, and truthful Ready/Failed/Absent visibility.

Implement the script exactly as the stable entry point; the Rust benchmark performs measurement, JSON serialization, and threshold comparisons:

```bash
#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C
export LEINDEX_ENFORCE_PERF=1
export LEINDEX_PERF_OUTPUT="target/leindex-performance.json"

cargo bench --bench mcp_tool_latency --all-features
test -s "$LEINDEX_PERF_OUTPUT"
echo "LeIndex performance report: $LEINDEX_PERF_OUTPUT"
```

Give `mcp_tool_latency.rs` a custom `main`: five untimed warm-ups, measured loops, percentile calculation over sorted `Duration::as_micros`, RSS/counter capture, pretty JSON write to `LEINDEX_PERF_OUTPUT`, and `assert!` checks only when `LEINDEX_ENFORCE_PERF=1`. Without that environment variable, run the normal Criterion groups for developer comparison.

- [x] **Step 4: Add a dedicated performance workflow**

`.github/workflows/performance-regression.yml` runs on `workflow_dispatch`, weekly schedule, and pushes to `master` touching `src/cli`, `src/search`, `src/graph`, `src/storage`, or the worker crate. It builds `--release --all-features`, runs deterministic fast-path tests, then `scripts/check-performance.sh`. Upload benchmark JSON and daemon logs as artifacts on success or failure.

Use this workflow structure:

```yaml
name: performance-regression
on:
  workflow_dispatch:
  schedule:
    - cron: "20 5 * * 1"
  push:
    branches: [master]
    paths:
      - "src/cli/**"
      - "src/search/**"
      - "src/graph/**"
      - "src/storage/**"
      - "crates/leindex-embed/**"
      - "benches/mcp_tool_latency.rs"
      - "scripts/check-performance.sh"
jobs:
  regression:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --release --all-features --test mcp_fast_paths --test index_job_recovery --test search_residency
      - run: bash scripts/check-performance.sh
      - if: always()
        uses: actions/upload-artifact@v4
        with:
          name: leindex-performance
          path: |
            target/leindex-performance.json
            ~/.leindex/logs/
          if-no-files-found: warn
```

Do not add hardware-specific GPU thresholds to `.github/workflows/release.yml`; release remains portable and its existing test job covers deterministic gates.

- [x] **Step 5: Run the full verification locally**

Run: `cargo fmt --all -- --check`

Expected: PASS.

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `cargo test --workspace --all-features --exclude memcheck -- --test-threads=1`

Expected: PASS.

Run: `bash scripts/check-performance.sh`

Expected: PASS with every actual measurement at or below its threshold and counter invariants equal to zero/one as specified.

- [ ] **Step 6: Commit**

```bash
git add benches/mcp_tool_latency.rs tests/mcp_fast_paths.rs tests/index_job_recovery.rs tests/search_residency.rs scripts/check-performance.sh .github/workflows/performance-regression.yml docs/PERFORMANCE_BENCHMARKS.md
git commit -m "test: gate LeIndex fast paths and residency"
```

---

### Task 15: Align public contracts, MCP guidance, and the 1.9.0 release

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/leindex-embed/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `install.sh`
- Modify: `install.ps1`
- Modify: `packages/npm-leindex-mcp/package.json`
- Modify: `packages/npm-leindex-mcp/test.js`
- Modify: `packages/pypi-leindex/pyproject.toml`
- Modify: `packages/pypi-leindex/src/leindex/__init__.py`
- Modify: `package.json`
- Modify: `dashboard/package.json`
- Modify: `pi/package.json`
- Modify: `install_macos.sh`
- Modify: `README.md`
- Modify: `packages/pypi-leindex/README.md`
- Modify: `packages/npm-leindex-mcp/README.md`
- Modify: `INSTALLATION.md`
- Modify: `INSTALLATION_RUST.md`
- Modify: `RELEASE_NOTES.md`
- Modify: `docs/MCP.md`
- Modify: `docs/CLI.md`
- Modify: `docs/MIGRATION.md`
- Modify: `docs/AGENT_GUIDANCE.md`
- Modify: `docs/NEURAL_SETUP.md`
- Modify: `docs/PERFORMANCE_BENCHMARKS.md`
- Modify: `.github/workflows/release.yml`
- Test: `tests/install_script_test.rs`
- Test: `tests/cargo_install_layout_test.rs`
- Test: `tests/release_bundle_packaging_test.rs`

- [x] **Step 1: Update all public documentation in one pass**

Document:

- MCP `leindex.index` start/poll response, `job_id`, `wait`, phases, and no request-cancellation semantics.
- `max_latency_ms`/`allow_partial` as formatting/PDG context budgets, not operation cancellation or a reason to skip configured neural startup/inference.
- `route`, component status, phase timings, `symbol_index_miss`, and structured freshness health.
- exact review workflow (`text-search` + file slice), exact symbol fallback, Git category policy, semantic/deep routing, and task-context privacy limits.
- daemon `initializing/ready/failed` diagnostics, active neural use after `Ready`, and TF-IDF/PDG availability during initialization or explicit terminal failure.
- generation/checkpoint layout, resume behavior, rollback, and cleanup.
- Git-aware ignore/submodule behavior and non-Git WalkDir fallback.
- measured before/after latency and RSS from Task 14.

Keep root `README.md` and `packages/pypi-leindex/README.md` aligned section-for-section. Keep the npm README’s shorter package-specific introduction, but make its MCP config, index behavior, status fields, and troubleshooting wording match.

Update every MCP config example found by:

```bash
rg -l 'mcpServers|leindex.*mcp|leindex_mcp|leindex-mcp' README.md packages/*/README.md docs --glob '*.md'
```

Expected files include root/npm/PyPI READMEs plus `docs/CLI.md`, `docs/MCP.md`, and `docs/MIGRATION.md`. Do not leave old advice telling users to raise index timeout variables.

- [x] **Step 2: Bump every published version surface to 1.9.0**

Set exactly:

- `Cargo.toml` package version and `leindex-embed` dependency: `1.9.0`.
- `crates/leindex-embed/Cargo.toml`: `1.9.0`.
- `install.sh` comment and `SCRIPT_VERSION`: `1.9.0`.
- `install.ps1` comment, `$ScriptVersion`, `$ExpectedVersion`: `1.9.0`.
- `install_macos.sh` comment and `SCRIPT_VERSION`: `1.9.0`.
- npm `package.json`: `1.9.0`; update fixed-version archive safety tests.
- root `package.json`, `dashboard/package.json`, and `pi/package.json`: `1.9.0`.
- PyPI `pyproject.toml` and `src/leindex/__init__.py`: `1.9.0`.

Add 1.9.0 release notes and update current installation/neural guides. Retain explicitly historical references such as “pre-1.8.4 configuration” in source comments and migration history; the parity check targets current-version declarations, not historical prose.

Run `cargo check --workspace` to update `Cargo.lock`; do not edit lockfile checksums manually.

- [x] **Step 3: Strengthen CI parity and obsolete-timeout checks**

In `.github/workflows/release.yml`, retain existing Cargo/npm/PyPI parity checks and add root/dashboard/pi package, all three installers, worker crate, and Python `__version__` assertions. Add a repository check that fails if `LEINDEX_INDEX_TIMEOUT_SECS`, `DEFAULT_INDEX_TIMEOUT_SECS`, or the old “raise the timeout” guidance returns outside `docs/MIGRATION.md`’s historical note.

- [x] **Step 4: Run packaging and parity verification**

Run: `cargo test --test install_script_test --test cargo_install_layout_test --test release_bundle_packaging_test --all-features`

Expected: PASS.

Run:

```bash
test "$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)" = "1.9.0"
test "$(node -p "require('./packages/npm-leindex-mcp/package.json').version")" = "1.9.0"
test "$(node -p "require('./package.json').version")" = "1.9.0"
test "$(node -p "require('./dashboard/package.json').version")" = "1.9.0"
test "$(node -p "require('./pi/package.json').version")" = "1.9.0"
test "$(sed -n 's/^version = "\([^"]*\)"/\1/p' packages/pypi-leindex/pyproject.toml | head -1)" = "1.9.0"
test "$(sed -n 's/^version = "\([^"]*\)"/\1/p' crates/leindex-embed/Cargo.toml | head -1)" = "1.9.0"
```

Expected: all commands exit 0.

Run: `npm test --prefix packages/npm-leindex-mcp`

Expected: PASS.

Run: `python -m pytest packages/pypi-leindex/tests`

Expected: PASS.

- [x] **Step 5: Run final release-equivalent verification**

Run: `cargo fmt --all -- --check`

Expected: PASS.

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

Run: `cargo test --workspace --all-features --exclude memcheck -- --test-threads=1`

Expected: PASS.

Run: `bash scripts/check-performance.sh`

Expected: PASS.

Run: `git diff --check`

Expected: no output and exit 0.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/leindex-embed/Cargo.toml Cargo.lock install.sh install.ps1 install_macos.sh package.json dashboard/package.json pi/package.json packages/npm-leindex-mcp/package.json packages/npm-leindex-mcp/test.js packages/pypi-leindex/pyproject.toml packages/pypi-leindex/src/leindex/__init__.py README.md packages/pypi-leindex/README.md packages/npm-leindex-mcp/README.md INSTALLATION.md INSTALLATION_RUST.md RELEASE_NOTES.md docs/MCP.md docs/CLI.md docs/MIGRATION.md docs/AGENT_GUIDANCE.md docs/NEURAL_SETUP.md docs/PERFORMANCE_BENCHMARKS.md .github/workflows/release.yml tests/install_script_test.rs tests/cargo_install_layout_test.rs tests/release_bundle_packaging_test.rs
git commit -m "release: prepare LeIndex 1.9.0"
```

---

## Notepad recommendation traceability

| Recommendation | Implemented by | Proof |
|---|---|---|
| Remove timeout-as-fix; surface true phase | Tasks 1, 6, 7, 13 | No MCP/index timeout constants; owned job and daemon health tests |
| Git hard budget + partial/fresh state | Tasks 4, 13 | Live base status plus `pdg_status` and budget test |
| Split fast status from enrichment | Task 4 | Zero hydration/PDG counter test |
| Structured stale health fields | Tasks 5, 13 | Health JSON and session/generation advisory test |
| Resumable incremental force reindex | Tasks 6, 8 | Owned single-flight generation plus atomic scan/parse/PDG/lexical/neural checkpoints, bucketed parse artifacts with per-file BLAKE3/path validation, validated restart reuse, lexical-before-neural `CURRENT` publication, resident core reload at the publication seam, staged immutable generation directories, watcher publication, and active-generation hydration |
| PDG/TF-IDF are core result layers | Tasks 8, 13 | `run_lexical` publishes a PDG+TF-IDF generation before neural, every registry indexing path reloads the resident handle at that seam, and search/symbol/file/status/context responses expose component status |
| Stale exact source must not inherit stale graph relations | Tasks 2, 10 | Live parser fallback returns `pdg_status="stale"` with empty relations for stale catalog symbol/file reads; focused regressions cover dependency requests |
| Parse checkpoint throughput cliff | Task 8 | Deterministic two-hex bucket artifacts reduce 20k parse durability operations to at most 256 while retaining atomic hash/path validation; current 20k/20k gate passes |
| `max_latency_ms`, `allow_partial` | Task 13 | Common schema/defaults and partial-result test |
| Rust nested modules, impl methods, structs, variants, line ranges | Task 10 | Realistic fixture anchors and source slices |
| Sudo credential context chain | Tasks 12, 13 | Ordered data-dependency path assertion |
| Registry record relative to verification | Tasks 12, 13 | Ordered state-transition path assertion |
| Arch detection chain | Tasks 12, 13 | Ordered environment/dataflow path assertion |
| Hybrid symbol/comment/file-summary chunks | Task 11 | Deduplication/kind/range tests |
| Domain vocabulary | Task 11 | Bounded request context; no hard-coded vocabulary |
| Exact identifier weighting | Tasks 3, 11 | Exact route counter and ranking test |
| Ephemeral review/task context | Task 11 | 16 KiB/32 identifier bounds; no disk caching |
| Cross-function args/env dataflow | Task 12 | FlowFact propagation fixture |
| Install/verify/registry/UI state graph | Task 12 | StateTransition path fixture |
| Command argv/env/stdin edges | Task 12 | Three edge-kind assertions |
| Submodule skip/summarize | Tasks 5, 12 | Inventory excludes descendants safely and indexing emits one bounded Module summary node carrying gitlink commit identity |
| Exact review uses text search/slices | Tasks 3, 13 | Route metadata and zero-neural test |
| Symbol fallback and `symbol_index_miss` | Tasks 2, 10 | Stale catalog live-parser test |
| Git tracked/untracked/submodule/stage candidates | Task 4 | Porcelain-v2 and safe-candidate tests |
| Repeated warning downgraded to badge | Task 13 | Two-session/two-generation test |
| Phase timing/observability | Tasks 1, 5, 13, 14 | `_meta.timings`, health, and JSON p50/p95/RSS/counter exporter implemented; both the 20k/20k smoke and default 100/10 run pass |
| Neural underuse/availability | Tasks 7, 8, 11 | Cold-start hybrid integration test, MIGraphX provider probe, and release memcheck `neural_ms`/worker-RSS evidence; lexical publication remains independently reusable on explicit terminal failure |
| PDG underuse | Tasks 4, 12, 13 | Optional impact plus explicit deep flow expansion |
| Extremely rapid operation and memory behavior | Tasks 4, 5, 9, 14 | Fast-path/mmap/concurrency tests and threshold enforcement pass in both the 20k/20k smoke and default 100/10 report; latest exact-symbol p95 25.061 ms, exact-text 16.593 ms, semantic-hybrid 202.713 ms, with zero enrichment deltas on exact/live paths and one state-driven neural request on semantic work |

## Explicitly rejected designs

- Increasing 30/600-second limits: hides the same ownership and daemon-startup defects.
- Killing `spawn_blocking` work after a deadline: Rust cannot safely cancel it and current behavior proves disk/memory divergence.
- One actor per project: unnecessary once live/catalog paths bypass build state and immutable query generations are shared.
- A new database or vector library: existing SQLite, mmap, and search structures already contain the needed primitives.
- Persisting per-task vocabulary: privacy risk, cache pollution, and not needed for request-scoped exact boosts.
- Rewriting all Rust traversal: nested module recursion already passes; only proven gaps change.
- Auto-staging Git files: advisory candidates satisfy the workflow without mutating user state.
- Treating a progress JSON file as resumability: only atomically reusable artifacts advance phase state.

## Final stop condition

The overhaul is complete only when all workspace tests, packaging tests, npm/PyPI tests, clippy, formatting, `git diff --check`, and `scripts/check-performance.sh` pass; exact/live counter invariants are zero; every notepad row above points to a passing test; an indexing requester can disconnect without affecting the owned job; and TF-IDF/exact/live tools stay available while neural initialization transitions to `Ready` or an explicit `Failed`/`Absent` state without elapsed cancellation.
