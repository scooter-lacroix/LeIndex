# Plan 1 - A+ Surgical Wins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the easy RSS wins first: prune dependency weight, lower hard cache ceilings, remove duplicate in-memory representations, and narrow Tokio / MCP / registry hot spots without introducing the B-phase structural rewrites.

**Architecture:** Keep the current single-process architecture and make only in-place cuts. This plan does not introduce the flat embedding store, mmap swap, worker process, or other targeted-B/C changes from the design spec. The shape change here is intentionally surgical: smaller dependency graph, smaller SQLite page caches, smaller hot caches, fewer owned copies, and a narrower runtime footprint.

**Tech Stack:** Rust 2021, tokio, serde / serde_json, rusqlite, axum 0.7, tower 0.5, dashmap, lru, onnxruntime via `ort`, std filesystem / `/proc`, and the existing `cargo xtask memcheck` harness from Plan 0.

**Spec source:** `docs/superpowers/specs/2026-05-13-leindex-memory-reduction-design.md` sections 2, 5.1-5.8, 8.1-8.8, and 9.1, 9.5-9.7, with the A+ scope limited to low-risk cuts and compatibility-preserving refactors.

**Local exit criteria:**
- `cargo check --features full` passes.
- `cargo check --features "full turso"` passes, proving the libsql path is opt-in only.
- `cargo check --features "full onnx"` passes, proving the ONNX idle-unload path still compiles.
- `cargo test -p leindex` passes for the touched modules after updating assertions and compatibility fixtures.
- `cargo xtask memcheck` on `tests/fixtures/memcheck/small_repo/` lands in the after-A+ band from the design spec: idle RSS `<= 180 MiB`, peak indexing RSS `<= 450 MiB`, and peak query RSS `<= 250 MiB`, with no phase over committed baseline `+ 5%` or absolute ceiling `+ 10%`.
- Any intentional baseline refresh is accompanied by the required PR note for the memcheck metadata gate.

---

## File Structure

**Modify:**
- `Cargo.toml` - slim Tokio features, add `turso` gating, remove the `axum-06` / `tower-04` alias stack, and add any small dependency needed for the MCP session map cleanup.
- `src/bin/leindex.rs` - replace `#[tokio::main]` with an explicit runtime builder so the daemon can run with smaller worker and blocking pools.
- `src/global/registry.rs` - lower the global SQLite cache pragma.
- `src/storage/schema.rs` - express the per-project SQLite cache caps and mmap cap in the storage open path.
- `src/storage/turso_config.rs` - stay behind the `turso` feature gate only.
- `src/search/search.rs` - remove the legacy embedding alias, trim SearchEngine caches, and reduce duplicate owned state.
- `src/search/vector.rs` - update `SearchResult` and any node-id handling that needs `Arc<str>` interning.
- `src/search/ranking.rs` - update score result node-id ownership if the interning change crosses this boundary.
- `src/search/onnx/qwen.rs` - convert the session to `Option<Session>` and add explicit unload support.
- `src/search/onnx/reranker.rs` - same session lifecycle change for the reranker.
- `src/cli/index_builder.rs` - stop re-cloning TF-IDF embeddings where a slice will do, and hook idle unload after batch work.
- `src/cli/leindex/indexing.rs` - call unload at the end of the indexing batch flush path.
- `src/cli/memory.rs` - lower the default spill threshold and cache cap.
- `src/cli/index_cache.rs` - audit and cap any symbol / file caches that are still size-unbounded.
- `src/cli/mcp/edit_cache.rs` - add a hard byte ceiling to the edit preview cache.
- `src/cli/mcp/server.rs` - move session handshake state off a coarse `Mutex<HashMap<...>>`.
- `src/cli/registry.rs` - cap `index_slots` to live projects and evict stale slots on unregister.
- `src/edit/mod.rs` - update the `StorageConfig` test helper if the cache-size type changes.

**Tests:**
- `src/global/registry.rs` `#[cfg(test)]` module for SQLite pragma assertions.
- `src/storage/schema.rs` `#[cfg(test)]` module for writer / reader cache caps and mmap cap.
- `src/search/search.rs` `#[cfg(test)]` module for cache shape, compatibility, and interning behavior.
- `src/search/onnx/qwen.rs` and `src/search/onnx/reranker.rs` `#[cfg(test)]` modules for the unload path.
- `src/cli/memory.rs` `#[cfg(test)]` module for the lowered defaults.
- `src/cli/mcp/server.rs` and `src/cli/registry.rs` tests for the lock cleanup and slot eviction behavior.

