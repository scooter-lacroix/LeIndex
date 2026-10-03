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
3. Implement behind a new default-off variant in the existing
   `FeatureFlag` infrastructure initially, so rollout and rollback use the
   repository's standard controls.
4. Run the pipeline-specific tests, full validation, and a benchmark comparison.
5. Enable it by default only after correctness, cancellation, shutdown, and
   memory bounds are demonstrated.

## Deferred pipeline design

### Objective and go/no-go rule

Overlap tokenization of sub-batch N+1 with ORT inference of sub-batch N while
preserving the current output and lifecycle contract. This is an optional
performance optimization, not a correctness requirement.

Do not implement or enable it solely on architectural intuition. First measure
the current sequential path on representative short-symbol and long-document
workloads. Proceed only if tokenization is a material portion of wall time or
inter-batch GPU idle gaps and an overlap prototype demonstrates a useful
improvement without increasing peak RSS beyond one additional inference-sized
encoding batch plus the existing request-sized flat output allocation. The
pipeline does not improve time-to-first-inference because the current sequential
path already infers immediately after the first chunk is tokenized; require
that metric not to regress materially. Suggested acceptance threshold: at least
10% lower median steady-state request wall time, or at least a 10 percentage
point increase in measured GPU busy/utilization during the embedding phase,
with no more than 5% regression in time-to-first-inference or p95 latency.

### Prerequisites discovered from the current runtime

1. **Make cancellation request-scoped (independent correctness prerequisite).**
   `WorkerRuntime` is cloned across socket handlers and currently shares one
   `Arc<AtomicBool>`. Every new embed request resets that global flag, so
   concurrent requests can cancel or un-cancel one another even without the
   proposed pipeline. Replace it with a batch-id keyed cancel registry or an
   equivalent request-local token registered by RAII for the lifetime of
   `handle_embed`. Treat this as a correctness fix that should land before any
   pipeline experiment.
2. **Define transport and cancellation timing.** The pipe `run_loop` cannot
   dispatch a Cancel frame while it is synchronously executing an Embed frame.
   Socket mode can receive Cancel on a separate client-handler thread. ORT
   `session.run` is also not interruptible by the current API: cancellation
   observed during inference takes effect only after that call returns, before
   the next batch/result is accepted. Tests and documentation must distinguish
   these cases and must not promise immediate inference interruption.
3. **Define BatchId races and uniqueness.** Register the Embed token at dispatch
   entry before expensive decode/tokenization work. Reject a duplicate active
   `BatchId` deterministically as `InvalidRequest`. Define an
   unknown/pre-registration/completed Cancel as an acknowledged no-op
   (best-effort cancellation), unless a bounded pending-cancel tombstone design
   is explicitly adopted. Do not allow unknown IDs to create an unbounded
   registry. A suitable initial shape is
   `Arc<Mutex<HashMap<BatchId, Arc<AtomicBool>>>>`; the RAII registration guard
   removes only the exact token it inserted and never holds the registry lock
   during tokenization or inference.
4. **Propagate request context through every embed path.** The batch ID and
   request-local token must flow through direct embedding and
   `handle_embed_with_cache` → `embed_texts` for cache misses. Cache hits must
   not bypass registration cleanup, and mixed hit/miss requests must observe the
   same cancellation token and no-partial-response rule.
5. **Bound concurrent embed pipelines.** Socket mode permits up to 16 client
   handler threads, while the shared ORT session mutex serializes inference.
   Without an embed execution gate, each request could run tokenizer/Rayon work
   concurrently and create CPU/thread oversubscription. Implement an
   `Arc<(Mutex<PermitState>, Condvar)>`-style gate with an RAII permit and timed,
   cancellation-aware waits; initially allow one active embed pipeline. Cancel
   and health handlers must never acquire this permit. Benchmark before raising
   the permit count.
6. **Verify type contracts at compile time.** Add assertions for the exact
   references moved into the scoped producer (`Tokenizer`, encodings, error and
   message types). Do not rely on assumed `Send`/`Sync` behavior.

### Proposed architecture

- Keep the request thread as the consumer and sole owner of output assembly and
  ORT inference calls.
