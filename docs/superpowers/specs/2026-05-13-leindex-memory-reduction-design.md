# LeIndex Memory Reduction & R15 Interleave — Design Spec

- **Date:** 2026-05-13
- **Status:** Design — approved through brainstorming, awaiting user file review prior to plan-writing.
- **Owner:** LeIndex maintainers
- **Supersedes:** sections of `docs/R15_MODEL_DISTRIBUTION.md`, `docs/R15_IMPLEMENTATION_SUMMARY.md` "What Remains".

---

## 1. Problem

Single LeIndex daemon, 54-file project: 442 MiB idle, 700 MiB+ during indexing. Goal: drastically cut RSS while retaining or improving capability. Outstanding R15 tasks (3-16) for ONNX model bundling fold into the same plan rather than ship separately and then be ripped out.

## 2. Goals & Non-Goals

### Goals
- Reduce idle RSS to ≤ 100 MiB and peak indexing RSS to ≤ 300 MiB on a small project (target B from brainstorming).
- Preserve all current functionality. Optimizations may merge or restructure subsystems if equal/greater capability is retained.
- Bring R15 ONNX model distribution online without violating the memory targets.
- Establish a measurement & CI regime that prevents future bloat.

### Non-Goals
- New ANN backend (no usearch, qdrant, etc.).
- On-disk HNSW (existing INT8 HNSW addressed first).
- Removing the optional remote-embedding path.
- Continuous production allocation tracing.
- GPU acceleration as a launch requirement (designed for, deferred to C-phase).

## 3. Approach (sequencing)

A+ → targeted B → C (planned, deferred until after B lands).

**A+ — low-risk surgical wins:** dependency hygiene, cache caps, trivial dedup.
**Targeted B — narrow structural moves:** make existing mmap + INT8 paths the actual residency strategy; kill heap mirrors and serialization clones.
**C — out-of-process model worker:** ONNX runtime moves to a separate `leindex-embed` worker exe so the main daemon never resident-holds model weights. Includes ORT execution-provider plumbing (CPU/CUDA/etc.) for product-direction GPU work.

Algorithm/default flips (e.g., INT8 HNSW becoming default) require quality measurement gates before promotion.

---

## 4. Section 1 — Memory Budget & Accounting

### 4.1 Targets

Targets apply to the small-fixture path (`tests/fixtures/memcheck/small_repo/`) used by CI. Larger-codebase scaling formulas deferred until measurements exist.

| Phase | Idle RSS | Peak indexing RSS | Peak query RSS |
|---|---|---|---|
| Pre-work baseline (today) | 442 MiB | 700 MiB | TBD measure |
| After A+ | ≤ 180 MiB | ≤ 450 MiB | ≤ 250 MiB |
| After targeted B | ≤ 100 MiB | ≤ 300 MiB | ≤ 180 MiB |
| With C active | 100 MiB main + worker only during embed pass | 300 MiB combined | 180 MiB main |

### 4.2 Accounting

- **Primary metric:** Linux RSS via `/proc/self/status` `VmRSS`.
- **Secondary on Linux:** PSS via `/proc/self/smaps_rollup`; mapped_file vs anon split where available (mmap-heavy phases will distort raw RSS otherwise).
- **macOS:** `mach_task_basic_info.resident_size`.
- **Windows:** `GetProcessMemoryInfo.WorkingSetSize`.
- **Optional heap profiler:** `--features memprof` activates `dhat-rs` global allocator. Off by default.

### 4.3 Cache cap default

`MemoryConfig::default.max_cache_bytes` lowers from 500 MB → **96 MiB** (provisional; revisit after A+ data). `spill_threshold` 0.85 → 0.75.

### 4.4 Verification regime

- Per-phase canonical baseline: `docs/memory/baselines/<fixture>/<phase>.json`. One file per phase per fixture; date inside the JSON payload, **not in the filename**. Overwrite intentionally.
- Absolute budget config: `docs/memory/budgets/current.json` — single source of truth read by CI.
- Regression rule: PR fails if any phase exceeds (committed baseline + 5%) **or** (current.json ceiling + 10%).

---

## 5. Section 2 — A+ Subsystem Deletions & Cap Cuts

### 5.1 Tokio feature slim
- Replace `tokio = { version = "1.40", features = ["full"] }` with explicit feature list: `["rt-multi-thread","macros","fs","net","sync","time","signal","io-util"]`.
- Dev-dependencies retain `["full"]` for tests.
- Sized expectation: modest RSS win, clear binary/dep-graph hygiene win.

