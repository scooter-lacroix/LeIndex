# LeIndex Tzar Usage Report — Deep Root Analysis

Date: 2026-04-14
Source codebase: `/mnt/WD-SSD/code_index_update/LeIndexer/`
Purpose: Guide the next LeIndex debugging and remediation round based on experienced issues from the Radis_Rust Tzar review

---

## Part 1 — Codebase Understanding (Issue-Agnostic)

### 1.1 Architecture Overview

LeIndex is a single Rust crate (`leindex v1.5.2`) with 10 feature-gated modules:

```
lib.rs
├── parse     — tree-sitter AST extraction (16 languages)
├── graph     — PDG construction (petgraph-backed)
├── storage   — SQLite/Turso persistent storage
├── search    — Vector search + hybrid ranking
├── phase     — 5-phase analysis pipeline
├── cli       — CLI + MCP server + project registry
├── global    — Multi-project discovery
├── server    — HTTP API + WebSocket
├── edit      — Code editing with git2
└── validation — Index validation/drift
```

### 1.2 Data Flow: Index → Search → Tool Response

```
1. Parse: tree-sitter extracts SignatureInfo per file (name, params, calls, imports, byte_range)
2. Graph: extraction.rs builds PDG nodes + edges from signatures
   - Nodes: id = "file:qualified_name", complexity, byte_range
   - Edges: call graph extracted from sig.calls field
3. Search: LeIndex (cli/leindex.rs) builds:
   - TfIdfEmbedder from corpus → 768-dim vectors per node
   - SearchEngine indexes NodeInfo vec + embeddings
4. MCP: handlers.rs dispatches tool calls using PDG + SearchEngine
```

### 1.3 Key Source Files

| File | Lines | Role |
|------|-------|------|
| `src/cli/leindex.rs` | ~2600 | Core LeIndex struct — indexing, search, embedding |
| `src/cli/mcp/handlers.rs` | ~5170 | All 19 MCP tool handlers |
| `src/cli/registry.rs` | ~550 | Multi-project registry with LRU eviction |
| `src/graph/pdg.rs` | ~1490 | PDG data structure (petgraph wrapper) |
| `src/graph/extraction.rs` | ~1750 | PDG construction from signatures |
| `src/search/search.rs` | ~1220 | SearchEngine with vector index |
| `src/search/ranking.rs` | ~180 | Hybrid scoring (semantic + structural + text) |
| `src/parse/rust.rs` | ~1065 | Rust language parser |

### 1.4 Complexity Computation

**Location**: `src/graph/extraction.rs:1560`

```rust
fn signature_to_node(sig: &SignatureInfo, file_path: &str, language: &str) -> Node {
    let complexity = 1u32 + sig.parameters.len() as u32;
    // ...
}
```

For class nodes: `complexity = method_count` (line 146).

**This is NOT cyclomatic complexity.** It is `1 + parameter_count` for functions and `method_count` for classes.

### 1.5 Caller/Callee Graph

**Edge extraction**: `src/graph/extraction.rs:857` — `extract_call_edges()` resolves `sig.calls` entries against a resolution map (exact → suffix → last-segment → namespace → alias).

**Call extraction for Rust**: `src/parse/rust.rs:385` — `extract_rust_calls()` walks tree-sitter AST looking for `call_expression` and `macro_invocation` nodes.

**Caller count**: `src/cli/mcp/handlers.rs:1185` — `get_direct_callers()` uses `pdg.predecessors(target_id)` which is `petgraph::neighbors_directed(Incoming)`.

### 1.6 Semantic Search / Embedding

**Embedding generation**: `src/cli/leindex.rs:182-320` — `TfIdfEmbedder`
- Builds corpus from all node content
- Computes document frequency per token
- Filters to tokens with `min_df..max_df` (moderate frequency)
- Selects top-768 tokens by stratified IDF sampling
- Embeds each node as TF-IDF weighted 768-dim vector, L2-normalized

**Query embedding**: `src/cli/leindex.rs:2032` — Uses same TfIdfEmbedder to embed query text.

**Fallback**: `src/cli/leindex.rs:2045` — `generate_deterministic_embedding()` uses hash-based random vectors when embedder is unavailable.

**Scoring**: `src/search/ranking.rs:55-59` — Default weights: `semantic=0.5, structural=0.1, text=0.4`

---

## Part 2 — Issue Mapping: Experienced Behavior → Suspected Root Causes

