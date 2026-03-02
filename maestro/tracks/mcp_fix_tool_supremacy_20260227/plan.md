# Plan: MCP Fix & LeIndex Tool Supremacy

## Track Metadata
- **Track ID:** `mcp_fix_tool_supremacy_20260227`
- **Status:** NEW
- **Created:** 2026-02-27
- **Phases:** 5
- **Tasks:** 28
- **Primary Goal:** Fix MCP transport + embeddings, then build LeIndex into the complete LLM toolset for code navigation, understanding, and editing.

---

## Phase A: MCP Stdio Transport Fix

### [x] Task A.1: Fix double-newline in stdio response writer

**Files:** `crates/lepasserelle/src/cli.rs`

**Root Cause:** `writeln!` already appends `\n`. The format string `"{}\n"` produces double-newline. Claude Code's line-based JSON-RPC reader parses the blank line as JSON → `Unexpected EOF` → kills connection at 0ms.

**Exact changes (2 lines):**

Line ~676 (error response path):
```rust
// BEFORE:
} else if writeln!(stdout, "{}\n", response).is_err() {
// AFTER:
} else if writeln!(stdout, "{}", response).is_err() {
```

Line ~724 (normal response path):
```rust
// BEFORE:
} else if writeln!(stdout, "{}\n", response_json).is_err() {
// AFTER:
} else if writeln!(stdout, "{}", response_json).is_err() {
```

**Tests:**
- [ ] Build passes: `cargo build --release -p lepasserelle`
- [ ] Manual pipe test produces no blank lines between JSON responses
- [ ] `xxd` output of spy log shows single `0a` (not `0a 0a`) after each JSON message

- [x] Task A.1 complete — 34c9ac0

---

### Task A.2: Verify MCP connection in Claude Code

**Verification steps:**
1. Install: `cp target/release/leindex $(which leindex)`
2. Start fresh Claude Code session
3. `/mcp` panel shows leindex as "connected"
4. Ask "what leindex tools do you have?" — should list all 7 tools
5. `~/.claude/debug/latest` no longer shows `STDIO connection dropped after 0s uptime`

**Optional wire-level spy:**
```bash
cat > /tmp/leindex-spy << 'EOF'
#!/bin/bash
tee /tmp/mcp-spy-stdin-$$.log | leindex mcp 2>/tmp/mcp-spy-stderr-$$.log \
  | tee /tmp/mcp-spy-stdout-$$.log
EOF
chmod +x /tmp/leindex-spy
# Point ~/.claude.json command at /tmp/leindex-spy, verify no 0a 0a
```

- [ ] Task A.2 complete
- [ ] Task: Maestro - User Manual Verification 'Phase A' (Protocol in workflow.md)

---

## Phase B: Semantic Embedding Fix

### [x] Task B.1: Implement TF-IDF embedding system

**Files:**
- `crates/lepasserelle/src/leindex.rs` — new `TfIdfEmbedder` struct, replace `generate_deterministic_embedding()`

**Root Cause:** `leindex.rs:1077-1107` uses `DefaultHasher` to hash only the symbol name. Produces pseudorandom 768-dim vectors. Cosine similarity ≈ 0.0 for all pairs.

**Implementation — new struct on `LeIndex`:**
```rust
struct TfIdfEmbedder {
    idf: HashMap<String, f32>,       // IDF values by token
    vocab: Vec<String>,              // Ordered vocabulary (top-K by IDF)
    dimension: usize,                // 768 to match existing vector index
}

impl TfIdfEmbedder {
    fn build(documents: &[(String, String)]) -> Self {
        // 1. Tokenize each document via tokenize_code()
        // 2. Build document-frequency table
        // 3. Compute IDF = log(N / df) per token
        // 4. Select top-768 tokens by IDF as vocabulary
        // 5. Store ordered vocab for consistent vector indexing
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        // 1. Tokenize text via tokenize_code()
        // 2. Compute TF per vocab token
        // 3. TF * IDF for each dimension
        // 4. L2-normalize
    }
}

fn tokenize_code(text: &str) -> Vec<String> {
    // Split on whitespace + punctuation
    // Split camelCase: "getUserName" → ["get", "user", "name"]
    // Split snake_case: "get_user_name" → ["get", "user", "name"]
    // Lowercase all, filter len < 2
}
```

