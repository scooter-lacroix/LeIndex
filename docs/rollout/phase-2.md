# Rollout Phase 2: CAS Generation Store

## Prerequisite Phases
- Phase 1 (Generation Readers Opt-In)

## Flags Enabled
- `LEINDEX_FEATURE_GENERATION_MIGRATION=1`
- `LEINDEX_FEATURE_GENERATION_READERS=1`

## Description
Enable the one-time legacy → CAS migration sweep. On first `LeIndex::new` with
an existing v1.9.x `.leindex/`, the migration converts full-copy generations to
CAS-backed immutable mmap generations. This is the footprint reduction phase:
2.5 GiB → ≤60 MiB (this repo).

## Validation Commands
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
# Migration tests
cargo test -p leindex --features full -- test_legacy test_migration
```

## Success Criteria
- VAL-MIGRATE-001..005: all migration tests pass
- Footprint gate: `.leindex/` ≤ 60 MiB after migration
- No query result regression after migration

## Rollback Procedure
```bash
export LEINDEX_FEATURE_GENERATION_MIGRATION=0
# The pre-migration backup preserves the full-copy layout
```

## Evidence
- VAL-FOOTPRINT-001: `.leindex/` ≤ 60 MiB
- VAL-MIGRATE-001: legacy → CAS conversion correct
- docs/baselines/2026-08-04-ws4-migration.json: before/after footprint
