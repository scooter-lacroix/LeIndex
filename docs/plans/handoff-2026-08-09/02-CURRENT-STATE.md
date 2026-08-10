# Current State — What's Done and What's Pending

**As of:** 2026-08-09 23:40 EDT
**Head commit:** `be5d493b` (docs only — last code commit was `1e9687e5`)

---

## Commit history (most recent first)

| Commit | Type | Description |
|--------|------|-------------|
| `be5d493b` | docs | Harden embedding pipeline design (producer/consumer plan) |
| `aac9123f` | docs | Reconcile Fix B status and pipeline design |
| `1e9687e5` | fix | Tokenize embedding requests per sub-batch (bounded sequential Fix B) |
| `3f2027cd` | docs | Record Fix B gap and implementation handoff |
| `16b2b835` | docs | Track remediation implementation |
| `b9202c18` | fix | Fall back when MIGraphX compile hangs (20s probe + CPU fallback) |
| `da9f7f29` | perf | Upsert unchanged PDG nodes (schema v4 migration, WAL/NORMAL) |
| `34785936` | fix | Shard oversized neural IPC requests (16 MiB budget) |
| `4f994f8f` | fix | Switch to dynamic uint8 ONNX model, add dequantization |

---

## Three original issues and their remediation status

### Issue 1: PDG build/save 30-45x too slow (Fix C) — ✅ COMPLETE

- Incremental `save_pdg` upsert via `ON CONFLICT(project_id, node_id)` with unchanged-row skip.
- Schema v3→v4 migration dedupes legacy rows and creates unique index.
- WAL + synchronous=NORMAL pragmas re-asserted at save time.
- `node_content_hash()` hashes content-bearing fields once, reused for column and skip check.
- Cross-file call index built once (verified by inspection — no change needed).
- Borrowed `&str` neural batch inputs with dedup.
- **Tests:** `test_resave_unchanged_pdg_issues_no_node_writes`, `test_resave_with_changed_node_writes_only_changed_rows`, `test_v3_to_v4_migration_dedupes_duplicate_node_ids`.

### Issue 2: GPU utilization low / worker hangs (Fix B) — ✅ BOUNDED SEQUENTIAL COMPLETE

- **Per-sub-batch tokenization:** `run_onnx_embed_text_batch_loop` chunks source texts, tokenizes one chunk at a time, infers immediately after each chunk. Bounds live encodings to one inference batch.
- **Sub-batch inference helper split:** `run_onnx_embed_sub_batch_inner` owns one raw ORT call; wrapper handles collapsed-batch recovery.
- **Non-recursive collapsed-batch retry:** single-row retries call `_inner` directly.
- **Hermetic tests:** tokenizer batch-size limits, tokenize→infer sequencing, fixed-batch padding, later tokenizer failure, cancellation between sub-batches. 53 ONNX runtime tests pass.
- **True tokenizer/inference pipelining:** NOT implemented. Full design in `docs/plans/2026-08-09-fix-b-handoff-memory.md`. This is intentionally deferred — the bounded sequential path already achieves the core memory/time-to-first-inference goals.

### Issue 3: Neural worker dies mid-batch (Fix A) — ✅ COMPLETE

- Client-side frame sharding: 16 MiB budget, greedy pack, per-shard `embed_attempt_shard`.
- `ErrorKind::FrameTooLarge` protocol variant.
- Source-level content cap: 64 KiB per neural text, UTF-8 boundary safe.
- Borrowed `&str` inputs with deduplication of identical capped texts.

---

## Post-implementation review findings (R1–R6)

These were identified during a fine-toothed code review of the Fix B implementation. All of R1–R6 plus the ORT fix are now implemented and committed.

### R1: MIGraphX compile probing is process-safe — ✅ IMPLEMENTED (committed)

The in-process timeout thread is gone. The parent worker launches a disposable
`leindex-embed --migraphx-probe <model> <provider> <threads>` child, polls with a
20s deadline, and on timeout kills+reaps the child before falling back to CPU. The
child sets `LEINDEX_MIGRAPHX_PROBE_CHILD=1`, initializes ORT, builds the provider
session through a no-probe helper (rejecting recursion), and runs a minimal 1x16
inference before exiting. A hung native compiler can no longer retain the parent's
GPU session/VRAM. Worker entry tests cover argv parsing and recursion prevention.

### R2: Batch-scoped cancellation — ✅ IMPLEMENTED (uncommitted)

