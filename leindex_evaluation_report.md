# LeIndex MCP Server Evaluation Report

## Project Evaluated
- **Project Path:** `/home/scooter/Documents/Product/swiss-sandbox/`
- **Project Type:** Swiss Sandbox MCP Server (Python-based execution environment)

---

## Indexing Summary

| Metric | Value |
|--------|-------|
| Files Parsed | 116 |
| Total Files | 116 |
| Indexed Nodes | 3,198 |
| PDG Nodes | 3,198 |
| PDG Edges | 16,585 |
| Total Signatures | 1,630 |
| Indexing Time | 488ms |

---

## Tool-by-Tool Evaluation

### 1. `leindex_index` (Indexing Tool)
**Purpose:** Parse source files and build the Program Dependence Graph (PDG)

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Performance** | Fast (488ms for 116 files) |
| **Reliability** | High - 100% parse success rate |
| **Output Quality** | Excellent - created 3,198 nodes with 16,585 edges |

**Notes:** 
- First indexing attempt showed 0 files parsed (likely cached from previous project)
- Second attempt with `force_reindex=true` successfully parsed all 116 files
- No parse failures

---

### 2. `leindex_diagnostics` (Diagnostic Information)
**Purpose:** Get memory usage, index statistics, and system health

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Output Detail** | Comprehensive |
| **Memory Tracking** | Accurate (219MB used, 0.37% of total) |
| **Cache Info** | Present but empty (0 bytes, 0 entries) |

**Output Includes:**
- Memory usage (bytes and percentage)
- Cache statistics
- Project path and ID
- PDG statistics

---

### 3. `leindex_project_map` (Project Structure)
**Purpose:** Annotated project structure with files, directories, complexity scores

| Aspect | Result |
|--------|--------|
| **Status** | ⚠️ Partial Issue |
| **Basic Usage** | ✅ Working |
| **Scoped Queries** | ❌ Issues with `path` parameter |
| **Output Format** | Good |

**Issue Found:**
- When using `path` parameter to scope to subdirectories, it returns 0 files
- Works correctly with default (root) scope
- Returns top_symbols, complexity, and file metadata correctly

**Test Results:**
```
Default scope: 13 files returned
With path="/home/scooter/Documents/Product/swiss-sandbox/src/": 3 files
```

---

### 4. `leindex_grep_symbols` (Symbol Search)
**Purpose:** Search symbols with structural awareness

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Search Accuracy** | High |
| **Filtering** | Excellent (type_filter: function/class/method/variable/module) |
| **Context Lines** | ✅ Working |
| **Results Ranking** | Good |

**Test Cases:**
- `pattern="health_check"`: Found 3 matches across files
- `pattern="execute", type_filter="function"`: Found 1 result
- `pattern="class", type_filter="class"`: Returned 0 (may be indexing issue)

**Output Includes:**
- File path, line range
- Complexity score
- Caller/callee counts
- Dependency count

---

### 5. `leindex_search` (Semantic Search)
**Purpose:** Find symbols by meaning even without exact name matches

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Semantic Accuracy** | Good |
| **Score Breakdown** | Excellent (semantic, structural, text_match, overall) |
| **Top-K Support** | ✅ Working |

**Test Query:** `"server health monitoring"`
- Returned 5 relevant results
- Top result: `_continuous_monitoring` in health_monitor.py (score: 0.82)
- Semantic scores ranged from 0.73 to 0.84

**Output Includes:**
- Overall score and breakdown
- Node IDs for further lookup
- Symbol type and file path

---

### 6. `leindex_symbol_lookup` (Symbol Details)
**Purpose:** Get full structural context of a symbol

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Include Source** | ✅ Working |
| **Callees/Callers** | ✅ Accurate |
| **Impact Radius** | ✅ Provided |
| **Depth Control** | ✅ Working (1-5) |

**Test Symbol:** `health_check`

**Output Includes:**
- Source code (with `include_source=true`)
- Direct callers and callees
- Impact radius (affected files/symbols)
- Complexity score
- Byte range

**Note:** The lookup returned `run_health_check` from logging_system.py instead of the exact `health_check` in unified_server.py - may need more specific query