### 5.2 SQLite cache caps

Sized per connection role. **Writer connections own the bulk of cache; readers are thin.** This is the single source of truth for SQLite cache pragmas; Section 9.4 reader pool inherits these numbers, does not override.

- `src/global/registry.rs:117` global registry connection: `cache_size = -2000` (2 MiB). Single connection, rare access.
- Per-project store **writer** connection: `cache_size = -16000` (16 MiB). Hot path; primary residency for write-side page cache.
- Per-project store **reader** connections (added in 9.4): `cache_size = -2000` (2 MiB) each. Pool of 2 readers → 4 MiB total reader cache. Readers are point-lookup heavy; large per-reader cache is wasted.
- `mmap_size = 0` for global registry; `mmap_size = 67108864` (64 MiB cap) for project store (writer and readers share the OS-level mmap region).
- Total SQLite resident budget at steady state: 2 (global) + 16 (project writer) + 2×2 (project readers) = 22 MiB. Plus the shared 64 MiB mmap cap, of which actual touched pages count toward `mapped_file` not anon.
- **Audit all SQLite connection initializers** before claiming win — there may be additional sites.

### 5.3 libsql disposition — Option β (feature-gate)
- Currently zero `libsql::` usages in `src/`; carried in `Cargo.toml` and `src/storage/turso_config.rs`.
- Action: introduce feature `turso = ["dep:libsql","dep:rusqlite_migration"]`, off by default, **removed from `full`**.
- Delete only after a release window confirms no consumer.

### 5.4 Unify on axum 0.7 / tower 0.5
- Port `src/cli/mcp/*` from axum 0.6 → 0.7 (handlers, extractors, middleware, layers).
- Remove `axum-06`, `tower-04`, `tower-http-04`, plus the `http-body-util`, `http`, `bytes` aliases listed in the Cargo.toml MCP block.
- Repo's own Cargo.toml comments (around line 290) already flag this as scheduled debt.
- Sized expectation: modest RSS win; significant binary/build-graph win.

### 5.5 Embedding triplet — A+ scope (de-dup only)
- `NodeInfo` today (`src/search/search.rs:184-193`) carries `tfidf_embedding`, optional `neural_embedding`, and a legacy `embedding` alias. Plus `vector_index.insert(node.tfidf_embedding.clone())` (line 534) is a third copy.
- A+ actions:
  - Remove the legacy `embedding` field. **A `serde(alias)` is insufficient** — old `embedding` is `Option<Vec<f32>>`, new `tfidf_embedding` is plain `Vec<f32>`; the shapes do not unify.
  - Compatibility mechanism (mandatory):
    1. Introduce an internal `NodeInfoRepr` deserializer struct that accepts **both** shapes:
       - new: `tfidf_embedding: Vec<f32>` (default empty), `neural_embedding: Option<Vec<f32>>`.
       - old: `embedding: Option<Vec<f32>>` (mapped into `tfidf_embedding` if `Some(non_empty)`, else empty), `tfidf_embedding` may also be missing.
    2. Implement `Deserialize for NodeInfo` via this repr. Resolution rule: prefer `tfidf_embedding` if present and non-empty; otherwise promote `embedding` value (if present and non-empty); otherwise default empty.
    3. Always serialize the **new** layout. Old field name never written again.
    4. Compatibility window: one minor release. After that minor, the legacy-shape branch is removed and a one-shot migration tool (`leindex migrate-index`) is provided as the upgrade path.
  - Stop the alias write `node.embedding = Some(node.tfidf_embedding.clone())` at `src/search/search.rs:544`.
  - Where the vector path can take `&[f32]` instead of cloning, switch.
  - Audit whether `neural_embedding` should remain on `NodeInfo` at all given the active search path ignores it (full relocation handled in B-phase 6.4).
- Full flat `EmbeddingStore` redesign deferred to B-phase.

### 5.6 Hard cache caps + watermark
- `MemoryConfig::default { max_cache_bytes: 96_000_000, spill_threshold: 0.75 }`.
- Synchronous LRU eviction on insert when over `max_cache_bytes` — do not rely on the 30 s spill check to bound runaway.

