# Rollout Phase 9: Legacy Fallback Window

## Prerequisite Phases
- Phase 1-8: ALL must pass, including default-on flip

## Flags Enabled
All flags remain default ON. Additionally, `LEINDEX_LEGACY=1` opt-in is available.

## Description
During the fallback window, all legacy code paths remain reachable via the
`LEINDEX_LEGACY=1` environment variable. Users can opt back to legacy behavior
without downgrading. This window provides a safe observation period.

## Validation Commands
```bash
# Verify legacy fallback works
LEINDEX_LEGACY=1 cargo test -p leindex --features full
```

## Success Criteria
- VAL-ROLLOUT-013: legacy paths reachable with LEINDEX_LEGACY=1
- Zero rollback requests during observation window
- All default-on users continue functioning correctly

## Rollback Procedure
```bash
export LEINDEX_LEGACY=1
# OR disable individual flags:
export LEINDEX_FEATURE_DAEMON_CLIENT=0
# etc.
```

## Evidence
- VAL-ROLLOUT-013: legacy paths reachable during fallback window