### Issue 1: `caller_count: 0` on major structs

**Observed**: `DeepThoughtManager`, `RuVectorIndex`, `AROOptimizer`, `CompactionEngine` all returned `caller_count: 0` despite being used throughout the Radis codebase.

**Root Cause**: The call graph extraction (`extract_rust_calls` in `parse/rust.rs:385-453`) only extracts direct function/method call expressions from tree-sitter AST. It does **not** extract:
- Struct instantiation (`Foo::new()` — may resolve as a call to `new`, not to `Foo`)
- Type references in signatures (`fn bar(x: DeepThoughtManager)`)
- `use` statement consumers
- Trait implementations (`impl Trait for Struct`)
- Generic type parameters

The symbol resolution in `extract_call_edges` uses the `sig.calls` list, so if `DeepThoughtManager` is used via `DeepThoughtManager::new()`, the call resolves to `new` not to `DeepThoughtManager`. The struct node itself has no incoming call edges.

**File**: `src/parse/rust.rs:385-453`, `src/graph/extraction.rs:857-970`

### Issue 2: `complexity: 1` on all function nodes

**Observed**: Every function/method showed `complexity: 1` in `grep_symbols` results, providing no differentiation.

**Root Cause**: `src/graph/extraction.rs:1560` computes complexity as `1 + sig.parameters.len()`. For zero-param functions (common in Rust: `fn new() -> Self`), complexity = 1. For single-param functions, complexity = 2. This is a parameter count, not cyclomatic complexity.

The Rust parser does extract a `ComplexityMetrics` struct (`parse/rust.rs:246`) but it is stored in the `SignatureInfo` and **never consumed** by the PDG node construction. The PDG `Node.complexity` field is overwritten with the parameter-count heuristic.

**File**: `src/graph/extraction.rs:1560`, `src/parse/rust.rs:246`

### Issue 3: `ColdStoreRepository` returned zero results from `grep_symbols`

**Observed**: A known struct with real implementations returned 0 matches.

**Root Cause Chain**:
1. `grep_symbols` handler (handlers.rs:2050-2135) first does a semantic search pre-filter via `index.search(&pattern, ...)`, then matches results against the PDG
2. If the TF-IDF embedding vocabulary doesn't contain tokens from the query term (e.g. "ColdStoreRepository" tokenized as ["cold", "store", "repository"]), the semantic search returns nothing
3. The fallback direct PDG scan (handlers.rs:2139-2215) only activates if `all_matches.is_empty() || pattern contains regex chars`
4. The PDG node for `ColdStoreRepository` uses the fully-qualified ID `file_path:ColdStoreRepository`. The `grep_symbols` match checks `node.name.to_lowercase().contains(&pattern_lower)` — so if the node's `.name` is the short name and the query matches, it should work
5. But if the search pre-filter returns partial results for other nodes, the fallback scan doesn't trigger, and the target struct was missed by the pre-filter

**Most likely**: The semantic pre-filter returned results for *other* nodes (enough to not be empty), but the target node was not among them. The fallback scan only triggers on empty results or regex patterns.

**File**: `src/cli/mcp/handlers.rs:2050-2215`

### Issue 4: `language: "external"` on many results

**Observed**: Many `grep_symbols` results showed `language: "external"` and `byte_range: [0, 0]` instead of real source locations.

**Root Cause**: The PDG creates synthetic nodes for external/imported symbols that are referenced but not defined in the project. These are created during edge extraction when a callee symbol can't be resolved to an existing node. The Rust parser extracts `use` statements as `ImportInfo`, and some of these get promoted to nodes.

These external reference nodes have `byte_range: (0, 0)` and `language: "external"` because they have no source location — they represent symbols from other crates.

**File**: `src/graph/extraction.rs` (import node creation), `src/graph/external_deps.rs`

### Issue 5: File-level `total_complexity` in `project_map` was useful but misleading

**Observed**: `project_map` showed meaningful-looking complexity scores (e.g., 307, 216, 154) per file, but these are sums of `1 + param_count` across all functions in the file, not real complexity.

**Root Cause**: Same as Issue 2 — the per-file complexity is the sum of `(1 + param_count)` for each function, which happens to correlate roughly with file size/function count but is not cyclomatic complexity.

**File**: `src/cli/mcp/handlers.rs:1846-1889`

### Issue 6: `leindex_search` (semantic search) quality is poor for conceptual queries

