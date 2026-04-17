# LeIndex Refactoring Guide

> **Purpose**: Safe, incremental refactoring of `LeIndex` from a 3000-line, 88-method god struct
> into focused modules. Every step must compile clean and pass existing tests.
> 
> **Target audience**: AI coding agent executing refactoring steps.
> 
> **Constraint**: Zero regressions. Each step is independently verifiable via `cargo check`.

---

## 1. Current State Analysis

### 1.1 Structural Overview

```
src/cli/leindex.rs  — 3022 lines, 50 methods, 12 fields
src/cli/mcp/handlers.rs — 5345 lines, 20 handler structs
```

`LeIndex` is the central orchestrator. It owns:
- Project identity (`project_path`, `project_id`, `unique_id`, `storage_path`)
- Storage (`storage: Storage`)
- Search (`search_engine: SearchEngine`)
- Graph (`pdg: Option<ProgramDependenceGraph>`)
- Memory management (`cache_spiller: CacheSpiller`)
- Caches (`embedder`, `project_scan`, `file_stats_cache`)
- Stats (`stats: IndexStats`)

### 1.2 Method Classification by Mutability

**Read-only (`&self`) — 14 methods:**
```
project_path, storage_path, project_id, unique_id, display_name,
search_engine, pdg, get_stats, is_indexed, is_stale_fast,
file_stats, check_freshness, check_manifest_stale,
collect_source_files_with_hashes_readonly, generate_query_embedding
```

**Mutable (`&mut self`) — 24 methods:**
```
index_project, load_from_storage, analyze, close, coverage_report,
check_memory_and_spill, spill_pdg_cache, spill_vector_cache,
spill_all_caches, reload_pdg_from_cache, reload_vector_from_pdg,
source_file_paths, build_file_stats_cache, warm_caches,
get_project_scan, cache_project_scan, collect_source_file_paths
```

### 1.3 Field Coupling Matrix

| Field | # Methods Accessing It | Heaviest Users |
|-------|----------------------|----------------|
| `project_id` | 12 | storage, cache, diagnostics |
| `storage` | 7 | load/save/close/freshness |
| `pdg` | 9 | search, analysis, expand, spill/reload |
| `cache_spiller` | 9 | get/cache project_scan, spill/warm/stats |
| `search_engine` | 6 | search, diagnostics, spill/reload |
| `project_path` | 5 | scan, cache key, diagnostics |
| `stats` | 4 | index, diagnostics, load |
| `embedder` | 1 | generate_query_embedding |
| `file_stats_cache` | 1 | file_stats accessor |
| `project_scan` | 2 | get_project_scan, is_stale_fast (via self) |
| `unique_id` | 3 | diagnostics, display_name |

### 1.4 Key Coupling Points (DANGER ZONES)

These method pairs share mutable state and CANNOT be trivially split:

1. **`index_project` ↔ `load_from_storage`**: Both set `self.pdg`, `self.stats`, `self.project_scan` (via `get_project_scan`), `self.file_stats_cache` (via `build_file_stats_cache`). Indexing creates; loading restores. They never run concurrently on the same `LeIndex`.

2. **`cache_spiller` ↔ everything**: `get_project_scan` and `cache_project_scan` use `cache_spiller.store_mut()` (mutable) AND are called from read-only contexts like `is_stale_fast` (which uses `cache_spiller.store().peek()` — read-only). This is the primary source of `&self` vs `&mut self` tension.

3. **`search_engine.index_nodes()`**: Called from `index_nodes()` (private, during index/load). `search_engine` is read-only afterward. This means `search_engine` only needs `&mut self` during initialization.

4. **`build_file_stats_cache`**: Mutates `self.file_stats_cache`. Called from `index_project` and `load_from_storage`. Pure cache — could be lazy-initialized.

---

## 2. Target Architecture

### 2.1 Module Structure

