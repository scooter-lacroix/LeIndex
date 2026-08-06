# Rollout Phase 4: Bounded Scheduler

## Prerequisite Phases
- Phase 1-3

## Flags Enabled
- `LEINDEX_FEATURE_BOUNDED_SCHEDULER=1`
- All previous phase flags

## Description
Route heavy indexing work through the fair bounded scheduler (DRR by
client/project/class). Index jobs become stepped `BoundedJob`s with yield
points for cancellation and checkpoint resume. The admission controller gates
memory pressure (defer, never error).

## Validation Commands
```bash
cargo test -p leindex --features full -- scheduler
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Success Criteria
- VAL-SCHED-001..015: all scheduler tests pass
- No p95 read latency regression under contention
- AdmissionController returns only Admit/Defer/Reduce (never error)
- Index job killed mid-step resumes at checkpoint

## Rollback Procedure
```bash
export LEINDEX_FEATURE_BOUNDED_SCHEDULER=0
# Falls back to legacy spawn_blocking + error-at-cap indexing
```

## Evidence
- VAL-SCHED-006: same-project duplicate coalescing
- VAL-SCHED-013: no p95 read latency regression under contention
- VAL-SCHED-015: MemoryCapGuard error path removed from indexing hot loop