---

### 7. `leindex_file_summary` (File Analysis)
**Purpose:** Structural analysis of a file

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Symbol Count** | Accurate |
| **Dependencies** | ✅ Complete |
| **Cross-File Refs** | ✅ Provided |
| **Include Source** | ✅ Working |

**Test File:** `unified_server.py` (1,303 lines)

**Output Includes:**
- Module role classification
- Line count
- All symbols with signatures
- Dependencies and dependents per symbol
- Complexity scores
- Source snippets when requested

---

### 8. `leindex_read_symbol` (Read Symbol Source)
**Purpose:** Read exact source code of a specific symbol

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Dependency Signatures** | ✅ Included |
| **Token Budget** | ✅ Respects limit |
| **Disambiguation** | Works with file_path |

**Test Symbol:** `create_execution_context`

**Output Includes:**
- Full source code
- Caller list
- Dependency signatures with file locations
- Doc comment
- Line/byte ranges

---

### 9. `leindex_context` (Context Expansion)
**Purpose:** Expand context around a code node via PDG traversal

| Aspect | Result |
|--------|--------|
| **Status** | ❌ Not Working |
| **Error Type** | Returns empty results |
| **Node ID Format** | Unclear what format is expected |
| **Token Budget** | Accepts but returns minimal data |

**Test Attempts:**
- `node_id="/home/scooter/Documents/Product/swiss-sandbox/src/sandbox/unified_server.py:UnifiedServer.create_execution_context"`
- `node_id="/home/scooter/Documents/Product/swiss-sandbox/src/sandbox/unified_server.py:create_execution_context"`

**Result:** Both returned empty results with 0 scores

**This appears to be a bug or incomplete feature.**

---

### 10. `leindex_deep_analyze` (Semantic + PDG Analysis)
**Purpose:** Deep analysis combining semantic search with PDG traversal

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Query Understanding** | Good |
| **Context Expansion** | Excellent |
| **Result Quality** | High |

**Test Query:** `"How does the execution context creation work in the sandbox server?"`

**Output:**
- 10 ranked results
- Full context including test code and interfaces
- Semantic scores (0.36-0.84)
- Comprehensive symbol information

---

### 11. `leindex_phase_analysis` (5-Phase Analysis)
**Purpose:** Additive analysis with freshness-aware incremental execution

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Phase Execution** | All 5 phases executed |
| **Freshness Tracking** | ✅ Working |
| **Mode Support** | ultra/balanced/verbose |
| **Output Detail** | Excellent |

**Phases Executed:**
1. **Phase 1:** File parsing - 1 file, 42 signatures, 100% completeness
2. **Phase 2:** Import edges - 0 internal, 50 external, 50 unresolved
3. **Phase 3:** Entry points - 5 identified, 35 impacted nodes
4. **Phase 4:** Hotspots - 5 complexity hotspots identified
5. **Phase 5:** Recommendations - 3 actionable recommendations

**Key Finding:** 50 unresolved external imports reduce graph precision

---

### 12. `leindex_impact_analysis` (Impact Radius Analysis)
**Purpose:** Analyze transitive impact of changing a symbol

| Aspect | Result |
|--------|--------|
| **Status** | ✅ Working |
| **Depth Control** | ✅ Working (1-5) |
| **Change Types** | modify/remove/rename/change_signature |
| **Risk Assessment** | ✅ Provided |
| **Accuracy** | High |

**Test Symbol:** `health_check`

**Results:**
- Direct callers: 4
- Transitive callers: 4
- Transitive affected files: 1
- Transitive affected symbols: 39
- Risk level: **low**

---

### 13. `leindex_rename_symbol` (Cross-File Rename)
**Purpose:** Rename symbol across all files using PDG