### 5.7 ONNX provider lifecycle (in-process)
- `QwenEmbeddingProvider` and `QwenReranker` switch from `Arc<Mutex<Session>>` to `Arc<Mutex<Option<Session>>>`.
- Add `unload(&self)` that drops the inner `Session`. Reload lazily on next `embed()`.
- Indexing pipeline calls `unload()` after batch flush.
- Process-isolation worker (C-phase) is the long-term answer; this is the cheap A+ idle win when the `onnx` feature is compiled.

### 5.8 Audit-but-defer
- `src/leann_index/` legacy on-disk JSON store: deletion handled in B-phase (6.3) after grep-clean confirmation.
- `src/cli/memory.rs` (1478 LOC): only the cap defaults change in A+. The module is a real spill manager, not bloat.

---

## 6. Section 3 — Targeted B: Vector Residency

Frame: no new architecture. Wire mmap + INT8 paths that already exist; drop heap mirrors after build.

### 6.1 by-index slice API on MmapEmbeddingIndex

`MmapEmbeddingIndex.get_embedding` (`src/search/vector.rs:543`) returns `Option<Vec<f32>>` — copies on read; `find_node_index` (`src/search/vector.rs:596`) is linear today.

- Add `embedding_slice_by_index(idx: u32) -> Option<&[f32]>` and `find_node_index(node_id: &str) -> Option<u32>` as separate operations.
- Hot scoring path uses row-index access only; ID lookup happens once per query.
- `get_embedding` retained for one minor as a deprecated owned-copy convenience.
- Slice lifetime tied to `&MmapEmbeddingIndex`. No `Arc<Mmap>` clone-handles unless async ownership later forces it.

### 6.2 mmap as primary read path (3a-lite)

Independent of the full flat-store rewrite.

- `SearchEngine` gains `embeddings: ArcSwap<EmbeddingSource>` where `EmbeddingSource = Heap(VectorIndex) | Mmap(MmapEmbeddingIndex) | MmapWithDelta { base: MmapEmbeddingIndex, delta: HeapDelta, tombstones: BitSet }`.
- After build completes & flush succeeds: `engine.swap_to_mmap()` replaces `Heap` with `Mmap` and drops the heap mirror.
- `arc_swap` chosen over `RwLock` — read path is dominant; whole-source flip is rare.

#### 6.2.1 Row stability invariant (mandatory)

Every dependent structure references rows by `u32` index (posting lists 8.1, `node_id_to_idx` 8.1, cached search results 8.1, `EmbeddingStore.neural` 6.4). To stay coherent across mmap rewrites, the following invariant holds:

**Once assigned, a `(node_id → row_index)` mapping is stable for the lifetime of the index file.** Rows are append-only; deletions are tombstones. Compaction is an explicit, atomic operation that rebuilds *all* dependent indexes in the same step.

Concrete rules:

1. **Append-only mmap layout:** new rows go at the tail. Existing row offsets never move.
2. **Tombstone bitmap:** sidecar file `embeddings.tomb` (one bit per row). Search candidate filtering masks tombstoned rows. `MmapEmbeddingIndex.embedding_slice_by_index` returns `None` for tombstoned rows.
3. **Incremental reindex flow:**
   - Affected nodes' new embeddings go into `MmapWithDelta.delta` (a small heap `Vec<(row, Vec<f32>)>` overlay) and the old rows are tombstoned.
   - Search reads delta first, then mmap; tombstones override mmap.
   - On next flush: `delta` is appended to the mmap file as new tail rows, tombstones updated, dependent indexes patched (only delta rows added — no rebuild needed).
4. **Compaction trigger:** when tombstone ratio exceeds 30% (configurable: `LEINDEX_COMPACT_TOMB_RATIO`), a compaction pass runs:
   - Build new mmap file with live rows only, **assigning new row indexes**.
   - Rebuild *all* dependent indexes (`text_index`, `node_id_to_idx`, `node_tokens`, `EmbeddingStore.neural`, any cached search results) from the new mapping in the same atomic operation.
   - `ArcSwap` flips to the new `EmbeddingSource` only when the rebuild is complete and durable.
   - `search_cache` is fully invalidated on compaction.
5. **Persistence of dependent indexes:** the on-disk snapshot must include the row-index mapping in use at write time. Loaders rebuild dependent indexes against that mapping. Loading an old-format index goes through the 5.5 compatibility deserializer first; rows are then numbered sequentially as nodes appear.