```
src/cli/
├── leindex.rs          # Facade: LeIndex struct, delegates to modules
├── index_freshness.rs  # is_stale_fast, check_freshness, check_manifest_stale
├── index_builder.rs    # index_project, scan_project_files, merge_pdgs, index_nodes
├── index_cache.rs      # project_scan, file_stats_cache, cache_spiller wrappers
└── mcp/
    ├── mod.rs
    ├── handlers.rs     # all_tool_handlers() + shared helpers only
    ├── index.rs        # IndexHandler
    ├── search.rs       # SearchHandler
    ├── grep_symbols.rs # GrepSymbolsHandler
    ├── project_map.rs  # ProjectMapHandler
    ├── edit.rs         # EditPreviewHandler, EditApplyHandler
    ├── rename.rs       # RenameSymbolHandler
    ├── impact.rs       # ImpactAnalysisHandler
    ├── context.rs      # ContextHandler
    ├── deep_analyze.rs # DeepAnalyzeHandler
    ├── diagnostics.rs  # DiagnosticsHandler
    ├── file_summary.rs # FileSummaryHandler
    ├── symbol_lookup.rs# SymbolLookupHandler
    ├── read_symbol.rs  # ReadSymbolHandler
    ├── phase.rs        # PhaseAnalysisHandler, PhaseAnalysisAliasHandler
    ├── text_search.rs  # TextSearchHandler
    ├── read_file.rs    # ReadFileHandler
    ├── git_status.rs   # GitStatusHandler
    └── helpers.rs      # extract_string, resolve_scope, wrap_with_meta, etc.
```

### 2.2 LeIndex Facade (Target)

```rust
pub struct LeIndex {
    // Identity (never change after construction)
    project_path: PathBuf,
    storage_path: PathBuf,
    project_id: String,
    unique_id: UniqueProjectId,

    // Core data (set during index/load, read thereafter)
    storage: Storage,
    search_engine: SearchEngine,
    pdg: Option<ProgramDependenceGraph>,
    stats: IndexStats,
    embedder: Option<TfIdfEmbedder>,

    // Cache subsystem (extracted to IndexCache)
    cache: IndexCache,
}

impl LeIndex {
    // Identity accessors — stay here
    pub fn project_path(&self) -> &Path { &self.project_path }
    pub fn storage_path(&self) -> &Path { &self.storage_path }
    pub fn project_id(&self) -> &str { &self.project_id }
    pub fn unique_id(&self) -> &UniqueProjectId { &self.unique_id }
    pub fn display_name(&self) -> String { ... }
    
    // Core data accessors — stay here
    pub fn pdg(&self) -> Option<&ProgramDependenceGraph> { self.pdg.as_ref() }
    pub fn search_engine(&self) -> &SearchEngine { &self.search_engine }
    pub fn get_stats(&self) -> &IndexStats { &self.stats }
    pub fn is_indexed(&self) -> bool { ... }
    
    // Delegated methods — thin wrappers
    pub fn is_stale_fast(&self) -> bool { self.cache.is_stale_fast(&self.identity()) }
    pub fn index_project(&mut self, force: bool) -> Result<IndexStats> { ... }
    // etc.
}
```

### 2.3 New `IndexCache` Struct

Holds all caching logic: `cache_spiller`, `project_scan`, `file_stats_cache`.

```rust
// src/cli/index_cache.rs
pub struct IndexCache {
    cache_spiller: CacheSpiller,
    project_scan: Option<ProjectFileScan>,
    file_stats_cache: Option<HashMap<String, FileStats>>,
    // Staleness cache for is_stale_fast TTL
    stale_cache: Option<(std::time::Instant, bool)>,
}

impl IndexCache {
    pub fn is_stale_fast(&self, ctx: &FreshnessContext) -> bool { ... }
    pub fn get_project_scan(&mut self, ctx: &CacheContext) -> Result<ProjectFileScan> { ... }
    pub fn file_stats(&self) -> Option<&HashMap<String, FileStats>> { ... }
    pub fn build_file_stats_cache(&mut self, pdg: &ProgramDependenceGraph) { ... }
    // spill/reload/warm methods...
}
```

### 2.4 Context Structs (Avoid Borrow Conflicts)

The current design passes `&self` / `&mut self` everywhere, which causes borrow conflicts when a method needs both `self.pdg` and `self.cache_spiller`. The fix:

