# PR #10 - Prioritized Issues for Fix

## Immediate Blockers (Must Fix Before Merge)

### 1. Redo History Corruption - `src/edit/engine.rs:325-336, 722-744`
**Problem**: Records redo entry even when post-image read fails (modified_content is None), creating unreplayable history.

**Fix**: Propagate the `read_to_string` error instead of `.ok()`:
```rust
// BEFORE (BROKEN):
let modified_content = std::fs::read_to_string(&request.file_path).ok();

// AFTER (FIXED):
let modified_content = Some(std::fs::read_to_string(&request.file_path).map_err(|e| {
    EditError::HistoryError(format!(
        "Failed to capture modified content for redo for '{}': {}",
        request.file_path.display(),
        e
    ))
})?);
```

---

### 2. Error Masking in list_projects - `src/global/registry.rs:258-278`
**Problem**: `.filter_map(|r| r.ok())` silently drops DB errors and parsing failures.

**Fix**: Propagate errors properly:
```rust
// BEFORE (BROKEN):
let projects = stmt
    .query_map([], |row| { ... })?
    .filter_map(|r| r.ok())
    .collect();

// AFTER (FIXED):
let projects: Vec<ProjectInfo> = stmt
    .query_map([], |row| { ... })?
    .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;
```

---

### 3. UTF-8 Byte Slicing - `src/cli/mcp/helpers.rs:277-280`
**Problem**: `&old_text[..60]` can panic on multibyte UTF-8 characters.

**Fix**: Use character-based slicing:
```rust
// BEFORE (BROKEN):
let preview = if old_text.len() > 60 {
    format!("{}...", &old_text[..60])
} else {
    old_text.clone()
};

// AFTER (FIXED):
let preview = if old_text.chars().count() > 60 {
    format!("{}...", old_text.chars().take(60).collect::<String>())
} else {
    old_text.clone()
};
```

---

## High Priority (Should Fix)

### 4. O(N²) Hash Lookup - `src/cli/leindex/indexing.rs:153-188`
**Problem**: `source_files_with_hashes.iter().find()` called in loop reintroduces O(N²) complexity.

**Fix**: Create lookup map before loop:
```rust
// BEFORE loop:
let hash_lookup: HashMap<&PathBuf, HashType> = source_files_with_hashes.iter()
    .map(|(p, h)| (p, *h))
    .collect();

// INSIDE loop:
let hash = hash_lookup.get(&file_path).copied();
```

---

### 5. Yarn Multi-Selector Bug - `src/graph/external_deps.rs:455-468`
**Problem**: Comma split happens after @version parsing, causing incorrect package names.

**Fix**: Split before parsing:
```rust
let primary_spec = spec.split(',').next().unwrap_or(spec).trim();
// Then run all @version parsing on primary_spec (not spec)
```

---

### 6. pnpm Peer-Suffix Bug - `src/graph/external_deps.rs:499-513`
**Problem**: `@` inside peer-suffix parentheses mistaken for version delimiter.

**Fix**: Parse base spec before peer metadata:
```rust
let base_spec = spec.split('(').next().unwrap_or(spec);
// Then run rfind('@') on base_spec (not spec)
```

---

### 7. Input Validation Missing - `src/cli/mcp/symbol_lookup_handler.rs:106-124`
**Problem**: Accepts blank inputs like `""` or `"  "` without validation.

**Fix**: Trim and reject empty:
```rust
let symbols = syms.as_array()
    .unwrap_or(&vec![])
    .iter()
    .filter_map(|s| s.as_str())
    .map(|s| s.trim())
    .filter(|s| !s.is_empty())  // ← Add this
    .take(20)
    .collect();

if symbols.is_empty() {
    return Err(JsonRpcError::invalid_params(
        "Symbol list must contain at least one non-empty symbol"
    ));
}
```

---

## Low Priority (Nice to Have)

8-14. Documentation improvements (language specifiers, path links, status wording, code examples)

See full analysis: `PR10_CODERABBIT_ANALYSIS.md`

---

## Verification

After applying fixes, verify:
```bash
# Check for masked errors
rg "unwrap_or_default\(\)" src/ | grep -v "test"

# Check for byte slicing on strings
rg '\[\.\.\d+\]' src/ | grep -v '\.chars()'

# Run tests
cargo test --workspace

# Run clippy
cargo clippy --workspace -- -D warnings
```

---

## Summary

- **3 Critical** - Block merge (data integrity, error handling, crash risk)
- **4 Major** - Should fix (performance, correctness, robustness)
- **7 Minor** - Documentation quality
- **2 False positives** - Ignore (test strictness, diagnostic ordering)
- **1 Already fixed** - Dependency count issue

**Total Actionable**: 14 issues
**Immediate Action Required**: 3 critical blockers
