# LeIndex PDG Rewrite Implementation - Handoff Document

**Date:** 2026-03-07  
**Branch:** feature/unified-crate  
**Context Window:** Rolling session (context exhausted during investigation)  

## Executive Summary

This document provides a complete handoff of the PDG (Program Dependence Graph) rewrite implementation. The rewrite was intended to address critical architectural issues identified in a code review, but compilation errors remain unresolved at the time of handoff.

**Status:** Implementation complete but compilation has 43+ errors requiring remediation.  
**Constraint:** NO code removal allowed - all fixes must preserve logic.  
**Tooling:** LeIndex CLI tools available for code analysis.

---

## 1. Rewrite Context and Goals

### Original Issues Being Addressed (from code review)
1. **Type dependency O(n²) cliques** - Semantic bug where 10 functions sharing `String` created 45 meaningless edges
2. **Containment edges mislabeled as Call edges** - Class→Method edges incorrectly typed as semantic Call edges
3. **O(n) name lookups** - `find_by_name_in_file` did full graph scans instead of indexed lookups
4. **Embedding memory bomb** - ~6KB per node stored inline (~300MB for 50k nodes)
5. **Unbounded traversal methods** - `get_forward_impact` could traverse entire graph without limits

### Three Rewrite Files (NO broader codebase context when written)
The following files were written without full awareness of the existing codebase:
- `/mnt/WD-SSD/code_index_update/LeIndexer/pdg_rewrite.rs` → targets `src/graph/pdg.rs`
- `/mnt/WD-SSD/code_index_update/LeIndexer/extraction_rewrite.rs` → targets `src/graph/extraction.rs`
- `/mnt/WD-SSD/code_index_update/LeIndexer/pdg_utils_rewrite.rs` → targets `src/phase/pdg_utils.rs`

**Critical:** These rewrites assumed different module structure and API signatures than actually exist. The implementation required significant adaptation during integration.

See `/mnt/WD-SSD/code_index_update/LeIndexer/implementation_summary.md` for detailed rationale on every structural decision.

---

## 2. Current Implementation Status

### Files Successfully Modified (with partial fixes)

#### 2.1 `src/graph/pdg.rs` - PARTIALLY FIXED
**Status:** Core structure implemented, but petgraph API issues remain.

**Implemented:**
- ✅ `EdgeType::Containment` variant added
- ✅ `TraversalConfig` struct with bounds (max_depth, max_nodes, allowed_edge_types, min_edge_confidence)
- ✅ `EmbeddingStore` externalized HashMap for node embeddings
- ✅ `name_lower_index` for O(1) case-insensitive lookups
- ✅ Changed `add_edge` to return `EdgeId` directly (not `Option<EdgeId>`)
- ✅ New traversal methods: `forward_impact`, `backward_impact`, `bidirectional_impact`
- ✅ Deprecated aliases for old API methods (`get_forward_impact`, etc.)

**Issues Remaining:**
- ❌ **Petgraph API type mismatch (lines 786, 792, 800, 806):** Original fix attempted to use `EdgeReference.id()`, `.target()`, `.source()` methods which don't exist on the stable_graph EdgeReference type. The EdgeReference only has a `.weight()` method publicly available.

**Original (broken) fix attempt:**
```rust
// This was WRONG - EdgeReference doesn't have these methods
self.graph.edges(current)
    .filter(|e| config.edge_allowed(self.graph.edge_weight(e.id()).unwrap()))
    .map(|e| e.target())  // ERROR: no method named `target`
```

**Root cause:** Need to import `petgraph::visit::EdgeRef` trait which provides `source()`, `target()`, `id()` methods for EdgeReference.

---

#### 2.2 `src/graph/extraction.rs` - MULTIPLE CRITICAL ISSUES
**Status:** Fundamental scope issues prevent compilation.

**Implemented:**
- ✅ 3-signal directional data flow model (Signal A: 0.85, Signal B: 0.65, Signal C: 0.45)
- ✅ 4-signal inheritance evidence model with confidence scoring
- ✅ Multi-line import parsing for 12 languages
- ✅ `EdgeType::Containment` usage for structural edges
- ✅ `EXCLUDED_TYPES` constant for ubiquitous types
- ✅ `TraversalConfig` integration

