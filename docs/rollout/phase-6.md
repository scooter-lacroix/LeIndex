# Rollout Phase 6: Global Embed Cache

## Prerequisite Phases
- Phase 1-5

## Flags Enabled
- `LEINDEX_FEATURE_GLOBAL_EMBED_CACHE=1`
- All previous phase flags

## Description
Enable the global content-addressed embedding cache at `~/.leindex/cache/embeddings/`.
Cross-project dedup via 6-tuple CacheKey. Token-aware bounded batching replaces
count-only batching. Embed worker shares one model runtime profile.

## Validation Commands
```bash
cargo test -p leindex --features full -- cache embed
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Success Criteria
- VAL-CACHE-001..015: all cache tests pass
- CacheKey 6-tuple produces unique keys
- Cross-project dedup verified
- No source text stored after hashing (privacy gate)

## Rollback Procedure
```bash
export LEINDEX_FEATURE_GLOBAL_EMBED_CACHE=0
# Falls back to per-project embedding computation (no shared cache)
```

## Evidence
- VAL-CACHE-005: cross-project deduplication
- VAL-CACHE-012: two-worktree cache hit ratio
- VAL-CACHE-015: no source text stored after hashing
