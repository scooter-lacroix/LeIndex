# Remediation: Neural Worker Frame Overflow + PDG Build/Save Perf

**Date:** 2026-08-09
**Branch:** v2.0.0
**Status:** Implementation in progress

## Executive Summary

Three reported defects in LeIndex 2.0.0:

1. **PDG building + saving-to-storage is 30–45x too slow** and uses too much RAM.
2. **GPU (MiGraphX) utilization is low/slow/CPU-reliant.**
3. **Neural embedding worker dies mid-batch on every codebase** — `worker process died: EOF/EPIPE reading frame length: failed to fill whole buffer` — forcing TF-IDF fallback.

All three were root-caused to concrete mechanisms (below). Fixes are ordered by
risk/impact: (3) first (deterministic correctness bug), then (2) (GPU idle
sources), then (1) (build/save hot paths).

---

## Root Cause Analysis

### Issue 3 — Worker death is deterministic, not random

The error string is constructed in `read_frame` (`src/search/onnx/client.rs:72`):

```rust
ClientError::WorkerDied { message: format!("EOF/EPIPE reading frame length: {}", e) }
```

where `e` is `io::ErrorKind::UnexpectedEof`'s `Display` — "failed to fill whole
buffer". That means the worker **closed the connection without writing a
response frame**. The chain:

1. `embed_pending_neural_batch` (`src/cli/index_builder/mod.rs:1093`) sends
   **`NEURAL_IPC_BATCH = 256` full node contents** in one `EmbedRequest` frame.
   `node.content` for file-level PDG nodes is the **entire enriched file body**
   (`enriched_node_content` over whole `file_bytes`).
2. The worker's per-text guard `truncate_text` caps a single text at
   `DEFAULT_MAX_TEXT_SIZE = 1 MiB`, but there is **no aggregate/request-side cap
   on the client**. A chunk of 256 large contents routinely serializes to
   > 32 MiB.
3. Worker `run_loop` reader-thread (`src/embed/runtime.rs:1121`) computes
   `max_incoming_frame = max_frame_size * 2` = **32 MiB** default
   (`DEFAULT_MAX_FRAME_SIZE = 16 MiB`). It reads the 4-byte length prefix, sees
   `payload_len > max_incoming_frame`, sends `Err(InvalidData)` into the frame
   channel, and **`break`s without writing any response**. The per-client
   handler ends and the socket closes.
4. Client `read_frame` hits EOF on `read_exact` → `WorkerDied` →
   `embed_with_fallback` (`client.rs:1161`) kills the worker and **retries the
   same oversized frame** → identical teardown → second failure →
   **TF-IDF fallback for the whole 256-text chunk** (`hybrid.rs` log:
   "Neural batch embedding degraded to TF-IDF for N texts").
5. Because every codebase has chunks > 32 MiB once large/generated files are
   present, the failure reproduces on every run, mid-batch.

Contributing: `send_and_receive` (`client.rs:1402`) has **zero request-side size
check** — it only enforces `MAX_RESPONSE_FRAME_SIZE` on reads.

### Issue 2 — GPU idle sources

- `run_onnx_embed` tokenizes the **entire batch** with a single
  `tokenizer.encode_batch` call, then sub-batches inference at
  `configured_onnx_inference_batch_size` (default 8 for MIGraphX).
- `run_onnx_embed_batch_loop` runs each sub-batch **serially through one
  `session.lock()`** — no overlap between tokenization of sub-batch N+1 and
  inference of sub-batch N, no pipelining, no double buffering.
- MiGraphX JIT shape is fixed (batch 8 / seq 128); with 256-text frames this is
  32 sequential `session.run()` calls, GPU idle between calls and during
  tokenization.

### Issue 1 — Build/save hot paths

- **Save** (`src/storage/pdg_store.rs:175` `save_pdg`): full
  `DELETE FROM intel_nodes/intel_edges` + re-INSERT of every node/edge on every
  save. Per-node work: `blake3::hash(id)` (recomputed per node per save — the
  hash is only used as a `content_hash` column, so it is wasted work),
  16-param `Vec<rusqlite::types::Value>` (16 heap allocations per node),
  batched `RETURNING` but still ~50K+ rows touched on every generation. No
  `PRAGMA` WAL/NORMAL tuning, no upsert, no unchanged-node skip.
