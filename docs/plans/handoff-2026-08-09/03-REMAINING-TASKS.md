# Remaining Tasks — Prioritized

**As of:** 2026-08-10 (P0–P3 complete)

---

## Priority 0: Commit and validate the uncommitted working tree — ✅ DONE

- ✅ `TIRITH=0 cargo fmt --all --check`
- ✅ `TIRITH=0 cargo clippy --workspace --all-targets --features onnx -- -D warnings`
- ✅ `TIRITH=0 cargo test --workspace --exclude memcheck`
- ✅ Committed in logical groups (R2+R3+R4, tests, ORT stale-path fix, R1, R5+R6, cleanup-test serialization)

---

## Priority 1: R1 — Process-safe MIGraphX compile probe — ✅ DONE

The in-process timeout thread was replaced by a disposable `leindex-embed
--migraphx-probe` child (see `02-CURRENT-STATE.md`).

---

## Priority 2: R6 — Wire cancellation from client side — ✅ DONE (socket API)

`EmbeddingClient::cancel_batch(batch_id, reason)` sends Cancel over its own daemon
socket connection and verifies the acknowledged response. Pipe mode returns a clear
unsupported error. Auto-invocation from the indexing pipeline's external scheduler
remains a follow-up (callers today use the public API with a known BatchId).

---

## Priority 3: R5 — Per-sub-batch allocation micro-optimizations — ✅ DONE

Model input-name detection is cached in an `Arc<OnceLock<(bool,bool)>>`. The
`attention_mask` clone remains (ORT tensor construction consumes ownership while
pooling reads the original mask); no tokenizer-API rewrite was made.

---

## Priority 4: True tokenizer/inference pipelining (deferred Fix B) — IN PROGRESS

**Risk:** MEDIUM-HIGH (concurrency complexity). Design is in
`docs/plans/2026-08-09-fix-b-handoff-memory.md`.

**Prerequisites — ✅ satisfied:** R1 (process-safe MIGraphX probe) is complete, so
the pipeline cannot hang behind a leaked GPU compile.

**Design includes:** rendezvous channel, scoped producer, cancellation-aware
backpressure, BatchId-scoped registry, embed execution permit, panic/error
finalization, acceptance tests, and go/no-go benchmark criteria.
- [ ] Benchmark sequential path to establish whether pipelining is worth the complexity.

---

## Priority 5: Manual indexing verification

After all code fixes are committed:
- [ ] `TIRITH=0 cargo build --release --features onnx`
- [ ] `timeout 60 leindex setup --neural --gpu amd --warmup` — must complete (CPU fallback) within ~30s, not hang.
- [ ] `leindex index --force` on a mid-size repo.
- [ ] Confirm no "worker process died" errors.
- [ ] Confirm `total_admitted` matches prior baseline.
- [ ] Confirm save phase timings improved.
- [ ] Check worker logs for provider selection (should show CPU fallback with reason).

---

## Existing plan documents for reference

| Document | Content |
|----------|---------|
| `docs/plans/2026-08-09-post-implementation-remediation.md` | R1–R6 task checklist |
| `docs/plans/2026-08-09-fix-b-handoff-memory.md` | Detailed pipelining design with prerequisites |
| `docs/plans/2026-08-09-fix-b-deep-investigation.md` | Original Fix B gap analysis |
| `docs/plans/2026-08-09-execution-tracking.md` | Fix A/B/C status table |
| `docs/plans/2026-08-09-RESUME-GUIDE.md` | Historical recovery guide (marked as audit history) |
| `docs/plans/2026-08-09-neural-frame-overflow-and-pdg-perf-remediation.md` | Original remediation plan |
| `docs/plans/2026-08-09-outstanding-tasks.md` | Earlier task list (partially superseded by this file) |