---

## Task 1: Dependency and runtime feature slimming

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/bin/leindex.rs`
- Modify: `src/storage/turso_config.rs`
- Modify: `src/cli/mcp/server.rs`

**Step 1: Rewrite the Cargo feature graph**

Update `Cargo.toml` so the default `full` feature no longer drags `libsql` and `rusqlite_migration` in by default. Add a dedicated `turso = ["dep:libsql", "dep:rusqlite_migration"]` feature instead, and leave `storage` on the local SQLite path. Replace the `mcp-server` alias block so it points at `axum` 0.7, `tower` 0.5, and `tower-http` 0.5 instead of the 0.6 / 0.4 aliases. Add `dashmap` as the small extra dependency needed for the session state map.

Also shrink the Tokio dependency from `full` to the explicit runtime set the spec calls for, while keeping dev-dependencies broad enough for tests.

- [ ] Expected check: `cargo tree -e features -p leindex --features mcp-server` shows `axum v0.7` / `tower v0.5` and no `axum v0.6` or `tower v0.4` aliases.

**Step 2: Port the MCP stack to the unified HTTP stack**

Update `src/cli/mcp/server.rs` to use the axum 0.7 / tower 0.5 API surface. That means removing the old alias imports, updating router / response types where needed, and keeping the transport behavior the same. The point is dependency hygiene and a smaller graph, not a behavioral rewrite.

- [ ] Expected check: `cargo check --features full` compiles the MCP code without any `axum_06` or `tower_04` references.

**Step 3: Replace the top-level Tokio macro with an explicit runtime**

Change `src/bin/leindex.rs` from `#[tokio::main]` to a hand-built runtime:

- 2 worker threads.
- 1 MiB thread stack.
- `max_blocking_threads(8)`.

This is the narrowest way to land the runtime-budget win from the spec without touching the rest of the async architecture.

- [ ] Expected check: the binary still starts, and `cargo check --features full` finishes cleanly.

**Step 4: Commit**

```bash
git add Cargo.toml src/bin/leindex.rs src/storage/turso_config.rs src/cli/mcp/
git commit -m "feat(mem): slim runtime and MCP dependency graph"
```

---

## Task 2: SQLite cache caps and in-memory cache discipline

**Files:**
- Modify: `src/global/registry.rs`
- Modify: `src/storage/schema.rs`
- Modify: `src/cli/memory.rs`
- Modify: `src/cli/index_cache.rs`
- Modify: `src/cli/mcp/edit_cache.rs`
- Modify: `src/edit/mod.rs` if the storage config shape changes

**Step 1: Lower the global registry cache**

In `src/global/registry.rs`, change the registry connection pragma from the current positive page count to `cache_size = -2000` and keep `mmap_size` at zero for that connection. This keeps the global registry deliberately thin.

- [ ] Expected check: a targeted registry test confirms the pragma values on open.

**Step 2: Give the project-store open path explicit writer / reader caps**

Update `src/storage/schema.rs` so the SQLite connection setup can express the spec's asymmetric cache plan:

- writer cache: `cache_size = -16000`
- reader cache: `cache_size = -2000`
- shared project-store mmap cap: `mmap_size = 67108864`

If the current `StorageConfig.cache_size_pages: Option<usize>` cannot encode the negative SQLite cache size form cleanly, widen that type or add an explicit signed field before wiring the pragma calls. The plan should prefer a small type change over a second ad hoc config path.

Keep the existing `edit/mod.rs` test helper in sync if the type signature changes.

- [ ] Expected check: `cargo test -p leindex storage::schema` passes with assertions for the new pragma values.

**Step 3: Lower the memory-manager defaults**

In `src/cli/memory.rs`, change `MemoryConfig::default()` to:

- `spill_threshold = 0.75`
- `max_cache_bytes = 96_000_000`

Update the default-assertion tests in the same file so they codify the new numbers.

- [ ] Expected check: `cargo test -p leindex memory` passes and the default assertions match the new values.

**Step 4: Put byte ceilings on the remaining hot caches**

Cap the caches that still grow by project size rather than by explicit byte budget:

- `src/cli/mcp/edit_cache.rs`: hard cap the edit preview cache at 8 MiB total and reject individual entries above 256 KiB.
- `src/cli/index_cache.rs`: audit every in-memory cache in the subsystem and give any symbol / file cache that can scale with project size an explicit hard ceiling.

