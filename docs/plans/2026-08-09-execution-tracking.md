# LeIndex Remediation — Execution Tracking

**Plan:** `docs/plans/2026-08-09-neural-frame-overflow-and-pdg-perf-remediation.md`
**Started:** 2026-08-09 · **Branch:** v2.0.0
**Validation gate:** `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --exclude memcheck`

---

## Legend

- ✅ **Done + verified** (compiles/fmt pass; test/validation noted)
- 🔄 In progress
- ⬜ Not started

---

## Fix A — Neural worker death (Issue 3): frame-size-aware sharding + graceful worker error

| # | Task | Status | Notes / Verification |
|---|------|--------|----------------------|
| A1 | `ErrorKind::FrameTooLarge` variant in `src/embed/protocol.rs` | ✅ | Compiles under `--features onnx`. |
| A2 | `ClientError::FrameTooLarge` variant in `src/search/onnx/client_config.rs` | ✅ | Compiles. |
| A3 | `MAX_REQUEST_FRAME_BUDGET` (16 MiB) + `embed_request_frame_estimate()` in `client_config.rs` | ✅ | Generic over `AsRef<str>` so borrowed `&str` batches avoid cloning. |
| A4 | Client-side sharding in `embed_attempt` (`client.rs`): estimate → fast-path single frame or split into sub-chunks → per-shard `embed_attempt_shard` → concatenate `into_vectors()` in order | ✅ | `cargo check --features onnx` PASSES; `cargo fmt --check` PASSES. Retry path (`embed_with_fallback`) re-enters `embed_attempt` so it never resends one oversized frame. |
| A5 | Worker-side graceful `FrameTooLarge` in `runtime.rs` `run_loop`: reader thread peeks 8-byte batch id from oversized frame; main loop writes `error_frame(FrameTooLarge)` + continues instead of silent teardown | ✅ | `FrameReadError` enum added; `Io` preserves EOF/EPIPE handling; `cargo check --features onnx` PASSES. |
| A6 | Source-side content cap in `embed_pending_neural_batch` (truncate each text to 64 KiB before queueing; TF-IDF untouched) | ✅ | `cap_neural_text` truncates on a UTF-8 char boundary; full content still used for TF-IDF/lexical search. gated `#[cfg(any(onnx, remote-embeddings))]`. |

**Summary:** The deterministic death (256 full contents > 32 MiB worker cap → silent EOF) is eliminated at the client (A4 never sends an oversized frame), hardened at the worker (A5 answers with a precise error instead of dying), and bounded at the source (A6 caps per-text size). Verified: onnx-gated tests pass.

---

## Fix B — bounded per-sub-batch tokenization (Issue 2)

| # | Task | Status | Notes / Verification |
|---|------|--------|----------------------|
| B1 | Tokenize and infer one bounded text sub-batch at a time | ✅ | `run_onnx_embed_text_batch_loop` chunks source texts, calls `Tokenizer::encode_batch` for only the current chunk, then immediately infers it. This bounds live encodings and reduces time-to-first-inference. |
| B2 | Preserve provider padding, output shape, ordering, and collapsed-batch recovery | ✅ | Fixed-provider partial chunks are padded/trimmed; `run_onnx_embed_sub_batch_inner` owns one raw ORT call; wrapper retries collapsed batches with direct single-row inner calls, avoiding recursive padding. |
| B3 | Hermetic behavioral tests for sequencing and failure paths | ✅ | Added tests for per-call sizes and tokenize→infer ordering, fixed-batch padding, later tokenizer failure, and cancellation between sub-batches. ONNX runtime test module: 53 passed. |
| B4 | True tokenizer/inference pipeline | ⬜ | Intentionally not implemented. Current Fix B is bounded sequential tokenization/inference. The reviewed prerequisite, ownership, cancellation, boundedness, rollout, and acceptance design is in `2026-08-09-fix-b-handoff-memory.md`; production remains sequential/default-off until those gates pass. |
| B5 | Compile/lint verification | ✅ | `cargo fmt --all --check`, `cargo check --features onnx`, and `cargo clippy --workspace --all-targets --features onnx -- -D warnings` pass. |

**Scope clarification:** The implemented Fix B goal is bounded sequential per-sub-batch tokenization with immediate inference. It does not overlap tokenizer work with ORT inference. The prior whole-request tokenization claim has been removed from the runtime; the old production `run_onnx_embed_batch_loop` is now test-only, while the text loop owns production batching.
---

## Fix C — PDG build/save performance (Issue 1)

