# Plan 2 - Targeted B Vector Residency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the targeted-B residency work so LeIndex stops keeping heap mirrors of vector data, row indexes stay stable across mmap-backed updates, resident search caches shrink to row-oriented forms, and the runtime/storage topology stops re-expanding the process after the vector work lands.

**Architecture:** Keep the current daemon shape. Do not introduce a new worker process, a new ANN backend, or any C-phase model distribution work here. The B-phase shape is:

- `MmapEmbeddingIndex` becomes the primary read path for vector data.
- `SearchEngine` flips between `Heap`, `Mmap`, and `MmapWithDelta` sources through `ArcSwap`, with row indexes treated as stable for the life of a file.
- Deleted rows become tombstones; compaction is an explicit atomic rebuild that refreshes every row-indexed dependent structure together.
- Search-side caches stop storing duplicate node metadata and move toward row-keyed, byte-bounded forms.
- Runtime and storage changes are strictly about reducing allocation pressure and connection/thread fan-out, not about changing product behavior.

This plan assumes the measurement harness from Plan 0 exists and will be used as the final exit gate.

**Tech Stack:** Rust 2021, `memmap2`, `arc-swap`, `bitvec`, `dashmap`, `serde`, `serde_json`, `bincode`, `lru`, `tokio`, `rayon`, `rusqlite`, existing `hnsw_rs` / INT8 search code, Linux `/proc`-based memcheck from Plan 0.

**Spec source:** `docs/superpowers/specs/2026-05-13-leindex-memory-reduction-design.md` sections 3, 6.1-6.7, 8.1-8.8, and 9.1-9.8.

**Local exit criteria:**
- `cargo test --test search_vector_mmap` passes and proves the mmap read path can return borrowed slices without cloning.
- `cargo test --test search_residency` passes and proves heap-to-mmap swaps, tombstones, and compaction preserve row stability.
- `cargo test --test cache_budget` passes and proves search/edit/index caches stay under their byte ceilings.
- `cargo test --test cli_runtime_discipline` and `cargo test --test storage_reader_pool` pass and prove the runtime, watcher, and SQLite topology changes are in place.
- `cargo test --test search_int8_gate` passes with the spec thresholds: NDCG@10 drop <= 1%, p50 latency <= baseline +5%, p99 latency <= baseline +10%.
- `cargo xtask memcheck` on `tests/fixtures/memcheck/small_repo/` shows idle RSS <= 100 MiB, peak indexing RSS <= 300 MiB, and peak query RSS <= 180 MiB.
- `cargo test --features full` passes on Linux after all B-phase edits land.

---

## File Structure

**Create:**
- `tests/search_vector_mmap.rs` - borrowed slice and row lookup coverage for `MmapEmbeddingIndex`
- `tests/search_residency.rs` - heap-to-mmap swap, tombstone, and compaction coverage
- `tests/cache_budget.rs` - byte ceiling coverage for search/edit/index caches
- `tests/cli_runtime_discipline.rs` - tokio, rayon, watcher, and MCP session-state coverage
- `tests/storage_reader_pool.rs` - SQLite reader pool and connection-topology coverage
- `tests/search_int8_gate.rs` - INT8 recall/latency gate coverage

**Modify:**
- `Cargo.toml` - add `arc-swap`, `bitvec`, and `dashmap`; wire any test-only helpers needed by the new integration tests
- `src/search/vector.rs` - row-index lookup, borrowed slice access, and deprecated owned-copy shim
- `src/search/search.rs` - mmap-first source switching, row-stable updates, cache compression, and residency accounting
- `src/search/hnsw.rs` - INT8/default-selection touchpoint if the selector lives here
- `src/search/semantic.rs` - `NodeInfo` construction updates after legacy embedding fields move
- `src/cli/index_builder.rs` - `NodeInfo` creation and row-aware search-engine population
- `src/cli/leindex/indexing.rs` - incremental indexing path and row/stability assumptions
- `src/graph/pdg.rs` - snapshot cloning path and embedding serialization reduction
- `src/cli/mcp/edit_cache.rs` - hard byte ceiling for edit previews
- `src/cli/index_cache.rs` - bounded scan/index caches and per-project accounting
- `src/cli/memory.rs` - byte-size accounting hooks for caches and memory telemetry
- `src/bin/leindex.rs` - explicit tokio runtime builder
- `src/parse/parallel.rs` - bounded rayon pool construction
- `src/cli/mcp/server.rs` - `DashMap`-backed session handshake state
- `src/cli/registry.rs` - keep `index_slots` bounded to live projects
- `src/cli/watcher.rs` - keep the 500 ms debounce path and harden it with tests
- `src/storage/schema.rs` - fixed-size SQLite reader pool
- `src/storage/turso_config.rs` - connection-role audit and reader/writer classification
- `src/global/registry.rs` - SQLite initializer audit for the no-surprises pass
- `src/search/quantization/int8_hnsw.rs` - INT8 default promotion gate
- `src/storage/pdg_store.rs` - SQLite initializer audit if it owns a connection role
- `src/storage/nodes.rs` - SQLite initializer audit if it owns a connection role
- `tests/search_hnsw_integration.rs` - update NodeInfo fixtures after the legacy embedding mirror disappears