This is a guardrail task. The exact eviction policy can stay simple as long as the ceiling is enforced synchronously on insert.

- [ ] Expected check: dedicated cache tests continue to pass, and a new oversized-entry test fails before the file ever hits disk.

**Step 5: Commit**

```bash
git add src/global/registry.rs src/storage/schema.rs src/cli/memory.rs src/cli/index_cache.rs src/cli/mcp/edit_cache.rs src/edit/mod.rs
git commit -m "feat(mem): cap SQLite and hot cache residency"
```

---

## Task 3: SearchEngine de-dup and NodeInfo compatibility

**Files:**
- Modify: `src/search/search.rs`
- Modify: `src/search/vector.rs`
- Modify: `src/search/ranking.rs`
- Modify: `src/cli/index_builder.rs`

**Step 1: Remove the legacy embedding alias**

In `src/search/search.rs`, drop the old `NodeInfo.embedding` field and add an internal `NodeInfoRepr` deserializer that accepts both shapes:

- new shape: `tfidf_embedding: Vec<f32>` and `neural_embedding: Option<Vec<f32>>`
- old shape: `embedding: Option<Vec<f32>>`

Deserialize by preferring the new `tfidf_embedding` when present and non-empty, otherwise promote the old `embedding` value if it exists, otherwise default to empty. Always serialize only the new layout.

This is the compatibility bridge the spec calls for, and it keeps the old index files readable for one minor release.

- [ ] Expected check: a round-trip test for both the legacy and new payload shapes passes.

**Step 2: Stop cloning TF-IDF vectors twice**

Remove the `node.embedding = Some(node.tfidf_embedding.clone())` write path and change the insertion path so it borrows `&[f32]` whenever the downstream API allows it.

That gives the A+ de-dup win the spec is aiming for without changing the search result semantics.

- [ ] Expected check: the search tests that used to depend on the alias still pass, but the clone-only path is gone.

**Step 3: Compress the internal search indexes**

Replace the current `SearchEngine` maps in `src/search/search.rs` as follows:

- `complexity_cache: HashMap<String, u32>` -> row-indexed `Vec<u32>`
- `text_index: HashMap<String, HashSet<String>>` -> `HashMap<String, Vec<u32>>`
- `node_id_to_idx: HashMap<String, usize>` -> `HashMap<Arc<str>, u32>` or the equivalent row-indexed shape
- `node_tokens: HashMap<String, HashSet<String>>` -> a compact row-indexed token store
- `search_cache: LruCache<String, Vec<SearchResult>>` -> a byte-bounded cache with estimated value sizes and a hard ceiling of 16 MiB, plus a `max_entries` guard of 256

Intern `node_id` as `Arc<str>` wherever it crosses a boundary that was previously cloning strings. Keep the public result shape stable if the leaf API needs to stay on `String`, but do not leave the internal lookups on owned strings if a pointer copy will do.

- [ ] Expected check: `cargo test -p leindex search::tests` still passes, including the node lookup and token-scoring tests.

**Step 4: Keep the content-clear behavior intact**

Do not regress the existing content-clearing behavior after indexing. The point of this task is to reduce duplicate ownership in the hot search state, not to reintroduce persistent source blobs.

- [ ] Expected check: the search tests that depend on post-index content clearing still pass.

**Step 5: Commit**

```bash
git add src/search/search.rs src/search/vector.rs src/search/ranking.rs src/cli/index_builder.rs
git commit -m "feat(mem): remove search-side duplication and legacy embedding shape"
```

---

## Task 4: ONNX idle unload and runtime hot-spot cleanup

**Files:**
- Modify: `src/search/onnx/qwen.rs`
- Modify: `src/search/onnx/reranker.rs`
- Modify: `src/cli/index_builder.rs`
- Modify: `src/cli/leindex/indexing.rs`
- Modify: `src/cli/mcp/server.rs`
- Modify: `src/cli/registry.rs`

**Step 1: Make the ONNX session lifecycle droppable**

Change `QwenEmbeddingProvider` and `QwenReranker` from `Arc<Mutex<Session>>` to `Arc<Mutex<Option<Session>>>`. Add `unload(&self)` to both types so the inner session can be dropped explicitly, and make the next `embed()` or `rerank()` lazily rebuild the session.