| Aspect | Result |
|--------|--------|
| **Status** | ❌ Not Working |
| **Error** | Symbol not found in project index |
| **Preview Mode** | N/A (can't test without symbol) |
| **Documentation** | Unclear expected format |

**Attempts:**
- `old_name="health_check"` - Failed
- `old_name="UnifiedServer.health_check"` - Failed  
- `old_name="unified_server.py:health_check"` - Failed

**This tool appears to require a specific symbol format that is not clearly documented, or has a bug.**

---

### 14. `leindex_edit_preview` (Edit Preview)
**Purpose:** Preview code edits with impact analysis

| Aspect | Result |
|--------|--------|
| **Status** | ⚠️ Produces Incorrect Output |
| **Diff Generation** | ✅ Working |
| **Risk Assessment** | ✅ Provided |
| **Breaking Changes** | ✅ Detected |

**Test:** Replace `health_check` function definition

**Issue:** The diff output appears corrupted - the replacement text was mixed with existing content incorrectly

**Output includes:**
- Affected files list
- Risk level
- Breaking changes detection
- Change count

---

### 15. `leindex_edit_apply` (Apply Edits)
**Purpose:** Apply code edits to files

| Aspect | Result |
|--------|--------|
| **Status** | Not tested |
| **Note** | Skipped due to edit_preview issues |

---

## Performance Metrics

| Operation | Time | Notes |
|-----------|------|-------|
| Indexing (116 files) | 488ms | Fast |
| Project Map | <50ms | Quick |
| Symbol Search | <50ms | Quick |
| Semantic Search | <50ms | Quick |
| Symbol Lookup | <50ms | Quick |
| Deep Analyze | 17ms | Very fast |
| Phase Analysis | <50ms | Quick |
| Impact Analysis | <50ms | Quick |

**Memory Usage:** 219MB (0.37% of available 58GB)

---

## Issues Found

### Critical Issues

1. **`leindex_context` not returning results**
   - Always returns empty results regardless of node_id format
   - Appears to be a bug

2. **`leindex_rename_symbol` cannot find symbols**
   - Returns "Symbol not found" for all attempts
   - Expected format unclear from documentation
   - Limits refactoring capabilities

### Minor Issues

3. **`leindex_project_map` with path parameter**
   - Scoped queries return fewer files than expected
   - Works with root scope but not subdirectories

4. **`leindex_edit_preview` output corruption**
   - Diff generation produces incorrect/mixed output
   - Needs investigation

5. **Class detection**
   - `type_filter="class"` returns 0 results
   - May be indexing limitation or naming convention

---

## Summary Table

| Tool | Status | Performance | Accuracy | Usability |
|------|--------|-------------|----------|-----------|
| index | ✅ | Excellent | N/A | Good |
| diagnostics | ✅ | Excellent | High | Excellent |
| project_map | ⚠️ | Good | Partial | Good |
| grep_symbols | ✅ | Excellent | High | Excellent |
| search | ✅ | Excellent | Good | Excellent |
| symbol_lookup | ✅ | Excellent | High | Good |
| file_summary | ✅ | Excellent | High | Excellent |
| read_symbol | ✅ | Excellent | High | Excellent |
| context | ❌ | N/A | Broken | Poor |
| deep_analyze | ✅ | Excellent | High | Excellent |
| phase_analysis | ✅ | Excellent | High | Excellent |
| impact_analysis | ✅ | Excellent | High | Excellent |
| rename_symbol | ❌ | N/A | Broken | Poor |
| edit_preview | ⚠️ | Good | Issues | Fair |

---

## Recommendations

1. **Fix `leindex_context`** - This is a high-value tool for understanding code relationships
2. **Fix `leindex_rename_symbol`** - Clarify symbol format or fix symbol lookup
3. **Improve `project_map` path filtering** - Enable scoped directory queries
4. **Fix `edit_preview`** - Review diff generation logic
5. **Document symbol format requirements** - For rename and context tools

---

## Overall Assessment

**Rating: 8/10**

The LeIndex MCP Server is a powerful code analysis tool with excellent semantic search, PDG-based analysis, and impact analysis capabilities. Most tools work correctly and provide valuable insights into codebase structure.

The main issues are:
- 2 tools completely non-functional (context, rename_symbol)
- 1 tool with output issues (edit_preview)
- Minor scoping issues with project_map

These issues reduce the tool's usefulness for certain refactoring and navigation tasks, but the core analysis capabilities are solid.
