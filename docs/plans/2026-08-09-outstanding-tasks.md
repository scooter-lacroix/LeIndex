# Outstanding Remediation Tasks

**Date:** 2026-08-09
**Branch:** `v2.0.0`
**Source report:** `docs/plans/2026-08-09-fix-b-deep-investigation.md`
**Status:** Active implementation plan

## Completed baseline (do not redo)

- [x] Fix A: neural IPC request-frame budgeting/sharding.
- [x] Fix A: source neural text cap and borrowed/deduplicated inputs.
- [x] Fix A: worker frame-too-large protocol handling as present in the committed implementation.
- [x] Earlier ONNX batching correctness fix: process all dynamic-provider batches.
- [x] Earlier ONNX batching correctness fix: MIGraphX/ROCm final-batch pad/trim.
- [x] Fix C: incremental PDG node upsert, unchanged-row skip, stale-row deletion, WAL/NORMAL, schema migration.
- [x] MIGraphX/ROCm bounded compile probe and CPU fallback.
- [x] Baseline format, clippy, and workspace tests previously passed.

## Fix B runtime tasks

### B1 — Refactor embedding control flow to tokenize per inference sub-batch

- [ ] Validate `expected_dim` and empty-input behavior before entering the loop.
- [ ] Determine provider, fixed-batch status, and inference batch size once.
- [ ] Iterate over `texts.chunks(inference_batch_size)` rather than tokenizing all texts first.
- [ ] Call `tokenizer.encode_batch` only for the current text sub-batch.
- [ ] Run inference immediately after each sub-batch is tokenized.
- [ ] Append only real rows to the final flat output.
- [ ] Check cancellation between sub-batches and avoid retaining prior encodings.
- [ ] Ensure zero/invalid batch-size configuration cannot create an empty `chunks(0)` panic.

### B2 — Separate padding/retry wrapper from raw inference

- [ ] Make `run_onnx_embed_sub_batch` responsible for fixed-batch padding/trim and collapsed-batch retry semantics, or preserve those semantics in an equivalent clearly tested wrapper.
- [ ] Extract tensor construction and one `session.run` operation into `run_onnx_embed_sub_batch_inner` (or document and justify an equivalent name/design).
- [ ] Ensure collapsed-batch single-row retries cannot recursively reapply fixed-batch padding.
- [ ] Ensure all callers pass the correct fixed-batch behavior for normal and retry paths.
- [ ] Preserve input ordering, expected dimension checks, pooling, normalization, and output shape validation.

### B3 — Decide and implement the intended concurrency level

- [ ] Determine whether the required goal is bounded per-sub-batch tokenization only or true tokenizer/inference overlap.
- [ ] If true pipelining is required, design a bounded two-stage producer/consumer path with at most two live sub-batches.
- [ ] Do not share a tokenizer or ORT session unsafely across threads.
- [ ] Ensure cancellation, worker shutdown, panic propagation, and error propagation work across pipeline boundaries.
- [ ] If pipelining is intentionally deferred, update the remediation plan to explicitly state that Fix B means bounded sequential sub-batches, not overlap.

### B4 — Add behavioral tests

- [ ] Test that tokenizer invocation receives at most `inference_batch_size` texts per call.
- [ ] Test that inference is invoked after each tokenized sub-batch rather than after whole-request tokenization.
- [ ] Test dynamic provider counts `[1, batch, batch+1, 2*batch+1]` and output invariants.
- [ ] Test fixed provider full and partial batches, including padding and trimming.
- [ ] Test cancellation between sub-batches.
- [ ] Test tokenizer failure on a later sub-batch returns an error without corrupting prior output.
- [ ] Test collapsed batch output retries one sequence at a time without recursion/padding errors.
- [ ] Test empty input and invalid expected dimension.
- [ ] Keep tests hermetic: no real model load or MIGraphX compile for unit tests.

### B5 — Documentation and verification

- [ ] Update `2026-08-09-execution-tracking.md` so B1/B2/B3 status reflects actual code.
- [ ] Update `2026-08-09-RESUME-GUIDE.md` so the runtime redo section is no longer stale after implementation.
- [ ] Update `2026-08-09-neural-frame-overflow-and-pdg-perf-remediation.md` to distinguish bounded sequential tokenization from true pipelining.
- [ ] Update the Fix B investigation report with implementation results and test evidence.
- [ ] Run `cargo fmt --all --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace --exclude memcheck`.
- [ ] Run ONNX-gated runtime tests specifically.
- [ ] Review the final diff and verify no documentation claims exceed implementation.

## Installation/indexing gate after completion

- [ ] Build/install only after B1–B5 are complete and validation passes.
- [ ] Run the supported warmup path if using MIGraphX.
- [ ] Run a mid-size indexing test.
- [ ] Compare `total_admitted`, neural fallback behavior, save timings, and worker lifecycle logs against the baseline.
- [ ] Run the definitive GPU/CPU fallback test on the target host.