**Observed**: Searching for concepts like "authentication" or "memory management" returns noisy or irrelevant results.

**Root Cause**: The semantic search uses TF-IDF with 768-dim vectors, not LLM-generated embeddings. The TF-IDF embedder:
- Has moderate-frequency vocabulary filtering (min_df=N/1000, max_df=N/4)
- Uses stratified IDF sampling across the frequency range
- This means the vocabulary is a representative sample of ~768 tokens from the codebase
- Conceptual queries that use terms not in the vocabulary get zero-vector projections for those dimensions
- The scoring weights (semantic=0.5, text=0.4) mean semantic similarity dominates but is often zero or noisy

**File**: `src/cli/leindex.rs:182-320`, `src/search/ranking.rs:55-59`

---

## Part 3 — Recommendations

### 3.1 CRITICAL — Fix Complexity Computation

**Current**: `complexity = 1 + param_count`
**Problem**: Provides no useful signal for code understanding
**Fix**: Use the already-extracted `ComplexityMetrics` from the parser. The Rust parser (`parse/rust.rs:246`) already computes a tree-sitter based complexity metric. Wire it through to `Node.complexity` in `extraction.rs:1560`.

**Files to change**: `src/graph/extraction.rs:1554-1570`, `src/parse/traits.rs` (ensure `ComplexityMetrics` is on `SignatureInfo`)

### 3.2 CRITICAL — Fix Caller/Callee Resolution for Rust

**Current**: Only function call expressions are extracted. Struct instantiation, type references, and trait impl references are not tracked.
**Problem**: Major types show `caller_count: 0` despite being heavily used
**Fix Options** (increasing effort):
1. **Quick**: Also count `use` statement references as "callers" of the imported symbol
2. **Medium**: Add struct instantiation tracking — when `Foo::new()` is seen, add an edge to both `Foo::new` and `Foo`
3. **Full**: Add type-reference edges for function parameters and return types that reference known structs/traits

**Files to change**: `src/parse/rust.rs:385-453`, `src/graph/extraction.rs:857-970`

### 3.3 HIGH — Fix `grep_symbols` Fallback Logic

**Current**: Fallback PDG scan only triggers when semantic pre-filter returns empty or query contains regex chars
**Problem**: When pre-filter returns partial results for wrong nodes, the target is missed and fallback never runs
**Fix**: Always merge PDG scan results with semantic pre-filter results, deduplicating by node ID. The PDG scan is already O(N) with early termination — merging it ensures no symbols are missed.

**Files to change**: `src/cli/mcp/handlers.rs:2139-2215`

### 3.4 HIGH — Filter External Nodes from grep_symbols Results

**Current**: External reference nodes (language: "external", byte_range: [0,0]) are returned alongside real nodes
**Problem**: Pollutes results with unresolvable phantom entries
**Fix**: Add a filter in `grep_symbols` handler to exclude nodes where `node.language == "external"` or `node.byte_range == (0, 0)` unless the user explicitly requests them.

**Files to change**: `src/cli/mcp/handlers.rs:2080-2100`

### 3.5 MEDIUM — Improve Semantic Search Quality

**Current**: TF-IDF with 768-dim vectors, no LLM embeddings
**Problem**: Conceptual queries get poor results
**Fix Options**:
1. **Minimal**: Add a text-match boost path — when the query exactly substring-matches a node name or content, boost that result regardless of semantic score
2. **Medium**: Switch default HybridScorer weights for `search_mode: "code"` to `text=0.6, semantic=0.2, structural=0.2` to de-weight the weak semantic signal
3. **Full**: Integrate a local embedding model (e.g., ONNX runtime with a small code embedding model) for real semantic vectors

**Files to change**: `src/search/ranking.rs:55-59`, `src/cli/mcp/handlers.rs` (search handler)

### 3.6 MEDIUM — Expose Real Cyclomatic Complexity in Node Data

The `parse/rust.rs:246` `extract_complexity` method already walks the tree-sitter AST. Ensure it counts:
- `if`, `else if`, `match` arms, `while`, `for`, `loop`
- `&&`, `||` (boolean operators)
- `?` (try operator)
- `unwrap()`, `expect()` calls

Then store the result as `Node.complexity` instead of the parameter count heuristic.

**Files to change**: `src/parse/rust.rs:246-258`, `src/graph/extraction.rs:1554-1570`