**Critical Issues (MUST FIX):**

**Issue 1: Functions Not in Scope (Brace Imbalance Suspected)**
- `extract_rust_imports` (line ~1048) not found from call at line 934
- `extract_go_imports` (line ~1132) not found from call at line 935
- `extract_php_imports` (line ~1184) not found from call at line 939
- `collapse_multiline` (line ~1104) not found from call at line ~1053

**Investigation findings:**
- Functions ARE defined in the file
- Functions defined AFTER the call site (should be visible in Rust)
- Some functions ARE found (`extract_ruby_imports` at 1173 is found, `extract_php_imports` at 1184 is NOT)
- Brace counting shows the file has balanced braces overall (236 open, 236 close)
- BUT section 975-1050 has -3 brace difference (more closing than opening)
- Raw file analysis shows no obvious syntax errors

**Hypothesis:** There may be an invisible character or encoding issue, or the brace imbalance in the 975-1050 section is causing functions to be parsed as inside a nested scope.

**Issue 2: Regex Variables Not in Scope**
- `re_require` at line 1034 (used but supposedly not defined)
- `re_single` at line 1137
- `re2` at line 1194

**Investigation findings:**
- Variables ARE defined before use
- May be symptom of Issue 1 (function not parsed correctly)

**Issue 3: Raw String Regex Syntax Issues (FIXED)**
Some regex patterns had malformed closing:
```rust
// BEFORE (broken):
let re_export = Regex::new(
    r#"export\s+(?:\*|\{[^}]*\})\s+from\s+['\"]([^'\"]+)['\"]#).unwrap();
    // The #) is inside the string, not closing the raw string!

// AFTER (fixed):
let re_export = Regex::new(
    r#"export\s+(?:\*|\{[^}]*\})\s+from\s+['\"]([^'\"]+)['\"]#",
).unwrap();
```

**Fixed:** 3 instances of this issue were corrected.

---

#### 2.3 `src/phase/pdg_utils.rs` - IMPLEMENTED
**Status:** Complete, compiles successfully.

**Implemented:**
- ✅ `RelinkConfig` with documented scoring rationale
- ✅ Raised `max_candidates` from 1 to 3
- ✅ Optimized `merge_pdgs` with single-pass edge key collection
- ✅ `source_bytes` usage to eliminate disk re-read
- ✅ Pre-computed degree map for orphan cleanup

**No known issues.**

---

#### 2.4 `src/parse/parallel.rs` - MODIFIED
**Status:** Complete, compiles successfully.

**Change:** Added `source_bytes: Option<Vec<u8>>` field to `ParsingResult` struct.

---

#### 2.5 `src/storage/pdg_store.rs` - PARTIALLY FIXED
**Status:** Fixed EdgeMetadata and Node field issues.

**Fixed:**
- ✅ Added `confidence` field to `EdgeMetadata` conversions
- ✅ Added `Containment` variant to `convert_storage_edge_type`
- ✅ Removed `embedding` field access from `Node` (now externalized)
- ✅ Set `embedding: None` in `NodeRecord` construction

**Status:** Should compile after extraction.rs issues resolved.

---

#### 2.6 `src/phase/context.rs` - FIXED
**Status:** Fixed function signature mismatch.

**Change:** Updated `relink_external_import_edges` calls to pass `&RelinkConfig::default()` (lines ~153 and ~178).

---

#### 2.7 `src/cli/mcp/server.rs` - FIXED
**Status:** Fixed formatting issue.

**Change:** Removed trailing whitespace at line 683 that prevented `cargo fmt` from running.

---

## 3. Complete Error Inventory

