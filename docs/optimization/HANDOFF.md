# LeIndexer Optimization - Handoff Document

**Spec Bible**: SPEC_BIBLE.md  
**Task List**: TASK_LIST.md  
**Branch**: `feature/unified-crate`  
**Base commit**: `cf2d145`  
**Date**: 2026-04-27  

---

## Context for New Developer

You are picking up an optimization and integration effort on the LeIndexer codebase. The previous analyst (Droid) completed a comprehensive structural analysis and produced a spec bible with 28 blocking tasks across 8 phases.

This work is **optimization-focused**, not feature-focused. Goals:
- Reduce memory footprint (embedding store, string interning, content clearing)
- Improve performance (O(N) → O(1) lookups, BFS buffers, deduplication)
- Integrate orphan rewrite files that already exist but are disconnected
- Complete and wire the validation pipeline that is currently unused
- Reduce code duplication and improve maintainability

---

## Key Architectural Decisions

### 1. Rewrite Integration Strategy

Three orphan rewrite files (`pdg_rewrite.rs`, `extraction_rewrite.rs`, `pdg_utils_rewrite.rs`) were evaluated. They are **not** wholesale replacements. Instead:

- **pdg_rewrite**: Add `EmbeddingStore` (externalizes embeddings from Node) and helper methods (`remove_file`, bulk edge adders) to existing `pdg.rs`. The current `pdg.rs` already has `Containment` edges, `TraversalConfig`, serialization, and tests — keep those.
- **extraction_rewrite**: Replace the **body** of `extract_pdg_from_signatures()` in `extraction.rs` with the 5-phase pipeline. This fixes the O(N²) clique generation and multi-line import parsing. Requires `EmbeddingStore` from pdg rewrite.
- **pdg_utils_rewrite**: Replace entire `pdg_utils.rs` with rewrite. Requires `extraction_rewrite` integration first.

**Dependency chain**: pdg_rewrite → extraction_rewrite → pdg_utils_rewrite. Do not break this order.

### 2. Validation Pipeline Integration

The `src/validation/` module is fully built but **never called**. It must be integrated into the MCP edit handlers.

**Critical integration points**:
- `edit_preview_handler::execute()`: Validate before applying, return warnings in preview response
- `edit_apply_handler::execute()`: Reject edits with validation errors, include warnings if applicable
- `rename_symbol_handler::execute()`: Validate rename for conflicts and result syntax

**Unification required**: There are two `EditChange` types:
- `src/edit/mod.rs::EditChange` (ReplaceText, RenameSymbol) - used by MCP handlers
- `src/validation/edit_change.rs::EditChange` (edit_type, new_content) - used by validation

The validation module must operate on the canonical `EditChange` from `edit/mod.rs`. Choose: either update validation's `EditChange` to match, or create a conversion layer. The spec expects validation to consume the same type as handlers.

**Validator creation**: `LogicValidator` needs `Arc<ProgramDependenceGraph>` and `Arc<Storage>`. Add a method to `LeIndex`:
```rust
pub fn create_validator(&self) -> Option<LogicValidator> { ... }
```

**Response format**: Add a `validation` field to MCP edit responses:
```json
{
  "validation": {
    "is_valid": true,
    "syntax_errors": [],
    "reference_issues": [],
    "semantic_drift": [],
    "impact_report": { ... }
  }
}
```

### 3. Concurrency Change (T22)

`ProjectRegistry` currently uses `Mutex<LeIndex>`. The fix is to use `RwLock<LeIndex>` to allow concurrent reads while PDG is loading lazily on first request. This is a **high-risk** change. Test thoroughly for deadlocks and data races.

### 4. Edit Module Split (T05)

`src/edit/mod.rs` is a 2487-line monolith. Split into:
- `engine.rs` - core edit application logic
- `command.rs` - edit command definitions and parsing
- `history.rs` - undo/redo tracking
- `refactor.rs` - rename and whole-word operations

Public API surface must remain identical. Update `mod.rs` to re-export all public items.

---

## Risk Areas & Mitigation

### High-Risk Tasks

1. **T22 (RwLock)**: Can introduce deadlocks or race conditions.
   - *Mitigation*: Run with `--release` and `threadpool` stress tests. Use `loom` if available. Verify no panics in concurrent access patterns.
   
2. **T15 (EditChange unification)**: Type incompatibility could break handler-validation integration.
   - *Mitigation*: Ensure conversion is bidirectional and exhaustive. Add `From` implementations. Update all validation tests to use canonical type.

