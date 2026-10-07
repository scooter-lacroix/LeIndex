# Post-Implementation Remediation Tasks

**Date:** 2026-08-09 (updated 2026-08-10)
**Branch:** `v2.0.0`
**Scope:** LeIndex embedding worker and ONNX client.

## Status

### R1 — Process-safe MIGraphX compile probing — ✅ IMPLEMENTED

The worker now launches a disposable `leindex-embed --migraphx-probe` child for the MIGraphX warmup inference. The parent polls with a deadline, kills and reaps the child on timeout, and falls back to CPU. The child sets `LEINDEX_MIGRAPHX_PROBE_CHILD=1`, initializes ORT, builds the provider session through a no-probe helper, and runs the minimal inference. Recursive probe entry is rejected. This prevents a hung native compiler thread from remaining attached to the parent worker's GPU session.

### R2 — Batch-scoped cancellation — ✅ IMPLEMENTED

Cancellation uses an active `BatchId -> Arc<AtomicBool>` registry with RAII cleanup. Duplicate active IDs are rejected; unknown/completed IDs are acknowledged no-ops. Tokens are threaded through direct and cache-miss embedding paths. Pipe-mode cancellation is intentionally unavailable while the synchronous pipe loop handles an Embed request; socket mode can use a separate connection.

### R3 — Release-build output invariants — ✅ IMPLEMENTED

`EmbedResponse::try_new` and all sub-batch, fixed-padding, cache-miss, and aggregate paths validate exact vector lengths in release builds.

### R4 — Flat cache assembly — ✅ IMPLEMENTED

Cache hits and misses are assembled into one flat row-major buffer. Miss texts are borrowed, malformed cached rows are treated as misses, and the cache mutex is not held during inference.

### R5 — Per-sub-batch allocation reduction — ✅ IMPLEMENTED (measured qualitatively)

Model input-name detection is cached in a runtime `OnceLock`, avoiding repeated metadata scans for each inference sub-batch. `attention_mask.clone()` remains necessary because ORT tensor construction consumes ownership while pooling uses the original mask. No speculative tokenizer API rewrite was made.

### R6 — Client cancellation wiring — ✅ IMPLEMENTED (socket API)

`EmbeddingClient::cancel_batch` sends a Cancel frame over a separate Unix daemon socket connection and verifies the acknowledged Cancel response. Pipe mode returns a clear unsupported error because its worker loop synchronously dispatches Embed. The indexing pipeline does not yet automatically invoke cancellation from an external scheduler; callers can use the public client API when a BatchId is known.

## Validation

- `cargo fmt --all --check` — pending final gate
- `cargo clippy --workspace --all-targets --features onnx -- -D warnings` — pending final gate
- `cargo test --workspace --exclude memcheck` — pending final gate
- Targeted worker parser tests — passed (6/6)
- `cargo check --features onnx` — passed
- Targeted ONNX runtime/cache/client tests — pending final gate
- `cargo test --workspace --exclude memcheck` — pending final gate
- Review final diff for exact status claims and a clean working tree.
