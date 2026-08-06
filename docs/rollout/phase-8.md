# Rollout Phase 8: Default-On Flip

## Prerequisite Phases
- Phase 1-7: ALL must pass their runbook validation

## Flags Enabled
All rollout flags flipped to default ON:
- `DaemonClient`
- `GenerationReaders`
- `BoundedScheduler`
- `StreamingScan` / `StreamingParse` / `StreamingPdg` / `StreamingTfidf` / `StreamingNeural`
- `GlobalEmbedCache`
- `ValidatedModel`

## §16 Acceptance-Gate Audit

This phase can only proceed if ALL section 16 acceptance gates pass:

### Resource Gate
- [x] Steady-state aggregate RSS ≤ 1024 MiB (budget ledger verified)
- [x] Peak aggregate RSS ≤ 1024 MiB (during indexing)
- [x] No monotonic RSS growth across 100+ reindex cycles (VAL-ROLLOUT-009)
- [x] Idle CPU ~0 (daemon + worker idle-exit)
- [x] Thread budgets: Tokio workers=2, ORT threads bounded
- [x] GPU VRAM counted in budget ledger

### Performance Gate
- [x] No p50/p95/p99 query latency regression vs v1.9.5 baseline
- [x] Responsive-during-index (no-stall read via generation lease)
- [x] Wall-time index no regression
- [x] CPU-sec/MiB improvement (streaming pipeline + CAS dedup)

### Quality Gate
- [x] All 14 evaluation categories pass aggregate + per-category gates
- [x] No stale/omitted/partial results
- [x] Model identity reproducible (VAL-ROLLOUT-004)
- [x] Reranker ablation pass (VAL-EVAL-005)

### Reliability Gate
- [x] Crash/cancel preserves last valid generation (VAL-WRITER-005)
- [x] No duplicate daemon/worker (single-winner lock, VAL-DAEMON-006)
- [x] No cross-project confusion (VAL-EQUIV-001)
- [x] Mismatches fail safely (artifact validation, VAL-ROLLOUT-004)
- [x] Cleanup safe (never removes leased/current/rollback, VAL-ROLLOUT-010)

## Validation Commands
```bash
# Full verification matrix (24 scenarios)
cargo test --test fault_scenarios_01_06 -- --test-threads=1
cargo test --test fault_scenarios_07_12 -- --test-threads=1
cargo test --test soak_scenarios_13_18 -- --test-threads=1
cargo test --test soak_scenarios_19_24 -- --test-threads=1

# Gate audit
cargo test --test gate_audit_test -- --test-threads=1

# Full workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Success Criteria
- All 24 §13 verification scenarios pass with full telemetry capture
- All §16 acceptance gates evidenced as PASS
- Any unmet gate BLOCKS the flip (anti-cheat: never manufacture a pass)

## Rollback Procedure
```bash
# Set flag to false to override default-on
export LEINDEX_FEATURE_DAEMON_CLIENT=0
export LEINDEX_FEATURE_GENERATION_READERS=0
export LEINDEX_FEATURE_BOUNDED_SCHEDULER=0
export LEINDEX_FEATURE_GLOBAL_EMBED_CACHE=0
export LEINDEX_FEATURE_VALIDATED_MODEL=0
# Legacy code paths remain reachable
```

## Evidence
- VAL-ROLLOUT-007: functional scenarios 1-12 pass
- VAL-ROLLOUT-008: soak/fault scenarios 13-24 pass
- VAL-ROLLOUT-009: 100-reindex no monotonic growth
- VAL-ROLLOUT-012: gate audit PASS
- VAL-CROSS-005: full default-on system under 1 GiB aggregate
- VAL-CROSS-007: idle soak with zero resource growth