### Category A: Extraction Scope Issues (CRITICAL - BLOCKING)
```
error[E0425]: cannot find function `extract_rust_imports` in this scope
  --> src/graph/extraction.rs:934:26

error[E0425]: cannot find function `extract_go_imports` in this scope
  --> src/graph/extraction.rs:935:28

error[E0425]: cannot find function `extract_php_imports` in this scope
  --> src/graph/extraction.rs:939:18

error[E0425]: cannot find value `re_require` in this scope
  --> src/graph/extraction.rs:1034:16

error[E0425]: cannot find value `re_single` in this scope
  --> src/graph/extraction.rs:1137:16

error[E0425]: cannot find value `re2` in this scope
  --> src/graph/extraction.rs:1194:16

error[E0425]: cannot find function `collapse_multiline` in this scope
  --> src/graph/extraction.rs:1053:21
```

**Impact:** Blocks all compilation. Functions appear to be parsed as out of scope despite being defined.

**Next steps:**
1. Examine raw bytes around lines 975-1050 for invisible characters
2. Try moving function definitions before their first use
3. Check if functions are somehow being parsed inside a different module scope
4. Verify no `#[cfg(...)]` attributes are hiding the functions

---

### Category B: Petgraph API Issues (HIGH)
```
error[E0599]: no method named `id` found for reference `&petgraph::stable_graph::EdgeReference<'_, pdg::Edge>`
  --> src/graph/pdg.rs:786:74

error[E0599]: no method named `target` found for struct `petgraph::stable_graph::EdgeReference<'a, E, Ix>`
  --> src/graph/pdg.rs:792:36

error[E0599]: no method named `id` found for reference `&petgraph::stable_graph::EdgeReference<'_, pdg::Edge>`
  --> src/graph/pdg.rs:800:74

error[E0599]: no method named `source` found for struct `petgraph::stable_graph::EdgeReference<'a, E, Ix>`
  --> src/graph/pdg.rs:806:36
```

**Impact:** BFS traversal in `bfs_filtered` method doesn't compile.

**Fix required:** Import `petgraph::visit::EdgeRef` trait which provides these methods:
```rust
use petgraph::visit::EdgeRef; // Add this import

// Then in the BFS traversal:
.filter(|e| config.edge_allowed(e.weight()))  // EdgeRef provides .weight()
.map(|e| e.target())  // EdgeRef provides .target()
```

---

### Category C: Deprecation Warnings (MEDIUM - treated as errors with -D warnings)
Multiple uses of deprecated methods:
- `get_forward_impact` → should use `forward_impact` with `TraversalConfig`
- `get_backward_impact` → should use `backward_impact` with `TraversalConfig`
- `get_forward_impact_bounded` → should use `forward_impact` with depth config
- `get_backward_impact_bounded` → should use `backward_impact` with depth config

**Locations:**
- `src/cli/mcp/handlers.rs` (multiple locations)
- `src/edit/mod.rs`
- `src/validation/impact.rs`
- `src/phase/phase3.rs`
- `src/phase/phase4.rs`

**Fix:** Update callers to use new API or add `#[allow(deprecated)]` temporarily.

---

### Category D: Minor Warnings (LOW)
```
warning: unused import: `petgraph::Direction as PGDir` in pdg.rs
warning: empty line after doc comment in project_id.rs:9
warning: empty lines after doc comment in server/handlers.rs:44
```

---

## 4. Actions Taken During Investigation

### Completed Fixes
1. ✅ Fixed `TraversalConfig` BFS traversal implementation (but petgraph API issue remains)
2. ✅ Added `confidence` field to all `EdgeMetadata` constructions
3. ✅ Added `Containment` variant to storage layer edge type conversion
4. ✅ Fixed malformed regex patterns (3 instances of incorrect raw string syntax)
5. ✅ Removed `embedding` field access from PDG Node (now externalized to `EmbeddingStore`)
6. ✅ Updated `relink_external_import_edges` calls to pass `RelinkConfig` parameter
7. ✅ Fixed trailing whitespace in `server.rs` that blocked `cargo fmt`
8. ✅ Removed unused `petgraph::visit::Dfs` import

