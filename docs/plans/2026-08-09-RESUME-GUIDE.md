# LeIndex Remediation — Comprehensive Tracking & Resume Guide

**Plan:** `docs/plans/2026-08-09-neural-frame-overflow-and-pdg-perf-remediation.md`
**Started:** 2026-08-09 · **Branch:** `v2.0.0` · **Last updated:** 2026-08-09 18:20 EDT
**Validation gate:** `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --exclude memcheck`

> **READ THIS FIRST.** A new agent picking this up needs no other context —
> this doc records every decision, root cause, file state, and the exact
> remaining work. A file-corruption incident destroyed one file's uncommitted
> edits; that is documented below with the precise redo instructions.

## Current verified runtime state (updated 2026-08-09 19:59 EDT)

The earlier corruption note below is historical and no longer describes the
working tree. `src/embed/runtime.rs` now contains the MIGraphX compile-timeout
CPU fallback, bounded sequential per-sub-batch tokenization, the test-only
pre-tokenized batching helper, and `run_onnx_embed_sub_batch_inner`.

Fix B is implemented as **bounded sequential tokenization followed immediately
by inference**. It intentionally does not overlap tokenizer work with ORT
inference; true two-stage pipelining remains a separate, explicitly unstarted
task. New hermetic tests verify tokenizer batch sizes, tokenize→infer ordering,
fixed-batch padding, later tokenizer failure, and cancellation.

The current implementation and task status are tracked in:
`docs/plans/2026-08-09-outstanding-tasks.md` and
`docs/plans/2026-08-09-execution-tracking.md`.

---

## TL;DR — current state

- **Fixes A (A1–A6), bounded sequential Fix B (B1–B3), and C (C1–C5)
  are implemented and verified.**
- `src/embed/runtime.rs` contains the MIGraphX compile-hang guard and CPU
  fallback, per-sub-batch tokenization, the test-only pre-tokenized batching
  helper, and the raw `run_onnx_embed_sub_batch_inner` inference helper.
- True tokenizer/inference overlap is intentionally not implemented. It remains
  a separate future task because it requires a bounded producer/consumer design
  and careful cancellation/thread-safety handling.
- The historical corruption and redo instructions below are retained as audit
  history only; they are not a description of the current working tree.

---

## Historical audit: MIGraphX compile-hang finding

The following evidence and wording describe the pre-fallback failure mode. The
current implementation includes the bounded probe and CPU fallback; retain this
section only as root-cause history.

### What happens (proven empirically, not assumed)

On this host (AMD Radeon RX 7900 XTX, **ROCm 7.2.4**, ORT 1.25.0 from pip with
`libonnxruntime_providers_migraphx.so`):

1. `Session::builder()...commit_from_file(model)` **succeeds in ~6 seconds** for
   every model (f32 dynamic, uint8 dynamic, non-dynamic) at both
   `GraphOptimizationLevel::Level1` and `Level3`. Session BUILD is fine.
2. The **first `session.run(...)`** triggers ORT's lazy
   `MIGraphXExecutionProvider::Compile` → `migraphx_program_compile` →
   `migraphx::program::compile` → **`module::repeat_while_changes`**, which
   **spins forever** (pass loop never converges). Confirmed via a captured
   native stack trace (see §"Evidence").
3. This hangs **every model** (not just uint8), at batch=1/seq=16 and batch=8/
   seq=128, at Level1 and Level3. The only previously-working `.mxr` caches
   (`qwen3-embed-0_6b`, `sfr-embedding-code-400m`, dated 26 Jul) were compiled
   under an **earlier ROCm** before the 7.2.4 upgrade; all new compiles hang.
4. The **CPU provider works perfectly**: session build ~3 s, inference ~0.07 s
   for batch=1/seq=16. Output shape `(1, 16, 1024)`.

### Root cause

