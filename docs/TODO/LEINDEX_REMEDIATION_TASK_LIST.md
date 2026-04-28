# LeIndex Remediation — Blocking Implementation Task List

Date: 2026-04-14
Source: `docs/tzar_usage_report.md`
Codebase: `/mnt/WD-SSD/code_index_update/LeIndexer/`

---

## Phase A — Critical Fixes (Blocks All Downstream Quality)

### A.1 — Always merge PDG scan with semantic pre-filter in grep_symbols

**Priority**: CRITICAL — highest leverage, ~5 line change
**File**: `src/cli/mcp/handlers.rs`
**Lines**: 2139-2143

Replace:
```rust
let use_direct_scan = all_matches.is_empty()
    || pattern.contains('|')
    || pattern.contains('*')
    || pattern.contains('?')
    || pattern.contains('[');

if use_direct_scan && all_matches.len() < fetch_limit {
```

With:
```rust
if all_matches.len() < fetch_limit {
```

**Acceptance**: `grep_symbols` with pattern `ColdStoreRepository` against the Radis_Rust project returns ≥1 result with real file path and byte range.

---

### A.2 — Filter external/phantom nodes from grep_symbols results

**Priority**: CRITICAL
**Files**: `src/cli/mcp/handlers.rs`
**Locations**: Both the pre-filter loop (~line 2080) and the direct-scan loop (~line 2156)

Add after each `let node = match pdg.get_node(nid) { ... };`:
```rust
// Skip phantom external-reference nodes (no source location)
if node.byte_range == (0, 0) && node.language == "external" {
    continue;
}
```

**Acceptance**: `grep_symbols` results contain zero entries with `language: "external"` and `byte_range: [0, 0]` by default.

---

### A.3 — Wire real complexity from parser into PDG nodes

**Priority**: CRITICAL
**Files**: `src/parse/traits.rs`, `src/parse/rust.rs`, `src/graph/extraction.rs`

**Step 1** — Add field to `SignatureInfo` in `src/parse/traits.rs` after line 81:
```rust
/// Cyclomatic complexity extracted from AST
pub cyclomatic_complexity: u32,
```

**Step 2** — Populate in `src/parse/rust.rs` inside `extract_function_signature()` (~line 331):
```rust
let complexity_metrics = self.extract_complexity(node);
// ... in the returned SignatureInfo:
cyclomatic_complexity: complexity_metrics.cyclomatic.max(1) as u32,
```

**Step 3** — Set all other language parsers' `cyclomatic_complexity` to `0` (fallback will handle it).

**Step 4** — Consume in `src/graph/extraction.rs` at line 1560, replace:
```rust
let complexity = 1u32 + sig.parameters.len() as u32;
```
With:
```rust
let complexity = if sig.cyclomatic_complexity > 0 {
    sig.cyclomatic_complexity
} else {
    1u32 + sig.parameters.len() as u32
};
```

**Step 5** — For class nodes at line 146, replace:
```rust
complexity: method_nids.len() as u32,
```
With:
```rust
complexity: method_nids.iter()
    .filter_map(|&nid| pdg.get_node(nid))
    .map(|n| n.complexity)
    .sum::<u32>()
    .max(1),
```

**Acceptance**: `grep_symbols` for a function with 3 `if` branches and 2 `match` arms returns `complexity > 5`, not `complexity: 1`.

---

### A.4 — Fix Rust parser cyclomatic complexity extraction

**Priority**: CRITICAL (blocks A.3)
**File**: `src/parse/rust.rs`