**Integration points:**
1. Add `embedder: Option<TfIdfEmbedder>` field to `LeIndex` struct
2. `index_nodes()` (~line 1110): After collecting all nodes, build TfIdfEmbedder from node content, then use `embedder.embed()` instead of `generate_deterministic_embedding()`
3. `generate_query_embedding()` (~line 1065): Use `embedder.embed(query)` when embedder is available, fall back to deterministic for edge cases
4. Store `TfIdfEmbedder` as field on `LeIndex` so it persists between `index_nodes()` and `search()`

**Tests:**
- [ ] `tokenize_code("getUserName")` → `["get", "user", "name"]`
- [ ] `tokenize_code("get_user_name")` → `["get", "user", "name"]`
- [ ] Two related code snippets produce embedding cosine similarity > 0.3
- [ ] Two unrelated code snippets produce embedding cosine similarity < 0.2
- [ ] `TfIdfEmbedder::build()` handles empty document set without panic
- [ ] Generated embeddings have dimension 768
- [ ] Generated embeddings are L2-normalized (magnitude ≈ 1.0)

- [x] Task B.1 complete — c377bcf

---

### Task B.2: Verify semantic search produces non-zero scores

**Steps:**
```bash
leindex index /path/to/project --force
leindex search "authentication credential refresh" -p /path/to/project
```

**Expected:** `semantic` field is non-zero for semantically related results. Exploratory queries with no exact text matches should still return relevant results via semantic similarity.

**Acceptance criteria:**
- [ ] `semantic_score > 0.0` for at least 3 results on a related query
- [ ] Results for exploratory queries include semantically relevant nodes even with zero `text_match`

- [ ] Task B.2 complete
- [ ] Task: Maestro - User Manual Verification 'Phase B' (Protocol in workflow.md)

---

## Phase C: Tool Supremacy — Read/Grep/Glob Replacement

### Design Principles (apply to ALL Phase C tasks)
1. **Every tool response is self-contained** — no follow-up `Read` call needed
2. **`token_budget` parameter on every tool** — LLM controls context consumption
3. **Cross-file awareness is the differentiator** — standard tools operate on single files; LeIndex shows relationships
4. **Structured JSON responses** — clear sections, not wall-of-text dumps
5. **Tool descriptions state the advantage** — "5-10x more token efficient than reading the file"

### Registration pattern (apply to ALL new handlers)

**1. `handlers.rs` — add to `ToolHandler` enum + match arms:**
```rust
pub enum ToolHandler {
    // ... existing ...
    FileSummary(FileSummaryHandler),
    SymbolLookup(SymbolLookupHandler),
    ProjectMap(ProjectMapHandler),
    GrepSymbols(GrepSymbolsHandler),
    ReadSymbol(ReadSymbolHandler),
}
// Add arms to name(), description(), argument_schema(), execute()
```

**2. `server.rs` — register in `McpServer::new()` handlers vec**

**3. `cli.rs` — ensure stdio `handle_mcp_request()` dispatches `"tools/call"` to all handlers (already generic via handler list)**

---

### [x] Task C.1: Implement `leindex_file_summary` handler (replaces Read)

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Purpose:** Given a file path, return a comprehensive structured analysis — all symbols, their signatures, complexity, cross-file deps/dependents, import/export maps, module role. Returns everything an LLM needs to understand the file WITHOUT reading raw content.

**Description for MCP tool listing:**
```
"Get a comprehensive structural analysis of a file: all symbols with signatures, complexity scores, cross-file dependencies and dependents, import/export maps, and module role summary. Returns everything needed to understand a file without reading its raw content — typically 5-10x more token efficient than Read. Includes cross-file relationship information that Read cannot provide."
```

**Input schema:**
- `file_path` (string, required): Absolute path to the file
- `token_budget` (integer, default 1000): Max tokens for response
- `include_source` (boolean, default false): Include source snippets for key symbols
- `focus_symbol` (string, optional): Focus analysis on a specific symbol