Failure modes:
- Crash mid-append: tail bytes past last successful header are ignored on next open (header has row count).
- Crash mid-compaction: new file is written under a temp name and rename-atomically swapped; old file remains until rename succeeds.

### 6.3 Delete `leann_index/`

- Verify zero callers in `src/` via grep + CI build. **Use extreme caution; be thorough.**
- If clean: remove the directory, remove any leann compat code, remove fixtures referencing it.
- If a real caller exists: migrate caller to the mmap path, then delete.
- Do not preserve a zombie feature flag.

### 6.4 Move neural vectors off `NodeInfo`

- `EmbeddingStore.neural: Option<Vec<f32>>` (flat, dim-known) replaces per-node optional.
- ONNX feature off → `neural` is `None`, zero residency cost.
- ONNX feature on but TF-IDF-only search active → `neural` populated lazily on first hybrid query, evicted under cache cap.

### 6.5 Reduce snapshot peak (avoid clever lifetime tricks)

`SerializablePDG` clones all embeddings (`src/graph/pdg.rs:488`).

- Outcome required: no full embedding copy on snapshot.
- Mechanism flexible — preferred is **don't serialize embeddings via SerializablePDG at all** (or serialize directly from the flat store) rather than custom borrowed bincode.
- If neither is viable, fallback is a streaming serializer that flushes per-row.

### 6.6 Flat owned `EmbeddingStore`

- `struct EmbeddingStore { dim: u32, ids: Vec<NodeId>, data: Vec<f32>, neural: Option<Vec<f32>> }` (single allocation per field).
- `NodeInfo.embedding_row: u32` replaces the inline `Vec<f32>`.
- Schema bump: **one minor with read-compat for v1.6.x indexes**, write the new format on save. Drop legacy read after the next minor.
- Mmap file format unchanged (already row-major) — flat store mirrors it.

### 6.7 INT8 HNSW promotion (gated)

`src/search/quantization/int8_hnsw.rs` (754 LOC) is implemented but unused.

- **Phase 1 (measure):** feature `int8_default` opt-in. New `tests/search/recall_int8.rs` runs gold queries; computes NDCG@10, MRR@10, latency p50/p99 against fixture.
- **Phase 2 (promote):** flip default to INT8 when **NDCG@10 drop ≤ 1%** AND **p50 latency ≤ baseline +5%** AND **p99 latency ≤ baseline +10%**.
- f32 path lives behind `--features fp32_vectors` for benchmarks.

### 6.8 Section 3 ordering

1. 6.1 by-index slice API
2. 6.2 mmap-after-flush swap (3a-lite, before flat store)
3. 6.3 delete leann_index
4. 6.5 snapshot clone reduction (does not depend on `EmbeddingStore`)
5. 6.6 flat `EmbeddingStore` + read-compat migration (introduces the type)
6. 6.4 move neural off `NodeInfo` (lands `EmbeddingStore.neural` after 6.6 exists)
7. 6.7 INT8 promotion (after measurement gate passes)

Dependency note: 6.4 references `EmbeddingStore.neural`, which is only defined in 6.6, so 6.6 must land first. 6.5 has no dependency on `EmbeddingStore` and slots in earlier as a peak-shaving win.

---

## 7. Section 4 — C-phase: ONNX Worker Process & R15 Reshape

Designed now, built after A+ and targeted-B land.

### 7.1 Why a separate process

- Repeated in-process `Session::drop` does not always return RSS to OS (allocator pooling).
- Even INT8 Qwen3 + Tokenizer + ORT runtime cannot fit inside a 100 MiB main daemon.
- Worker crash isolates from the daemon — fall back to TF-IDF for the affected batch.

### 7.2 Process topology & transport

- New crate `crates/leindex-embed/` (separate crate, **not** an in-tree `[[bin]]`). Reason: enforce that the main crate cannot accidentally inherit ONNX baggage via feature spaghetti.
- Worker links: `ort`, `tokenizers`, `postcard`, `std`, optional minimal async runtime if needed for IPC/control. No `rusqlite`, `tree-sitter`, `axum`, or broad async stack.
- Transport: Unix Domain Socket (Linux/macOS) or named pipe (Windows). No TCP. Length-prefixed `postcard` frames.
- Wire shape (note flat layout):
  - `EmbedRequest { batch_id: u64, texts: Vec<String> }`
  - `EmbedResponse { batch_id: u64, dim: u32, count: u32, vectors: Vec<f32> /* row-major flat */ }` or `EmbedError { batch_id, message }`