```rust
/// Read-only context passed to submodules
pub struct FreshnessContext<'a> {
    pub project_path: &'a Path,
    pub storage_path: &'a Path,
    pub project_id: &'a str,
    pub storage: &'a Storage,
}

/// Read-write context for indexing operations
pub struct IndexContext<'a> {
    pub project_path: &'a Path,
    pub project_id: &'a str,
    pub storage: &'a mut Storage,
    pub search_engine: &'a mut SearchEngine,
}

impl LeIndex {
    fn identity(&self) -> FreshnessContext<'_> {
        FreshnessContext {
            project_path: &self.project_path,
            storage_path: &self.storage_path,
            project_id: &self.project_id,
            storage: &self.storage,
        }
    }
}
```

---

## 3. Execution Plan: Step-by-Step Refactoring

### Step 0: Preparation (Non-Breaking)

**Goal**: Set up the target file structure without moving any code.

```bash
# Create empty module files
touch src/cli/index_freshness.rs
touch src/cli/index_builder.rs
touch src/cli/index_cache.rs
```

Register modules in `src/cli/mod.rs`:
```rust
pub mod index_freshness;
pub mod index_builder;
pub mod index_cache;
```

**Verify**: `cargo check` — empty modules, no changes.

### Step 1: Extract `IndexFreshness` (LOW RISK)

**What moves**: `is_stale_fast()`, `check_freshness()`, `check_manifest_stale()`, and the helper `extract_unique_dirs()`.

**Why safe**: All three are `&self` methods. They access: `self.storage`, `self.storage_path`, `self.project_path`, `self.project_id`, `self.project_scan`, `self.cache_spiller`.

**How**:
1. Create `FreshnessContext` struct in `index_freshness.rs` with the 6 fields above as `&` references.
2. Move the 3 methods + `extract_unique_dirs` to `index_freshness.rs` as free functions taking `FreshnessContext` + direct field references.
3. In `LeIndex`, replace method bodies with delegation:
   ```rust
   pub fn is_stale_fast(&self) -> bool {
       index_freshness::is_stale_fast(&self.identity(), &self.cache.project_scan, ...)
   }
   ```
4. Keep the public API identical.

**Risk**: `is_stale_fast` accesses `self.cache_spiller` through `peek()` and `load_from_disk()`. These are read-only on `CacheSpiller` but require the `cache_spiller` reference. Pass it via context.

**Verify**: `cargo check`, then run `is_stale_fast` via MCP tool call.

### Step 2: Extract `IndexCache` (MEDIUM RISK)

**What moves**: `CacheSpiller`, `project_scan`, `file_stats_cache` fields, plus:
- `get_project_scan()`, `cache_project_scan()`
- `build_file_stats_cache()`
- `check_memory_and_spill()`, `spill_pdg_cache()`, `spill_vector_cache()`, `spill_all_caches()`
- `reload_pdg_from_cache()`, `reload_vector_from_pdg()`
- `warm_caches()`, `get_cache_stats()`

**Why medium risk**: `get_project_scan` uses `&mut self` on `cache_spiller`, but `is_stale_fast` only needs `&self`. Moving `cache_spiller` into a sub-struct resolves this — `IndexCache` owns the spiller, `LeIndex` owns `IndexCache`.

**How**:
1. Create `IndexCache` struct in `index_cache.rs` with the 3 fields.
2. Move the 11 methods to `impl IndexCache`.
3. In `LeIndex`, replace:
   ```rust
   cache_spiller: CacheSpiller,
   project_scan: Option<ProjectFileScan>,
   file_stats_cache: Option<HashMap<String, FileStats>>,
   ```
   with:
   ```rust
   cache: IndexCache,
   ```
4. Update all `self.cache_spiller` → `self.cache.cache_spiller` (mechanical find-replace).
5. Update all `self.project_scan` → `self.cache.project_scan`.
6. Update all `self.file_stats_cache` → `self.cache.file_stats_cache`.

**Borrow conflict resolution**: After extraction, `LeIndex::is_stale_fast(&self)` can call `self.cache.is_stale_fast_readonly()` (takes `&self`) while `LeIndex::get_project_scan(&mut self)` calls `self.cache.get_project_scan(&mut self.cache.cache_spiller)`. No conflict because they target different fields of `self.cache`.