---

## Task 1: Borrowed mmap reads and row lookup

**Files:**
- Modify: `src/search/vector.rs`
- Create: `tests/search_vector_mmap.rs`
- Modify: `Cargo.toml` if a small helper dependency is needed for the test harness

- [ ] **Step 1: Write the failing tests first**

Create `tests/search_vector_mmap.rs` with coverage for:

- `embedding_slice_by_index(idx: u32) -> Option<&[f32]>`
- `find_node_index(node_id: &str) -> Option<u32>`
- the deprecated `get_embedding(node_id)` shim still returning an owned `Vec<f32>` only for compatibility

Expected assertions:

- the borrowed slice has the correct length and contents
- the row lookup returns the expected `u32`
- the owned-copy shim matches the borrowed slice

- [ ] **Step 2: Verify the legacy `leann_index/` tree stays gone**

Run:

```bash
rg --files src | rg '^src/leann_index/'
```

Expected output:

- no paths are printed
- if a stale caller or module reappears in the tree, remove it before the commit and keep the grep clean

- [ ] **Step 3: Implement the API in `src/search/vector.rs`**

Add the row-oriented methods to `MmapEmbeddingIndex`:

```rust
pub fn embedding_slice_by_index(&self, idx: u32) -> Option<&[f32]>;
pub fn find_node_index(&self, node_id: &str) -> Option<u32>;
```

Keep `get_embedding` as a deprecated convenience wrapper for one compatibility window, but make the hot path use the borrowed slice directly.

The read path should stay strict about row bounds and return `None` for invalid or tombstoned rows.

- [ ] **Step 4: Run the focused test**

Run:

```bash
cargo test --test search_vector_mmap
```

Expected output:

