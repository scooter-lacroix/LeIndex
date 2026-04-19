# Round 32 Review Fixes — Implementation Plan

## 2 New Findings Since Commit 2f77e14

---

### FIX-1: normalize_external_nodes misses prefixed language values [P1] ✅ VALIDATED

**Files**: `src/cli/leindex.rs`
**Reporter**: chatgpt-codex-connector P1

`normalize_external_nodes()` only matches `language == "external"` (exact).
But `annotate_external_nodes()` sets:
- `language: "external:cargo"` (line 1066)
- `language: "external:system"` (line 1079)
- `language: "external:npm"`, `"external:pip"`, etc.

These remain `NodeType::Module` after migration, and all 7 consumer sites
(edit/mod.rs, pdg_utils.rs, phase2.rs) only check `NodeType::External`,
so they slip through as regular symbols in rename/relink/search.

**Fix**: Expand the predicate:
```rust
if (node.language == "external" || node.language.starts_with("external:"))
    && node.node_type != NodeType::External
```

---

### FIX-2: Code fence closing syntax broken in both docs [Minor] ✅ VALIDATED

**Files**: `docs/LEINDEX_REFACTORING_GUIDE.md`, `docs/IMPROVEMENT_IMPLEMENTATION_PLAN.md`
**Reporter**: coderabbitai minor

The `re.sub(r'^```\s*$', '```text', ...)` in Round 31 replaced ALL ```` ``` ````
(including closing fences) with ```` ```text ````, breaking markdown rendering.
25 bad fences in refactoring guide, 56 in implementation plan.

**Fix**: Opening fences keep their language tag. Closing fences must be plain ```` ``` ````.
Need to properly alternate: first ```` ```lang ```` opens, next ```` ``` ```` closes.

---

## Execution

Both fixes are independent. Single commit.