Replace `extract_complexity()` at line 246 with:
```rust
fn extract_complexity(&self, node: &tree_sitter::Node<'_>) -> ComplexityMetrics {
    let mut cyclomatic = 1u32;

    fn walk(node: &tree_sitter::Node<'_>, cc: &mut u32) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "if_expression" | "else_clause"
                | "while_expression" | "for_expression" | "loop_expression"
                | "match_arm" => {
                    *cc += 1;
                }
                "binary_expression" => {
                    if let Some(op) = child.child_by_field_name("operator") {
                        match op.kind() {
                            "&&" | "||" => *cc += 1,
                            _ => {}
                        }
                    }
                }
                "try_expression" => {
                    *cc += 1;
                }
                _ => {}
            }
            walk(&child, cc);
        }
    }

    walk(node, &mut cyclomatic);

    ComplexityMetrics {
        cyclomatic: cyclomatic as usize,
        cognitive: cyclomatic as usize,
        branches: cyclomatic.saturating_sub(1) as usize,
        ..Default::default()
    }
}
```

**Acceptance**: A Rust function containing `if {} else if {} match { A => {}, B => {} }` returns `cyclomatic >= 5`.

---

## Phase B — High-Priority Fixes (Fixes Core Value Prop)

### B.1 — Add struct instantiation edges to Rust call graph

**Priority**: HIGH
**File**: `src/parse/rust.rs`

In `extract_rust_calls()` at line 385, add a new match arm inside the `for child in node.children(...)` loop:
```rust
// After existing "call_expression" and "macro_invocation" arms:

"struct_expression" => {
    if let Some(name_node) = child.child_by_field_name("name") {
        if let Ok(name) = name_node.utf8_text(source) {
            calls.push(name.to_string());
        }
    }
    // Recurse into struct fields for nested calls
    let inner = extract_rust_calls(&child, source);
    calls.extend(inner);
}
```

**Acceptance**: A function containing `let x = MyStruct { field: 1 };` produces a call edge from that function to `MyStruct`.

---

### B.2 — Extract scoped path prefix as struct reference

**Priority**: HIGH
**File**: `src/parse/rust.rs`

In `extract_rust_calls()`, when processing `call_expression` nodes, also extract the type prefix from `Type::method()` patterns:
```rust
"call_expression" => {
    // Existing call name extraction logic...
    
    // NEW: extract the type prefix from scoped calls like Foo::new()
    if let Some(func_node) = child.child_by_field_name("function") {
        if func_node.kind() == "scoped_identifier" {
            if let Some(path_node) = func_node.child_by_field_name("path") {
                if let Ok(path_text) = path_node.utf8_text(source) {
                    // Add the type name itself as a reference
                    let type_name = path_text.to_string();
                    if !type_name.is_empty()
                        && type_name.chars().next().map_or(false, |c| c.is_uppercase())
                    {
                        calls.push(type_name);
                    }
                }
            }
        }
    }
}
```

**Acceptance**: `DeepThoughtManager::new()` produces edges to both `DeepThoughtManager::new` and `DeepThoughtManager`.

---

### B.3 — Resolve struct-name callees to class/struct PDG nodes

**Priority**: HIGH
**File**: `src/graph/extraction.rs`

In `extract_call_edges()` after the callee resolution loop (~line 913-970), when a callee is resolved, also check if the bare name (without method suffix) maps to a class node:
```rust
// After resolving callee_nid for the method:
// Also link caller → struct node if the callee name matches a class node
let bare_type = callee_name.split("::").next().unwrap_or(&callee_name);
if bare_type != callee_name {
    let type_qualified = format!("{}:{}", 
        sig.qualified_name.rsplit(':').last().unwrap_or(""),
        bare_type
    );
    // Try exact match first, then last-segment match
    let struct_nid = node_ids.get(bare_type)
        .or_else(|| last_map.get(bare_type).and_then(|v| v.first()));
    if let Some(&snid) = struct_nid {
        let pair = (caller_id, snid);
        if !seen.contains(&pair) {
            seen.insert(pair);
            edges.push(pair);
        }
    }
}
```

**Acceptance**: `caller_count` for `DeepThoughtManager` is > 0 when functions in the project call `DeepThoughtManager::new()` or instantiate it.

---

### B.4 — Adjust default search scoring weights

**Priority**: HIGH
**File**: `src/search/ranking.rs`

