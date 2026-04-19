# Round 35 Review Fixes — Implementation Plan

## New Findings Since Commit 12d7016

9 comments are stale re-reviews already addressed in rounds 33-34:
3105301261, 3105301262, 3105308211, 3105854227, 3105854230, 3105854231,
3105898408, 3106020081, 3106020084.

Two genuinely new findings:

---

### FIX-1: Scoped semantic search false-zero [Major] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reporter**: coderabbitai major

`index.search(&query, top_k + offset, ...)` fetches at most `top_k + offset`
results. The scope filter then removes out-of-scope matches. If ALL fetched
results are outside the scope, `total_filtered == 0` triggers the
"No semantic matches found" response — even though lower-ranked in-scope
results exist beyond the fetch window.

Example: `top_k=20, scope="src/api/"` where the top 20 results are all in
`src/core/`. In-scope matches at rank 21+ are invisible.

**Fix**: When scope is set and `total_filtered == 0`, retry with expanded
top_k (e.g., 10x) before reporting zero matches. This is a bounded retry —
if the expanded search still yields 0 scoped results, the zero response is
genuine.

```rust
let mut fetch_k = top_k + offset;
let mut all_results = index.search(&query, fetch_k, query_type)...;
let filtered: Vec<_> = all_results.iter().filter(|scope|...).collect();

// Retry with expanded window if scope eliminated everything
if filtered.is_empty() && scope.is_some() && !all_results.is_empty() {
    fetch_k = (fetch_k * 10).min(1000);
    all_results = index.search(&query, fetch_k, query_type)...;
    // re-filter...
}
```

---

### FIX-2: Rename silently skips unreadable files [Major] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reporter**: coderabbitai major

The rename preview closure uses `let Ok(original) = read_to_string(...) else { continue; }`.
Unreadable files silently disappear from `files_to_modify`. When
`preview_only=false`, the partial rename is applied and the response says
`"applied": true` — but files the user expected to be renamed were skipped.

**Fix**: Return `Result` from the closure. If a file can't be read, propagate
the error instead of continuing:

```rust
move || -> Result<(Vec<Value>, Vec<String>), String> {
    ...
    let original = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed reading '{}': {}", file_path, e))?;
    ...
    Ok((diffs, files_to_modify))
}
```

---

## Execution

| Step | Fix | Risk | Impact |
|------|-----|------|--------|
| 1 | Scoped retry expansion | Low | Eliminates false-zero |
| 2 | Rename read error propagation | Low | No silent partial renames |

Single commit.
