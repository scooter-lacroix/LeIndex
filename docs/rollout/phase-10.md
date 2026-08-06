# Rollout Phase 10: Legacy Removal

## Prerequisite Phases
- Phase 1-9: ALL must pass
- Phase 9 observation window completed with zero rollback requests

## Description
After the fallback window completes with zero rollback requests, legacy code
paths are deleted:
- Full-copy generations
- Heap-mirror reads
- Error-at-cap behavior
- Count-only batching
- Per-harness inline server (replaced by daemon + shim)
- FP16-only model path

The clean tree passes full validation with zero dead code or clippy warnings.

## Validation Commands
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Success Criteria
- VAL-ROLLOUT-014: legacy paths removed, full validation passes
- Zero dead code or clippy warnings
- Ship set: code + README.md + CHANGELOG.md + BENCHMARKS.md only

## Rollback Procedure
This phase is terminal. The previous commit can be reverted if needed:
```bash
git revert HEAD
```

## Evidence
- VAL-ROLLOUT-014: legacy removal validation
- cargo clippy: zero warnings after removal
- VAL-ROLLOUT-019: scaffolding removed