Replace default `HybridScorer::new()` at line 55:
```rust
pub fn new() -> Self {
    Self {
        semantic_weight: 0.25,
        structural_weight: 0.15,
        text_weight: 0.60,
    }
}
```

Add mode-specific constructors:
```rust
/// Scorer tuned for code symbol search (text-dominant)
pub fn for_code() -> Self {
    Self {
        semantic_weight: 0.25,
        structural_weight: 0.15,
        text_weight: 0.60,
    }
}

/// Scorer tuned for natural-language/prose search
pub fn for_prose() -> Self {
    Self {
        semantic_weight: 0.50,
        structural_weight: 0.10,
        text_weight: 0.40,
    }
}
```

Then wire `search_mode` parameter in `leindex_search` handler to select the right scorer.

**Acceptance**: `leindex_search` with `search_mode: "code"` and query `"ColdStoreRepository"` returns the target as the top result.

---

## Phase C — System Improvements

### C.1 — Add `NodeType::External` variant

**File**: `src/graph/pdg.rs`

Add to the `NodeType` enum:
```rust
pub enum NodeType {
    Function,
    Method,
    Class,
    Module,
    Variable,
    /// Imported/referenced symbol not defined in this project
    External,
}
```

Update `src/graph/extraction.rs` everywhere that currently sets `language: "external".to_string()` to instead use `NodeType::External`.

Update `node_type_str()` in `src/cli/mcp/handlers.rs` to handle the new variant:
```rust
fn node_type_str(nt: &NodeType) -> &str {
    match nt {
        NodeType::Function => "function",
        NodeType::Method => "method",
        NodeType::Class => "class",
        NodeType::Module => "module",
        NodeType::Variable => "variable",
        NodeType::External => "external",
    }
}
```

Then simplify the A.2 filter to:
```rust
if matches!(node.node_type, NodeType::External) {
    continue;
}
```

**Acceptance**: External nodes are typed, not string-tagged.

---

### C.2 — Deduplicate re-export nodes in grep_symbols

**File**: `src/cli/mcp/handlers.rs`

In `GrepSymbolsHandler::handle()`, change `seen_ids` to deduplicate by name+file instead of just node ID:
```rust
// Replace:
let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
// ...
if !matches || seen_ids.contains(&node.id) { continue; }
seen_ids.insert(node.id.clone());

// With:
let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
let mut seen_names: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
// ...
let name_file_key = (node.name.clone(), node.file_path.clone());
if !matches || seen_ids.contains(&node.id) || seen_names.contains(&name_file_key) {
    continue;
}
seen_ids.insert(node.id.clone());
seen_names.insert(name_file_key);
```

**Acceptance**: A symbol with a `pub use` re-export and a definition in the same file appears only once.

---

### C.3 — Add staleness warning to tool responses

**File**: `src/cli/mcp/handlers.rs`

Add a helper:
```rust
fn maybe_add_staleness_warning(result: &mut Value, index: &LeIndex) {
    if index.is_stale_fast() {
        if let Some(obj) = result.as_object_mut() {
            obj.insert(
                "_warning".to_string(),
                Value::String(
                    "Index may be stale. Call leindex_index with force_reindex=true for fresh results.".into(),
                ),
            );
        }
    }
}
```

Call it at the end of every read-path handler (`grep_symbols`, `symbol_lookup`, `search`, `file_summary`, `project_map`, `read_symbol`, `read_file`, `text_search`, `context`, `deep_analyze`, `impact_analysis`, `git_status`).

**Acceptance**: When files on disk are newer than the index, responses include `_warning` field.

---

### C.4 — Structured zero-result responses

**File**: `src/cli/mcp/handlers.rs`

