# Fix B Deep Investigation Report

**Date:** 2026-08-09
**Branch:** `v2.0.0`
**Scope:** Verify the actual Fix B implementation against the intended GPU-utilization and bounded-memory goal before installation testing.

## Initial investigation record (historical)

The findings below describe the pre-implementation state observed before commit
`1e9687e5`. They are retained as an audit trail. The **Implementation update**
section at the end of this document is the authoritative current status.

The current code implements the earlier ONNX sub-batch correctness fix:

- all dynamic-provider sub-batches are processed;
- fixed-provider full batches are passed unchanged;
- fixed-provider partial final batches are padded and trimmed;
- output ordering and `EmbedResponse` count/dimension invariants are preserved;
- cancellation is checked between inference sub-batches.

However, the intended Fix B behavior is absent:

- tokenization still runs once over the entire request;
- inference does not begin until the entire request has been tokenized;
- tokenizer memory is not bounded by inference batch size;
- there is no `run_onnx_embed_sub_batch_inner` split;
- there is no tokenization/inference pipelining or double buffering;
- tests do not verify tokenizer call boundaries or tokenization/inference sequencing.

The tracking documents previously overstated Fix B as complete and must be corrected.

## Actual runtime path

`WorkerRuntime::run_onnx_embed` at `src/embed/runtime.rs:1684` currently:

1. Builds `Vec<&str>` for all request texts.
2. Calls `tokenizer.encode_batch(text_refs, true)` for the complete request.
3. Retains every resulting `tokenizers::Encoding`.
4. Chunks those already-tokenized encodings for inference.
5. Serially calls `session.run` through the batch loop.

The batch loop is `run_onnx_embed_batch_loop` at `src/embed/runtime.rs:1746`. The actual tensor construction and session execution are in `run_onnx_embed_sub_batch` at `src/embed/runtime.rs:1802`.

There is no `run_onnx_embed_sub_batch_inner` symbol in the current tree.

## Implemented behavior

### Dynamic providers

For CPU/CUDA, every `encodings.chunks(inference_batch_size)` entry reaches the runner through the `else` branch. This fixes the historical missing-`else` defect.

### Fixed providers

For MIGraphX/ROCm, a final partial chunk is cloned, padded with the first encoding to the stable batch size, inferred, and trimmed to the real row count.

### Batch configuration

`configured_onnx_inference_batch_size` correctly distinguishes fixed-shape models, dynamic models, dynamic uint8 model names, and stable MIGraphX/ROCm batch sizes.

### Cancellation

The cancel flag is checked between inference sub-batches. It cannot interrupt the initial whole-request tokenization.

### Tests

The existing tests validate sub-batch chunking and pad/trim behavior using synthetic encodings and a mock runner. They do not exercise or prove per-sub-batch tokenizer invocation.

## Missing behavior

### Per-sub-batch tokenization

Desired structure:

```rust
for sub_texts in texts.chunks(inference_batch_size) {
    let encodings = tokenizer.encode_batch(sub_texts, true)?;
    run_inference(&encodings)?;
}
```

Current structure tokenizes all texts before chunking. Therefore time-to-first-inference and tokenizer memory remain request-sized.

### Inference helper split

The intended design calls for a wrapper handling padding/retry and an inner helper containing tensor construction/session execution. The current implementation has only `run_onnx_embed_sub_batch`.

### Pipelining

There is no overlap between tokenization of batch N+1 and inference of batch N. The worker performs complete tokenization, then serial inference under the session mutex.

## Goal matrix

| Goal | State |
|---|---|
| Process every inference sub-batch | Implemented |
| Dynamic provider batches unchanged | Implemented |
| Fixed provider pad/trim | Implemented |
| Stable MIGraphX batch shape | Implemented |
| Whole-request tokenization | Implemented, but not the desired Fix B behavior |
| Tokenize per inference sub-batch | Missing |
| Begin inference after first tokenized sub-batch | Missing |
| Bound tokenizer memory by batch size | Missing |
| Cancel during tokenization | Missing |
| `run_onnx_embed_sub_batch_inner` split | Missing |
| Tokenization/inference pipelining | Missing |
| Tokenization sequencing tests | Missing |
| MIGraphX compile timeout/CPU fallback | Implemented separately |

## Implementation update

The bounded sequential Fix B implementation is now present:

- `run_onnx_embed_text_batch_loop` chunks source texts before tokenization;
- each chunk is tokenized with at most `inference_batch_size` texts;
- inference starts immediately after each chunk is tokenized;
- fixed-batch padding and trimming are preserved;
- `run_onnx_embed_sub_batch_inner` owns one raw ORT call;
- collapsed-batch recovery calls the inner helper directly for single rows;
- hermetic tests verify batch sizes, event ordering, fixed padding, later
  tokenizer failure, and cancellation.

The implementation is deliberately sequential. True tokenizer/inference
pipelining remains outstanding and is tracked as B4 in
`docs/plans/2026-08-09-execution-tracking.md`.
