# Round 33 Review Fixes — Implementation Plan

## New Findings Since Commit a00f5b5

Comments 3104452827 and 3104452955 are **already addressed** in round 32.
Six genuinely new findings remain.

---

### FIX-1: Lazy load causes unnecessary full reindex on every project reload [P2/HIGH IMPACT] ✅ VALIDATED
**Files**: `src/cli/registry.rs`
**Reporter**: codex P2

After TASK-06 removed `load_from_storage()` from `create_and_insert()`, every
project reload has `search_engine.node_count() == 0`, so `is_indexed()` returns
false, and `get_or_create()` triggers `index_handle()` which does a full
reindex. This makes lazy loading actually WORSE — every project gets reindexed
on every process restart.

**Fix**: In `create_and_insert()`, restore `load_from_storage()` but catch
errors gracefully. The "lazy" benefit still applies to the `ensure_pdg_loaded()`
path for handlers that explicitly need PDG — they skip double-loading since
PDG is already in memory after the initial load.

Actually, simpler fix: keep lazy loading but fix `is_indexed()` to also check
the storage DB. If `leindex.db` exists and has indexed_files, the project IS
indexed even if PDG isn't in memory.

**Best fix**: Restore the load in `create_and_insert()` since that's the warm
path for project registration. The lazy `ensure_pdg_loaded()` then becomes a
no-op (PDG already loaded). Lazy loading only benefits handlers called between
`create_and_insert()` and the first PDG access, which is zero time in practice.

```rust
// In create_and_insert(), restore:
let _ = leindex.load_from_storage();
```

---

### FIX-2: get_project_scan walks filesystem before checking persistent cache [P2] ✅ VALIDATED
**Files**: `src/cli/leindex.rs`
**Reporter**: codex P2

`get_project_scan(false)` calls `scan_project_files()` (full walkdir) at line
1031 BEFORE the persistent cache check at line 1035. Cold-start calls pay
O(repo size) even when a serialized scan exists on disk.

**Fix**: Try persistent cache first, only scan on miss:
```rust
fn get_project_scan(&mut self, refresh: bool) -> Result<ProjectFileScan> {
    if !refresh {
        let project_id = self.project_id.clone();
        if let result @ Ok(_) = self.cache.get_project_scan(&project_id, false, || Err(anyhow!("miss"))) {
            return result;
        }
    }
    let scan = self.scan_project_files()?;
    let project_id = self.project_id.clone();
    self.cache.cache_project_scan(&project_id, &scan);
    self.cache.project_scan = Some(scan.clone());
    Ok(scan)
}
```

---

### FIX-3: ProjectMapHandler missing ensure_pdg_loaded [High] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reporter**: gemini high

With lazy loading, `index.pdg()` returns None for indexed-but-unloaded
projects. ProjectMapHandler doesn't call `ensure_pdg_loaded()`.

**Fix**: Add `ensure_pdg_loaded()` after lock acquisition (line ~1852):
```rust
let mut index = handle.lock().await;
index.ensure_pdg_loaded().map_err(|e| JsonRpcError::indexing_failed(...))?;
```

Note: If FIX-1 restores load_from_storage, this becomes a no-op. Still needed
for defense-in-depth.

---

### FIX-4: Semantic scope filter uses hardcoded `/` [Medium] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reporter**: gemini medium

`format!("{}/", scope_base)` hardcodes `/`. On Windows, `MAIN_SEPARATOR` is `\`.

**Fix**: Use `format!("{}{}", scope_base, std::path::MAIN_SEPARATOR)`.

---

### FIX-5: index_cache byte calculation inaccurate [Medium] ✅ VALIDATED (LOW PRIORITY)
**Files**: `src/cli/index_cache.rs`
**Reporter**: gemini medium

`pdg_bytes` includes all store entries, not just PDG. `vector_bytes = total - pdg_bytes`
is wrong when other entries exist. Only affects log reporting.

**Fix**: Record `total_bytes()` before each spill to measure delta:
```rust
let before_spill = self.cache_spiller.store().total_bytes();
self.spill_pdg_cache(project_id, pdg)?;
pdg_bytes = self.cache_spiller.store().total_bytes() - before_spill;
```

---

### FIX-6: Docs task count mismatch [Minor] ✅ VALIDATED
**Files**: `docs/IMPROVEMENT_IMPLEMENTATION_PLAN.md`
**Reporter**: coderabbit minor

Scope says "9 tasks (#1-#4 and #6-#10)" but only 8 exist.

**Fix**: Change to "8 tasks" and "recommendations #1-#4 and #6-#9".

---

## Execution Order

| Step | Fix | Risk | Impact |
|------|-----|------|--------|
| 1 | Restore load_from_storage | Low | Fixes critical perf regression |
| 2 | ProjectMapHandler ensure_pdg_loaded | Low | Defense-in-depth |
| 3 | get_project_scan cache-first | Low | Avoids unnecessary walkdir |
| 4 | Semantic scope MAIN_SEPARATOR | Low | Windows compat |
| 5 | index_cache byte delta | Low | Accurate reporting |
| 6 | Docs task count | None | Accuracy |

Single commit.