**Verify**: `cargo check`. Run full MCP workflow: index → search → grep_symbols.

### Step 3: Extract `IndexBuilder` (MEDIUM RISK)

**What moves**: `index_project()`, `index_nodes()`, `merge_pdgs()`, `remove_file_from_pdg()`, `save_to_storage()`, `collect_source_files_with_hashes()` (and readonly variant), `collect_source_file_paths()`, `scan_project_files()`, `hash_file()`.

**Why medium risk**: `index_project` is the most complex method (~200 lines). It accesses `self.pdg`, `self.storage`, `self.search_engine`, `self.stats`, `self.cache.project_scan`, `self.cache.cache_spiller`. After Step 2, most of these are accessible through clean references.

**How**:
1. Create `IndexContext` struct that borrows the needed fields:
   ```rust
   pub struct IndexContext<'a> {
       pub project_path: &'a Path,
       pub project_id: &'a str,
       pub storage: &'a mut Storage,
       pub search_engine: &'a mut SearchEngine,
       pub cache: &'a mut IndexCache,
   }
   ```
2. Move methods to `index_builder.rs` as functions on `IndexContext`.
3. `LeIndex::index_project` becomes:
   ```rust
   pub fn index_project(&mut self, force: bool) -> Result<IndexStats> {
       let mut ctx = IndexContext { ... };
       index_builder::index_project(&mut ctx, force, &mut self.pdg, &mut self.stats, &self.embedder)
   }
   ```

**Tricky part**: `index_project` sets `self.pdg = Some(pdg)` at the end. The PDG must be returned, not set internally. Use a return value pattern.

**Verify**: `cargo check`. Run full index → search cycle.

### Step 4: Split `handlers.rs` (LOW RISK, HIGH EFFORT)

**What moves**: Each handler struct + its `schema()` and `execute()` methods into its own file.

**Why safe**: Handlers are independent structs with no shared mutable state. They only receive `&Arc<ProjectRegistry>` and `args: Value`. Splitting is pure file reorganization.

**How**:
1. Create `src/cli/mcp/helpers.rs` with all shared functions:
   - `extract_string`, `extract_usize`, `extract_bool`
   - `validate_file_within_project`, `resolve_scope`
   - `wrap_with_meta`, `read_source_snippet`, `byte_range_to_line_range`
   - `get_direct_callers`, `node_type_str`
   - `parse_edit_changes`, `apply_changes_in_memory`, `replace_whole_word`
   - `normalise_ws`, `find_normalised_whitespace`, `make_diff`, `glob_match`
   - `phase_analysis_schema`

2. For each handler (in dependency order):
   a. Create `src/cli/mcp/{name}.rs`
   b. Move the struct + `impl` block
   c. Add `use super::helpers::*;`
   d. Register in `src/cli/mcp/mod.rs`: `mod {name}; pub use {name}::*;`

3. Keep `src/cli/mcp/handlers.rs` with:
   - `all_tool_handlers()` function
   - `ToolHandler` enum (all variants)
   - The enum's `name()` and `execute()` dispatch methods

**Handler dependencies** (for import resolution):
- `EditApplyHandler` depends on `parse_edit_changes`, `apply_changes_in_memory`, `make_diff`
- `RenameSymbolHandler` depends on `get_direct_callers`, `replace_whole_word`, `find_normalised_whitespace`
- `ImpactAnalysisHandler` depends on `get_direct_callers`
- `GrepSymbolsHandler` depends on `get_direct_callers`, `node_type_str`