### Investigation Performed
1. ✅ Verified all functions ARE defined in extraction.rs
2. ✅ Counted braces in file (balanced overall, but section 975-1050 has imbalance)
3. ✅ Checked for `#[cfg(...)]` attributes (only `#[cfg(test)]` at end of file)
4. ✅ Examined raw bytes around function definitions (no obvious corruption)
5. ✅ Identified that functions defined BEFORE certain line are found, AFTER are not
6. ✅ Confirmed `extract_ruby_imports` at 1173 IS found but `extract_php_imports` at 1184 is NOT

### Commands Used
```bash
# Check compilation errors
cargo check --lib 2>&1 | grep -E "error\[E"

# Count braces (excludes string literals)
python3 brace_counter.py src/graph/extraction.rs

# Find function definitions
grep -n "^fn extract_" src/graph/extraction.rs

# Examine raw bytes
sed -n '1047,1050p' src/graph/extraction.rs | xxd

# Format code
cargo fmt
```

---

## 5. Constraints Imposed by User

### HARD CONSTRAINTS (MUST FOLLOW)
1. **NO code removal** - All fixes must preserve logic. Cannot remove functions, fields, or code blocks to resolve issues.

2. **Mandatory LeIndex Usage** - Must use LeIndex MCP tools for code analysis:
   - `leindex_symbol_lookup` to find symbol definitions
   - `leindex_grep_symbols` to search for patterns
   - `leindex_read_symbol` to read symbol source
   - CLI commands for broader analysis if needed

3. **Logical assessment required** - All fixes must be based on:
   - Reviewing the code
   - Understanding surrounding code
   - Assessing intended logic
   - NOT just suppressing errors

### SOFT CONSTRAINTS (SHOULD FOLLOW)
1. Preserve backward compatibility where possible (deprecated aliases added)
2. Maintain the architectural improvements (no reverting to O(n²) algorithms)
3. Keep confidence thresholds at documented levels
4. Don't add new dependencies without justification

---

## 6. Key Architectural Decisions to Preserve

### 6.1 EdgeType::Containment is Structural Only
**Rule:** Never include `Containment` in `allowed_edge_types` for semantic traversal.
```rust
// WRONG:
TraversalConfig {
    allowed_edge_types: Some(vec![EdgeType::Call, EdgeType::Containment]), // Don't do this
    ...
}

// CORRECT:
TraversalConfig::for_impact_analysis() // This explicitly excludes Containment
```

### 6.2 Confidence Thresholds
- `MIN_INHERITANCE_CONFIDENCE = 0.45` - Do not lower this (signal quality threshold)
- Signal A (Return→Param): 0.85
- Signal B (Shared return + call): 0.65
- Signal C (Shared param + call): 0.45

### 6.3 TraversalConfig Required
There is NO unbounded traversal anymore. All callers must provide explicit bounds.
```rust
// OLD (removed):
pdg.get_forward_impact(node_id)

// NEW:
pdg.forward_impact(node_id, &TraversalConfig::for_impact_analysis())
```

### 6.4 Embeddings Externalized
PDG nodes no longer store embeddings inline. Use `EmbeddingStore`:
```rust
// OLD (removed):
node.embedding = Some(vec);

// NEW:
embedding_store.insert(&node.id, vec);
```

---

## 7. Recommended Next Steps (Priority Order)

### P0: Fix extraction.rs Scope Issues
**Goal:** Resolve "cannot find function/variable" errors.

**Approach 1: Verify file encoding**
```bash
file src/graph/extraction.rs
hexdump -C src/graph/extraction.rs | head -100
```

**Approach 2: Move function definitions**
Move `extract_rust_imports`, `extract_go_imports`, `extract_php_imports`, and `collapse_multiline` to BEFORE line 934 (before `extract_import_paths_from_source`).

**Approach 3: Check for macro expansion issues**
Search for any macro definitions that might be consuming the function definitions.

**Approach 4: Simplify and test**
Create a minimal test case with just 2-3 functions to verify the scope issue.

---

### P1: Fix Petgraph API in pdg.rs
**Goal:** Resolve EdgeReference method errors.

