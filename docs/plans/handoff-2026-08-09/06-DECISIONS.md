# Key Decisions and Rationale

**As of:** 2026-08-09

---

## 1. Bounded sequential tokenization, not concurrent pipelining

**Decision:** Fix B implements per-sub-batch tokenization followed immediately by inference, sequentially within the request thread. True tokenizer/inference overlap (producer/consumer pipeline) is intentionally deferred.

**Rationale:**
- The sequential path already achieves the core goals: bounded tokenizer memory (only one inference batch of encodings is live at a time) and reduced time-to-first-inference (inference starts after the first chunk is tokenized, not after the entire request).
- A concurrent pipeline adds significant complexity: scoped threads, cancellation-aware backpressure, BatchId-scoped cancellation, embed execution permits, panic handling, and benchmark validation.
- The pipeline's performance benefit is uncertain — tokenization may not be a material fraction of wall time. A go/no-go benchmark decision is required before investing in the complexity.
- Full design is documented in `docs/plans/2026-08-09-fix-b-handoff-memory.md`.

## 2. Batch-scoped cancellation (not global flag)

**Decision:** Replaced the single shared `Arc<AtomicBool> cancel_flag` with a `HashMap<BatchId, Arc<AtomicBool>>` registry. Each embed request registers at dispatch entry and cleans up via RAII on every return path.

**Rationale:**
- `WorkerRuntime` is `#[derive(Clone)]` and shared across socket handler threads (up to 16 concurrent). A single global cancel flag means any new embed request resets it (`store(false, Relaxed)` at dispatch entry), which can un-cancel another in-flight request. Conversely, cancelling one request would cancel all.
- The fix is a correctness prerequisite, not merely a pipeline feature. It must be in place before any concurrent work.

## 3. Release-build output validation

**Decision:** `EmbedResponse::try_new()` validates `vectors.len() == count * dimension` in all build profiles. Every sub-batch result is checked for exact expected length before appending.

**Rationale:**
- The previous `debug_assert_eq!` in `EmbedResponse::new` was a no-op in release builds. A malformed ONNX output (wrong batch dimension, model returning fewer rows) would silently produce inconsistent wire metadata.
- Exact-length checks on every sub-batch catch collapsed-batch outputs that slip through the sentinel-based retry, and catch model mismatches early with an actionable error message.

## 4. Flat cache assembly

**Decision:** The cache-aware embed path (`handle_embed_with_cache`) now allocates one flat `Vec<f32>` output buffer and a `Vec<bool>` filled mask. Cache hits are copied directly into their row ranges. Miss texts are borrowed as `&str`, not cloned. Miss results are copied directly into the output. No `Vec<Option<Vec<f32>>>` intermediate.

**Rationale:**
- The previous code cloned every cache-miss text into a new `Vec<String>`, allocated `Vec<Option<Vec<f32>>>` for results, cloned each hit vector, and then flattened the results at the end. This was N+1 heap allocations per request that partially undid the flat-buffer design goals.
- The flat path reduces allocations to: one output `Vec<f32>`, one `Vec<bool>` mask, and the miss embedding result. All copies are `copy_from_slice` into pre-allocated ranges.
- The cache mutex is never held during ONNX inference (probe → release → infer → re-acquire), preventing contention with concurrent requests.

## 5. ORT_DYLIB_PATH as explicit user override only

**Decision:** The daemon launcher (`configure_worker_command`) no longer promotes the persisted config `ort_dylib_path` into the `ORT_DYLIB_PATH` environment variable.

**Rationale:**
- `leindex setup` records the exact versioned pip soname (e.g., `libonnxruntime.so.1.25.0`). When pip upgrades ORT, the old `.so` disappears. If the launcher promotes this stale path into `ORT_DYLIB_PATH`, it becomes the highest-priority override and bypasses the worker's own intelligent resolution chain.
- The worker reads the same config file and resolves stale paths via `resolve_config_ort_path()` — it searches the same directory for `libonnxruntime.so.*` and picks the best version. If the exact path is stale, it falls through to the sibling search, then to bundled, pip, system, and bare-loader fallbacks.
- `ORT_DYLIB_PATH` should remain what its name implies: an explicit, intentional user override.

## 6. MIGraphX compile timeout (in-process, known to be incomplete)

**Decision:** A 20-second timeout probe was added around the first `session.run()` for MIGraphX/ROCm providers. On timeout, fall back to CPU.

**Known limitation (R1):** The probe uses `std::thread::spawn` + `recv_timeout`. On timeout, the spawned thread cannot be killed and retains the GPU session/VRAM. This is a known unsoundness — the fallback to CPU works functionally, but the hung thread leaks resources.

**Why it was shipped anyway:** Without any timeout, the worker hangs forever on ROCm 7.2.4 (the `repeat_while_changes` pass loop never converges). The 20s timeout at least allows the index to complete via CPU fallback. The process-safe fix (child process probe) is the highest-priority remaining task.

## 7. Schema v4 migration (dedup before unique index)

**Decision:** Before creating the unique `uq_intel_nodes_project_node` index (required for `ON CONFLICT` upsert), the v3→v4 migration dedupes legacy rows by keeping the lowest `id` per `(project_id, node_id)` and dropping orphan edges.

**Rationale:**
- Legacy rows may carry duplicate `node_id` values (e.g., two `init` functions in different files both defaulting to `node_id = "init"`). A unique index cannot be created on a table with duplicates.
- The migration is idempotent and safe: it only removes strictly duplicate `(project_id, node_id)` pairs, keeping the first inserted row and its edges.

## 8. Neural IPC frame sharding (Fix A)

**Decision:** The client estimates serialized frame size before sending. If it exceeds 16 MiB, the batch is greedily split into sub-shards that each fit. Results are concatenated in input order.

**Rationale:**
- The worker's incoming-frame guard rejects frames larger than `max_frame_size * 2` (32 MiB default). Batching 256 large node contents routinely exceeded this, causing the worker to close the connection silently (the original "worker process died: EOF/EPIPE" bug).
- 16 MiB was chosen with headroom under the 32 MiB worker cap. Bincode adds a small envelope per frame, so the estimate (`text.len() + 32` per string) is conservative.