**Response structure:**
```json
{
  "file_path": "/abs/path/file.rs",
  "language": "rust",
  "line_count": 450,
  "symbols": [
    {
      "name": "UserAuth",
      "type": "struct",
      "line_range": [15, 28],
      "signature": "pub struct UserAuth { ... }",
      "complexity": 3,
      "dependencies": ["TokenStore", "SessionManager"],
      "dependents": ["LoginHandler", "ApiMiddleware"],
      "cross_file_refs": [
        {"symbol": "TokenStore", "file": "src/token.rs", "relationship": "field_type"}
      ]
    }
  ],
  "imports": {"internal": [...], "external": [...]},
  "module_role": "Authentication core — defines UserAuth struct used by 4 handler files"
}
```

**Implementation approach:**
1. Lock leindex, get PDG reference
2. Iterate all PDG nodes where `node.file_path` matches the requested file
3. For each node: traverse outgoing edges (Call, Import, DataDependency) → dependencies
4. For each node: traverse incoming edges → dependents
5. Collect cross-file refs from edges where source/target file_path differs
6. Build import lists from Import-type edges
7. Synthesize `module_role` from node types + edge counts
8. If `focus_symbol` set, prioritize that symbol's context
9. Truncate to `token_budget` using `TokenFormatter::truncate()`

**Tests:**
- [ ] Returns correct symbols for a known indexed file
- [ ] Cross-file references populated when edges exist
- [ ] `focus_symbol` filters response to focused symbol
- [ ] `token_budget` respected (response fits within budget)
- [ ] Returns error for non-existent file path

- [x] Task C.1 complete — c8a508f

---

### [x] Task C.2: Implement `leindex_symbol_lookup` handler (replaces Grep)

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Purpose:** Find a symbol across the project — definition, signature, full call graph (callers + callees), data dependencies, and transitive impact radius.

**Description:**
```
"Look up any symbol (function, class, method, variable) and get its full structural context: definition location, signature, callers, callees, data dependencies, and impact radius showing how many symbols and files would be affected by changes. Replaces Grep + multiple Read calls with a single structured response."
```

**Input schema:**
- `symbol` (string, required): Symbol name to look up
- `token_budget` (integer, default 1500)
- `include_source` (boolean, default false): Include source code of definition
- `include_callers` (boolean, default true)
- `include_callees` (boolean, default true)
- `depth` (integer, default 2, min 1, max 5): Call graph traversal depth

**Implementation approach:**
1. `pdg.find_by_symbol(symbol)` for exact match
2. If not found: fuzzy match — iterate all nodes, substring + case-insensitive match on `node.name` and `node.id`
3. Traverse outgoing Call edges for callees (up to `depth` levels)
4. Traverse incoming Call edges for callers (up to `depth` levels)
5. Traverse DataDependency edges for data deps
6. Use `pdg.get_forward_impact(node_id)` for transitive impact
7. If `include_source`: read source bytes from disk using `node.byte_range`
8. Truncate to `token_budget`

**Tests:**
- [ ] Exact match returns correct symbol with file, line range, signature
- [ ] Fuzzy match finds symbol when exact match fails
- [ ] Callers and callees populated from PDG edges
- [ ] Impact radius counts transitive dependents
- [ ] Returns structured error when symbol not found anywhere

- [x] Task C.2 complete — c8a508f

---

### [x] Task C.3: Implement `leindex_project_map` handler (replaces Glob/ls)

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Purpose:** Return the project's file structure annotated with module roles, symbol counts, complexity hotspots, and inter-module dependency arrows.

**Description:**
```
"Get an annotated project structure map showing files, directories, symbol counts, complexity hotspots, and inter-module dependency arrows. Unlike Glob which returns flat file lists, this shows the project's architecture — which modules depend on which, where complexity lives, and what the entry points are."
```

**Input schema:**
- `path` (string, optional): Subdirectory to scope to (default: project root)
- `depth` (integer, default 3, min 1, max 10): Tree depth
- `token_budget` (integer, default 2000)
- `sort_by` (string, enum: complexity/name/dependencies/size, default complexity)
- `include_symbols` (boolean, default false): Include top symbols per file