- Main streams the response straight into the destination `EmbeddingStore.data` slice — no `Vec<Vec<f32>>` intermediate.

### 7.3 Lifecycle

- Cold start: spawn worker on first embed request (indexing or hybrid query).
- Idle teardown: SIGTERM/exit after `LEINDEX_EMBED_IDLE_SECS` (default 60 s) without traffic. Configurable.
- Health: 30 s pings while a session is active.
- Crash: `try_with_worker(retry=1)`; on second failure fall back to TF-IDF for the batch and emit a warning.
- Backpressure: outgoing-batch buffer ≤ 1 MiB main-side. **Requests larger than the buffer are split into multiple `EmbedRequest` frames keyed by `batch_id`; main re-stitches responses by id before writing into the destination flat store.** Single-text requests larger than 1 MiB are truncated to the model's max sequence length first (which always fits well under 1 MiB) — chunking happens at the chunker (`src/search/onnx/chunking.rs`), not at the IPC layer.
- Socket path: `$XDG_RUNTIME_DIR/leindex-embed-<pid>.sock` mode 0700.

### 7.4 Bundled model lifecycle (replaces R15 tasks 7–13)

- Worker `mmap`s the ONNX file (use `Session::builder().commit_from_memory_directly` over an mmap buffer if `commit_from_file` is found to copy).
- Main daemon never references model files.
- Distribution: installers ship `leindex` (main) and `leindex-embed` (worker) side by side; npm/PyPI place under `<pkg>/bin/` and `<pkg>/models/`.

### 7.5 Three orthogonal model-size levers

| Lever | Today | Target | Mechanism |
|---|---|---|---|
| INT8 quantization of weights | f32 ~600 MiB pair | INT8 ~150 MiB pair | `onnxruntime.quantization.quantize_dynamic` on conversion |
| Single model, drop reranker until proven | embed + reranker = 600 MiB | embed only | `--features reranker` opt-in |
| Smaller alternative | Qwen3-0.6B 300 MiB | gemma-300m ~90 MiB | `LEINDEX_EMBED_MODEL=gemma\|qwen3` |

**Final default-shipped model not locked here** — choice deferred until quality, memory, and startup-latency evals run. Small INT8 model is the leading candidate.

### 7.6 ORT Execution Provider plumbing (in scope)

GPU acceleration is a product-direction concern and is included in the C-phase design surface even though it is not a launch requirement.

- Worker selects EP via config/env: `auto | cpu | cuda` (later: `coreml | directml | rocm`).
- Default `auto`: try GPU EP first when available; fall back to CPU on init failure (warn, no hard error).
- Main daemon stays EP-agnostic; worker owns all selection logic.
- Worker startup report (one JSON line on stdout) includes: chosen EP, fallback reason if any, model name, quantization mode, warm-load latency.

### 7.7 R15 task interleave

| R15 Task | Original phase | New phase |
|---|---|---|
| 3 Download Reranker | pending | C-phase, gated by reranker proof |
| 4 Convert embed → ONNX | pending | C-phase prep |
| 5 Convert reranker → ONNX | pending | gated |
| 6 Quantize ONNX | pending | C-phase, mandatory |
| 7 Add ONNX to `models/` | pending | C-phase |
| 8 build.rs verify | done in repo | split: main build skips, worker build enforces |
| 9 Cargo.toml include | pending | worker package only |
| 10 Release workflow bundle | pending | C-phase, separate artifact channel |
| 11 npm bundle | pending | C-phase, sub-dir for models |
| 12 PyPI bundle | pending | C-phase |
| 13 Install scripts | pending | C-phase, add worker exe + models steps |
| 14 Test loading from bundle | pending | C-phase, worker integration test |
| 15 Update R15_MODEL_DISTRIBUTION.md | pending | end of C-phase |
| 16 Update R15_IMPLEMENTATION_SUMMARY.md | pending | end of C-phase |

### 7.8 Phased rollout inside C

1. **C-1:** Worker exe builds; IPC harness; integration test with a TF-IDF stub model.
2. **C-2:** Real Qwen3 wired through worker; memcheck confirms main ≤ 100 MiB while worker active.
3. **C-3:** INT8 quantize pipeline + gemma-300m alt; gold-query NDCG gate.
4. **C-4:** Installer/bundle work (R15 tasks 9-13).
5. **C-5:** Docs (R15 tasks 15-16) + remove the in-process ONNX path.