3. **T16-T19 (Validation wiring)**: Could cause edit preview/apply to reject valid edits or accept invalid ones.
   - *Mitigation*: Start with `warnings-only` mode (don't reject on warnings), then gradually enable error rejection. Add test cases for each validator branch.

4. **T09 (extraction_rewrite integration)**: Core pipeline change could break PDG construction.
   - *Mitigation*: Port all tests from rewrite. Compare PDG edge counts on a sample corpus before/after. Run full test suite.

5. **T05 (edit/mod.rs split)**: Public API breakage could affect downstream crates.
   - *Mitigation*: Keep `mod.rs` as a facade that re-exports everything. No public item moves between files without explicit re-export.

### Medium-Risk Tasks

- **T08 (pdg_rewrite integration)**: Adding `EmbeddingStore` changes memory layout. Verify serialization/deserialization still works.
- **T10 (pdg_utils_rewrite integration)**: Relink logic changes could affect index correctness. Compare query results on a test index.
- **T23 (BFS scratch buffer)**: `RefCell` misuse could cause panics in single-threaded contexts only. Ensure no `borrow()` holds across yield points.

---

## Testing Strategy

### Unit Tests
- All existing unit tests must pass after each phase.
- Port tests from rewrite files (T08, T09, T10).
- Add tests for validation integration: simulate syntax errors, reference errors, and verify correct rejection/warning.

### Integration Tests
- Run full indexing pipeline on a sample corpus (at least 100 files, multi-language) after T09 and T10.
- Execute end-to-end edit preview/apply flow with validation after T16-T19.
- Test concurrent handler requests after T22.

### Performance Benchmarks
- Benchmark `semantic_search()` latency before/after T11 (target: O(1) node lookup).
- Benchmark memory usage after T08 (EmbeddingStore externalization) and T26 (Arc<str> interning).
- Profile BFS operations after T23.

### Regression Checks
- Compare search result sets (top-10) before and after each phase to ensure correctness.
- Validate that MCP protocol responses match expected schema.

---

## Rollback Procedures

### If Something Breaks

1. **Phase 1-2 (Low risk)**: Fix directly. No rollback needed.
2. **Phase 3 (Rewrite integration)**:
   - If T08 breaks: `git checkout src/graph/pdg.rs` - revert to original.
   - If T09 breaks: revert `src/graph/extraction.rs` and keep T08 if it worked standalone.
   - If T10 breaks: revert `src/phase/pdg_utils.rs` and keep T08-T09 if they worked.
   - Rewrite files remain in repo; you can retry integration with fixes.
3. **Phase 4 (Search engine)**:
   - If T11 breaks O(1) lookup: `git checkout src/search/search.rs`.
   - If T12 causes ownership issues: revert and re-think clone strategy.
4. **Phase 5 (Validation)**:
   - If validation blocks valid edits: add `--skip-validation` flag or config toggle to disable temporarily.
   - If types conflict: revert T15, implement proper conversion instead of unification.
5. **Phase 6 (Handler layer)**:
   - If T22 causes deadlocks: revert to `Mutex`, revisit RwLock placement.
   - If macro (T04) causes hygiene issues: expand macro inline and debug.
6. **Phase 7 (PDG/Graph)**: Revert individual tasks; they are largely isolated.

**Git strategy**: Each task should be a separate commit. Use `git bisect` to isolate breaking changes. Do not combine multiple tasks in one commit.

---

## Important Files Reference

| Path | Purpose |
|------|---------|
| `SPEC_BIBLE.md` | Full spec with findings, rationale, risk assessment |
| `TASK_LIST.md` | Ordered 28-task checklist |
| `src/search/search.rs` | SearchEngine - T11, T12, T14 |
| `src/graph/pdg.rs` | PDG core - T23, T24, T25, T26, T08 |
| `src/graph/extraction.rs` - T09 | AST-to-PDG extraction |
| `src/phase/pdg_utils.rs` - T10 | PDG merge/relink |
| `src/edit/mod.rs` - T05, T06, T15 | Edit engine |
| `src/validation/` - T15-T19 | Validation pipeline |
| `src/cli/mcp/` - T04, T20, T21, T22 | MCP handlers |
| `pdg_rewrite.rs` - T08 | Orphan rewrite source |
| `extraction_rewrite.rs` - T09 | Orphan rewrite source |
| `pdg_utils_rewrite.rs` - T10 | Orphan rewrite source |

---

## Getting Started

1. Read SPEC_BIBLE.md fully.
2. Run baseline benchmarks: `cargo test --workspace` should pass. Memory baseline: ` CIPHER=1 cargo run --release -- bin/leindex index ...` and monitor RSS.
3. Start with **Phase 1** (T01-T03) - these are hygiene tasks that reduce noise.
4. After each task, commit with message following Conventional Commits: `feat:`, `refactor:`, `fix:`, `perf:`.
5. Run `cargo check` and `cargo test --workspace` before committing.
6. Do not proceed to next phase until current phase tests pass.

---

## Questions?

Refer to SPEC_BIBLE.md for detailed rationale, code snippets, and dependency graphs. If something is unclear, re-read the analysis sections for that finding.
