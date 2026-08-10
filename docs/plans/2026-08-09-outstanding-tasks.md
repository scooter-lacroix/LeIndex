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

- [x] Validate `expected_dim` and empty-input behavior before entering the loop.
- [x] Determine provider, fixed-batch status, and inference batch size once.
- [x] Iterate over `texts.chunks(inference_batch_size)` rather than tokenizing all texts first.
- [x] Call `tokenizer.encode_batch` only for the current text sub-batch.
- [x] Run inference immediately after each sub-batch is tokenized.
- [x] Append only real rows to the final flat output.
- [x] Check cancellation between sub-batches and avoid retaining prior encodings.
- [x] Ensure zero/invalid batch-size configuration cannot create an empty `chunks(0)` panic.

### B2 — Separate padding/retry wrapper from raw inference

- [x] Preserve fixed-batch pad/trim and collapsed-batch retry semantics in the wrapper/inner-helper split.
- [x] Extract tensor construction and one `session.run` operation into `run_onnx_embed_sub_batch_inner`.
- [x] Ensure collapsed-batch single-row retries call the inner helper directly and cannot recursively reapply padding.
- [x] Preserve input ordering, expected dimension checks, pooling, normalization, and output shape validation.

### B3 — Decide and implement the intended concurrency level

- [x] Explicitly define completed Fix B scope as bounded sequential tokenization followed immediately by inference.
- [ ] True tokenizer/inference overlap remains unimplemented and requires the detailed design in `docs/plans/2026-08-09-fix-b-handoff-memory.md` before implementation.

### B4 — Add behavioral tests

- [x] Test tokenizer invocation receives at most `inference_batch_size` texts per call.
- [x] Test inference is invoked after each tokenized sub-batch rather than after whole-request tokenization.
- [x] Test dynamic provider counts and output invariants.
- [x] Test fixed provider full and partial batches, including padding and trimming.
- [x] Test cancellation between sub-batches.
- [x] Test tokenizer failure on a later sub-batch returns an error without corrupting prior output.
- [x] Keep tests hermetic: no real model load or MIGraphX compile for unit tests.
- [ ] Add a real-model collapsed-batch recovery test if a stable fixture becomes available.

### B5 — Documentation and verification

- [x] Update `2026-08-09-execution-tracking.md` to reflect actual code and explicitly separate pipelining.
- [x] Update `2026-08-09-RESUME-GUIDE.md` with current runtime state.
- [x] Update `2026-08-09-neural-frame-overflow-and-pdg-perf-remediation.md` to distinguish bounded sequential tokenization from true pipelining.
- [x] Update the Fix B investigation report with implementation results and test evidence.
- [x] Run `cargo fmt --all --check`.
- [x] Run `cargo clippy --workspace --all-targets --features onnx -- -D warnings`.
- [x] Run ONNX-gated runtime tests: 53 passed.
- [ ] Run `cargo test --workspace --exclude memcheck` after the final Fix B commit.
- [ ] Review the final diff and verify no documentation claims exceed implementation.

## Installation/indexing gate after completion

- [ ] Build/install only after B1–B5 are complete and validation passes.
- [ ] Run the supported warmup path if using MIGraphX.
- [ ] Run a mid-size indexing test.
- [ ] Compare `total_admitted`, neural fallback behavior, save timings, and worker lifecycle logs against the baseline.
- [ ] Run the definitive GPU/CPU fallback test on the target host.