The goal is simple: after an indexing burst, the idle daemon should be able to give the model memory back to the allocator instead of holding it forever.

Add a tiny test-only helper if needed so the unload path can be exercised without a real model file.

- [ ] Expected check: `cargo check --features "full onnx"` still compiles the provider modules, and the unload unit tests pass without requiring bundled model assets.

**Step 2: Call unload after the batch flush path**

In `src/cli/index_builder.rs` and `src/cli/leindex/indexing.rs`, call `unload()` after the batch flush that finishes an indexing pass. Keep the behavior behind the `onnx` feature so the TF-IDF-only path stays unchanged.

- [ ] Expected check: the indexing path still produces the same outputs, but the ONNX session is not held open after the batch finishes.

**Step 3: Move the MCP handshake map off a coarse mutex**

In `src/cli/mcp/server.rs`, replace `session_handshakes: Arc<Mutex<HashMap<...>>>` with `DashMap` so per-session state does not funnel through one global lock. Keep the semantics identical, only narrow the contention shape.

- [ ] Expected check: the MCP server tests still pass, and the code no longer relies on the coarse handshake mutex.

**Step 4: Cap registry slot growth to live projects**

In `src/cli/registry.rs`, keep `index_slots`, but make sure stale slots are evicted when a project unregisters and that the map only reflects live projects. This is a small but real memory win in long-running multi-project sessions.

- [ ] Expected check: a registry lifecycle test confirms that unregistering a project removes its slot bookkeeping.

**Step 5: Leave the watcher debounce alone**

`src/cli/watcher.rs` already has the 500 ms debounce the spec wants. Keep that behavior intact while the surrounding lock and lifecycle work moves around it.

- [ ] Expected check: no watcher test changes are needed beyond keeping the existing debounce invariant green.

**Step 6: Commit**

```bash
git add src/search/onnx/qwen.rs src/search/onnx/reranker.rs src/cli/index_builder.rs src/cli/leindex/indexing.rs src/cli/mcp/server.rs src/cli/registry.rs
git commit -m "feat(mem): unload ONNX sessions and trim MCP contention"
```

---

## Task 5: Final verification and handoff

**Files:**
- All touched files above, plus the Plan 0 memcheck surface.

**Step 1: Run the full touched-module suite**

Run:

```bash
cargo check --features full
cargo check --features "full turso"
cargo check --features "full onnx"
cargo test -p leindex
```

Expected:

- no unresolved old alias dependencies
- no regression in the touched unit tests
- no compile break from the ONNX unload path or the `turso` gate

**Step 2: Re-run memcheck**

Run:

```bash
cargo xtask memcheck
```

Expected:

- every phase stays within the after-A+ band from the design spec
- any regression is visible as a failing diff against the committed baselines
- the resulting RSS profile is lower than the pre-A+ baseline on the small fixture

**Step 3: Refresh baselines only if the change is intentional**

If the memory win is real and the plan owner wants the committed baselines to move, run the baseline update path from Plan 0 and include the required PR note. Do not casually refresh baselines just to hide a regression.

**Step 4: Final commit**

```bash
git commit -m "test(mem): verify A+ memory cuts and cache caps"
```

---

## Risks & Mitigations

- Tightening cache caps can expose hidden growth elsewhere. Mitigation: keep the caps hard, then use memcheck and the touched-module tests to find the next real hot spot instead of quietly loosening the cap.
- The `NodeInfo` compatibility bridge can accidentally prefer the wrong field if it is too clever. Mitigation: make the deserializer preference explicit, keep the one-minor read-compat window, and add fixtures for both shapes.
- ONNX unload can turn into session churn if it is called too aggressively. Mitigation: only unload after a batch flush or clearly idle point, then lazy-reload on demand.
- The MCP stack port can be noisy because of alias churn. Mitigation: change the manifest first, then port the handlers file-by-file and keep the compile check in the loop.
- The runtime-builder change in `src/bin/leindex.rs` can subtly affect blocking behavior. Mitigation: keep the blocking pool small but not tiny, and verify the daemon still starts cleanly under the existing CLI paths.

## Open Items

- Do not start the B-phase flat-store or mmap rewrite here. Those belong to the targeted-B plan.
- Do not introduce a worker process for ONNX here. The in-process unload win is the only A+ model-lifecycle change in scope.
- Do not build a general-purpose centralized eviction controller here. The A+ plan only needs hard caps and local hygiene.
