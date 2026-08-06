# Rollout Phase 1: Generation Readers Opt-In

## Prerequisite Phases
- None (first phase)

## Flags Enabled
- `LEINDEX_FEATURE_GENERATION_READERS=1`

## Description
Enable the mmap generation read-path handlers for search/symbol/deep-analyze.
The legacy heap-mirror path remains as fallback. Both paths produce bit-identical
results (VAL-EQUIV-001/002/003).

## Validation Commands
```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
LEINDEX_FEATURE_GENERATION_READERS=1 cargo test -p leindex --features full
```

## Success Criteria
- All validation commands pass with zero warnings and zero errors
- Read-path equivalence tests pass (VAL-EQUIV-001/002/003)
- No query result regression vs heap-mirror path

## Rollback Procedure
```bash
export LEINDEX_FEATURE_GENERATION_READERS=0
# Legacy heap-mirror path serves all reads
```

## Evidence
- VAL-EQUIV-001: bit-for-bit read-path equivalence test passes
- VAL-EQUIV-002: no-stall read during indexing verified
- VAL-READER-001..010: all reader correctness tests pass
