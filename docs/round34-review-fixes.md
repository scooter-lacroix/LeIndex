# Round 34 Review Fixes — Implementation Plan

## New Findings Since Commit 2327768

Comments already fixed in rounds 32-33 (stale re-reviews): 3104452827, 3104452955,
3105301261, 3105301262, 3105308211, 3105854227, 3105854230, 3105854231.

Three genuinely new findings:

---

### FIX-1: ensure_pdg_loaded uses DB existence as guard (new project false positive) [Major] ✅ VALIDATED
**Files**: `src/cli/leindex.rs`
**Reporter**: coderabbitai major

`LeIndex::new()` calls `open_storage_with_retry()` which runs
`initialize_schema()` → creates `leindex.db` eagerly. So on a brand-new
project that has NEVER been indexed, `storage_path.join("leindex.db").exists()`
returns true, and `ensure_pdg_loaded()` calls `load_from_storage()` which
loads an empty PDG and rebuilds an empty search index — wasted work.

**Fix**: Check for actual indexed content instead of DB file existence:
```rust
pub fn ensure_pdg_loaded(&mut self) -> Result<()> {
    if self.pdg.is_none() {
        let has_indexed_files = !crate::storage::pdg_store::get_indexed_files(
            &self.storage, &self.project_id
        ).unwrap_or_default().is_empty();
        if has_indexed_files {
            self.load_from_storage()?;
        }
    }
    Ok(())
}
```

But `get_indexed_files` queries storage every time. Better: cache the result
on first check, or simply check `search_engine.node_count() > 0` as a fast
path (already populated after load).

Actually the simplest: just check if load produces any nodes, and if not,
don't store the empty PDG. But that changes load semantics.

Best approach: gate on storage having actual indexed files:
```rust
if self.pdg.is_none() && self.has_indexed_content() {
    self.load_from_storage()?;
}
```

Where `has_indexed_content()` checks the DB for any indexed_file entries.

---

### FIX-2: Focus ranking generates embedding per-file (N embed calls) [Medium] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reporter**: gemini medium

`generate_query_embedding` is called once per file in the focus ranking loop.
For 200+ files, that's 200+ TF-IDF computations.

**Fix**: Skip embedding for files with empty symbol lists (score 0).
For non-empty: use a HashMap cache keyed by the symbol text to deduplicate
files with identical symbol sets.

---

### FIX-3: exclude_dirs only matches full path prefix, not leaf name [Medium] ✅ VALIDATED
**Files**: `src/graph/external_deps.rs`
**Reporter**: gemini medium

`exclude_dirs` patterns like `target` only match if the FULL relative path
starts with `target` (i.e., top-level only). `SKIP_DIRS` matches by leaf
directory name anywhere in the tree. This is inconsistent.

**Fix**: Add a leaf-name check matching SKIP_DIRS behavior:
```rust
|| file_name.as_ref() == trimmed  // match by leaf directory name
```

---

## Execution

| Step | Fix | Risk | Impact |
|------|-----|------|--------|
| 1 | ensure_pdg_loaded guard | Low | Eliminates wasted load on new projects |
| 2 | Focus ranking cache | Low | N→unique embed calls |
| 3 | exclude_dirs leaf match | Low | Matches user expectations |

Single commit.
