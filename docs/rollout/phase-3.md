# Rollout Phase 3: Daemon + Shim

## Prerequisite Phases
- Phase 1 (Generation Readers)
- Phase 2 (CAS Generation Store)

## Flags Enabled
- `LEINDEX_FEATURE_DAEMON_CLIENT=1`
- `LEINDEX_FEATURE_GENERATION_READERS=1`
- `LEINDEX_FEATURE_GENERATION_MIGRATION=1`

## Description
Enable the user-scoped `leindexd` daemon and stdio shim path. MCP tool calls
from agent harnesses route through tiny stdio shims (5-15 MiB RSS) to a single
shared daemon process, eliminating per-harness process multiplication.

## Validation Commands
```bash
cargo build --release --features daemon-client
cargo test --test leindexd_smoke_test --release
cargo test --test leindexd_single_winner_test --release
cargo test --test daemon_tool_parity_test --release
cargo test --features daemon-client --test shim_forward_test --release
```

## Success Criteria
- VAL-DAEMON-001..010: all daemon tests pass
- VAL-SHIM-001..005: all shim tests pass
- Daemon serves same tool set as inline server (VAL-DAEMON-005)
- Shim RSS 5-15 MiB (VAL-SHIM-003)
- Single-winner startup lock (VAL-DAEMON-006)

## Rollback Procedure
```bash
export LEINDEX_FEATURE_DAEMON_CLIENT=0
# Falls back to legacy inline `leindex mcp --stdio` server
# Daemon process exits on idle timeout
```

## Evidence
- VAL-CROSS-001: no-stall read during write under daemon
- VAL-CROSS-005: aggregate RSS ≤ 1 GiB with daemon + 3 shims