- test binary builds cleanly
- the borrowed-slice assertions pass
- no clone-heavy regressions appear in the new test

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/search/vector.rs tests/search_vector_mmap.rs
git commit -m "feat(search): add borrowed mmap vector access"
```

---

## Task 2: Mmap-first embedding residency and row stability

**Files:**
- Modify: `src/search/search.rs`
- Modify: `src/search/vector.rs`
- Create: `tests/search_residency.rs`

- [ ] **Step 1: Add failing residency tests**

Create `tests/search_residency.rs` with cases that prove:

- `SearchEngine` can swap from heap-backed vectors to mmap-backed vectors after a flush
- `swap_to_mmap()` drops the heap mirror instead of keeping both copies alive
- `MmapWithDelta` resolves delta rows before base mmap rows
- tombstoned rows do not surface through search or row lookup
- compaction invalidates stale cached search results and rebuilds the row map atomically

- [ ] **Step 2: Introduce the source enum and swap surface**

Implement the B-phase source model in `src/search/search.rs`:

```rust
enum EmbeddingSource {
    Heap(VectorIndex),
    Mmap(MmapEmbeddingIndex),
    MmapWithDelta {
        base: MmapEmbeddingIndex,
        delta: Vec<(u32, Vec<f32>)>,
        tombstones: bitvec::vec::BitVec,
    },
}
```

Back it with `ArcSwap<EmbeddingSource>` so the read path can stay lock-light and the rare source flip becomes a whole-object replacement.

Add the explicit row-stability rule to the code and comments:

- assigned rows are append-only
- tombstones are sidecar state, not row renumbering
- compaction is the only operation that reassigns row numbers, and it rebuilds dependent indexes together

- [ ] **Step 3: Make the hot path row-aware**

Change semantic scoring and vector access so the query path resolves `node_id -> row` once and then scores by row index only.

`get_embedding` stays only as a temporary compatibility shim. New code should prefer row slices and avoid allocating owned vectors in the search path.

- [ ] **Step 4: Run the residency test**

Run:

```bash
cargo test --test search_residency
```

Expected output:

- heap-to-mmap swap assertions pass
- tombstone behavior is stable
- compaction rebuild tests pass

- [ ] **Step 5: Commit**

```bash
git add src/search/search.rs src/search/vector.rs tests/search_residency.rs
git commit -m "feat(search): make mmap the primary vector resident path"
```

---

## Task 3: Compress search-side resident state

**Files:**
- Modify: `src/search/search.rs`
- Modify: `src/search/semantic.rs`
- Modify: `src/cli/index_builder.rs`
- Modify: `src/cli/leindex/indexing.rs`
- Modify: `src/graph/pdg.rs`
- Create or extend: `tests/cache_budget.rs`

- [ ] **Step 1: Write the cache/row-budget tests**

Add tests that prove:

- `complexity_cache` no longer stores a duplicate per-node string key when a row value can be derived directly
- `text_index` values are compact row-index lists, not sets of node IDs
- `node_tokens` are compact row-index or token-id lists
- `Arc<str>` node IDs are used where IDs cross structure boundaries
- `SerializablePDG` no longer clones the embedding store as a giant heap mirror

- [ ] **Step 2: Remove the legacy embedding mirror from `NodeInfo`**

Update `NodeInfo` and all its construction sites so the legacy inline embedding mirror disappears:

- `src/search/search.rs`
- `src/search/semantic.rs`
- `src/cli/index_builder.rs`
- `src/cli/leindex/indexing.rs`
- `tests/search_hnsw_integration.rs`

If compatibility is needed during the transition, deserialize old shapes through a repr layer, but always serialize the new layout.

- [ ] **Step 3: Re-key the resident search caches**

Replace `HashSet<String>` values in the text/token caches with compact row-oriented vectors, and keep the per-node token cache in a form that favors intersection/merge over hash iteration.

Drop the redundant complexity cache if the row-indexed node data already provides the same answer with less memory.

Keep retained source content out of resident search state: after indexing, `NodeInfo.content` should stay cleared, and anything larger than 4 KiB should be rendered from byte ranges or re-read lazily instead of being cached in memory.

- [ ] **Step 4: Reduce snapshot cloning**

In `src/graph/pdg.rs`, change the serialization path so it no longer clones embeddings wholesale just to cross the snapshot boundary. Prefer a flat store or streaming row serialization over borrowed-trick complexity.

- [ ] **Step 5: Run the cache-budget test**

Run:

```bash
cargo test --test cache_budget
```

Expected output:

- compact-cache assertions pass
- node ID interning assertions pass
- snapshot clone reduction assertions pass

- [ ] **Step 6: Commit**

```bash
git add src/search/search.rs src/search/semantic.rs src/cli/index_builder.rs src/cli/leindex/indexing.rs src/graph/pdg.rs tests/cache_budget.rs
git commit -m "refactor(search): compress resident metadata and snapshots"
```

---

## Task 4: Byte ceilings for caches and memory accounting

**Files:**
- Modify: `src/search/search.rs`
- Modify: `src/cli/mcp/edit_cache.rs`
- Modify: `src/cli/index_cache.rs`
- Modify: `src/cli/memory.rs`
- Extend: `tests/cache_budget.rs` if the cache-cap cases fit there cleanly

- [ ] **Step 1: Put an actual byte ceiling on the search cache**

Move `search_cache` from an entry-count-only structure to a byte-bounded cache with a small entry limit and a hard value budget.

Spec target:

- `max_entries = 256`
- `max_bytes = 16 MiB`
- value-byte accounting recorded on insert so the cache can evict before it runs hot

- [ ] **Step 2: Cap edit preview payloads**

In `src/cli/mcp/edit_cache.rs`, reject oversized edit-preview entries instead of letting them balloon the hot cache.

Spec target:

- per-entry hot payload cap: 256 KiB
- total edit cache cap: 8 MiB

- [ ] **Step 3: Bound index caches and expose accounting**

In `src/cli/index_cache.rs`, keep `project_scan` and `file_stats_cache` from growing without an owner. The cache should remain per-project and report size instead of silently expanding.

In `src/cli/memory.rs`, add accounting hooks so cache users can report byte estimates without inventing a second eviction control plane.

- [ ] **Step 4: Run the cache-budget test**

Run:

```bash
cargo test --test cache_budget
```

Expected output:

- search cache ceilings hold
- edit cache ceilings hold
- index cache accounting tests pass

- [ ] **Step 5: Commit**

```bash
git add src/search/search.rs src/cli/mcp/edit_cache.rs src/cli/index_cache.rs src/cli/memory.rs tests/cache_budget.rs
git commit -m "perf(memory): bound search and edit caches"
```

---

## Task 5: Runtime, watcher, and SQLite discipline

**Files:**
- Modify: `src/bin/leindex.rs`
- Modify: `src/parse/parallel.rs`
- Modify: `src/cli/leindex/indexing.rs`
- Modify: `src/cli/mcp/server.rs`
- Modify: `src/cli/registry.rs`
- Modify: `src/cli/watcher.rs`
- Modify: `src/storage/schema.rs`
- Modify: `src/storage/turso_config.rs`
- Modify: `src/global/registry.rs`
- Create: `tests/cli_runtime_discipline.rs`
- Create: `tests/storage_reader_pool.rs`

- [ ] **Step 1: Replace the default tokio macro with an explicit runtime**

Change `src/bin/leindex.rs` so the daemon uses a manually configured multi-thread runtime instead of the blanket `#[tokio::main]` default.