**Implementation approach:**
1. Get file inventory from phase analysis context or walk project directory
2. For each file: look up PDG nodes → symbol count, total complexity, language
3. Aggregate per-directory: total complexity, symbol count
4. For dependency arrows: aggregate Import/Call edges between files in different directories
5. Sort per `sort_by`
6. Truncate tree at `depth` and trim to `token_budget`

**Tests:**
- [ ] Returns tree with correct directory structure
- [ ] Symbol counts match PDG node counts per file
- [ ] `depth` parameter limits tree depth
- [ ] `sort_by=complexity` puts high-complexity files first
- [ ] `token_budget` respected

- [x] Task C.3 complete — c8a508f

---

### [x] Task C.4: Implement `leindex_grep_symbols` handler (replaces Grep)

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Purpose:** Search for symbols/patterns across the indexed codebase with structural awareness. Results include symbol type, dependency graph role, and related context.

**Description:**
```
"Search for symbols and patterns across the indexed codebase with structural awareness. Unlike text-based grep, results include each match's type (function/class/method), its role in the dependency graph, and related symbols. Supports exact match, substring, and semantic search."
```

**Input schema:**
- `pattern` (string, required): Symbol name, substring, or natural language query
- `scope` (string, optional): Limit to file/directory path
- `type_filter` (string, enum: function/class/method/variable/module/all, default all)
- `token_budget` (integer, default 1500)
- `include_context_lines` (integer, default 0, min 0, max 10): Source context lines
- `max_results` (integer, default 20, min 1, max 100)

**Implementation approach:**
1. First: exact symbol name match via `pdg.find_by_symbol(pattern)`
2. Second: substring match across all `node.name` and `node.id` fields
3. Third: semantic search via `leindex.search(pattern, max_results)`
4. Deduplicate across strategies
5. Filter by `type_filter` (match against `node.node_type`)
6. Filter by `scope` (prefix match on `node.file_path`)
7. For each result: include file, line range, node type, complexity, signature
8. If `include_context_lines > 0`: read source lines from file
9. Truncate to `token_budget`

**Tests:**
- [ ] Exact match returns correct result
- [ ] Substring match finds partial name matches
- [ ] `type_filter=function` only returns function nodes
- [ ] `scope` limits results to specified path
- [ ] Semantic fallback finds results when exact/substring fail
- [ ] Results deduplicated across search strategies

- [x] Task C.4 complete — c8a508f

---

### [x] Task C.5: Implement `leindex_read_symbol` handler (replaces targeted Read)

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Purpose:** Read the source code of a specific symbol with structural context — doc comment, signature, and dependency signatures. Reads exactly what the LLM needs, not an entire file.

**Description:**
```
"Read the source code of a specific symbol along with its doc comment, signature, and the signatures of its dependencies and dependents. Reads exactly what you need instead of an entire file — far more token efficient for targeted understanding."
```

**Input schema:**
- `symbol` (string, required): Symbol to read source for
- `file_path` (string, optional): Disambiguate when symbol exists in multiple files
- `include_dependencies` (boolean, default true): Include dependency signatures
- `token_budget` (integer, default 2000)

**Implementation approach:**
1. Find symbol in PDG → get `file_path` and `byte_range`
2. Read source bytes from disk using byte_range: `std::fs::read(&file_path)` then slice
3. Extract doc comment: read lines above byte_range start looking for `///` or `/** */`
4. Traverse outgoing edges → collect dependency signatures (read their first line / signature from byte_range)
5. Traverse incoming edges → collect dependent signatures
6. Cap total output to `token_budget`

**Tests:**
- [ ] Returns source code for a known symbol
- [ ] Doc comment extracted correctly
- [ ] Dependency signatures included when `include_dependencies=true`
- [ ] `file_path` disambiguates when symbol exists in multiple files
- [ ] Returns error for symbol not found

- [x] Task C.5 complete — c8a508f

---

### Task C.6: Enhance existing `leindex_search` response enrichment