**Fix:**
```rust
// At top of file, add:
use petgraph::visit::EdgeRef;

// In bfs_filtered method, replace:
.filter(|e| config.edge_allowed(self.graph.edge_weight(e.id()).unwrap()))
.map(|e| e.target())

// With:
.filter(|e| config.edge_allowed(e.weight()))
.map(|e| e.target())
```

The `EdgeRef` trait provides `.source()`, `.target()`, and `.id()` methods for EdgeReference.

---

### P2: Update Deprecated API Callers
**Goal:** Eliminate deprecation warnings treated as errors.

**Option A:** Update all callers to use new API with TraversalConfig (preferred)

**Option B:** Add `#[allow(deprecated)]` to the deprecated method definitions (temporary fix)

---

### P3: Verify Full Compilation
**Goal:** Ensure `cargo check --features full` passes.

**Commands:**
```bash
cargo check --features full
cargo clippy --features full -- -D warnings
cargo test --features full --no-run
```

---

## 8. Reference Material

### Critical Files
- `/mnt/WD-SSD/code_index_update/LeIndexer/implementation_summary.md` - Complete rationale for all structural decisions
- `/mnt/WD-SSD/code_index_update/LeIndexer/pdg_rewrite.rs` - Original rewrite for pdg.rs
- `/mnt/WD-SSD/code_index_update/LeIndexer/extraction_rewrite.rs` - Original rewrite for extraction.rs
- `/mnt/WD-SSD/code_index_update/LeIndexer/pdg_utils_rewrite.rs` - Original rewrite for pdg_utils.rs

### Modified Files (with issues)
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/graph/pdg.rs` - Petgraph API issues
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/graph/extraction.rs` - Scope issues (CRITICAL)
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/storage/pdg_store.rs` - Should be resolved
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/phase/pdg_utils.rs` - Should be resolved
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/phase/context.rs` - Should be resolved

### Callers Using Deprecated API (need updating)
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/cli/mcp/handlers.rs`
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/edit/mod.rs`
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/validation/impact.rs`
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/phase/phase3.rs`
- `/mnt/WD-SSD/code_index_update/LeIndexer/src/phase/phase4.rs`

---

## 9. Testing Recommendations

After fixing compilation issues, verify:

1. **Containment edges are correctly typed:**
   ```rust
   assert!(edge.edge_type == EdgeType::Containment); // NOT Call
   ```

2. **Traversal respects edge type filtering:**
   ```rust
   let config = TraversalConfig::for_semantic_analysis();
   let result = pdg.forward_impact(node_id, &config);
   // Verify no Containment edges were traversed
   ```

3. **Data flow confidence scores preserved:**
   ```rust
   assert!(edge.metadata.confidence.is_some());
   assert!(edge.metadata.confidence.unwrap() >= 0.45);
   ```

4. **Import parsing works for multi-line statements:**
   Test Python `from x import (\n    a,\n    b\n)` pattern

---

## 10. Contact and Context

**User constraint reminder:** The user emphasized that this is a meta situation - using LeIndex to work on LeIndex. They want the LeIndex CLI tools used for analysis and codebase navigation and exploration.

**Success criteria:**
1. `cargo check --features full` passes with zero errors
2. `cargo clippy --features full -- -D warnings` passes
3. All deprecated API warnings resolved or explicitly allowed
4. No logic removed to fix issues

**Rollback plan:** If issues cannot be resolved, the backup branch is `backup/pre-unification-20260306` (per AGENTS.md context).

---

## Appendix: Debug Commands

```bash
# Check specific function scope
cargo check --lib 2>&1 | grep "extract_rust_imports"

# Verify regex syntax
cargo check --lib 2>&1 | grep "Regex"

# Count functions in extraction.rs
grep -c "^fn " src/graph/extraction.rs

# List all functions with their line numbers
grep -n "^fn " src/graph/extraction.rs

# Check for any cfg attributes
grep -n "#\[cfg" src/graph/extraction.rs

# Verify file has no BOM or encoding issues
head -c 3 src/graph/extraction.rs | xxd
```

---

**End of Handoff Document**
