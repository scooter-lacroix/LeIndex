# Rollout Phase 5: Streaming Pipeline

## Prerequisite Phases
- Phase 1-4

## Flags Enabled
- `LEINDEX_FEATURE_STREAMING_SCAN=1`
- `LEINDEX_FEATURE_STREAMING_PARSE=1`
- `LEINDEX_FEATURE_STREAMING_PDG=1`
- `LEINDEX_FEATURE_STREAMING_TFIDF=1`
- `LEINDEX_FEATURE_STREAMING_NEURAL=1`
- All previous phase flags

## Description
Convert scan/parse/PDG/TF-IDF/neural from accumulate-then-write to streaming-
bounded: one batch in, one batch out. Direct staged writes to CAS. No
corpus-wide materialization. Per-stage RSS independent of corpus size.

## Validation Commands
```bash
cargo test -p leindex --features full -- stream
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Success Criteria
- VAL-STREAM-001..015: all streaming tests pass
- Per-stage RSS independent of corpus size
- No source body retained during scan
- Kill-mid-pipeline checkpoint resume verified

## Rollback Procedure
```bash
export LEINDEX_FEATURE_STREAMING_SCAN=0
export LEINDEX_FEATURE_STREAMING_PARSE=0
export LEINDEX_FEATURE_STREAMING_PDG=0
export LEINDEX_FEATURE_STREAMING_TFIDF=0
export LEINDEX_FEATURE_STREAMING_NEURAL=0
# Falls back to legacy materialize-then-write pipeline
```

## Evidence
- VAL-STREAM-001: per-stage RSS independence from corpus size
- VAL-STREAM-010: retrieval metrics identical to pre-streaming path
