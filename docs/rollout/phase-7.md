# Rollout Phase 7: Validated Model Profile

## Prerequisite Phases
- Phase 1-6

## Flags Enabled
- `LEINDEX_FEATURE_VALIDATED_MODEL=1`
- All previous phase flags

## Description
Enable the WS11 validated model profile as the production default:
CodeRankEmbed 137M (INT8 quantized) with no reranker. This replaces the legacy
FP16 Qwen3 + reranker baseline under the section 7 budget target (≤1 GiB).

## Validation Commands
```bash
cargo test -p leindex --features full -- eval
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Success Criteria
- VAL-EVAL-001..010: all eval tests pass
- Winning model fits within 1 GiB budget
- INT8 read-path parity verified
- No manufactured pass when physics conflicts with budget

## Rollback Procedure
```bash
export LEINDEX_FEATURE_VALIDATED_MODEL=0
# Falls back to legacy FP16 Qwen3 + reranker
```

## Evidence
- VAL-EVAL-007: budget ledger fit confirmed for winning profile
- VAL-EVAL-006: INT8 read-path within parity band
- VAL-CROSS-004: model eval winner + INT8 read-path + budget fit