Spec target:

- worker threads: 2
- blocking threads: 8
- stack size: 1 MiB

Keep short bridges as `spawn_blocking`, but avoid using the async executor as a place to run long CPU-bound work.

- [ ] **Step 2: Bound rayon and parsing work**

In `src/parse/parallel.rs`, construct the rayon pool explicitly with the small stack size from the spec and a modest thread cap.

Wire the pool through the indexing path in `src/cli/leindex/indexing.rs` so parse/index work uses the bounded pool instead of the global default.

- [ ] **Step 3: Replace the MCP session mutex hotspot**

Switch `src/cli/mcp/server.rs` from `Arc<Mutex<HashMap<...>>>` to `DashMap` for the per-session handshake state.

Keep `src/cli/registry.rs` as the legitimate per-project coordination point, but cap the project map to live projects only.

- [ ] **Step 4: Keep watcher coalescing and prove it**

`src/cli/watcher.rs` already has a 500 ms coalescing window; this task keeps that shape and adds tests so the debounce stays a batch, not a per-event reindex storm.

- [ ] **Step 5: Give SQLite a fixed reader pool**

Move storage access in `src/storage/schema.rs` to the fixed reader-pool shape from the spec, then audit `src/storage/turso_config.rs`, `src/global/registry.rs`, and any other SQLite connection initializers to make sure each connection is using the right role and cache behavior.

- [ ] **Step 6: Run the runtime/storage tests**

Run:

```bash
cargo test --test cli_runtime_discipline
cargo test --test storage_reader_pool
```

Expected output:

- runtime-builder assertions pass
- watcher debounce assertions pass
- MCP session-state assertions pass
- SQLite reader-pool assertions pass

- [ ] **Step 7: Commit**

```bash
git add src/bin/leindex.rs src/parse/parallel.rs src/cli/leindex/indexing.rs src/cli/mcp/server.rs src/cli/registry.rs src/cli/watcher.rs src/storage/schema.rs src/storage/turso_config.rs src/global/registry.rs tests/cli_runtime_discipline.rs tests/storage_reader_pool.rs
git commit -m "perf(runtime): tighten executor and sqlite topology"
```

---

## Task 6: INT8 promotion gate and final proof

**Files:**
- Modify: `src/search/quantization/int8_hnsw.rs`
- Modify: `src/search/search.rs` or `src/search/hnsw.rs` if the default selector lives there
- Create: `tests/search_int8_gate.rs`

- [ ] **Step 1: Write the quality gate before changing the default**

