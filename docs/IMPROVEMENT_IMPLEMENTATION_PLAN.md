# LeIndex Improvements — Blocking Implementation Plan

> **Scope**: 9 tasks (recommendations #1-#4 and #6-#10 from the assessment).
> Recommendation #5 (LeIndex modularization) is covered separately in
> `docs/LEINDEX_REFACTORING_GUIDE.md` and is NOT part of this plan.
>
> **Branch**: `feature/unified-crate`
> **Verification**: `cargo check` after every task. No `cargo test` (LLVM linker segfault).
>
> **Commit convention**: One commit per task, message format:
> `feat(improvements): <short description>`

---

## Dependency Graph

```
TASK-01 (callers/callees names)
  └── TASK-02 (semantic search mode)
        └── TASK-03 (project_map focus param)

TASK-04 (external node migration)

TASK-06 (lazy PDG loading)
TASK-07 (is_stale_fast TTL)
TASK-08 (storage versioning)
TASK-09 (skip dir consolidation)
```

Four independent chains. TASK-01→02→03 is sequential. All others are independent
and can be done in any order or in parallel.

**Recommended execution order**: 01 → 04 → 09 → 08 → 02 → 03 → 06 → 07
(Simplest/most impactful first, building up to ones that touch more code.)

---

## TASK-01: Add callers/callees names to grep_symbols output

**Risk**: Low | **LOC change**: ~20 lines added | **Files**: 1

### Exact code locations

Two identical blocks in `src/cli/mcp/handlers.rs`:

**Location A** — semantic pre-filter results (line 2182-2193):
```rust
let caller_count = get_direct_callers(pdg, nid).len();
let dep_count = pdg.neighbors(nid).len();

let mut entry = serde_json::json!({
    "name": node.name,
    ...
    "caller_count": caller_count,
    "dependency_count": dep_count,
    ...
});
```

**Location B** — direct PDG scan results (line 2278-2289):
Same pattern, identical code.

### Implementation steps

1. **Location A** (line 2182): After `let dep_count = ...`, add:
   ```rust
   let callers: Vec<String> = get_direct_callers(pdg, nid)
       .iter()
       .take(50)
       .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
       .collect();
   let callees: Vec<String> = pdg.neighbors(nid)
       .iter()
       .take(50)
       .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
       .collect();
   ```
2. Add to the `json!({})` macro:
   ```rust
   "callers": callers,
   "callees": callees,
   ```
3. **Location B** (line 2278): Same two changes.

### Edge cases
- External nodes in callers/callees: names like `"serde"`, `"std"` — include them, the LLM
  needs to see external deps to understand coupling.
- Cap at 50 prevents response bloat for hub functions (e.g., `main()` might have 200 callers).
- Empty arrays (no callers/callees) are fine — JSON `[]` is informative.

### Verification
```bash
cargo check
```

---

## TASK-02: Add semantic search mode to grep_symbols

**Risk**: Medium | **LOC change**: ~80 lines | **Files**: 2-3
**Depends on**: TASK-01 (same handler)

### Key discovery: Semantic search ALREADY EXISTS internally

`LeIndex::search()` (line 1294) already sets `semantic: true` and `query_embedding`:
```rust
let search_query = SearchQuery {
    query: query.to_string(),
    top_k,
    semantic: true,
    query_embedding: Some(self.generate_query_embedding(query)),
    threshold: Some(0.1),
    ..
};
```

And `GrepSymbolsHandler::execute()` already calls `index.search(&pattern, ...)` for candidate
pre-filtering (line 2114). So the infrastructure is all there.

### What's missing

The `mode` parameter to let the LLM choose between:
- `"exact"` (default): current behavior — text matching + semantic pre-filter
- `"semantic"`: pure semantic ranking — cosine similarity only, no text match required

### Implementation steps

1. **Schema** (`GrepSymbolsHandler::argument_schema()`, line ~2100):
   Add parameter:
   ```json
   "mode": {
       "type": "string",
       "enum": ["exact", "semantic"],
       "description": "Search mode: 'exact' for name matching (default), 'semantic' for concept-based search",
       "default": "exact"
   }
   ```

2. **Execute** (`GrepSymbolsHandler::execute()`, line 2089):
   - Parse `mode`: `let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("exact").to_owned();`
   - When `mode == "semantic"`:
     - Call `index.search(&pattern, candidate_limit, None)` as before (already does semantic)
     - Skip the text-matching PDG scan entirely (the second block at ~2240+)
     - Add `score` field from `SearchResult.score` to output
   - When `mode == "exact"`: current behavior unchanged

3. **Output**: When semantic mode, add to each entry:
   ```rust
   "score": result.score,
   ```

### Blocking issue
None. `SearchEngine::search()` returns `Vec<SearchResult>` which already has a `score` field.
The vector_index HNSW search is already populated during `index_nodes()`.

### Verification
```bash
cargo check
```

---

## TASK-03: Add focus parameter to project_map for semantic file ranking

**Risk**: Medium | **LOC change**: ~60 lines | **Files**: 1
**Depends on**: TASK-02 (uses same embedding infrastructure)

### Key discovery

`generate_query_embedding()` is `pub fn` on `LeIndex` (line 2219). It's accessible from
handlers via `index.generate_query_embedding("some text")`. The `file_stats_cache` already
has `symbol_names: Vec<String>` per file — we can embed those.

### Implementation steps

1. **Schema** (`ProjectMapHandler::argument_schema()`, line ~1800):
   Add parameter:
   ```json
   "focus": {
       "type": "string",
       "description": "Semantic focus area — ranks files by relevance to this topic (e.g., 'authentication', 'database layer')"
   }
   ```

2. **Execute** (`ProjectMapHandler::execute()`, line ~1828):
   - Parse: `let focus = args.get("focus").and_then(|v| v.as_str()).map(String::from);`
   - After building `file_map` (line ~1875), if `focus` is Some:
     a. Generate focus embedding: `let focus_emb = index.generate_query_embedding(&focus);`
     b. For each file in `file_map`, compute relevance:
        ```rust
        // Embed concatenated symbol names for the file
        let file_text = syms.join(" ");
        let file_emb = index.generate_query_embedding(&file_text);
        let score = cosine_similarity(&focus_emb, &file_emb);
        ```
     c. Store score in entry: `entry["relevance_score"] = serde_json::json!(score);`
     d. Sort by `relevance_score` descending (overrides `sort_by`)
   - If `focus` is None: existing behavior unchanged.

3. **cosine_similarity**: Import from `src/search/vector.rs` — function is private.
   Either:
   - Make it `pub fn cosine_similarity()` (simplest, 1-word change)
   - Or duplicate the ~15-line function locally
   Recommend making it `pub`.

### Blocking issue
`cosine_similarity` in `src/search/vector.rs:230` is private (`fn`, not `pub fn`).
Must add `pub`.

### Verification
```bash
cargo check
```

---

## TASK-04: Migrate external nodes on PDG load — eliminate dual-check bug class

**Risk**: Low-Medium | **LOC change**: ~30 lines added, ~8 lines removed | **Files**: 5
**Depends on**: Nothing

### Key discovery

`ProgramDependenceGraph::get_node_mut()` EXISTS (line 757):
```rust
pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
    self.graph.node_weight_mut(id)
}
```

But we need to iterate ALL nodes, not access by specific ID. petgraph provides
`graph.node_weights_mut()` which returns `&mut [Node]` — perfect for bulk mutation.

### Implementation steps

**Phase A — Add migration function** (in `src/cli/leindex.rs`):

```rust
/// Normalize external nodes: ensure any node with `language == "external"`
/// also has `NodeType::External`. Eliminates the dual-check bug class
/// caused by legacy PDG data that set language without the enum variant.
fn normalize_external_nodes(pdg: &mut ProgramDependenceGraph) {
    use crate::graph::pdg::NodeType;
    let mut migrated = 0usize;
    for node in pdg.node_weights_mut() {
        if node.language == "external" && node.node_type != NodeType::External {
            node.node_type = NodeType::External;
            migrated += 1;
        }
    }
    if migrated > 0 {
        info!("Normalized {} external nodes to NodeType::External", migrated);
    }
}
```

Check if `node_weights_mut()` exists on PDG. If not, use:
```rust
for node_idx in pdg.node_indices() {
    if let Some(node) = pdg.get_node_mut(node_idx) { ... }
}
```

**Phase B — Call at both PDG insertion points**:

1. `load_from_storage()` (line 1586): After `let pdg = pdg_store::load_pdg(...)` and before `self.index_nodes(&pdg)`:
   ```rust
   Self::normalize_external_nodes(&mut pdg);  // needs pdg to be `let mut`
   ```

2. `index_project()` (line 1036): Before `self.pdg = Some(pdg)`:
   ```rust
   Self::normalize_external_nodes(&mut pdg);
   ```

**Phase C — Remove dual-check guards** (separate commit, after Phase B verified):

Remove `language != "external"` from these consumer sites:
- `src/edit/mod.rs:1357` — `&& node.language != "external" // Legacy compat`
- `src/edit/mod.rs:1367` — `&& imp_node.language != "external"`
- `src/edit/mod.rs:1382` — `&& back_node.language != "external"`
- `src/phase/phase2.rs:42` — `|| target.language == "external";` (remove the OR clause)

Keep the `NodeType::External` checks — they're the canonical mechanism now.

### Blocking issue
Must verify `node_weights_mut()` or equivalent exists on `ProgramDependenceGraph`.
If not, add it as a pass-through to petgraph's `graph.node_weights_mut()`.

### Verification
```bash
cargo check  # after Phase A + B
# Then after Phase C:
cargo check
```

---

## TASK-06: Lazy PDG loading for read-only queries

**Risk**: Medium-High | **LOC change**: ~40 lines | **Files**: 2
**Depends on**: Nothing (but easier after handlers split if that happens first)

### Current flow (the problem)

```
get_or_load() → LeIndex::new() → leindex.load_from_storage()
                                       ↓
                                 Loads entire PDG into memory
                                 (10-50MB for large projects)
                                 Rebuilds search index
                                 Builds file stats cache
```

This happens even for `leindex_read_file` which just reads a file from disk.

### Proposed flow

```
get_or_load() → LeIndex::new() → (no load)
                                       ↓
                                 First pdg() call triggers load_from_storage()
```

### Implementation steps

1. **In `registry.rs::create_and_insert()`** (line 229):
   Change: `let _ = leindex.load_from_storage();`
   To: Remove the call entirely. The index starts "cold" — `is_indexed()` returns false
   based on `search_engine.is_empty()`, not PDG presence.

   **BUT**: The corruption detection at line 234 calls `detect_corruption()` which checks
   the DB file exists. And `index_handle()` at line 300 calls `is_stale_fast()` which
   checks indexed_files in storage. These work without PDG in memory.

2. **In `LeIndex`**, add lazy loading to `pdg()`:
   Problem: `pdg()` is `&self`. Can't call `load_from_storage(&mut self)` from it.

   Solution: Add a new method used by handlers:
   ```rust
   pub fn ensure_pdg_loaded(&mut self) -> Result<()> {
       if self.pdg.is_none() && !self.storage_path.join("leindex.db").exists() == false {
           // DB exists but PDG not loaded — lazy load
           self.load_from_storage()?;
       }
       Ok(())
   }
   ```

3. **In handlers**: Replace `index.pdg().ok_or_else(...)` with:
   ```rust
   index.ensure_pdg_loaded().map_err(|e| JsonRpcError::...)?;
   let pdg = index.pdg().ok_or_else(|| ...)?;
   ```
   This affects ~16 handler locations.

4. **For `get_or_create()` staleness check** (registry.rs:134):
   `is_stale_fast()` doesn't need PDG in memory — it uses `get_indexed_files()` from
   storage directly. No change needed.

### Blocking issue
The corruption detection path (line 234) calls `fresh.index_project(true)` which
creates a fresh index. This works without lazy loading. But the normal path at
line 229 `leindex.load_from_storage()` is unconditional — removing it means
the first handler call pays the load cost. This is the intended tradeoff.

### Verification
```bash
cargo check
```

---

## TASK-07: Cache is_stale_fast result with 2-second TTL

**Risk**: Low | **LOC change**: ~25 lines | **Files**: 1-2
**Depends on**: Nothing

### The problem

`is_stale_fast(&self)` is called from:
1. `registry.rs:134` — `get_or_create()` (every tool call)
2. `registry.rs:308` — `index_handle()` dedup check
3. `handlers.rs:1139` — `wrap_with_meta()` (adds `_warning` to every response)

At 50 calls/minute, each doing 10-20 directory stats + DB query.

### Implementation approach

Can't cache inside `is_stale_fast(&self)` — need mutation. Two options:

**Option A (recommended)**: Cache in `registry.rs` at the `get_or_create()` level.

```rust
// In ProjectRegistry, add:
stale_cache: RwLock<HashMap<PathBuf, (std::time::Instant, bool)>>,
```

In `get_or_create()`:
```rust
let stale = {
    let cache = self.stale_cache.read().await;
    if let Some((ts, result)) = cache.get(&canonical) {
        if ts.elapsed() < std::time::Duration::from_secs(2) {
            *result
        } else {
            drop(cache);
            let idx = handle.lock().await;
            let fresh = !not_indexed && idx.is_stale_fast();
            self.stale_cache.write().await.insert(canonical, (std::time::Instant::now(), fresh));
            fresh
        }
    } else {
        let idx = handle.lock().await;
        let fresh = !not_indexed && idx.is_stale_fast();
        self.stale_cache.write().await.insert(canonical, (std::time::Instant::now(), fresh));
        fresh
    }
};
```

**Option B**: Cache in `LeIndex` using interior mutability:
```rust
stale_cache: std::sync::RwLock<Option<(std::time::Instant, bool)>>,
```

Option A is cleaner because `ProjectRegistry` already has `RwLock` infrastructure.

### Implementation steps (Option A)

1. Add `stale_cache` field to `ProjectRegistry` struct in `registry.rs`.
2. Wrap the `is_stale_fast()` call in `get_or_create()` with TTL logic.
3. Invalidate cache entry after `index_handle()` completes (force fresh check).

### Verification
```bash
cargo check
```

---

## TASK-08: Version the PDG storage format

**Risk**: Low | **LOC change**: ~50 lines | **Files**: 2
**Depends on**: Nothing

### Current state

`schema.rs::initialize_schema()` uses PRAGMA checks (`PRAGMA table_info`) to add missing
columns one at a time. No version number. Every new feature adds an `ALTER TABLE IF NOT EXISTS`
block.

### Implementation steps

1. **Add `schema_version` table** in `initialize_schema()` (before other tables):
   ```sql
   CREATE TABLE IF NOT EXISTS schema_version (
       key TEXT PRIMARY KEY,
       version INTEGER NOT NULL
   );
   INSERT OR IGNORE INTO schema_version (key, version) VALUES ('schema', 1);
   ```
   Current schema = version 1. All existing DBs get version 1 auto-inserted.

2. **Add `run_migrations()` method** on `Storage`:
   ```rust
   fn run_migrations(&mut self) -> SqliteResult<()> {
       let current: u32 = self.conn.query_row(
           "SELECT COALESCE(MAX(version), 0) FROM schema_version WHERE key = 'schema'",
           [], |row| row.get(0)
       ).unwrap_or(0);

       if current < 1 {
           // Version 1: baseline — all existing tables. No-op.
           // Future versions add migrations here:
           // if current < 2 { self.migrate_v1_to_v2()?; }
           // if current < 3 { self.migrate_v2_to_v3()?; }
       }

       self.conn.execute(
           "INSERT OR REPLACE INTO schema_version (key, version) VALUES ('schema', ?)",
           [CURRENT_SCHEMA_VERSION],
       )?;
       Ok(())
   }
   ```

3. **Call in `open_with_config()`** after `initialize_schema()`:
   ```rust
   storage.initialize_schema()?;
   storage.run_migrations()?;  // NEW
   Ok(storage)
   ```

4. **Define constant**: `const CURRENT_SCHEMA_VERSION: u32 = 1;`

### Future migration example (for documentation)
```rust
// When adding source_directories to ProjectFileScan cache format:
// fn migrate_v1_to_v2(&mut self) -> SqliteResult<()> {
//     // Clear cached project scans that lack source_directories
//     self.conn.execute(
//         "DELETE FROM analysis_cache WHERE cache_key LIKE '%project_scan%'",
//         []
//     )?;
//     Ok(())
// }
```

### Verification
```bash
cargo check
```

---

## TASK-09: Consolidate skip directory lists into one shared constant

**Risk**: Low | **LOC change**: ~20 lines moved, ~3 lines removed | **Files**: 4
**Depends on**: Nothing

### Delta analysis

```
Union of all lists: 24 entries
leindex.rs (most comprehensive): 23 entries
Missing from leindex.rs: ".nuxt"  (only in handlers.rs)
```

### Implementation steps

1. **Create `src/cli/skip_dirs.rs`**:
   ```rust
   //! Shared directory exclusion list for source file scanning, text search,
   //! and dependency manifest discovery. One place to update.

   /// Directories to skip during all filesystem traversals.
   /// Must be sorted alphabetically if used with binary_search.
   pub const SKIP_DIRS: &[&str] = &[
       // Build outputs
       "build", "coverage", "dist", "out", "target",
       // IDE / editor
       ".idea", ".vscode",
       // Index data
       ".leindex",
       // Package managers / dependencies
       "bower_components", "node_modules", "vendor",
       // Python
       "__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox",
       // Python virtual environments
       ".venv", "env", "venv",
       // Web frameworks
       ".next", ".nuxt",
       // Version control
       ".git", ".hg", ".svn",
   ];
   ```

2. **Register in `src/cli/mod.rs`**: Add `pub mod skip_dirs;`

3. **Replace in `src/cli/leindex.rs`**:
   - Remove `ALWAYS_SKIP_DIRS` constant (lines 35-63)
   - Add `use crate::cli::skip_dirs::SKIP_DIRS;`
   - Replace `ALWAYS_SKIP_DIRS.contains(...)` with `SKIP_DIRS.contains(...)`

4. **Replace in `src/cli/mcp/handlers.rs`**:
   - Remove local `SKIP_DIRS` constant (lines 3827-3838)
   - Add `use crate::cli::skip_dirs::SKIP_DIRS;`
   - Update usage at line 3846

5. **Replace in `src/graph/external_deps.rs`**:
   - Remove local `SKIP_DIRS` constant (lines 1126-1143)
   - Add `use crate::cli::skip_dirs::SKIP_DIRS;`
   - Update usage at line 1162

### Verification
```bash
cargo check
# Then verify same file count:
# Build the binary, index the project itself, compare file count
```

---

## Summary: Blocking Execution Order

| Step | Task | Blockers | Risk | Estimated LOC |
|------|------|----------|------|---------------|
| 1 | TASK-01 (callers/callees) | None | Low | +20 |
| 2 | TASK-04 (external node migration) | None | Low-Med | +30, -8 |
| 3 | TASK-09 (skip dirs) | None | Low | +20, -60 |
| 4 | TASK-08 (storage versioning) | None | Low | +50 |
| 5 | TASK-02 (semantic search) | TASK-01 | Medium | +80 |
| 6 | TASK-03 (project_map focus) | TASK-02 | Medium | +60 |
| 7 | TASK-06 (lazy PDG loading) | None | Med-High | +40 |
| 8 | TASK-07 (is_stale_fast TTL) | None | Low | +25 |

**Total**: ~325 lines added, ~68 lines removed, across ~10 files.