**Files:**
- `crates/lerecherche/src/search.rs` — add fields to `SearchResult`
- `crates/lepasserelle/src/leindex.rs` — populate new fields during search

**Purpose:** Make search results immediately actionable without follow-up calls. Add signature, symbol type, doc summary, complexity, caller/dependency counts.

**Current SearchResult fields:** `rank, node_id, file_path, symbol_name, language, score, context, byte_range`

**New fields to add:**
```rust
pub struct SearchResult {
    // ... existing fields ...
    pub symbol_type: String,          // "function", "class", "method", etc.
    pub signature: Option<String>,    // First line / function signature
    pub doc_summary: Option<String>,  // First line of doc comment
    pub complexity: u32,              // Node complexity score
    pub caller_count: usize,          // Number of incoming Call edges
    pub dependency_count: usize,      // Number of outgoing Call/Import edges
}
```

**Implementation:** After search results are returned, iterate results and for each node_id, look up the PDG node to populate the new fields. Count incoming/outgoing edges.

**Tests:**
- [ ] `SearchResult` serialization includes new fields
- [ ] `symbol_type` correctly maps from `NodeType` enum
- [ ] `caller_count` and `dependency_count` match actual edge counts
- [ ] Backward compatible: existing consumers of SearchResult still work

- [ ] Task C.6 complete

---

### Task C.7: Enhance `leindex_phase_analysis` single-file deep dive

**Files:** `crates/lephase/src/phase1.rs` (or new per-file output in format.rs)

**Purpose:** When `path` points to a single file, produce output comprehensive enough to fully replace `Read`: all symbols with signatures, line ranges, complexity breakdown, cross-file deps, doc comments, inline TODOs.

**Implementation:** Enhance the `focus_files` path in phase analysis balanced/verbose modes. When exactly one focus file is set:
- Include per-symbol signature and line range in Phase 1 output
- Include per-symbol cross-file dependency list in Phase 2 output
- Include per-function complexity breakdown in Phase 4 output

**Tests:**
- [ ] Single-file phase analysis in balanced mode includes symbol signatures
- [ ] Single-file phase analysis includes cross-file dependency information
- [ ] Token budget still respected

- [ ] Task C.7 complete

---

### Task C.8: Register all new Phase C handlers

**Files:**
- `crates/lepasserelle/src/mcp/handlers.rs` — add to `ToolHandler` enum + all match arms
- `crates/lepasserelle/src/mcp/server.rs` — register in `McpServer::new()` handlers vec

**Acceptance criteria:**
- [ ] `ToolHandler` enum has 5 new variants (FileSummary, SymbolLookup, ProjectMap, GrepSymbols, ReadSymbol)
- [ ] All 5 match arms added to `name()`, `description()`, `argument_schema()`, `execute()`
- [ ] All 5 registered in server.rs handlers vec
- [ ] `cargo build -p lepasserelle` passes
- [ ] `tools/list` MCP request returns all 12 tools (7 existing + 5 new)

- [x] Task C.8 complete — c8a508f
- [ ] Task: Maestro - User Manual Verification 'Phase C' (Protocol in workflow.md)

---

## Phase D: Tool Supremacy — Context-Aware Editing

### [x] Task D.1: Implement `leedit` core — `read_file_content()` and `generate_diff()`

**File:** `crates/leedit/src/lib.rs`

**Currently:** Both return placeholders.

**`read_file_content()`:** Replace with actual `std::fs::read_to_string(file_path)`

**`generate_diff()`:** Add `diffy` crate to `leedit/Cargo.toml`, use it to produce unified diffs:
```rust
fn generate_diff(&self, original: &str, modified: &str, file_path: &Path) -> Result<String> {
    Ok(diffy::create_patch(original, modified).to_string())
}
```

**`analyze_impact()`:** Use `pdg.get_forward_impact(node_id)` for each affected symbol. Traverse edges to find all affected files. Compute risk based on count and type of dependents.

**`apply_change()`:** Read file, apply EditChange variant (ReplaceText: splice bytes; RenameSymbol: find-and-replace via PDG refs), write back.