**Suggested order** (fewest dependencies first):
1. `helpers.rs` — shared functions
2. `index.rs` — IndexHandler (no PDG dependency)
3. `git_status.rs` — GitStatusHandler (standalone)
4. `read_file.rs` — ReadFileHandler (standalone)
5. `text_search.rs` — TextSearchHandler (standalone)
6. `search.rs` — SearchHandler
7. `diagnostics.rs` — DiagnosticsHandler
8. `file_summary.rs` — FileSummaryHandler
9. `symbol_lookup.rs` — SymbolLookupHandler
10. `read_symbol.rs` — ReadSymbolHandler
11. `context.rs` — ContextHandler
12. `deep_analyze.rs` — DeepAnalyzeHandler
13. `phase.rs` — PhaseAnalysisHandler + AliasHandler
14. `project_map.rs` — ProjectMapHandler
15. `grep_symbols.rs` — GrepSymbolsHandler
16. `impact.rs` — ImpactAnalysisHandler
17. `edit.rs` — EditPreviewHandler + EditApplyHandler
18. `rename.rs` — RenameSymbolHandler

**Verify**: After EACH handler move: `cargo check`. If it passes, commit before moving the next.

### Step 5: Clean Up (LOW RISK)

After all extractions:
1. Remove dead `use` statements from `leindex.rs`
2. Add `#[inline]` to trivial delegation methods
3. Verify line count: target <500 lines for `leindex.rs` facade
4. Run `cargo clippy` on all new modules

---

## 4. Safety Rules (MANDATORY)

### Rule 1: One Step Per Commit

Each step (1-5) gets its own commit. Sub-steps within Step 4 (handler splits) each get a commit. If any step fails `cargo check`, revert and fix before proceeding.

### Rule 2: Public API Preservation

The public API of `LeIndex` MUST NOT change during refactoring:
- Same method names, same signatures, same return types
- Same field accessors (project_path, pdg, search_engine, etc.)
- `ToolHandler` enum variants remain the same

### Rule 3: No Behavioral Changes

This is pure structural refactoring. No new features, no bug fixes, no algorithm changes. If you find a bug during refactoring, note it in a TODO comment but don't fix it.

### Rule 4: Compile After Every File Change

```bash
cargo check  # after creating empty modules
cargo check  # after moving first method
cargo check  # after updating delegation
cargo check  # after each handler split
```

### Rule 5: Test At Natural Checkpoints

- After Step 1: Call `leindex_grep_symbols` via MCP
- After Step 2: Call `leindex_index` then `leindex_search`
- After Step 3: Full index cycle
- After Step 4 (every 5 handlers): MCP server smoke test
- After Step 5: Full `cargo clippy`

---

## 5. Estimated Complexity

| Step | Lines Moved | Risk | Time |
|------|------------|------|------|
| Step 0: Preparation | 0 | None | 5 min |
| Step 1: IndexFreshness | ~250 | Low | 30 min |
| Step 2: IndexCache | ~300 | Medium | 45 min |
| Step 3: IndexBuilder | ~400 | Medium | 60 min |
| Step 4: handlers.rs split | ~5300 | Low/High effort | 90 min |
| Step 5: Cleanup | ~50 | Low | 15 min |
| **Total** | **~6300** | | **~4 hours** |

---

## 6. What NOT To Do

1. **Don't change `ProgramDependenceGraph`** — it's a separate crate with its own API.
2. **Don't change `Storage`** — the SQLite layer is already well-modularized.
3. **Don't change `SearchEngine`** — it's already a separate module.
4. **Don't merge `LeIndex::new()` into construction** — the complex init logic (path resolution, storage open, cache restore) is fine where it is.
5. **Don't change `ProjectRegistry`** — the registry/locking/concurrency model is orthogonal.
6. **Don't "fix" the `&self` vs `&mut self` split on `cache_spiller`** by making everything `&mut self` — that would make the staleness check more expensive and break the read-only query path.

---

## 7. Success Criteria

- [ ] `leindex.rs` < 500 lines (facade + construction)
- [ ] `index_freshness.rs` contains all staleness logic
- [ ] `index_cache.rs` contains CacheSpiller, project_scan, file_stats_cache
- [ ] `index_builder.rs` contains indexing pipeline
- [ ] Each MCP handler in its own file
- [ ] `handlers.rs` only contains `ToolHandler` enum + `all_tool_handlers()`
- [ ] `cargo check` passes at every intermediate commit
- [ ] No public API changes visible to `registry.rs` or external callers
- [ ] `cargo clippy` clean on all new/modified files