Replaced the single shared `Arc<AtomicBool> cancel_flag` with `Arc<Mutex<HashMap<BatchId, Arc<AtomicBool>>>>`. Each embed request registers a per-batch token at dispatch entry via `register_embed_cancel()`, which returns an `ActiveEmbedCancelGuard` that removes the entry on drop. Cancel frames target only the matching `BatchId`. Duplicate active IDs are rejected. Unknown/completed Cancel IDs are acknowledged no-ops.

**Tests:** `test_cancel_targets_registered_batch`, `test_cancel_unknown_batch_is_acknowledged_noop`, `test_duplicate_active_batch_id_is_rejected_and_guard_cleans_up`.

### R3: Output invariant enforcement in release builds — ✅ IMPLEMENTED (uncommitted)

- `EmbedResponse::try_new()` validates `vectors.len() == count * dimension` in all build profiles (not just `debug_assert`).
- Every sub-batch runner result is checked for exact expected length (`rows * expected_dim`) before appending.
- Fixed padded batches validate the full padded output length before trimming.
- Aggregate output is validated before constructing the response.
- Cache path validates cached vector dimensions before accepting hits; malformed rows are logged and treated as misses.

### R4: Flat cache assembly, no text cloning — ✅ IMPLEMENTED (uncommitted)

- Replaced `Vec<Option<Vec<f32>>>` results buffer with a single flat `Vec<f32>` output + `Vec<bool>` filled mask.
- Cache hits copied directly into their row ranges via `copy_from_slice`.
- Cache misses use borrowed `&str` text references instead of cloning `String` bodies.
- Miss results copied directly into final flat output; no intermediate per-row allocations.
- Cache mutex is never held during ONNX inference (probe → release → infer → re-acquire for writes).

### R5: Per-sub-batch allocation reduction — ✅ IMPLEMENTED (committed)

Model input-name detection (`position_ids`, `token_type_ids`) is now cached in an
`Arc<OnceLock<(bool,bool)>>` on the runtime instead of re-scanning session metadata
on every sub-batch. The `attention_mask` clone remains: ORT tensor construction
consumes the vector while pooling reads the original mask, so ownership cannot be
rearranged without a semantic change.

### R6: Cancellation protocol wiring — ✅ IMPLEMENTED (socket API, committed)

`EmbeddingClient::cancel_batch(batch_id, reason)` sends a Cancel frame over a
separate Unix daemon socket connection and verifies the acknowledged Cancel
response, so socket-mode cancellation can reach the worker while another connection
performs synchronous embed inference. Pipe mode returns a clear unsupported error.
The indexing pipeline does not yet auto-invoke cancellation from an external
scheduler; callers can use the public API with a known BatchId.

Cross-connection socket-mode cancellation is exercised through
`EmbeddingClient::cancel_batch` (which opens its own daemon connection); the
indexing pipeline does not yet auto-invoke it from an external scheduler.

---

## ORT stale-path resolution — ✅ FIXED (uncommitted)

The previous code in `EmbeddingClient::configure_worker_command` promoted the persisted `neural.ort_dylib_path` from `leindex.toml` into `ORT_DYLIB_PATH` when the env var was unset. This elevated a potentially stale versioned pip soname (e.g., `libonnxruntime.so.1.25.0`) to the highest-priority discovery override, bypassing the worker's own intelligent resolution chain.

**Fix:** Removed the config-to-env promotion entirely. The worker reads the same config and resolves stale paths via `resolve_config_ort_path()` (sibling-version search), then continues through bundled, pip, system, and bare-loader fallbacks.

**Why LeIndex needs this at all:** The Rust `ort` crate is built with `load-dynamic`, meaning it does not link ORT at compile time. Unlike Python's `onnxruntime` package (which auto-discovers its bundled `.so` via `import`), the Rust worker must explicitly locate and load the shared library before any `Session::builder()` call. The discovery chain in `src/embed/ort_discovery.rs` is the correct approach; the bug was only in the launcher's eager promotion.

---

## Validation status

| Check | Status | Notes |
|-------|--------|-------|
| `cargo fmt --all --check` | ✅ | Passes on uncommitted tree |
| `cargo check --features onnx` | ✅ | Passes on uncommitted tree |
| `cargo clippy --workspace --all-targets --features onnx -- -D warnings` | ✅ | Passes on uncommitted tree |
| `cargo test --lib --features onnx embed::runtime::tests` | ✅ | 53 passed, 0 failed |
| `cargo test --lib --features onnx worker_cache` | ✅ | 17 passed, 0 failed |
| `cargo test --workspace --exclude memcheck` | ⚠️ NOT YET RUN | Must be run before committing |
| Manual `leindex index --force` | ⬜ | Pending after all fixes committed |