**Tests:**
- [ ] `read_file_content()` returns actual file content
- [ ] `generate_diff()` produces valid unified diff
- [ ] `analyze_impact()` returns non-empty affected nodes for symbols with dependents
- [ ] `apply_change(ReplaceText)` modifies file content correctly
- [ ] `apply_change(RenameSymbol)` replaces symbol name in file

- [x] Task D.1 complete — 4b105cd

---

### [x] Task D.2: Implement `leindex_edit_preview` handler

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Purpose:** Before any edit, show the full impact: diff, affected symbols, affected files, breaking changes, risk level, and suggestions.

**Input schema:**
- `file_path` (string, required)
- `changes` (array of objects, required): Each with `type` (replace_text/rename_symbol/extract_function/inline_variable), `old_text`, `new_text`, `start_line`, `end_line`, `symbol_name`, `new_name`

**Response:** diff, affected_symbols, affected_files, breaking_changes, risk_level, suggestions

**Implementation:** Parse changes into `EditChange` variants → call `EditEngine::preview_edit()` → use PDG for cross-file impact analysis → return structured response.

**Tests:**
- [ ] Preview returns valid diff for text replacement
- [ ] Affected files list populated from PDG edges
- [ ] Risk level computed based on dependent count

- [x] Task D.2 complete — c7def6b

---

### [x] Task D.3: Implement `leindex_edit_apply` handler

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Input schema:** Same as edit_preview + `dry_run` (boolean, default false), `auto_fix_imports` (boolean, default false)

**Implementation:** If `dry_run=true`, delegate to preview. Otherwise: create worktree session → apply changes → validate (re-parse for syntax) → merge worktree → report result with impact.

**Tests:**
- [ ] `dry_run=true` returns preview without modifying files
- [ ] Apply creates the change in the file
- [ ] Result includes files_modified list

- [x] Task D.3 complete — c7def6b

---

### [x] Task D.4: Implement `leindex_rename_symbol` handler

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Input schema:**
- `old_name` (string, required)
- `new_name` (string, required)
- `scope` (string, optional): Limit to file/directory
- `preview_only` (boolean, default true): Safety default

**Implementation:**
1. `pdg.find_by_symbol(old_name)` → definition node
2. Traverse incoming edges → all reference sites (file_path, byte_range)
3. For each: record specific text to replace
4. Generate unified diff across all files
5. If `preview_only=false`: apply via leedit worktree

**Tests:**
- [ ] Preview lists all files containing references
- [ ] Diff shows old_name → new_name replacement at each site
- [ ] `preview_only=true` does not modify any files

- [x] Task D.4 complete — c7def6b

---

### [x] Task D.5: Implement `leindex_impact_analysis` handler

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

**Input schema:**
- `symbol` (string, required)
- `change_type` (string, enum: modify/remove/rename/change_signature, default modify)
- `depth` (integer, default 3, min 1, max 5): Transitive depth

**Implementation:**
1. Find symbol in PDG
2. `pdg.get_forward_impact(node_id)` for direct + transitive dependents
3. Group by depth level
4. Count affected symbols and files per level
5. Generate risk assessment string

**Tests:**
- [ ] Direct callers listed correctly
- [ ] Transitive dependents expand with depth
- [ ] File count aggregated correctly
- [ ] Risk assessment text generated

- [x] Task D.5 complete — c7def6b

---

### [x] Task D.6: Register all Phase D handlers

**Files:**
- `crates/lepasserelle/src/mcp/handlers.rs` — add EditPreview, EditApply, RenameSymbol, ImpactAnalysis to enum
- `crates/lepasserelle/src/mcp/server.rs` — register in handlers vec
- `crates/leedit/Cargo.toml` — add `diffy` dependency

**Acceptance criteria:**
- [ ] `ToolHandler` enum has 4 new variants
- [ ] All match arms added
- [ ] All registered in server.rs
- [ ] `cargo build -p lepasserelle -p leedit` passes
- [ ] `tools/list` returns all 16 tools (12 from Phase C + 4 new)

- [x] Task D.6 complete — c7def6b
- [ ] Task: Maestro - User Manual Verification 'Phase D' (Protocol in workflow.md)

---