### 7.9 Out of scope for C
- Model auto-update / download-on-demand. Bundled-only for C.
- Streaming embeddings (per-chunk push). Batched only for C.

---

## 8. Section 5 — Cache Topology & Memory Discipline

### 8.1 SearchEngine internal redundancy (`src/search/search.rs:370-387`)

| Field | Today | Action |
|---|---|---|
| `nodes: Vec<NodeInfo>` | full | Diet handled by 6.6 (flat store). |
| `complexity_cache: HashMap<String,u32>` | unbounded; key duplicates `node_id` | **Delete.** Replace with `Vec<u32>` indexed by `embedding_row`. |
| `text_index: HashMap<String,HashSet<String>>` | unbounded inverted index | Compress: values become sorted `Vec<u32>` row-indices. Real compression + better cache locality + intersect-merge friendly. |
| `node_id_to_idx: HashMap<String,usize>` | needed | Keep; switch value to `u32`. After 6.6, mentally a row/source index. |
| `node_tokens: HashMap<String,HashSet<String>>` | per-node tokens | **Compact + re-key, do not delete.** Row-index keyed; values become compact token-id lists (`smallvec` or sorted `Vec<u32>`). |
| `search_cache: LruCache<String,Vec<SearchResult>>` | 1000 entries, unbounded value bytes | Bytes-bounded: `max_bytes = 16 MiB`, `max_entries = 256`. Estimated value bytes recorded per insert. |

### 8.2 Edit caches (`src/cli/mcp/edit_cache.rs`)
- Hard byte cap default 8 MiB. Per-entry payload ≤ 256 KiB (reject larger; edit re-reads).

### 8.3 Index caches (`src/cli/index_cache.rs`, 287 LOC)
- Audit for unbounded growth. If keyed by registered project → fine, add stats. If keyed by symbol/file → cap.

### 8.4 Central accounting first, eviction later

- Phase 1: `MemoryRegistry` reports usage only. Each cache registers a `byte_size_estimate()` callback.
- Phase 2 (only if measurements show need): coordinated eviction across caches.
- Per-cache hard caps are the floor; central eviction is not the first move. Avoid making the cache control plane bigger than the caches.

### 8.5 Lazy globals — selective audit
- 26 globals total; most regex/parser, harmless.
- Targeted check: any `Lazy::new(|| Mutex::new(HashMap` that holds **per-project** state — those are bugs (project switch should free). Audit, do not blanket-touch.

### 8.6 `Arc<str>` interning of `node_id`
- Switch `node_id` to `Arc<str>` everywhere it crosses boundaries (`nodes`, lookup maps, `SearchResult`).
- Single allocation per ID; clones are pointer copies.

### 8.7 Indexing pipeline transient buffers
- Implementation owned by Section 6 (6.3 backpressure). Mentioned here only because it caps cache-adjacent peak.

### 8.8 Source-content retention
- `SearchEngine` already clears content after indexing (`src/search/search.rs:561`). Steady-state SearchEngine is **not** the target.
- Real target: upstream/transient retention in the indexing pipeline and other caches.
- Rule: any retained content > 4 KiB switches to byte-range only; lazy load on render via plain file read, buffered read, or mmap (mechanism flexible, outcome fixed).

### 8.9 Section 5 ordering

1. 8.1 SearchEngine compression (biggest single win)
2. 8.6 `Arc<str>` interning (so accounting is meaningful)
3. 8.8 content cap policy
4. 8.2, 8.3 bounded edit/index caches
5. 8.4 central accounting (phase 1 only)
6. 8.5 global audit cleanup tail

---

## 9. Section 6 — Async Runtime, I/O & Concurrency Discipline

### 9.1 Tokio runtime config (lands with 5.1)
- `Builder::new_multi_thread().worker_threads(2).thread_stack_size(1024 * 1024)` for the daemon.
- Stack starts at **1 MiB** (cut to 512 KiB only after recursion audit).
- Rationale qualitative: lower worker/thread caps reduce reserved stack footprint and scheduler overhead.

