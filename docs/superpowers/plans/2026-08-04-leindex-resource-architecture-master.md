# LeIndex Resource Architecture - Master Plan

> **For agentic workers:** This is the master tracking plan. Execute sub-plans sequentially via superpowers:subagent-driven-development. Each sub-plan is in its own file. Update this master after each sub-plan completes.

**Goal:** Transform LeIndex from per-harness heavyweight processes to a single user-scoped daemon with sub-1 GiB aggregate memory.

**Architecture:** One `leindexd` daemon + tiny stdio shims + shared `leindex-embed` worker. Streaming index pipeline with immutable mmap generations. Fair bounded scheduler with global admission control.

**Spec:** `docs/superpowers/specs/2026-08-04-leindex-resource-architecture-design.md`

---

## Sub-Plan Execution Order

| # | Plan File | Workstreams | Status | Depends On |
|---|-----------|-------------|--------|------------|
| 1 | `2026-08-04-ws1-2-baseline-containment.md` | WS1: Baseline Profiler, WS2: Safe Containment | 🟨 Drafted | None |
| 2 | `2026-08-04-ws3-daemon-shim.md` | WS3: User-Scoped Daemon + Stdio Shim | 🟨 Drafted | #1 |
| 3a | `2026-08-04-ws4-generation-store.md` | WS4: Immutable mmap Generation Store + Size Reduction | 🟨 Drafted | #2 |
| 3b | `2026-08-04-ws5-scheduler.md` | WS5: Fair Bounded Scheduler + Admission Controller | 🟨 Drafted | #3a |
| 4 | `2026-08-04-ws6-9-streaming-pipeline.md` | WS6-9: Streaming Scan/Parse/PDG/TF-IDF/Neural | 🟨 Drafted | #3a |
| 5 | `2026-08-04-ws10-worker-cache.md` | WS10: Shared Worker + Global Embedding Cache | 🟨 Drafted | #4 |
| 6 | `2026-08-04-ws11-model-eval.md` | WS11: Model/Reranker Conversion + Bake-off | 🟨 Drafted | #5 |
| 7 | `2026-08-04-ws12-13-migration-rollout.md` | WS12-13: Migration, Compatibility, Rollout | 🟨 Drafted | #6 |

---

## Decision Log

| Date | Sub-Plan | Decision | Rationale | Approved? |
|------|----------|----------|-----------|-----------|
| 2026-08-04 | — | Group 13 workstreams into 7 sub-plans under this master | Manageable execution units; each independently testable | ✅ User |
| 2026-08-04 | — | Plan mode read-only until each sub-plan vetted against ground truth | Avoid reinventing existing infra (memcheck, MemoryCapGuard, McpProjectLock, leindex-embed) | ✅ User |
| 2026-08-04 | SP1 | Extend `tools/memcheck`, not reinvent sampler | `tools/memcheck/src/{sampler,workload,report}.rs` already covers RSS/PSS/anon/mapped/worker + 12 canonical phases | ✅ User |
| 2026-08-04 | SP1 | MALLOC_ARENA_MAX via launcher env, not runtime set_var | glibc reads it at malloc init (pre-main); runtime set_var is a no-op. jemalloc is feature-gated (`memprof`) | ✅ User |
| 2026-08-04 | SP3a | Split SP3 → SP3a (WS4) + SP3b (WS5) | WS4 generation-store design grew large (CAS/mmap/leases/retention/size-reduction); merits own spec+plan. WS5 scheduler is independent concern | ✅ User |
| 2026-08-04 | SP3a | Generation-store approach: content-addressed CAS + mmap manifests (Approach 1) | Only approach satisfying no-2×-RAM via page-sharing for incremental reindex; decomposes cleanly (WS4=reader/format, WS6-9=writer) | ✅ User |
| 2026-08-04 | SP3a | DB layer: copy-into-CAS with VACUUM-normalize (not WAL-snapshot) | sha256 evidence shows adjacent gens byte-identical → CAS dedup collapses; WAL carries -wal/-shm bloat | ✅ User (delegated) |
| 2026-08-04 | SP3a | Neural reads: quantized-native SIMD dot-products (not dequant-on-read) | Wins both RSS and speed (INT8 dot ~4× f32); user directive "lowest+fastest wins" | ✅ User (delegated) |
| 2026-08-04 | SP3a | LeIndex stays !Sync; readers lease mmap generation (no connection pool) | Goal is no-stall, not cross-core parallelism; readers never touch writer Mutex | ✅ User |
| 2026-08-04 | SP3a | Retention: current+1 previous; jobs 128MiB/project + delete-on-publish | Spec §10.2 literal; footprint mandate; completed jobs have zero resume value | ✅ User (delegated) |
| 2026-08-04 | SP3a | All TBD items MUST be resolved (measured decision) for plan completion | User mandate — no dangling TBDs | ✅ User |

## Issues Encountered

| Date | Sub-Plan | Issue | Resolution |
|------|----------|-------|------------|
| | | | |