In `GrepSymbolsHandler::handle()`, after pagination, if results are empty:
```rust
if paginated.is_empty() {
    let pdg = index.pdg().unwrap();
    return Ok(serde_json::json!({
        "results": [],
        "count": 0,
        "total_matched": 0,
        "has_more": false,
        "suggestion": format!(
            "No symbols matching '{}' found in {} indexed symbols across {} files. \
             Try: broader substring, check case, or use leindex_text_search for raw text.",
            pattern,
            pdg.node_count(),
            pdg.file_count()
        )
    }));
}
```

Apply similar pattern to `SymbolLookupHandler` and `SearchHandler`.

**Acceptance**: Zero-result responses include `suggestion`, `indexed_symbol_count`, and `indexed_file_count`.

---

### C.5 — Cache per-file PDG statistics

**File**: `src/cli/leindex.rs`

Add to `LeIndex` struct:
```rust
/// Cached per-file statistics, invalidated on reindex
file_stats_cache: Option<HashMap<String, FileStats>>,
```

```rust
#[derive(Clone)]
struct FileStats {
    symbol_count: usize,
    total_complexity: u32,
    symbol_names: Vec<String>,
}
```

Populate after `index_nodes()`:
```rust
fn build_file_stats_cache(&mut self) {
    let Some(pdg) = self.pdg() else { return };
    let mut cache: HashMap<String, FileStats> = HashMap::new();
    for nid in pdg.node_indices() {
        if let Some(node) = pdg.get_node(nid) {
            let entry = cache.entry(node.file_path.clone()).or_insert_with(|| FileStats {
                symbol_count: 0,
                total_complexity: 0,
                symbol_names: Vec::new(),
            });
            entry.symbol_count += 1;
            entry.total_complexity += node.complexity;
            entry.symbol_names.push(node.name.clone());
        }
    }
    self.file_stats_cache = Some(cache);
}
```

Invalidate in `index_nodes()`:
```rust
self.file_stats_cache = None;
```

Expose via:
```rust
pub fn file_stats(&self) -> Option<&HashMap<String, FileStats>> {
    self.file_stats_cache.as_ref()
}
```

Then use in `ProjectMapHandler` and `FileSummaryHandler` instead of re-iterating the PDG.

**Acceptance**: Repeated `project_map` calls are measurably faster (no full PDG iteration).

---

### C.6 — Add `include_source: bool` parameter to grep_symbols

**File**: `src/cli/mcp/handlers.rs`

Add to `GrepSymbolsHandler` input schema:
```rust
"include_source": {
    "type": "boolean",
    "description": "Include full symbol source code in results (default: false)",
    "default": false
}
```

In the handler, after building each entry:
```rust
let include_source = extract_bool(&args, "include_source", false);

// ... inside the match loop, after building `entry`:
if include_source {
    if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
        entry["source"] = Value::String(src);
    }
}
```

**Acceptance**: `grep_symbols` with `include_source: true` returns full symbol source without a follow-up `read_symbol` call.

---

## Blocking Dependencies

| Task | Blocked By |
|------|-----------|
| A.1 | — |
| A.2 | — |
| A.3 | A.4 |
| A.4 | — |
| B.1 | — |
| B.2 | — |
| B.3 | B.1, B.2 |
| B.4 | — |
| C.1 | A.2 |
| C.2 | A.1 |
| C.3 | — |
| C.4 | A.1 |
| C.5 | A.3 |
| C.6 | — |

## Recommended Execution Order

1. A.1 + A.2 + A.4 (independent, can parallel)
2. A.3 (depends on A.4)
3. B.1 + B.2 + B.4 (independent, can parallel)
4. B.3 (depends on B.1 + B.2)
5. C.1 through C.6 (independent, any order after Phase A)

## Validation Gate

After all Phase A + B tasks:
```bash
cargo test -p leindex --quiet
cargo check -p leindex --quiet
# Then reindex Radis_Rust and verify:
# - grep_symbols("ColdStoreRepository") returns ≥1 result
# - grep_symbols("DeepThoughtManager") returns caller_count > 0
# - grep_symbols results contain zero "external" phantom nodes
# - complexity values differentiate simple vs complex functions
```