## Phase E: Integration Testing & Tool Description Polish

### Task E.1: MCP stdio end-to-end integration tests

**File:** `crates/lepasserelle/tests/mcp_stdio_e2e.rs` (new)

**Tests:**
- [ ] Send `initialize` request → receive valid response → no double-newline in output
- [ ] Send `tools/list` → all 16 tools listed
- [ ] Send `tools/call` for `leindex_file_summary` → valid structured response
- [ ] Send `tools/call` for `leindex_symbol_lookup` → valid response with callers/callees
- [ ] Send `tools/call` for `leindex_project_map` → valid tree structure
- [ ] Send notification `notifications/initialized` → no response (notifications don't get replies)

- [x] Task E.1 complete — 9249185

---

### Task E.2: Tool description optimization for LLM preference

**File:** `crates/lepasserelle/src/mcp/handlers.rs`

Review and optimize every tool `description()` to:
1. State the concrete advantage over standard tools (e.g., "5-10x more token efficient")
2. State what unique information it provides (e.g., "cross-file dependency analysis")
3. Be specific about return content (not vague "analyzes a file")

**Tests:**
- [ ] Every tool description contains a concrete advantage statement
- [ ] Every tool description mentions what standard tool it replaces or supersedes
- [ ] No description exceeds 300 characters (concise for tool listing display)

- [x] Task E.2 complete — 773cb1b

---

### Task E.3: Token efficiency benchmarks

**File:** `docs/TOOL_SUPREMACY_BENCHMARKS.md` (new)

Measure token usage for common tasks:

| Task | Standard Tools (tokens) | LeIndex Tools (tokens) | Savings |
|------|------------------------|----------------------|---------|
| Understand 500-line file | Read: ~2000 | file_summary: ~400 | ~80% |
| Find all symbol usages | Grep: ~800 + 3×Read: ~6000 | symbol_lookup: ~500 | ~93% |
| Navigate project structure | Glob: ~300 + 5×Read: ~10K | project_map: ~800 | ~92% |
| Preview edit impact | N/A (no equivalent) | edit_preview: ~300 | New capability |

**Tests:**
- [ ] Benchmark document created with measured values
- [ ] At least 3 tasks show >5x token reduction

- [x] Task E.3 complete — 1e1c063

---

### Task E.4: Update Claude Code configuration documentation

**File:** `docs/MCP.md` (update existing)

Add section documenting:
- Recommended `~/.claude.json` configuration
- Complete tool listing with descriptions
- Usage examples for each new tool
- Comparison table vs standard tools

- [x] Task E.4 complete — 7c7768e
- [ ] Task: Maestro - User Manual Verification 'Phase E' (Protocol in workflow.md)

---

## Dependency Graph

```
A.1 → A.2 → B.1 → B.2 → C.6 → C.1, C.2, C.3, C.4, C.5 (parallel) → C.7 → C.8
                                                                              ↓
                                                                        D.1 → D.2, D.3, D.4, D.5 (parallel) → D.6
                                                                                                                  ↓
                                                                                                            E.1 → E.2 → E.3 → E.4
```

Phase A must complete first (nothing works without MCP transport).
Phase B must follow (semantic search is foundation for all C tools).
Phase C tools C.1-C.5 are independent of each other after C.6.
Phase D depends on Phase C being complete (for testing workflows).
Phase E is the final polish pass.

---

## Progress Summary

| Phase | Progress | Tasks Complete |
|---|---:|---:|
| A - MCP Transport Fix | 50% | 1/2 |
| B - Semantic Embedding Fix | 50% | 1/2 |
| C - Read/Grep/Glob Replacement | 75% | 6/8 |
| D - Context-Aware Editing | 100% | 6/6 |
| E - Polish & Testing | 100% | 4/4 |
| **Total** | **77%** | **18/22** |

## Notes
- All changes go to `operations/test-changes` branch per workflow.md
- Existing tools must remain backward-compatible — additive only
- Source reading from disk via byte_range, NOT from PDG storage
- Cross-file info comes from edge traversal, NOT re-parsing
- Token budget enforcement via `TokenFormatter::truncate()` from lephase