ROCm 7.2.4 / MIGraphX 2.15.0 compiler regression: the `repeat_while_changes`
optimization pass loop does not converge on the Qwen3-Embedding graph family.
This is an **upstream MIGraphX bug**, not a LeIndex code defect — but LeIndex
must handle it because the default config selects MIGraphX, so `WorkerRuntime::new`
currently hangs forever and the index never completes.

### Evidence (reproducible)

- `/tmp/ort_infer.py` — python ORT probe. `timeout 120 python3 /tmp/ort_infer.py
  <model> <batch> <seq> <level>` → RC=124 (timeout), stack ends in
  `migraphx::program::compile` → `repeat_while_changes`.
- `/tmp/ort_probe.py` — python ORT session-build-only probe → completes in ~6 s
  (proves BUILD is fine; HANG is at first RUN).
- `/tmp/ort_cpu2.log` — CPU provider full inference: build 3.01 s, inference
  0.071 s, output `(1,16,1024)`.
- `rocm-smi`: 7900 XTX, ROCm 7.2.4; `cat /opt/rocm/.info/version` = 7.2.4.
- ORT dylib: `/home/scooter/.mlstack/global/lib/python3.12/site-packages/onnxruntime/capi/libonnxruntime.so.1.25.0`
  + `libonnxruntime_providers_migraphx.so` (links `libmigraphx_c.so.3` →
  `/opt/rocm/lib/migraphx/lib/libmigraphx.so.2015000.0`).

### THE FIX (design ready, needs re-implementation in runtime.rs)

Add a **bounded warmup probe + CPU fallback** to `build_session`:

1. New free fn `build_cpu_session(model_path, ort_threads) -> Result<Session, ort::Error>`
   — builds a Level1 CPU-only session (mirrors `try_provider_or_cpu`'s fallback).
2. New free fn `probe_migraphx_compile_timeout(session: &Arc<Mutex<Session>>,
   max_wait: Duration) -> Result<(), String>` — spawns a helper thread that builds
   minimal tensors (batch=1, seq=min(16, configured_seq_len); input_ids,
   attention_mask, position_ids, token_type_ids all-zeros) and runs one
   `session.run(...)`, feeding only the inputs the model declares (mirror the
   `(uses_position_ids, uses_token_type_ids)` 4-arm match in
   `run_onnx_embed_sub_batch_inner`). Returns `Ok(())` on completion,
   `Err(reason)` on timeout (`rx.recv_timeout(max_wait)` → `Err(RecvTimeoutError::Timeout)`).
3. In `build_session`, after `let session = session_builder.commit_from_file(model_path)?;`,
   add:
   ```rust
   if matches!(provider_name, "migraphx" | "rocm") {
       const MIGRAPHX_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
       let session_arc = Arc::new(Mutex::new(session));
       match probe_migraphx_compile_timeout(&session_arc, MIGRAPHX_PROBE_TIMEOUT) {
           Ok(()) => {
               let session = match Arc::try_unwrap(session_arc) {
                   Ok(mutex) => mutex.into_inner().unwrap_or_else(|p| p.into_inner()),
                   Err(_) => build_cpu_session(model_path, ort_threads)?,
               };
               return Ok(SessionBuildOutcome { session, provider_status });
           }
           Err(reason) => {
               tracing::warn!("{reason}; falling back to CPU for {}", model_path.display());
               return Ok(SessionBuildOutcome {
                   session: build_cpu_session(model_path, ort_threads)?,
                   provider_status: ProviderRuntimeStatus::fallback_to_cpu(
                       format!("MIGraphX compile probe: {reason}")),
               });
           }
       }
   }
   ```
   Both helpers must be **free fns at module scope** (not inside `impl WorkerRuntime`),
   gated `#[cfg(feature = "onnx")]`, and must `use`/reference
   `crate::embed::runtime_env::{configured_onnx_sequence_len, build_position_ids}`.

**Why this is correct:** the probe pays one ~20 s bounded wait only at session
build (once per worker process). On a healthy MIGraphX (older ROCm or after an
upstream fix), the tiny compile completes in seconds and the GPU session is kept.
On the hanging ROCm 7.2.4, it fails fast to CPU (~3 s + 0.07 s/inference) so the
index completes instead of blocking forever. This satisfies "all model ops must be
rapid; if they can't be, degrade instead of hang."

---

### Historical runtime redo instructions

The following section records the recovery procedure used earlier in this
session. It is retained for auditability; the listed runtime work is now
implemented and verified as described in the current-state section above.

### Redo 1 — Fix B: tokenize per sub-batch in `run_onnx_embed` (GPU utilization)

In `run_onnx_embed` (around the embed-attempt body), replace the single up-front
whole-batch `tokenizer.encode_batch(text_refs, true)` + its `if encodings.is_empty()`
guard with:
- Keep `let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();`
- Replace the up-front tokenize with `if texts.is_empty() { return Ok(EmbedResponse::new(vec![], 0, expected_dim)); }`
- Inside the `while let Some(sub_texts) = sub_windows.next()` loop, tokenize
  **each sub-batch** with `tokenizer.encode_batch(sub_texts.to_vec(), true)`,
  then call `self.run_onnx_embed_sub_batch(session, &encodings, expected_dim, fixed_batch)?`
  and extend `all_pooled`.
- The `fixed_batch` padding/trim logic moves INTO `run_onnx_embed_sub_batch`
  (which gains a `fixed_batch: bool` param) so the loop body stays bounded.
- The retry-once collapsed-batch fallback loop in `finalize_embed_output`'s err
  arm must pass `false` for `fixed_batch` when retrying single sequences.

### Redo 2 — test bindings for the removed `run_onnx_embed_batch_loop`

The Fix-B refactor removed the public `run_onnx_embed_batch_loop`, but 4 onnx
tests in `runtime_test.rs` (`test_embed_batch_cpu_all_sub_batches_processed`,
`test_embed_batch_migraphx_full_sub_batches_no_padding`,
`test_embed_batch_migraphx_partial_sub_batch_padded_and_trimmed`,
`test_embed_response_invariant_all_providers`) call it. Re-add a
**`#[cfg(test)]` mockable helper** with the exact signature the tests use:

```rust
#[cfg(feature = "onnx")]
#[cfg(test)]
fn run_onnx_embed_batch_loop(
    &self,
    encodings: &[tokenizers::Encoding],
    inference_batch_size: usize,
    fixed_batch: bool,
    dim: usize,
    runner: impl FnMut(&[tokenizers::Encoding], usize) -> Result<Vec<f32>, WorkerError>,
) -> Result<Vec<f32>, WorkerError> {
    let mut runner = runner;
    let mut all_pooled = Vec::with_capacity(encodings.len() * dim);
    for sub in encodings.chunks(inference_batch_size) {
        if fixed_batch && sub.len() < inference_batch_size {
            let mut padded = sub.to_vec();
            if let Some(template) = sub.first() {
                padded.resize(inference_batch_size, template.clone());
            }
            let pooled = runner(&padded, dim)?;
            all_pooled.extend_from_slice(&pooled[..sub.len() * dim]);
        } else {
            let pooled = runner(sub, dim)?;
            all_pooled.extend_from_slice(&pooled);
        }
    }
    Ok(all_pooled)
}
```

Also split the real inference into `run_onnx_embed_sub_batch_inner` (the existing
tensor-build + `session.run` body) and mark it
`#[cfg_attr(test, allow(dead_code, clippy::only_used_in_recursion))]` so the test
helper (which mocks the runner) doesn't trip dead-code on the real path.

### Redo 3 — the MIGraphX compile-hang guard (the new fix, specified in §"THE FIX" above)

Implement `build_cpu_session` + `probe_migraphx_compile_timeout` and wire the
probe into `build_session` as shown. **This is the most important remaining item**
— without it the default-config worker hangs forever on ROCm 7.2.4.

### Verification after runtime.rs redo

- `cargo fmt --all --check`
- `cargo check --features onnx`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --lib --features onnx embed::runtime::tests::test_embed_batch` (3 tests)
- `cargo test --lib --features onnx embed::runtime::tests::test_embed_response_invariant_all_providers`
- **The definitive runtime proof:** `cargo test --lib --features onnx
  real_pipe_embed_seeds_migraphx_cache -- --ignored --nocapture` should now
  either embed on GPU (if MIGraphX healthy) OR fall back to CPU and still return
  vectors — never hang. Also run `timeout 60 leindex setup --neural --gpu amd
  --warmup` → must complete (CPU fallback) within ~30 s, not hang.

---

## Historical implementation audit

The following section records the files and validation state during the earlier
corruption/recovery incident. It is retained for auditability only. The
current-state banner and execution-tracking document are authoritative.

### Historical Fix A audit

The Fix A details below describe the earlier recovery snapshot; they are not a
current working-tree status report. Current status is authoritative in the
execution-tracking table above.

- **A1** `src/embed/protocol.rs`: `ErrorKind::FrameTooLarge` variant.
- **A2/A3** `src/search/onnx/client_config.rs`: `ClientError::FrameTooLarge`,
  `MAX_REQUEST_FRAME_BUDGET` (16 MiB), `embed_request_frame_estimate<S: AsRef<str>>`.
- **A4** `src/search/onnx/client.rs`: client-side sharding in `embed_attempt`
  (estimate → fast-path single frame or split → per-shard `embed_attempt_shard`
  → concatenate `into_vectors()`). Genericized over `AsRef<str>`:
  `embed_with_fallback<S>`, `embed_attempt<S>`, `embed_attempt_shard<S>`.
- **A5** `src/embed/runtime.rs` (historical recovery item): worker-side
  graceful `FrameTooLarge` in `run_loop` (`FrameReadError` enum, batch-id peek,
  error-frame + continue). This was re-applied and is present in the current
  runtime.
- **A6** `src/cli/index_builder/mod.rs`: `cap_neural_text` (64 KiB UTF-8-safe
  truncation, `#[cfg(any(onnx, remote-embeddings))]`) + borrowed `&str` dedupe
  in `embed_pending_neural_batch`.

### Historical Fix B audit

- **B1/B2** were in runtime.rs (tokenize-per-sub-batch + dead-loop removal) —
  lost in corruption, must be redone (Redo 1 & 2 above).
- **B3** compile-verify: was GREEN before corruption.

### Historical Fix C audit

- **C1** `src/storage/pdg_store.rs`: `save_pdg` upsert (`ON CONFLICT(project_id,
  node_id) DO UPDATE ... WHERE content_hash != excluded.content_hash`), pre-query
  existing rows, reuse unchanged db ids (zero writes), stale-node delete.
  Regression tests: `test_resave_unchanged_pdg_issues_no_node_writes`,
  `test_resave_with_changed_node_writes_only_changed_rows`.
- **C2** `src/storage/pdg_store.rs`: `save_pdg` re-asserts `journal_mode=WAL;
  synchronous=NORMAL`.
- **C3** `src/storage/pdg_store.rs`: `node_content_hash()` (hash content fields
  once, reuse for column + skip check).
- **C4** `src/graph/extraction_cross_file.rs`: verified built-once (no change
  needed — `build_cross_file_call_indexes` already single-pass).
- **C5** `src/cli/index_builder/mod.rs` + `hybrid.rs`: borrowed `&str` neural
  batch (`embed_neural_batch_blocking<S: AsRef<str>>`), dedupe identical capped
  texts, `append_neural_batch` borrows.

### Schema migration (C1 dependency) ✅

- `src/storage/schema.rs`: `SCHEMA_VERSION` 3→4, `migrate_v3_to_v4` (dedupe
  legacy `intel_nodes` keeping lowest id per `(project_id, node_id)`, drop
  orphan edges), unique index `uq_intel_nodes_project_node`. Test:
  `test_v3_to_v4_migration_dedupes_duplicate_node_ids`.

### Test-hermeticity fixes (B2 fallout) ✅

- `src/embed/worker_main.rs`: `test_runtime_handles_embed_request` +
  `test_run_loop_single_request` use `__leindex_test_no_model__` (hermetic, no
  real model load) — fixes the onnx test-suite hang that was masking the real
  MIGraphX issue.
- `src/embed/runtime_test.rs`: `test_u8_dequant_preserves_unit_norm` fixed — old
  data used `0.5` components (outside the quantizer's representable range
  `[-0.299, 0.401]`, clipped → norm 0.80); switched to in-range `[0.35; 8]`.

---

## Files & their current state

| File | State | Notes |
|------|-------|-------|
| `src/storage/schema.rs` | ✅ modified, tests pass | v3→v4 migration + unique index. |
| `src/storage/pdg_store.rs` | ✅ modified, tests pass | upsert save + hash reuse + stale delete. |
| `src/cli/index_builder/mod.rs` | ✅ modified | `cap_neural_text` (A6) + borrowed dedupe (C5). |
| `src/cli/index_builder/hybrid.rs` | ✅ modified | `embed_neural_batch_blocking<S: AsRef<str>>`. |
| `src/search/onnx/client.rs` | ✅ modified | sharding + `AsRef<str>` generics. |
| `src/search/onnx/client_config.rs` | ✅ modified | `FrameTooLarge`, budget, estimate generic. |
| `src/embed/protocol.rs` | ✅ modified | `ErrorKind::FrameTooLarge`. |
| `src/embed/worker_main.rs` | ✅ modified | hermetic test model names. |
| `src/embed/runtime_test.rs` | ✅ modified | fixed `test_u8_dequant...` in-range data. |
| `src/embed/runtime.rs` | ✅ current | Contains frame-too-large handling, MIGraphX fallback, bounded per-sub-batch tokenization, test-only pre-tokenized batching helper, and raw inner inference helper. |

## Historical validation snapshot

The failure/pending entries below are retained as incident history only. They
were superseded by commit `1e9687e5` and the successful post-commit validation
recorded in `docs/plans/2026-08-09-execution-tracking.md`.

### Historical validation outcomes

- Earlier `cargo fmt --all --check`: ✅
- Earlier no-feature checks/tests: ✅
- Earlier ONNX checks/tests: pending at that incident point; superseded by the
  current successful ONNX clippy/runtime and workspace validation recorded in
  `docs/plans/2026-08-09-execution-tracking.md`.
- Manual installation/indexing verification remains pending.

## How to resume after a future interruption

1. Read the current-state banner, `docs/plans/2026-08-09-execution-tracking.md`,
   and `docs/plans/2026-08-09-outstanding-tasks.md`.
2. Confirm the runtime and documentation status with `git status` and the
   authoritative tracking table.
3. Do not repeat historical redo instructions unless the current tree actually
   lacks the named symbols.
4. Run the documented validation gate before installation/indexing.
5. Manual installation/indexing verification remains the final pending gate.

## Process notes / hazards

- **The apply-edit tool intermittently de-indents leading comment/brace lines.**
  Always run `cargo fmt --all` after edits; it is the normalization authority.
- **Do NOT use inline `python3 -c "..."` in fish** — fish's quoting/escaping
  destroyed `runtime.rs`. Write python scripts to `/tmp/*.py` files and run
  `python3 /tmp/file.py` instead.
- **Long-running commands crash the agent session** in this environment. Run
  GPU/compile operations with a `timeout` wrapper and redirect to a log file;
  read the log afterward with `tail`. Do not tail/`cat` (alias→bat pages) during
  a running compile — it has triggered session crashes.
- Temporary probe scripts live in `/tmp` (`ort_probe.py`, `ort_infer.py`,
  `inspect_onnx.py`); they are the empirical evidence and can be re-run.