Create `tests/search_int8_gate.rs` so the INT8 path is only promoted if the quality gate passes on the fixture set.

The gate should print and compare:

- NDCG@10
- MRR@10
- p50 latency
- p99 latency

Spec thresholds:

- NDCG@10 drop <= 1%
- p50 latency <= baseline +5%
- p99 latency <= baseline +10%

- [ ] **Step 2: Wire the promotion logic**

In `src/search/quantization/int8_hnsw.rs`, make the INT8 path the opt-in default only after the gate passes.

Keep the FP32 path available behind a benchmark-only feature flag so regressions can be measured against a stable baseline.

- [ ] **Step 3: Run the INT8 gate**

Run:

```bash
cargo test --test search_int8_gate
```

Expected output:

- the gold-query metrics print
- the threshold comparison passes
- the default promotion path stays off until the gate is green

- [ ] **Step 4: Run the Plan 0 memcheck harness as the final residency proof**

Run:

```bash
cargo xtask memcheck
```

Expected output:

- idle RSS stays at or below 100 MiB
- peak indexing RSS stays at or below 300 MiB
- peak query RSS stays at or below 180 MiB
- no phase regresses beyond the committed baselines

- [ ] **Step 5: Commit**

```bash
git add src/search/quantization/int8_hnsw.rs src/search/search.rs src/search/hnsw.rs tests/search_int8_gate.rs
git commit -m "feat(search): gate INT8 promotion on quality metrics"
```

---

## Risks & Mitigations

- **Row-stability bugs can silently corrupt search results.** Mitigation: keep the append-only invariant explicit, test tombstones directly, and require compaction to rebuild every row-indexed dependent structure in one atomic step.
- **Borrowed mmap slices can regress into hidden clones.** Mitigation: keep `get_embedding` as a compatibility shim only, and assert the hot path uses `embedding_slice_by_index` in tests.
- **INT8 recall may not meet the promotion gate.** Mitigation: leave the FP32 path intact and keep the promotion opt-in until the gate stays green.
- **Runtime changes can shift latency or startup shape.** Mitigation: keep the thread caps small, run the runtime tests after each batch, and finish with the memcheck harness on the small fixture.
- **SQLite topology changes can create lock contention if the pool is too large.** Mitigation: start with two readers, not more, and only revisit if tests or profiling show starvation.
- **`src/global/registry.rs` and other SQLite initializers can drift out of the audit.** Mitigation: keep the initializer audit in the checklist instead of assuming one storage path covers all of them.

---

## Appendix - Repo Touchpoints

- `src/search/vector.rs:543, 596` - row lookup and borrowed slice access
- `src/search/search.rs:165-210, 375-409, 481-566, 611-739, 755-996, 1248-1260` - `NodeInfo`, cache compression, vector residency, and memory accounting
- `src/search/hnsw.rs` - INT8 default-selection touchpoint if the selector lives here
- `src/search/semantic.rs` - `NodeInfo` constructors that must stop supplying the legacy mirror
- `src/cli/index_builder.rs:998-1243` - node construction and search-engine population
- `src/cli/leindex/indexing.rs:65, 447, 569, 717` - indexing path and row-stability assumptions
- `src/graph/pdg.rs:425-579, 1319-1337` - snapshot serialization and embedding-store clone reduction
- `src/cli/mcp/edit_cache.rs` - edit-preview byte ceiling
- `src/cli/index_cache.rs` - project scan and file stats bounds
- `src/cli/memory.rs` - cache accounting hooks
- `src/bin/leindex.rs:7` - runtime builder entrypoint
- `src/parse/parallel.rs:1-220` - bounded rayon pool
- `src/cli/mcp/server.rs:1-120` - session handshake state
- `src/cli/registry.rs:1-260` - live-project coordination
- `src/cli/watcher.rs:1-60` - 500 ms debounce
- `src/storage/schema.rs` and `src/storage/turso_config.rs` - SQLite reader/writer topology
- `src/global/registry.rs:117` - SQLite initializer audit point
- `src/storage/pdg_store.rs` and `src/storage/nodes.rs` - additional SQLite initializer audit points if they own connection setup
- `src/search/quantization/int8_hnsw.rs` - INT8 promotion gate
- `tests/search_hnsw_integration.rs` - fixtures that need the `NodeInfo` shape updated
