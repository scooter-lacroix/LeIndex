# Fix B Implementation Handoff and Memory

## Current repository state

- Repository: `/mnt/WD-SSD/code_index_update/LeIndexer-release-1.8.4`
- Branch: `v2.0.0`
- Current Fix B implementation commit: `1e9687e5`.
- Current docs/status correction commit follows the implementation commit.
- Current runtime: bounded sequential per-sub-batch tokenization followed
  immediately by inference; no concurrent pipeline is implemented.
- Current report: `docs/plans/2026-08-09-fix-b-deep-investigation.md`
- Current task list: `docs/plans/2026-08-09-outstanding-tasks.md`

## Verified facts

`src/embed/runtime.rs` now chunks source texts before tokenization through
`run_onnx_embed_text_batch_loop`. It calls `Tokenizer::encode_batch` only for
the current chunk, then immediately runs that chunk. The earlier pad/trim
sub-batch correctness fix, `run_onnx_embed_sub_batch_inner`, and the MIGraphX
compile timeout/CPU fallback are present.

True tokenizer/inference overlap is intentionally deferred. Do not describe
bounded sequential batching as a producer/consumer pipeline.

## Safe implementation order

1. Review the detailed pipeline design below and decide whether measured
   indexing results justify its complexity.
2. If approved, add a dedicated pipeline abstraction and tests without
   changing the established sequential path until the new path is proven.
3. Implement behind an explicit feature/configuration switch initially, if
   operational rollback is required.
4. Run the pipeline-specific tests, full validation, and a benchmark comparison.
5. Enable it by default only after correctness, cancellation, shutdown, and
   memory bounds are demonstrated.

## Deferred pipeline design

### Objective

Overlap tokenization of sub-batch N+1 with ORT inference of sub-batch N while
preserving the current output and lifecycle contract. The pipeline must have a
bounded resident set and must never allow unbounded queued text, encodings, or
inference results.

### Proposed architecture

- A producer owns the tokenizer and receives borrowed/owned source-text chunks
  in input order.
- A bounded channel of capacity **one** carries a `PreparedBatch` containing:
  sequence index, real row count, fixed-batch flag, and tokenized encodings.
- A consumer owns the ORT session mutex and performs padding, one raw inference
  call, collapsed-batch recovery, trimming, and output assembly.
- At most two sub-batches may be live: the batch being inferred and one batch
  waiting in the channel. The producer must block when the channel is full.
- Results are appended by sequence index, or stored in a bounded indexed slot,
  so ordering remains deterministic even if the implementation later changes
  scheduling details.

### Ownership and thread safety

- Do not share a mutable tokenizer across threads. Clone an immutable tokenizer
  if the tokenizers crate's clone semantics are verified; otherwise keep the
  tokenizer exclusively on the producer thread.
- Keep ORT `Session` access exclusively on the consumer side through the current
  `Arc<Mutex<Session>>` contract. Do not invoke `session.run` concurrently.
- Use an explicit `PipelineMessage`/result type so tokenizer errors, inference
  errors, panics, and cancellation are distinguishable and propagated once.

### Cancellation and shutdown

- Check cancellation before producing a batch, while waiting to send, and after
  each consumed inference result.
- On cancellation, close the producer side, drain or drop the bounded channel,
  and wait for the producer thread to exit before returning the worker error.
- On tokenizer or inference failure, send one terminal error, close the channel,
  join the producer, and prevent any later result from becoming a response.
- On worker shutdown/panic, ensure no detached producer thread survives the
  request or retains request text after the response path exits.

### Provider semantics

- Tokenize only real texts. For fixed-batch providers, pad on the consumer side
  immediately before inference and trim output to the real row count.
- Collapsed-batch retries must call `run_onnx_embed_sub_batch_inner` directly
  for one-row inputs; they must not re-enter the producer or apply fixed-batch
  padding recursively.
- Preserve `EmbedResponse` ordering and the `vectors.len() == count * dimension`
  invariant for every batch count.

### Acceptance tests

- A sequencing test proves tokenization of batch N+1 can occur while batch N
  inference is blocked, using barriers and a mock inference runner.
- A boundedness test proves no more than two sub-batches are resident and the
  channel never exceeds capacity one.
- Tests cover producer tokenizer failure, consumer inference failure,
  cancellation before production, cancellation while blocked on send, and
  cancellation during/after inference.
- Tests cover dynamic providers, fixed full batches, fixed partial pad/trim,
  collapsed-batch retry, empty input, and output ordering.
- A thread-join test proves no producer survives an error or cancellation.
- A benchmark compares sequential and pipelined time-to-first-inference,
  total throughput, peak RSS, and GPU utilization; the pipeline is not accepted
  if memory grows with request count or cancellation leaks threads.

## Delegation safety

- Do not spawn multiple agents that edit the same files.
- Prefer read-only subagents for architecture and test review.
- If an agent is given an implementation task, restrict it to a disjoint file set or use a separate worktree/branch if supported.
- Record every delegated task and result in the execution tracking document.
- If the session crashes, resume from this file and the outstanding task list; do not infer state from memory.

## Key correctness constraints

- Preserve fixed-batch MIGraphX/ROCm pad/trim semantics.
- Preserve collapsed-batch retry semantics without recursive fixed-batch padding.
- Preserve output ordering and `count * dimension` invariants.
- Keep tokenizer and ORT session use thread-safe.
- Keep cancellation checks between bounded sub-batches.
- Do not claim true pipelining until the bounded producer/consumer implementation
  and the acceptance tests above exist.