- Use `std::thread::scope` so the producer may safely borrow the request's
  `texts` slice and `Arc<Tokenizer>` without cloning all source text or requiring
  a detached `'static` thread.
- Use `std::sync::mpsc::sync_channel(0)` as a **rendezvous channel**, not capacity
  one. With capacity one, the producer can build a third batch while one batch
  is queued and one is being inferred. A rendezvous channel bounds the steady
  state to the batch being inferred plus the batch prepared by the producer.
- The producer tokenizes real text chunks in input order and emits:

  ```text
  PreparedBatch { sequence, real_count, encodings }
  ProducerMessage::Batch(PreparedBatch)
  ProducerMessage::Error(WorkerError)
  ProducerMessage::Finished
  ```

- The consumer receives in FIFO order and takes ownership of each prepared
  encoding vector. For a fixed provider, it pads by moving/resizing that vector
  immediately before inference—not by cloning a second full encoding batch—then
  runs `run_onnx_embed_sub_batch`, trims to `real_count`, and appends directly to
  the flat output. A reorder map is unnecessary with one FIFO producer and one
  consumer; sequence numbers remain assertions/diagnostics.
- Keep a local request stop token shared by producer and consumer. It is set on
  external cancellation, tokenizer failure, inference failure, output invariant
  failure, or consumer exit.

### Cancellation-aware backpressure

`SyncSender::send` can block indefinitely and cannot poll cancellation. The
producer must use `try_send` in a bounded retry loop:

- `Full(message)`: retain the message, check the request stop token, then
  `yield_now` or sleep for a short bounded interval before retrying.
- `Disconnected`: exit immediately.
- Before tokenization and between retries, check both request cancellation and
  worker shutdown.

The consumer uses `recv_timeout` so it can observe external cancellation,
shutdown, producer panic/disconnection, and idle/activity bookkeeping instead
of blocking forever.

### Ownership and thread safety

- The scoped producer exclusively invokes the tokenizer for that request.
  `Arc<Tokenizer>` may be borrowed or cloned only after compile-time `Send`/`Sync`
  assertions succeed; no mutable tokenizer sharing is allowed.
- The request/consumer thread exclusively invokes the embedding session. Keep
  the existing `Arc<Mutex<Session>>` because other socket requests and rerank
  lifecycle code may still share runtime state; do not call `session.run`
  concurrently.
- Tokenizers' `encode_batch` may use Rayon internally. Benchmark and cap active
  embed pipelines to prevent nested/request-level oversubscription.
- Keep errors owned and source-text-free where possible. `PreparedBatch` must
  not retain original source strings after tokenization.

### Cancellation, errors, panic, and shutdown

- Register a request-local cancellation token under the Embed frame's `BatchId`
  at dispatch entry; remove it with an RAII guard on every return path.
- Reject a duplicate active `BatchId`. Unknown/pre-registration/completed Cancel
  frames acknowledge as no-ops and must not allocate registry entries.
- Cancellation cannot preempt a currently executing ORT call. If cancellation
  arrives during `session.run`, discard/ignore that batch's result when the call
  returns, set stop, join the producer, and return the cancellation error before
  starting another batch.
- On consumer inference failure, set the local stop token before returning so a
  producer blocked in `try_send` exits.
- On producer tokenizer failure, send one terminal error if possible, then exit.
- Spawn the scoped producer and retain its `ScopedJoinHandle`. Explicitly call
  `join`; map `Err(panic_payload)` to one internal `WorkerError`. Do not rely on
  implicit scope exit, which would propagate an unjoined child panic.
- Structure the scoped pipeline around one coordinator result and one
  finalization block. Consumer errors must be captured rather than returned via
  an early `?`; finalization always sets stop, drops the receiver as needed,
  explicitly joins the producer, maps a producer panic, removes the cancel
  registration/permit through RAII, and only then returns the selected error or
  completed vectors.
- Worker shutdown must set/propagate the local stop token. The current shutdown
  flag and request cancellation are distinct conditions and should produce
  distinguishable diagnostics.

### Provider and output semantics

- Tokenize only real texts. For fixed-batch providers, pad on the consumer side
  immediately before inference and trim output to the real row count.