### 9.2 Lazy rayon thread pool
- `rayon::ThreadPoolBuilder::new().num_threads(min(4, num_cpus)).stack_size(512_000)` constructed on first index, dropped after `LEINDEX_RAYON_IDLE_SECS` (default 5 min) idle.
- Indexing operations grab the pool explicitly via `pool.install(...)`.

### 9.3 Indexing pipeline backpressure (moved from 5.7)
- `crossbeam::bounded::<ParseJob>(BACKPRESSURE_DEPTH)` where depth tracks `INFLIGHT_BYTES / mean_file_bytes` from a rolling sample.
- `LEINDEX_INDEX_INFLIGHT_BYTES` default 16 MiB.

### 9.4 SQLite reader pool — single writer, fixed 2 readers

**Depends on Section 5.2 cache pragma changes landing first**, otherwise extra connections increase resident cost. Cache pragmas live entirely in 5.2; this section adds the connection topology only.

- `Storage { writer: Mutex<Connection>, readers: Pool<Connection> }`.
- Pool size **2 fixed** initially. Expand to 4 only if profiling shows read starvation.
- Reader connections inherit reader pragmas defined in 5.2 (`cache_size = -2000`, shared `mmap_size`). No override here.
- Routing: `&self` methods → reader; `&mut self` methods → writer.
- Audit storage call sites for read/write classification.

### 9.5 Eliminate non-storage `Arc<Mutex<…>>` hotspots

- `cli/mcp/server.rs:94 session_handshakes` → **DashMap** (live mutating per-key state; ArcSwap is wrong shape here).
- `cli/registry.rs:182 index_slots` → keep (legitimate per-project lock primitive); cap map to live projects, evict on unregister.

### 9.6 spawn_blocking discipline (practical, not absolutist)

- Long-running or parallel CPU work → rayon.
- Short blocking bridges (single small file IO from inside async edge) → `spawn_blocking` is fine.
- Set `tokio::runtime::Builder::max_blocking_threads(8)` (default 512 is wrong shape for this app).

### 9.7 notify watcher event coalescing
- Existing `incremental_reindex_from_watcher` triggers per event.
- Add 500 ms debounced batching: collect events in a window, dedup paths, single reindex per window.

### 9.8 Async/blocking boundary in MCP handlers

**Only after measurement.** Could add indirection without real gain if 9.4 already removes the worst lock bottleneck.

- If contention persists: classify handlers; offload synchronous storage calls to rayon via a small `storage_op(|w| ... ).await` helper bridging with `tokio::sync::oneshot`.

### 9.9 Section 6 ordering

1. 9.1 tokio runtime config (with 5.1)
2. 9.6 blocking pool cap
3. 9.2 lazy rayon pool
4. 9.5 non-storage mutex cleanup
5. 9.7 watcher debounce
6. 9.3 indexing backpressure
7. 9.4 SQLite reader pool (after 5.2 pragmas)
8. 9.8 async/blocking boundary in MCP (only if measurement requires)

---

## 10. Section 7 — Profiling Harness, CI Gates & Runbook

### 10.1 Harness binary
- New `tools/memcheck/` workspace crate (dev-only). Binary `memcheck`.
- Drives a deterministic workload script against a fresh `leindex` process, samples RSS every 250 ms, writes JSON.
- Workload script (TOML phases): `idle_warm → index → idle_post → query → reindex → idle_final`. Phase dwell ≥ **2-3 s** with a sample-count threshold (keep CI fast).
- Output JSON per phase: `{rss_min, rss_max, rss_p95, mapped_file_kb, anon_kb, sample_count, duration_ms}`.
- Independent of `cli/memory.rs` (concepts shared, code not).

### 10.2 Fixtures
- `tests/fixtures/memcheck/small_repo/` (~50 files; matches the original complaint scenario). Versioned in repo.
- `tests/fixtures/memcheck/medium_repo/` (~5k symbols; deterministic generator in `tools/memcheck/gen_fixture.rs`). Generated on demand.
- Reranker/ONNX gates run only when model files are locally available; CI uses TF-IDF path.

### 10.3 CI gate
- New job `memory_budget` in `.github/workflows/ci.yml`:
  1. Build release.
  2. Run memcheck on `small_repo`.
  3. Compare against `docs/memory/baselines/small_repo/<phase>.json`.
  4. Fail if any phase exceeds (committed baseline + 5%) **or** (`docs/memory/budgets/current.json` ceiling + 10%).