### 3.7 LOW — Add Symbol Kind Filtering for External Nodes

Add a `node_type` variant for `External` or a flag `is_external: bool` on `Node` so handlers can distinguish real definitions from phantom references without string-matching on the language field.

**Files to change**: `src/graph/pdg.rs` (Node struct or NodeType enum)

### 3.8 LOW — Axum Version Unification

The codebase runs two axum stacks simultaneously: axum 0.6 (aliased as `axum-06`) for MCP transport and axum 0.7 for the HTTP server. This doubles the dependency surface and complicates maintenance. The `Cargo.toml` already documents the unification plan (lines 256-263).

**Files to change**: `Cargo.toml`, `src/cli/mcp/server.rs`, `src/cli/mcp/sse.rs`

---

## Part 4 — Priority Matrix

| # | Issue | Severity | Effort | Impact on Radis |
|---|-------|----------|--------|-----------------|
| 3.1 | Complexity computation | CRITICAL | Low | Every `project_map` and `grep_symbols` call returns misleading data |
| 3.2 | Caller/callee resolution | CRITICAL | Medium | Core value prop of PDG is broken for struct/type nodes |
| 3.3 | grep_symbols fallback | HIGH | Low | Known symbols return zero results |
| 3.4 | Filter external nodes | HIGH | Low | Noise in search results |
| 3.5 | Semantic search quality | MEDIUM | Low-High | Poor conceptual search |
| 3.6 | Real cyclomatic complexity | MEDIUM | Medium | Unlocks meaningful complexity analysis |
| 3.7 | External node type | LOW | Low | Cleaner API |
| 3.8 | Axum unification | LOW | Medium | Maintenance only |

---

## Part 5 — Summary

The core architecture of LeIndex is sound — tree-sitter parsing, petgraph-backed PDG, SQLite storage, and a clean MCP handler dispatch pattern. The weaknesses are concentrated in three areas:

1. **Graph fidelity**: The PDG captures function-call edges but misses type-reference, struct-instantiation, and trait-impl edges. This makes `caller_count` unreliable for anything that isn't a direct function call.

2. **Metric accuracy**: The `complexity` field is a parameter-count proxy, not cyclomatic complexity. The parser already extracts real complexity data but it's discarded during PDG construction.

3. **Search reliability**: The `grep_symbols` handler's two-phase design (semantic pre-filter → fallback PDG scan) has a logic gap where the fallback doesn't trigger when the pre-filter returns partial results. This causes known symbols to return zero hits.

All three issues are localized and fixable. The highest-leverage fix is #3.3 (grep_symbols fallback) — a ~10-line change that eliminates the zero-result failure mode.

---

## Part 6 — Recommended Fixes (Implementation Guidance)

### Fix 3.1 — Wire Real Complexity Into PDG Nodes

**File**: `src/graph/extraction.rs`

Replace the complexity computation at line 1560:

```rust
// BEFORE (parameter-count proxy):
let complexity = 1u32 + sig.parameters.len() as u32;

// AFTER (use parser-extracted complexity):
let complexity = if sig.complexity_metrics.cyclomatic > 0 {
    sig.complexity_metrics.cyclomatic as u32
} else {
    // Fallback: 1 + branch_count + param_count when cyclomatic not available
    1u32 + sig.complexity_metrics.branches as u32 + sig.parameters.len() as u32
};
```

**Prerequisite**: Ensure `SignatureInfo` carries a `complexity_metrics: ComplexityMetrics` field. The Rust parser (`parse/rust.rs:246`) already computes `extract_complexity()` but its result may not be stored on `SignatureInfo`. If not, add:

```rust
// In src/parse/traits.rs, add to SignatureInfo:
pub complexity_metrics: ComplexityMetrics,

// In src/parse/rust.rs extract_function_signature(), add:
let metrics = self.extract_complexity(node);
// ... then set complexity_metrics: metrics in the returned SignatureInfo
```

For class nodes at line 146, keep `method_count` as complexity or switch to sum of method complexities:

```rust
// BEFORE:
complexity: method_nids.len() as u32,

// AFTER (sum of member complexities):
complexity: method_nids.iter()
    .filter_map(|&nid| pdg.get_node(nid))
    .map(|n| n.complexity)
    .sum::<u32>()
    .max(1),
```

### Fix 3.2 — Add Struct Instantiation and Type-Reference Edges for Rust

**File**: `src/parse/rust.rs`