- Collapsed-batch retries remain inside `run_onnx_embed_sub_batch` and call
  `run_onnx_embed_sub_batch_inner` directly for one-row inputs; they must not
  re-enter the producer or recursively apply fixed-batch padding.
- Validate every produced batch: `encodings.len() == real_count`, nonzero batch
  size, and returned vector length is sufficient before trimming.
- Preserve input order and `vectors.len() == count * dimension` for every
  successful response. Errors return no partial response.

### Implementation staging

1. Add batch-scoped cancellation and tests independently; retain the sequential
   embedding loop and thread the request context through direct and cache-miss
   embedding paths.
2. Add the `Mutex + Condvar + RAII` embed execution permit and test concurrent
   socket requests do not reset/cancel each other or oversubscribe tokenization.
3. Extract a generic, hermetic pipeline driver accepting tokenize/infer closures.
4. Add pipeline tests and benchmarks while production remains sequential.
5. Integrate real tokenizer/session closures behind a new experimental
   `FeatureFlag` variant (for example `EmbeddingPipeline`) defaulting off, with
   an `LEINDEX_FEATURE_*` mapping and tests through the existing flag
   infrastructure. Do not add a one-off environment parser in `runtime.rs`.
6. Compare sequential vs pipeline behavior on CPU and healthy GPU providers.
7. Remove the flag or enable by default only after all acceptance gates pass.

### Acceptance tests

#### Cancellation and concurrency

- Two concurrent embed requests have distinct cancellation tokens; cancelling A
  never stops or resets B.
- Unknown, pre-registration, completed, and duplicate Cancel frames follow the
  documented no-op/duplicate-active contract without growing the registry.
- Socket-mode Cancel from a second connection stops the matching request at the
  next safe boundary; pipe-mode limitations are explicitly tested/documented.
- Cancellation before tokenization, during tokenization boundary checks, while
  producer is retrying `try_send`, during ORT inference (observed only after
  `session.run` returns), and after the final batch.
- Consumer inference failure sets stop and the producer joins promptly.
- Producer tokenizer error/panic becomes one worker error and leaves no thread.

#### Boundedness and sequencing

- A barrier-controlled test proves batch N+1 tokenization overlaps blocked batch
  N inference.
- Instrumented live-batch counters prove no more than two encoding batches are
  resident. The test must fail if channel capacity is changed to one without an
  equivalent bound.
- The embed permit caps simultaneously active producer pipelines under multiple
  socket clients.
- Sequence assertions prove FIFO ordering; no reorder buffer grows with request
  count.

#### Provider/output behavior

- Dynamic counts `[1, batch, batch+1, 2*batch+1]` preserve ordering and shape.
- Fixed full and partial batches preserve pad/trim behavior.
- Collapsed-batch recovery uses one-row inner inference without recursive
  padding.
- Empty input, tokenizer count mismatch, undersized output, invalid dimension,
  shutdown, and channel disconnect all return the documented result/error.

#### Performance and observability acceptance

- Add per-request/sub-batch tracing fields for batch ID, sequence, real/padded
  rows, tokenization duration, channel wait duration, inference duration,
  cancellation boundary, and selected sequential/pipeline path. Never log
  source text or token IDs.
- Compare sequential and pipelined total wall time, inter-batch channel/wait
  gaps, peak RSS, CPU utilization/thread count, GPU utilization, and verify that
  time-to-first-inference does not regress.
- Use fixed corpus fixtures and record model/provider, sequence length, batch
  size, warm/cold cache state, ORT thread count, tokenizer parallelism settings,
  and run count so results are reproducible.
- Test both many short symbols and fewer maximum-length texts; tokenizer overlap
  may help one and hurt the other.
- Separate cold model/cache compilation from steady-state measurements and use
  repeated samples with median and tail latency rather than one run.
- Reject/default-off the pipeline if improvement is below the go/no-go threshold,
  peak RSS exceeds one additional encoding batch, thread count is unbounded,
  cancellation/join latency is unacceptable, or output differs bit-for-bit from
  the sequential path for the same model/provider inputs.

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