- **Baseline-edit metadata check:** if any file under `docs/memory/baselines/` changes, the PR must include a matching note in the PR template checklist; otherwise the lightweight metadata check fails.
- Linux only initially. macOS/Windows added later when RSS semantics handled.

### 10.4 Local dev workflow
- `cargo xtask memcheck` → spawns harness on `small_repo`, prints diff vs baseline, exits non-zero on regression.
- `cargo xtask memcheck --update-baseline` → regenerates JSON; **commit message must include a justification line** (descriptions vanish, commits stay).
- `docs/memory/RUNBOOK.md`: how to read JSON, common regression patterns, when to update baselines, and a note that **heaptrack** is a useful local Linux complement (not required for CI).

### 10.5 Optional deep heap profile
- `--features memprof` enables `dhat-rs` global allocator. Off by default (allocator switch has perf cost).
- Used for individual investigations, not CI.

### 10.6 PR template addition
- Checklist item: "If this PR changes any cap, cache, or buffer default, or modifies a file under `docs/memory/baselines/` — attach a memcheck diff or justification to the PR description."
- Soft enforcement; reviewers gate on it. Combined with the metadata check in 10.3 it gives a hard floor.

### 10.7 Production telemetry
- Add `--memory-report=path.json` flag. Daemon writes a per-phase summary on shutdown.
- Output kept simple: `{phase, rss_max, anon_kb, mapped_file_kb, sample_count}`. No giant dumps.
- Off by default; opt-in via flag or `LEINDEX_MEMORY_REPORT=path` env.

### 10.8 Out of scope
- Continuous tracing of every allocation in production.
- Long-term timeseries dashboards.

---

## 11. Risks & Open Items

- **Schema bump for 6.6:** one minor with read-compat is the planned tactic. If the format change reveals downstream consumers we have not surveyed, may need to extend the back-compat window.
- **INT8 promotion:** quality gate may not pass; f32 stays default in that case (capability preserved, memory win partially deferred).
- **C-phase IPC overhead:** worker round-trip latency for small embed batches needs measurement; may require batching adjustments.
- **macOS/Windows memcheck:** initial CI Linux-only; cross-OS parity is a follow-up not a blocker.
- **`leann_index/` deletion:** must be thorough — extreme caution before removing.
- **Audit dependencies:** all SQLite connection initializers must be enumerated before claiming the 5.2 win is complete.

## 12. Migration Notes

- Indexes built with v1.6.x continue to load via the read-compat path through one minor after 6.6 ships.
- Users of optional libsql/Turso paths must enable `--features turso` (5.3).
- ONNX users (post-C) install both the main exe and the worker exe. Installer scripts handle both.

## 13. Appendix — Repo touchpoints (non-exhaustive)

- `Cargo.toml` — features prune (5.1, 5.3, 5.4); add memprof feature (10.5); add new workspace member `tools/memcheck` (10.1) and later `crates/leindex-embed` (7.2).
- `src/global/registry.rs:117` — SQLite cache pragma (5.2).
- `src/cli/memory.rs:46` — cache cap defaults (5.6).
- `src/search/search.rs:184-193` — embedding triplet (5.5); `:370-387` — SearchEngine caches (8.1); `:561` — content clear (already correct, do not regress).
- `src/search/vector.rs:543, :596` — slice/index API (6.1); `:361-590` — MmapEmbeddingIndex (6.2).
- `src/search/onnx/qwen.rs:51` — provider lifecycle (5.7).
- `src/search/quantization/int8_hnsw.rs` — INT8 promotion target (6.7).
- `src/graph/pdg.rs:488` — snapshot clone (6.5).
- `src/cli/mcp/server.rs:94` — DashMap conversion (9.5).
- `src/cli/registry.rs:182` — keep, cap eviction (9.5).
- `src/cli/watcher.rs` — debounce (9.7).
- `src/leann_index/` — delete after grep clean (6.3).
- `src/storage/turso_config.rs` — gate behind `turso` feature (5.3).
- `src/cli/mcp/*` — port to axum 0.7 / tower 0.5 (5.4).

## 14. Document evolution

- `docs/R15_MODEL_DISTRIBUTION.md` — superseded for the worker/process design surface; updated at end of C-phase (R15 task 15).
- `docs/R15_IMPLEMENTATION_SUMMARY.md` — "What Remains" section updated at end of C-phase (R15 task 16).
- This spec is the source of truth until those updates land.