Extend `extract_rust_calls()` to also capture struct instantiation paths:

```rust
fn extract_rust_calls(node: &tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            // Existing: function/method calls
            "call_expression" => { /* existing logic */ }
            "macro_invocation" => { /* existing logic */ }

            // NEW: struct instantiation — `Foo { ... }` or `Foo::new()`
            "struct_expression" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source) {
                        calls.push(name.to_string());
                    }
                }
            }

            // NEW: path expressions with :: that reference known types
            // e.g., `DeepThoughtManager::new()` → extract "DeepThoughtManager"
            "call_expression" if child.child(0).map(|c| c.kind()) == Some("scoped_identifier") => {
                if let Some(scope) = child.child(0) {
                    if let Some(path_node) = scope.child_by_field_name("path") {
                        if let Ok(path) = path_node.utf8_text(source) {
                            calls.push(path.to_string());
                        }
                    }
                }
            }

            _ => {
                // Recurse into child nodes
                let inner = extract_rust_calls(&child, source);
                calls.extend(inner);
            }
        }
    }

    calls
}
```

**File**: `src/graph/extraction.rs`

In `extract_call_edges()`, when resolving a callee name that matches a class/struct node, add the edge to the struct node as well as any `::method` node:

```rust
// After resolving callee_id, also check if the callee name matches a class node:
if let Some(&struct_nid) = node_ids.get(&callee_name) {
    let pair = (caller_id, struct_nid);
    if !seen.contains(&pair) {
        seen.insert(pair);
        edges.push(pair);
    }
}
```

### Fix 3.3 — Always Merge PDG Scan with Semantic Pre-Filter

**File**: `src/cli/mcp/handlers.rs`

Replace the conditional fallback at lines 2139-2143:

```rust
// BEFORE:
let use_direct_scan = all_matches.is_empty()
    || pattern.contains('|')
    || pattern.contains('*')
    || pattern.contains('?')
    || pattern.contains('[');

if use_direct_scan && all_matches.len() < fetch_limit {

// AFTER (always merge):
if all_matches.len() < fetch_limit {
```

This removes the condition that suppresses the PDG scan when semantic pre-filter returns partial results. The `seen_ids` HashSet already handles deduplication, so merging is safe.

### Fix 3.4 — Filter External Nodes from grep_symbols

**File**: `src/cli/mcp/handlers.rs`

Add a filter after the node lookup in both the pre-filter path (~line 2080) and the direct scan path (~line 2156):

```rust
// Add after: let node = match pdg.get_node(nid) { ... };
// In BOTH the pre-filter loop and the direct-scan loop:

if node.byte_range == (0, 0) && node.language == "external" {
    continue;
}
```

Alternatively, add an `include_external` parameter (default `false`) so users can opt-in:

```rust
let include_external = extract_bool(&args, "include_external", false);

// Then in the filter:
if !include_external && node.language == "external" {
    continue;
}
```

### Fix 3.5 — Improve Search Scoring Defaults

**File**: `src/search/ranking.rs`

Change the default `HybridScorer` weights to de-emphasize the TF-IDF semantic signal:

```rust
// BEFORE:
impl HybridScorer {
    pub fn new() -> Self {
        Self {
            semantic_weight: 0.5,
            structural_weight: 0.1,
            text_weight: 0.4,
        }
    }
}

// AFTER:
impl HybridScorer {
    pub fn new() -> Self {
        Self {
            semantic_weight: 0.25,
            structural_weight: 0.15,
            text_weight: 0.60,
        }
    }
}
```

This makes text matching dominant (which is actually more reliable with TF-IDF) and gives structural signal more weight than the noisy semantic component.

Also add a constructor for search-mode-specific weights:

```rust
/// Create scorer tuned for code search (text-dominant)
pub fn for_code() -> Self {
    Self {
        semantic_weight: 0.25,
        structural_weight: 0.15,
        text_weight: 0.60,
    }
}

/// Create scorer tuned for prose/natural-language search (semantic-dominant)
pub fn for_prose() -> Self {
    Self {
        semantic_weight: 0.50,
        structural_weight: 0.10,
        text_weight: 0.40,
    }
}
```

### Fix 3.6 — Real Cyclomatic Complexity in Rust Parser

**File**: `src/parse/rust.rs`

Extend `extract_complexity()` to count real branch points:

```rust
fn extract_complexity(&self, node: &tree_sitter::Node<'_>) -> ComplexityMetrics {
    let mut cyclomatic = 1u32; // Base complexity
    let mut cursor = node.walk();

    fn walk_complexity(node: &tree_sitter::Node<'_>, cyclomatic: &mut u32) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                // Branch points
                "if_expression" | "else_clause" |
                "while_expression" | "for_expression" | "loop_expression" |
                "match_arm" |  // each arm is a branch
                "binary_expression" => {
                    // Check for && and || operators
                    if let Some(op) = child.child_by_field_name("operator") {
                        let op_text = op.kind();
                        if op_text == "&&" || op_text == "||" {
                            *cyclomatic += 1;
                        }
                    } else {
                        *cyclomatic += 1;
                    }
                }
                "try_expression" => { // ? operator
                    *cyclomatic += 1;
                }
                _ => {}
            }
            walk_complexity(&child, cyclomatic);
        }
    }

    walk_complexity(node, &mut cyclomatic);

    ComplexityMetrics {
        cyclomatic,
        cognitive: cyclomatic, // Simplified; cognitive complexity needs nesting depth tracking
        branches: cyclomatic.saturating_sub(1),
        ..Default::default()
    }
}
```

---

## Part 7 — General System Improvements

### 7.1 Add `node_type: External` Variant

**File**: `src/graph/pdg.rs`

```rust
pub enum NodeType {
    Function,
    Method,
    Class,
    Module,
    Variable,
    External,  // NEW: imported/referenced but not defined in this project
}
```

Then update `extraction.rs` to use `NodeType::External` instead of setting `language: "external"` as a string marker. This gives handlers a type-safe way to filter.

### 7.2 Add Symbol Deduplication in grep_symbols

Currently `grep_symbols` can return the same logical symbol multiple times if it appears as both a `pub use` re-export and a definition. Add deduplication by `node.name + node.file_path`:

```rust
// In handlers.rs grep_symbols, change seen_ids to also track name+file:
let dedup_key = format!("{}:{}", node.name, node.file_path);
if seen_ids.contains(&dedup_key) { continue; }
seen_ids.insert(dedup_key);
```

### 7.3 Add Batch Symbol Lookup

The `symbol_lookup` handler currently supports `symbols: Vec<String>` for batch lookups, but internally it loops sequentially. For large batches, pre-compute a name→NodeId map once and resolve all symbols in O(k) instead of O(k*N):

```rust
// Pre-build lookup map once:
let name_map: HashMap<&str, Vec<NodeId>> = pdg.node_indices()
    .filter_map(|nid| pdg.get_node(nid).map(|n| (n.name.as_str(), nid)))
    .fold(HashMap::new(), |mut m, (name, nid)| {
        m.entry(name).or_default().push(nid);
        m
    });

// Then resolve each symbol against the map
```

### 7.4 Cache PDG Statistics

`project_map` and `file_summary` recompute per-file statistics on every call by iterating the full PDG. Cache these as a `HashMap<String, FileStats>` on `LeIndex` and invalidate on reindex:

```rust
struct FileStats {
    symbol_count: usize,
    total_complexity: u32,
    symbol_names: Vec<String>,
}
```

### 7.5 Add Index Freshness Signal to Tool Responses

When the index is stale (`is_stale_fast()` returns true), include a warning in tool responses so the AI consumer knows results may be outdated:

```rust
if index.is_stale_fast() {
    result["_warning"] = Value::String(
        "Index may be stale. Call leindex_index with force_reindex=true for fresh results.".into()
    );
}
```

### 7.6 Structured Error Responses for Missing Symbols

When `grep_symbols` or `symbol_lookup` finds zero results, return structured diagnostic info instead of just an empty array:

```rust
if matches.is_empty() {
    return Ok(serde_json::json!({
        "results": [],
        "total_matched": 0,
        "suggestion": format!(
            "No symbols matching '{}' found. Try: broader pattern, check spelling, or run leindex_text_search for raw text matches.",
            pattern
        ),
        "indexed_symbol_count": pdg.node_count(),
        "indexed_file_count": pdg.file_count()
    }));
}
```

### 7.7 Add `include_source` to grep_symbols

Currently `grep_symbols` supports `include_context_lines` which reads a fixed number of lines. Add an `include_source: bool` option that returns the full symbol source (like `read_symbol` does) directly in the result. This eliminates a common two-tool round-trip pattern.