## Deviations Requested

| Date | Sub-Plan | Deviation | Reason | Approved? |
|------|----------|-----------|--------|-----------|
| | | | | |

## Ground-Truth Findings (per sub-plan)

### SP1: Baseline + Containment
- `tools/memcheck/` is a full harness (sampler/workload/report/diff + tests). VAL-MEASURE/VAL-CPHASE coverage.
- `src/cli/memory_cap.rs` `MemoryCapGuard` exists with throttling.
- jemalloc feature-gated behind `memprof` (`src/bin/leindex.rs:14-16`); default = glibc.
- `#[tokio::main]` in `src/bin/leindex.rs` uses `available_parallelism` (no override).
- Env knobs exist: `LEINDEX_WORKER_ORT_THREADS`, `LEINDEX_ONNX_INFERENCE_BATCH_SIZE`, `LEINDEX_ONNX_SEQUENCE_LEN`, `LEINDEX_WORKER_MIN_AVAILABLE_MB`.

### SP2: Daemon + Shim
- `leindex-embed` worker binary already exists (`src/bin/leindex-embed.rs` → `src/embed/worker_main.rs`): socket accept loop, `MAX_SOCKET_CLIENT_THREADS=16`, `PR_SET_PDEATHSIG`, idle timeout, frame protocol.
- `McpProjectLock` (`src/cli/mcp/lock.rs`): advisory run-dir sidecars at `~/.leindex/run/leindex-mcp-<hash>.{lock,start}`. Advisory only (stdio = 1:1 with agent pipe).
- `mcp/server.rs` already has a unix-socket transport + `ProcessIdleClock`.
- `leindex cleanup --stale-daemons` (`src/cli/cleanup.rs`) sweeps run-dir.
- **No `leindexd` daemon exists.** Model is still per-harness `leindex mcp --stdio`.

### SP3-7: (vet before drafting)

## Progress Notes

### Sub-Plan 1: Baseline + Containment
- **Started:** —
- **Completed:** —
- **Notes:** Approved 2026-08-04. Saved.

### Sub-Plan 2: Daemon + Shim
- **Started:** —
- **Completed:** —
- **Notes:** —

### SP3a: WS4 Generation Store
- **Spec:** `docs/superpowers/specs/2026-08-04-ws4-generation-store-design.md`
- **Plan:** `docs/superpowers/plans/2026-08-04-ws4-generation-store.md`
- **Started:** —
- **Completed:** —
- **Notes:** Brainstormed 2026-08-04. Production bloat diagnosed (2.5 GiB vs 145 MiB real on 419-file repo; jobs 2 GiB/114 jobs, full-copy gens, loose retention). Design: CAS + mmap manifests + leases + byte-bounded retention. TBD items (refcount store, INT8 readpath, sparse TF-IDF, interning) are explicit measured-decision tasks.

### SP3b: WS5 Scheduler
- **Started:** —
- **Completed:** —
- **Notes:** Brainstorm pending. Ground truth: `MemoryCapGuard` is per-job throttle (not global admission); `index_slots` is same-project consolidation (not fairness). Build WorkBudget/Step/BoundedJob + DRR + global admission from scratch.

### Sub-Plan 4: Streaming Pipeline
- **Started:** —
- **Completed:** —
- **Notes:** —

### Sub-Plan 5: Worker + Cache
- **Started:** —
- **Completed:** —
- **Notes:** —

### Sub-Plan 6: Model Eval
- **Started:** —
- **Completed:** —
- **Notes:** —

### Sub-Plan 7: Migration + Rollout
- **Started:** —
- **Completed:** —
- **Notes:** —

---

## Acceptance Gates (from Spec §16)

### Resource
- [ ] Aggregate steady RAM ≤ 1 GiB (3 clients, 2 projects)
- [ ] Aggregate full-index peak RAM ≤ 1 GiB (production model profile)
- [ ] Zero monotonic RSS/swap growth across 100 reindexes
- [ ] Idle CPU near zero over long soak
- [ ] Thread counts within explicit budgets
- [ ] GPU memory/utilization reported and within approved profile

### Performance
- [ ] Common tool p50/p95/p99 no worse than baseline
- [ ] Search responsive during indexing
- [ ] Full/incremental index wall time no worse than baseline
- [ ] CPU-seconds per indexed MiB/node improve materially
- [ ] No pathological cold-start loop or model reload churn

### Quality
- [ ] Aggregate retrieval metrics meet predeclared equivalence gates
- [ ] Protected categories meet per-category gates
- [ ] No stale, omitted, or partial index behavior
- [ ] Model/prompt/pooling/normalization identity reproducible
- [ ] Reranker removal/replacement/quantization each pass ablation

### Reliability
- [ ] Crash/cancel tests preserve last valid generation
- [ ] No duplicate daemon/worker under startup races
- [ ] No cross-project context confusion
- [ ] Protocol/artifact mismatches fail safely
- [ ] Cleanup never removes leased/current/rollback generations

### Repository Quality
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