| # | Task | Status | Notes |
|---|------|--------|-------|
| C1 | `save_pdg` upsert (`INSERT ... ON CONFLICT(project_id, node_id) DO UPDATE`) + skip unchanged `content_hash` rows | ✅ | `save_nodes` pre-queries existing `(id, node_id, content_hash)`; unchanged rows reuse their db id and issue ZERO writes; changed rows upsert via the unique `uq_intel_nodes_project_node` index. Stale rows deleted in bounded `IN` batches. Verified by `test_resave_unchanged_pdg_issues_no_node_writes` (0 node writes) + `test_resave_with_changed_node_writes_only_changed_rows` (1 write). |
| C2 | `PRAGMA journal_mode=WAL; synchronous=NORMAL` on the write connection | ✅ | Writer already opens WAL+NORMAL; `save_pdg` now re-asserts both so legacy/non-default-config DBs are upgraded before the bulk write. |
| C3 | Drop per-node wasted `blake3::hash(id)` recompute (hash once, reuse as `content_hash`) | ✅ | `node_content_hash()` hashes the content-bearing fields (path, symbol, qualified name, language, node type, complexity, byte range) once per node — reused for the column AND the unchanged skip check. Legacy rows keyed on old `blake3(id)` read as changed once and rewrite on first save. |
| C4 | Cross-file call index built **once** over all signatures (eliminate O(S²) rescan in `extraction_cross_file.rs`) | ✅ | Verified by inspection: `build_cross_file_call_indexes` builds `qname_to_node`/`exact_map`/`suffix_map`/`last_map` once over `pdg.node_indices()`; `resolve_cross_file_call_edges_inner` is a single pass over signatures with hashmap lookups — no per-signature rescan. |
| C5 | RAM: avoid cloning 256 contents in `embed_pending_neural_batch` (borrow `&str`); skip neural for already-hoisted contents | ✅ | `embed_pending_neural_batch` borrows `&str` via the generic `embed_neural_batch_blocking(&[&str])`, dedupes identical capped texts within each chunk, and stores one vector per unique content. `append_neural_batch` borrows too. |

**Schema migration (required by C1):** v3→v4 bumps `SCHEMA_VERSION` to 4; `migrate_v3_to_v4` dedupes legacy `intel_nodes` rows (keeps lowest id per `(project_id, node_id)`, dropping duplicate edges) before `initialize_query_indexes` creates the unique `uq_intel_nodes_project_node` index. Verified by `test_v3_to_v4_migration_dedupes_duplicate_node_ids`.

---

## Cross-cutting / Validation

| # | Task | Status | Notes |
|---|------|--------|-------|
| V1 | `cargo fmt --all --check` | ✅ | Passes. |
| V2 | `cargo check --features onnx` | ✅ | Passes. |
| V3 | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ | Passes. |
| V4 | `cargo test --workspace --exclude memcheck` | ✅ | Completed after the final Fix B commit; workspace tests and doctests passed with 0 failures. |
| V5 | Manual `leindex index --force` on a mid-size repo (no "worker process died"; `total_admitted` matches prior; save phase ms) | ⬜ | Pending final manual verification. |

---

## Files touched (current diff)

- `src/embed/protocol.rs` — `ErrorKind::FrameTooLarge`
- `src/embed/runtime.rs` — `FrameReadError` enum; reader-thread batch-id peek; main-loop `FrameTooLarge` response; tokenize-per-sub-batch in `run_onnx_embed`; removed dead `run_onnx_embed_batch_loop` (+ added `#[cfg(test)]` mockable `run_onnx_embed_batch_loop` for the VAL-ONNX tests); `run_onnx_embed_sub_batch` padded/trimmed; `run_onnx_embed_sub_batch_inner` real inference
- `src/embed/runtime_test.rs` — fixed `test_u8_dequant_preserves_unit_norm` (in-range data)
- `src/embed/worker_main.rs` — hermetic model name in 2 worker-entry tests (fixes onnx test hang)
- `src/search/onnx/client.rs` — sharding in `embed_attempt` + `embed_attempt_shard`, genericized over `AsRef<str>`
- `src/search/onnx/client_config.rs` — `ClientError::FrameTooLarge`, `MAX_REQUEST_FRAME_BUDGET`, `embed_request_frame_estimate<S: AsRef<str>>`
- `src/cli/index_builder/hybrid.rs` — `embed_neural_batch_blocking<S: AsRef<str>>`
- `src/cli/index_builder/mod.rs` — `cap_neural_text` (A6) + borrowed `&str` dedupe in `embed_pending_neural_batch` (C5) + borrowed `append_neural_batch`
- `src/storage/schema.rs` — v3→v4 migration + unique `(project_id, node_id)` index
- `src/storage/pdg_store.rs` — upsert `save_pdg` + `node_content_hash` + stale-node delete (C1–C3), regression tests
- `docs/plans/2026-08-09-execution-tracking.md` — this doc

## Process notes

- The apply-edit tool intermittently de-indents leading comment/brace lines; `cargo fmt --all` is the normalization authority and is run after each batch of edits.
- Temporary fix scripts (de-indent repairs) were removed after use.