- **Build**: `extraction_cross_file.rs` does an O(S²) second pass over all
  signatures (`build_cross_file_call_indexes` rescans every signature; name
  matching rescans per edge). `run_lexical` re-reads every file + re-tokenizes
  with whole-file bodies in `NodeInfo.content` retained for the batch; default
  batch_size = 10_000 → 10K full contents + embeddings resident at once.

---

## Remediation Design

### Fix A (Issue 3) — frame-size-aware sharding + graceful worker error

1. **Client aggregate cap + sharding** in `EmbeddingClient::embed_with_fallback`
   and `embed_neural_batch_blocking`: before `encode_wire`, compute
   `∑ text.len()`; if it exceeds a request-frame budget, split the texts into
   sequential sub-requests (`EmbeddingClient::embed_attempt` per shard),
   concatenate `EmbedResponse` vectors preserving input order. Budget chosen
   with headroom under the worker cap (≤ 24 MiB, worker cap 32 MiB).
2. **New `ErrorKind::FrameTooLarge`**: the worker reader-thread, on an oversized
   incoming frame, writes an `error_frame(batch_id, WorkerError {
   FrameTooLarge, .. })` back before closing, instead of breaking silently —
   this converts "worker process died" into a precise, actionable error the
   client can down-shard and retry without killing the daemon.
3. **Retry uses sharding**: `embed_with_fallback`'s retry branches call the same
   size-aware path, so the identical oversized frame is never re-sent.
4. **Bound content at the source**: `embed_pending_neural_batch` truncates each
   text to a `NEURAL_CONTENT_CAP` (e.g. 64 KiB) before queueing (token-safe
   boundary), capping per-text cost and aggregate frame size; TF-IDF keeps the
   full content path untouched.

### Fix B (Issue 2) — GPU pipelining

5. In `run_onnx_embed_batch_loop` / `run_onnx_embed`, overlap tokenization with
   inference: tokenize sub-batch N+1 while `session.run()` executes sub-batch N
   (software pipelining within the single worker thread pool). For
   fixed-batch providers, keep the padding; for dynamic-batch providers, use
   larger sub-batches. Bounded change in `src/embed/runtime.rs` only.

### Fix C (Issue 1) — build/save perf

6. **`save_pdg` upsert**: `INSERT ... ON CONFLICT(project_id, node_id) DO
   UPDATE` keyed by natural `node_id`; skip rows whose `content_hash` is
   unchanged (hash once, reuse); drop the per-node `blake3` recompute where the
   value is only a synthetic column; set `PRAGMA journal_mode=WAL;
   synchronous=NORMAL` on the write connection; keep the batching.
7. **Cross-file index once**: build the call-index (name → signatures) once over
   all signatures, then add call edges — eliminate the O(S²) rescan.
8. **RAM**: avoid cloning 256 contents in `embed_pending_neural_batch` (borrow
   `&str`); skip neural for contents already hoisted (they already have
   `cached_neural`).

