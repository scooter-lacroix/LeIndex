# Round 31 Review Fixes — Implementation Plan

## New Findings from Reviewers (on improvement commits)

Filtered to findings that are genuinely new (not previously addressed), 
valid against current code, and affect correctness/performance.

---

### FIX-1: Semantic mode doesn't enforce token_budget [P1/High] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reported by**: codex P1, gemini high, coderabbit major

Semantic branch at ~line 2245 sets `"truncated": true` but returns the full
oversized payload. With `include_source=true`, callers, and callees, a single
result can be 10KB+.

**Fix**: After building `paginated`, apply the same char_budget truncation as
exact mode:
```rust
let mut truncated_results = Vec::new();
let mut total = 0;
for entry in paginated {
    total += entry.to_string().len();
    if total > char_budget { break; }
    truncated_results.push(entry);
}
let truncated = total > char_budget;
// Use truncated_results and truncated flag
```

---

### FIX-2: Semantic mode scope filter is not separator-aware [P2] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reported by**: codex P2, coderabbit major

`starts_with(s.trim_end_matches(...))` lets `/repo/src_extra/` match scope
`/repo/src/`. The exact-mode uses `scope_str` with trailing separator.

**Fix**: Reuse the same scope check pattern from exact mode. Use `scope_str`
(which includes trailing separator) instead of `scope`:
```rust
if let Some(ref s) = scope {
    let scope_with_sep = format!("{}{}", s.trim_end_matches(std::path::MAIN_SEPARATOR), std::path::MAIN_SEPARATOR);
    let file_with_sep = format!("{}{}", node.file_path.trim_end_matches(std::path::MAIN_SEPARATOR), std::path::MAIN_SEPARATOR);
    if !file_with_sep.starts_with(&scope_with_sep) {
        continue;
    }
}
```
Actually simpler: just use the same `scope_str` variable from the outer scope
(which is already computed with trailing separator for exact mode).

---

### FIX-3: Semantic mode doesn't filter external nodes [Medium] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reported by**: gemini medium

Exact mode filters `NodeType::External && byte_range == (0,0)`. Semantic mode
has no such filter. External nodes like `serde`, `std` leak into results.

**Fix**: Add after scope/type filter:
```rust
use crate::graph::pdg::NodeType;
if matches!(node.node_type, NodeType::External) && node.byte_range == (0, 0) {
    continue;
}
```

---

### FIX-4: read_source_snippet called twice per match [Medium] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reported by**: gemini medium

When both `context_lines > 0` and `include_source` are true, the file is read
twice. This affects BOTH semantic and exact mode.

**Fix**: Read once, use for both:
```rust
let source = if context_lines > 0 || include_source {
    read_source_snippet(&node.file_path, node.byte_range)
} else {
    None
};
if context_lines > 0 {
    if let Some(ref src) = source {
        entry["context"] = Value::String(src.lines().take(context_lines).collect::<Vec<_>>().join("\n"));
    }
}
if include_source {
    if let Some(ref src) = source {
        // ... truncation logic
    }
}
```
Apply to BOTH semantic and exact mode locations.

---

### FIX-5: chars().count() is inefficient for truncation check [Medium] ✅ VALIDATED
**Files**: `src/cli/mcp/handlers.rs`
**Reported by**: gemini medium

`src.chars().count()` iterates the entire string to check if >4000 chars.
Since we only need to know if it exceeds the limit, we can short-circuit.

**Fix**: Replace with:
```rust
let was_truncated = src.len() > 4000 * 4; // UTF-8: max 4 bytes per char
```
Or more precisely:
```rust
let was_truncated = src.char_indices().nth(4000).is_some();
```
This stops after finding the 4001st character instead of counting all.
Apply to all 3 locations (lines 2232, 2355, 2460).

---

### FIX-6: index_freshness doesn't skip hidden directories [Major] ✅ VALIDATED
**Files**: `src/cli/index_freshness.rs`
**Reported by**: coderabbit major

The walkdir filter only checks `SKIP_DIRS`, but external_deps.rs also skips
non-root hidden directories (`.cache/`, `.yarn/`, etc.). A manifest under
`.cache/` would falsely mark the index as stale.

**Fix**: Add hidden-dir check to the filter_entry closure:
```rust
.filter_entry(|e| {
    if let Some(name) = e.file_name().to_str() {
        if SKIP_DIRS.contains(&name) && e.file_type().is_dir() {
            return false;
        }
        // Skip non-root hidden directories (like .cache/, .yarn/)
        if e.path() != ctx.project_path && name.starts_with('.') && e.file_type().is_dir() {
            return false;
        }
    }
    true
})
```

---

### FIX-7: Docs — unlabeled code blocks [Minor] ✅ VALIDATED
**Files**: `docs/IMPROVEMENT_IMPLEMENTATION_PLAN.md`, `docs/LEINDEX_REFACTORING_GUIDE.md`
**Reported by**: coderabbit minor x3

Unlabeled ``` fences. Add `rust`, `json`, `bash`, or `text` labels.

---

### FIX-8: Docs — plan scope says 9 tasks but only shows 8 [Minor] ✅ VALIDATED
**Files**: `docs/IMPROVEMENT_IMPLEMENTATION_PLAN.md`
**Reported by**: coderabbit minor

Header says recommendations #1-#4 and #6-#10 (9 tasks) but the plan only has
8 tasks (TASK-01 through TASK-09, skipping #5). Fix the header text.

---

## Execution Order

| Step | Fix | Risk | LOC |
|------|-----|------|-----|
| 1 | FIX-1: semantic token_budget truncation | Low | ~10 |
| 2 | FIX-2: semantic scope separator-aware | Low | ~5 |
| 3 | FIX-3: semantic external node filter | Low | ~4 |
| 4 | FIX-4: deduplicate read_source_snippet (both modes) | Low | ~20 |
| 5 | FIX-5: efficient truncation check (3 locations) | Low | ~3 |
| 6 | FIX-6: hidden-dir skip in index_freshness | Low | ~4 |
| 7 | FIX-7: label code blocks in docs | Low | ~20 |
| 8 | FIX-8: fix plan scope header | Low | ~1 |

All 8 fixes are independent. Single commit.