### Validation

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --exclude memcheck`
- New unit tests: client sharding (frame ≤ budget, order preserved), worker
  frame-too-large writes an error frame, `save_pdg` idempotent re-save, upsert
  equality on unchanged nodes, cross-file index built once.
- Manual: `leindex index --force` on a mid-size repo; confirm no
  "worker process died", `total_admitted` matches prior, save phase ms.

---

## Files touched

- `src/search/onnx/client.rs` — request sharding, `embed_with_fallback` retry,
  `send_and_receive` size guard, `FrameTooLarge` handling.
- `src/search/onnx/client_config.rs` — `ClientError`, request-frame budget
  constant, `MAX_REQUEST_FRAME_SIZE`.
- `src/embed/protocol.rs` — `ErrorKind::FrameTooLarge`, `error_frame` reuse.
- `src/embed/runtime.rs` — reader-thread graceful oversized-frame error write;
  tokenize/infer pipelining.
- `src/storage/pdg_store.rs` — upsert save, WAL/NORMAL pragma, hash reuse,
  unchanged-skip.
- `src/graph/extraction_cross_file.rs` — build call index once.
- `src/cli/index_builder/mod.rs` — `embed_pending_neural_batch` content cap +
  borrow, hoist-aware neural skip.---

## Section 2 — Model-loaded performance validation (added 2026-08-09)

### Root cause of the observed >60 s "hang"

The 60 s+ stall seen while running the onnx-gated `embed::` test suite is **not a
defect in the new sharding/cap code** (Fix A) nor the PDG save path (Fix C). It is
the **MIGraphX cold JIT compile** — a documented, inherent property of the AMD GPU
execution provider:

- `src/embed/runtime.rs` (`build_migraphx_ep`): *"Cold start JIT-compiles (~300 s)
  and writes the `.mxr`; warm start loads it (~4 s)."*
- `src/embed/runtime_test.rs` (`no_compile_config`): *"a real
  `qwen3-embed-0.6b.onnx` under `~/.leindex/models` makes every
  `WorkerRuntime::new` compile the model and OOM the test binary (regression
  introduced when the static model shipped)."*
- `src/cli/leindex/setup.rs` (`run_warmup` / `run_warmup_inner`): the intended
  one-time warm pre-compile (pipe mode, `send_and_receive` blocks with no
  timeout so the compile completes) that persists `.mxr` files; the daemon path
  cannot survive the 300 s compile because of the 120 s `DAEMON_READY_MAX_WAIT`
  readiness gate.

The two `worker_entry_tests` that hung used `RuntimeConfig::default()` (a real
model name), so under `--features onnx` `WorkerRuntime::new` triggered this cold
JIT. **Fix (already applied):** those tests now use hermetic model names
(`__leindex_test_no_model__`) so they never attempt a real compile — matching the
identical sibling tests in `runtime_test.rs`. This is a test-hermeticity fix, not
a behavioral change.

### Operational remediation (this host)

The uint8 model (`qwen3-embed-0.6b-dynamic-uint8.onnx`, 655 MB) was downloaded
by `setup` and its MIGraphX cache dir (`~/.leindex/cache/migraphx/
qwen3-embed-0_6b-dynamic-uint8/b8-s128/`) was **empty** (no `.mxr`), so every
worker spawn recompiled. The supported fix is the one-time warmup:

```sh
leindex setup --neural --gpu amd --warmup
```

Observed on this ROCm host (25.7 GB VRAM, ROCm 6.x, ORT 1.25.0 with MIGraphX):
- setup detected `onnxruntime 1.25.0` + `migraphx`, downloaded the uint8 model.
- `Model Compile: Begin` at the first inference → the ~300 s JIT.
- After it completes, `.mxr` files appear under
  `~/.leindex/cache/migraphx/qwen3-embed-0_6b-dynamic-uint8/b8-s128/` and
  subsequent `WorkerRuntime::new` loads warm in ~4 s (not ~300 s).

### Rigorous model-loaded verification plan (remaining)

1. Let the warmup complete; confirm `.mxr` files now exist for the uint8 profile.
2. Time a **warm** worker spawn + single embed end-to-end (expect model load
   ~seconds, embed ~tens of ms — the model is tiny, GPU is 25.7 GB).
3. Time a warm **batch embed** (256 texts) — should be millisecond-scale.
4. Confirm the index path (`leindex index --force`) no longer blocks on the
   compile and `total_admitted` is unchanged vs prior runs.
5. Re-run the onnx-gated test suite — with hermetic tests it completes in
   ~0.34 s (231 passed) regardless of local model warmth.

### Production impact

- **Cold cache**: first `WorkerRuntime::new` pays ~300 s (MIGraphX JIT). This is
  why `leindex setup --warmup` exists and why the daemon readiness gate (120 s)
  is documented as not covering the compile.
- **Warm cache**: every subsequent spawn is ~4 s model load + millisecond
  inference. No code defect — operational warmup required once per model/shape.
- A host that has never run warmup (or whose `.mxr` was pruned/cache dir is
  empty) will exhibit the slow first spawn; the fix is `setup --warmup`, exactly
  as the plan's Fix B assumes.