# LeIndex MCP Server

## Overview

The Model Context Protocol (MCP) is an open standard for connecting AI assistants to external tools and data sources. LeIndex provides an MCP server that exposes its code indexing, search, and analysis capabilities to any MCP-compatible client.

### Why MCP?

- **Universal Integration**: Works with Claude Code, Cursor, and any MCP-compatible AI tool
- **Standardized Protocol**: JSON-RPC 2.0 based, well-documented, and extensible
- **Real-time Feedback**: SSE streaming for long-running operations like indexing
- **Type-safe**: Full JSON schema for all tool parameters and responses

---

## Starting the Server

### HTTP Server Mode

Start the MCP server for HTTP-based clients:

```bash
# Default: 127.0.0.1:3000
leindex serve

# Custom host and port
leindex serve --host 0.0.0.0 --port 8080

# Override port via environment variable
LEINDEX_PORT=8080 leindex serve
```

**Endpoints:**

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/mcp` | POST | JSON-RPC 2.0 endpoint |
| `/mcp/tools/list` | GET | List available tools |
| `/mcp/index/stream` | POST | SSE streaming for indexing |
| `/health` | GET | Health check |

### Stdio Mode

For subprocess-based integration (Claude Code, etc.):

```bash
leindex mcp --stdio
```

This reads JSON-RPC from stdin and writes responses to stdout, with logs to stderr.

---

## Tool Comparison vs Standard Tools

LeIndex tools are designed to **replace or supersede** standard Claude Code tools for
code navigation tasks. The table below shows the token efficiency advantage:

| Task | Standard Tools | Tokens | LeIndex Tool | Tokens | Savings |
|------|---------------|-------:|--------------|-------:|--------:|
| Understand a file | `Read` (full file) | ~2 000 | `leindex_file_summary` | ~380 | **81%** |
| Find all callers | `Grep` + 3×`Read` | ~5 800 | `leindex_symbol_lookup` | ~420 | **93%** |
| Navigate project | `Glob` + 5×`Read` | ~8 500 | `leindex_project_map` | ~650 | **92%** |
| Find symbol uses | `Grep` | ~1 200 | `leindex_grep_symbols` | ~310 | **74%** |
| Read a function | `Read` (full file) | ~1 800 | `leindex_read_symbol` | ~220 | **88%** |
| Preview a rename | N/A | ∞ | `leindex_edit_preview` | ~280 | **New** |
| Cross-file rename | `Grep` + N×`Edit` | ~12 000 | `leindex_rename_symbol` | ~340 | **97%** |
| Change impact | N/A | ∞ | `leindex_impact_analysis` | ~260 | **New** |

> See [TOOL_SUPREMACY_BENCHMARKS.md](TOOL_SUPREMACY_BENCHMARKS.md) for detailed analysis.

**Correctness Notes (v1.5.0):**
- `leindex_file_summary` now reports `byte_range` (previously mislabeled as `line_range`)
- `leindex_grep_symbols` description accurately reflects supported search modes (exact match and substring)
- `leindex_symbol_lookup` and `leindex_impact_analysis` now honor the `depth` parameter for bounded traversal
- `leindex_rename_symbol` uses word-boundary-aware matching to prevent false-positive substring replacements
- `leindex_edit_apply` sorts byte-range changes in reverse order to prevent offset corruption in multi-change requests

---

## Available Tools

### `leindex_index`

Index a project for code search and analysis. Parses all source files, builds the Program Dependence Graph, and creates the semantic search index.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "project_path": {
      "type": "string",
      "description": "Absolute path to the project directory to index"
    },
    "force_reindex": {
      "type": "boolean",
      "description": "If true, re-index even if already indexed (default: false)",
      "default": false
    }
  },
  "required": ["project_path"]
}
```

**Example Request:**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "leindex_index",
    "arguments": {
      "project_path": "/home/user/my-project",
      "force_reindex": false
    }
  }
}
```

**Example Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\n  \"files_parsed\": 156,\n  \"nodes_created\": 2847,\n  \"edges_created\": 3291,\n  \"index_size_bytes\": 2457600,\n  \"indexing_time_ms\": 3420\n}"
      }
    ],
    "isError": false
  }
}
```

---

### `leindex_search`

Search indexed code using semantic search. Returns the most relevant code snippets matching your query, ranked by composite score (semantic + text match + structural).

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Search query (e.g., 'authentication', 'database connection')"
    },
    "project_path": {
      "type": "string",
      "description": "Project directory (auto-indexes on first use; omit to use current project)"
    },
    "top_k": {
      "type": "integer",
      "description": "Maximum number of results to return (default: 10)",
      "default": 10,
      "minimum": 1,
      "maximum": 100
    },
    "offset": {
      "type": "integer",
      "description": "Skip the first N results for pagination (default: 0)",
      "default": 0,
      "minimum": 0
    }
  },
  "required": ["query"]
}
```

**Example Request:**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "tools/call",
  "params": {
    "name": "leindex_search",
    "arguments": {
      "query": "authentication middleware",
      "top_k": 5
    }
  }
}
```

**Example Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "[\n  {\n    \"node_id\": \"auth_middleware\",\n    \"file_path\": \"src/middleware/auth.rs\",\n    \"score\": 0.89,\n    \"snippet\": \"pub async fn auth_middleware(req: Request) -> Result<Request, Error> {\\n    let token = req.headers().get(\\\"Authorization\\\");\\n    ...\"\n  }\n]"
      }
    ],
    "isError": false
  }
}
```

---

### `leindex_deep_analyze`

Perform deep code analysis with context expansion. Uses semantic search combined with Program Dependence Graph traversal to provide comprehensive understanding of code behavior and data flow.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Analysis query (e.g., 'How does authentication work?', 'Where is user data stored?')"
    },
    "project_path": {
      "type": "string",
      "description": "Project directory (auto-indexes on first use; omit to use current project)"
    },
    "token_budget": {
      "type": "integer",
      "description": "Maximum tokens for context expansion (default: 2000)",
      "default": 2000,
      "minimum": 100,
      "maximum": 100000
    }
  },
  "required": ["query"]
}
```

**Example Request:**

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "tools/call",
  "params": {
    "name": "leindex_deep_analyze",
    "arguments": {
      "query": "How does the error handling system work?",
      "token_budget": 4000
    }
  }
}
```

**Example Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\n  \"primary_results\": [...],\n  \"expanded_context\": {\n    \"callers\": [...],\n    \"callees\": [...],\n    \"data_flow\": [...]\n  },\n  \"summary\": \"Error handling flows through the Result type pattern...\"\n}"
      }
    ],
    "isError": false
  }
}
```

---

### `leindex_context`

Expand context around a specific code node using Program Dependence Graph traversal. Shows callers, callees, data dependencies, and sibling nodes.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "node_id": {
      "type": "string",
      "description": "Node ID to expand context around (short name like 'my_func' or full ID like 'file.py:Class.method')"
    },
    "project_path": {
      "type": "string",
      "description": "Project directory (auto-indexes on first use; omit to use current project)"
    },
    "token_budget": {
      "type": "integer",
      "description": "Maximum tokens for context (default: 2000)",
      "default": 2000,
      "minimum": 100,
      "maximum": 100000
    }
  },
  "required": ["node_id"]
}
```

**Example Request:**

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "tools/call",
  "params": {
    "name": "leindex_context",
    "arguments": {
      "node_id": "handle_request",
      "token_budget": 3000
    }
  }
}
```

**Example Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\n  \"node\": {\"id\": \"handle_request\", \"kind\": \"function\", ...},\n  \"callers\": [{\"id\": \"main\", \"file\": \"src/main.rs\"}],\n  \"callees\": [{\"id\": \"parse_headers\", \"file\": \"src/http.rs\"}],\n  \"related_symbols\": [...]\n}"
      }
    ],
    "isError": false
  }
}
```

---

### `leindex_diagnostics`

Get diagnostic information about the indexed project, including memory usage, index statistics, and system health.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "project_path": {
      "type": "string",
      "description": "Project directory (omit to use current project)"
    }
  },
  "required": []
}
```

**Example Request:**

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "tools/call",
  "params": {
    "name": "leindex_diagnostics",
    "arguments": {}
  }
}
```

**Example Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\n  \"project_path\": \"/home/user/my-project\",\n  \"indexed\": true,\n  \"files_count\": 156,\n  \"nodes_count\": 2847,\n  \"edges_count\": 3291,\n  \"memory_usage_mb\": 24.5,\n  \"index_age_seconds\": 3600\n}"
      }
    ],
    "isError": false
  }
}
```

---

### `leindex_phase_analysis`

Run additive 5-phase analysis with freshness-aware incremental execution. Defaults to all 5 phases when `phase` is omitted.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "phase": {
      "oneOf": [
        { "type": "integer", "minimum": 1, "maximum": 5 },
        { "type": "string", "enum": ["all", "1", "2", "3", "4", "5"] }
      ],
      "default": "all"
    },
    "mode": {
      "type": "string",
      "enum": ["ultra", "balanced", "verbose"],
      "default": "balanced"
    },
    "path": {
      "type": "string"
    },
    "max_files": {
      "type": "integer",
      "default": 2000
    },
    "max_focus_files": {
      "type": "integer",
      "default": 20
    },
    "top_n": {
      "type": "integer",
      "default": 10
    },
    "max_chars": {
      "type": "integer",
      "default": 12000
    },
    "include_docs": {
      "type": "boolean",
      "default": false
    },
    "docs_mode": {
      "type": "string",
      "enum": ["off", "markdown", "text", "all"],
      "default": "off"
    }
  },
  "required": []
}
```

**Example Request (Single Phase):**

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "method": "tools/call",
  "params": {
    "name": "leindex_phase_analysis",
    "arguments": {
      "phase": 1,
      "mode": "balanced",
      "path": "/home/user/my-project/src"
    }
  }
}
```

**Example Request (All Phases):**

```json
{
  "jsonrpc": "2.0",
  "id": 7,
  "method": "tools/call",
  "params": {
    "name": "leindex_phase_analysis",
    "arguments": {
      "phase": "all",
      "mode": "verbose",
      "include_docs": true,
      "docs_mode": "markdown"
    }
  }
}
```

**Example Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\n  \"executed_phases\": [1, 2, 3, 4, 5],\n  \"phase_1\": {\"files_scanned\": 156, \"hotspots\": [...]},\n  \"phase_2\": {\"symbol_count\": 2847, \"top_symbols\": [...]},\n  ...\n}"
      }
    ],
    "isError": false
  }
}
```

---

### `phase_analysis` (Alias)

Compatibility alias for `leindex_phase_analysis`. Identical functionality with shorter name.

---

## Phase C Tools — Read/Grep/Glob Replacements

These tools provide **structural awareness** and **cross-file dependency information**
that standard tools cannot provide. Each tool's response is self-contained — no
follow-up `Read` calls needed.

### `leindex_file_summary`

Structural analysis of a file: all symbols, signatures, complexity scores,
cross-file deps/dependents, and module role. **5-10x more token efficient than Read.**

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "file_path": { "type": "string", "description": "Absolute path to the file" },
    "project_path": { "type": "string", "description": "Project directory (auto-indexes on first use)" },
    "token_budget": { "type": "integer", "default": 1000 },
    "include_source": { "type": "boolean", "default": false },
    "focus_symbol": { "type": "string" }
  },
  "required": ["file_path"]
}
```

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 10, "method": "tools/call",
  "params": {
    "name": "leindex_file_summary",
    "arguments": { "file_path": "/project/src/auth.rs", "token_budget": 800 }
  }
}
```

**Response includes:** `file_path`, `language`, `line_count`, `symbols` (name, type, line_range, complexity, dependencies, dependents), `module_role`.

---

### `leindex_symbol_lookup`

Look up a symbol and get its full structural context: definition, signature, callers,
callees, data dependencies, and impact radius. **Replaces Grep + multiple Read calls.**

Supports both single symbol and batch mode (up to 20 symbols per call).

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "symbol": { "type": "string", "description": "Symbol name to look up (single symbol)" },
    "symbols": { "type": "array", "items": { "type": "string" }, "description": "Batch mode: look up multiple symbols in one call (max 20)" },
    "project_path": { "type": "string", "description": "Project directory (auto-indexes on first use)" },
    "token_budget": { "type": "integer", "default": 1500 },
    "include_source": { "type": "boolean", "default": false },
    "include_callers": { "type": "boolean", "default": true },
    "include_callees": { "type": "boolean", "default": true },
    "depth": { "type": "integer", "default": 2, "minimum": 1, "maximum": 5 }
  },
  "required": []
}
```

> **Note:** Provide either `symbol` (single) or `symbols` (batch), not both. The `required` array is empty because either field satisfies the requirement.

**Example (single):**

```json
{
  "jsonrpc": "2.0", "id": 11, "method": "tools/call",
  "params": {
    "name": "leindex_symbol_lookup",
    "arguments": { "symbol": "handle_request", "include_callers": true }
  }
}
```

**Example (batch):**

```json
{
  "jsonrpc": "2.0", "id": 12, "method": "tools/call",
  "params": {
    "name": "leindex_symbol_lookup",
    "arguments": {
      "symbols": ["handle_request", "authenticate", "StorageEngine"],
      "include_callers": true,
      "token_budget": 4000
    }
  }
}
```

**Response includes:** `symbol`, `file`, `byte_range`, `type`, `complexity`, `callers`, `callees`, `impact_radius`. Batch mode wraps results in `{ "batch": true, "count": N, "results": [...] }`.

---

### `leindex_project_map`

Annotated project structure map: files, directories, symbol counts, complexity
hotspots, and inter-module dependency arrows. **Replaces Glob + directory reads.**

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "Subdirectory to scope to (default: project root)" },
    "project_path": { "type": "string", "description": "Project directory (auto-indexes on first use)" },
    "depth": { "type": "integer", "default": 3, "minimum": 1, "maximum": 10 },
    "token_budget": { "type": "integer", "default": 2000 },
    "sort_by": { "type": "string", "enum": ["complexity", "name", "dependencies", "size"], "default": "complexity" },
    "include_symbols": { "type": "boolean", "default": false },
    "offset": { "type": "integer", "default": 0, "description": "Skip the first N files for pagination" },
    "limit": { "type": "integer", "description": "Maximum number of files to return" }
  }
}
```

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 12, "method": "tools/call",
  "params": {
    "name": "leindex_project_map",
    "arguments": { "depth": 3, "sort_by": "complexity" }
  }
}
```

**Response includes:** Nested directory/file tree with `symbol_count`, `complexity`, `language`, `dependencies_out`, `dependencies_in` per file.

---

### `leindex_grep_symbols`

Search for symbols across the indexed codebase with structural awareness. Unlike
text-based grep, results include symbol type, dependency graph role, and optional
source context lines.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "pattern": { "type": "string", "description": "Symbol name, substring, or query" },
    "project_path": { "type": "string", "description": "Project directory (auto-indexes on first use)" },
    "scope": { "type": "string", "description": "Limit to file/directory path" },
    "type_filter": { "type": "string", "enum": ["function", "class", "method", "variable", "module", "all"], "default": "all" },
    "token_budget": { "type": "integer", "default": 1500 },
    "include_context_lines": { "type": "integer", "default": 0, "minimum": 0, "maximum": 10, "description": "Source context lines around each match" },
    "max_results": { "type": "integer", "default": 20, "minimum": 1, "maximum": 200 },
    "offset": { "type": "integer", "default": 0, "description": "Skip the first N results for pagination" }
  },
  "required": ["pattern"]
}
```

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 13, "method": "tools/call",
  "params": {
    "name": "leindex_grep_symbols",
    "arguments": { "pattern": "auth", "type_filter": "function", "max_results": 10 }
  }
}
```

**Response includes:** Array of matches with `name`, `file`, `line_range`, `node_type`, `complexity`.

---

### `leindex_read_symbol`

Read the source code of a specific symbol with its doc comment and dependency
signatures. **Reads exactly what you need — supersedes targeted Read.**

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "symbol": { "type": "string", "description": "Symbol to read source for" },
    "file_path": { "type": "string", "description": "Disambiguate when symbol exists in multiple files" },
    "project_path": { "type": "string", "description": "Project directory (auto-indexes on first use)" },
    "include_dependencies": { "type": "boolean", "default": true },
    "token_budget": { "type": "integer", "default": 2000 }
  },
  "required": ["symbol"]
}
```

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 14, "method": "tools/call",
  "params": {
    "name": "leindex_read_symbol",
    "arguments": { "symbol": "IndexHandler", "include_dependencies": true }
  }
}
```

**Response includes:** `symbol`, `file`, `source` (exact byte-range content), `doc_comment`, `dependency_signatures`.

---

## Phase D Tools — Context-Aware Editing

These tools provide **safe, impact-aware code editing** with no equivalent in standard
Claude Code tools. Always preview before applying.

### `leindex_edit_preview`

Preview a code edit: unified diff, affected symbols/files, breaking changes, and risk
level — all before touching the filesystem. **No equivalent in standard tools.**

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "file_path": { "type": "string", "description": "Absolute path to the file to edit" },
    "changes": {
      "type": "array",
      "description": "List of changes. Each has 'type' (replace_text/rename_symbol) and type-specific fields.",
      "items": { "type": "object" }
    }
  },
  "required": ["file_path", "changes"]
}
```

**Change types:**
- `replace_text`: `{ "type": "replace_text", "old_text": "...", "new_text": "...", "start_byte": 100, "end_byte": 150 }`
- `rename_symbol`: `{ "type": "rename_symbol", "old_name": "OldName", "new_name": "NewName" }`

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 15, "method": "tools/call",
  "params": {
    "name": "leindex_edit_preview",
    "arguments": {
      "file_path": "/project/src/auth.rs",
      "changes": [{ "type": "rename_symbol", "old_name": "authenticate", "new_name": "verify_identity" }]
    }
  }
}
```

**Response includes:** `diff` (unified diff), `affected_symbols`, `affected_files`, `breaking_changes`, `risk_level` (low/medium/high), `change_count`.

---

### `leindex_edit_apply`

Apply code edits to files. Use `dry_run=true` to get a preview without modifying files.
**Always run `leindex_edit_preview` first.**

**Parameters:** Same as `leindex_edit_preview` plus:
- `dry_run` (boolean, default `false`): If true, return preview without modifying files.

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 16, "method": "tools/call",
  "params": {
    "name": "leindex_edit_apply",
    "arguments": {
      "file_path": "/project/src/auth.rs",
      "changes": [{ "type": "replace_text", "old_text": "foo", "new_text": "bar" }],
      "dry_run": false
    }
  }
}
```

**Response includes:** `success`, `changes_applied`, `files_modified`.

---

### `leindex_rename_symbol`

Rename a symbol across all files using PDG to find all reference sites. Generates a
unified multi-file diff. **Replaces manual Grep + multi-file Edit.**

> **Note:** Rename matching uses word-boundary detection. Renaming `get` will not affect `get_user` or `widget`.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "old_name": { "type": "string", "description": "Current symbol name" },
    "new_name": { "type": "string", "description": "New symbol name" },
    "scope": { "type": "string", "description": "Limit rename to a file or directory path" },
    "preview_only": { "type": "boolean", "default": true, "description": "Return diff without applying (safety default)" }
  },
  "required": ["old_name", "new_name"]
}
```

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 17, "method": "tools/call",
  "params": {
    "name": "leindex_rename_symbol",
    "arguments": { "old_name": "UserData", "new_name": "UserProfile", "preview_only": true }
  }
}
```

**Response includes:** `old_name`, `new_name`, `files_affected`, `preview_only`, `diffs` (per-file), `applied`.

---

### `leindex_impact_analysis`

Analyze the transitive impact of changing a symbol: all affected symbols/files at each
dependency depth level with a risk assessment. **No equivalent in standard tools.**

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "symbol": { "type": "string", "description": "Symbol to analyze impact for" },
    "change_type": {
      "type": "string",
      "enum": ["modify", "remove", "rename", "change_signature"],
      "default": "modify"
    },
    "depth": { "type": "integer", "default": 3, "minimum": 1, "maximum": 5 }
  },
  "required": ["symbol"]
}
```

**Example:**

```json
{
  "jsonrpc": "2.0", "id": 18, "method": "tools/call",
  "params": {
    "name": "leindex_impact_analysis",
    "arguments": { "symbol": "StorageEngine", "change_type": "change_signature", "depth": 3 }
  }
}
```

**Response includes:** `symbol`, `file`, `change_type`, `direct_callers`, `transitive_affected_symbols`, `transitive_affected_files`, `risk_level` (low/medium/high), `summary`.

---

## SSE Streaming

For long-running indexing operations, use the SSE endpoint for real-time progress:

**Request:**

```bash
curl -N -X POST http://localhost:3000/mcp/index/stream \
  -H "Content-Type: application/json" \
  -d '{"project_path": "/home/user/my-project", "force_reindex": true}'
```

**Event Stream:**

```
event: message
data: {"type":"progress","stage":"starting","current":0,"total":0,"message":"Starting indexing for: /home/user/my-project","timestamp_ms":1707800000000}

event: message
data: {"type":"progress","stage":"collecting","current":0,"total":0,"message":"Collecting source files...","timestamp_ms":1707800000100}

event: message
data: {"type":"progress","stage":"parsing","current":50,"total":156,"message":"Parsing files...","timestamp_ms":1707800005000}

event: message
data: {"type":"complete","stage":"indexing","current":0,"total":0,"message":"Done: 156 files","timestamp_ms":1707800010000}
```

**Progress Event Schema:**

```json
{
  "type": "object",
  "properties": {
    "type": {
      "type": "string",
      "enum": ["progress", "complete", "error"]
    },
    "stage": { "type": "string" },
    "current": { "type": "integer" },
    "total": { "type": "integer" },
    "message": { "type": "string" },
    "timestamp_ms": { "type": "integer" }
  }
}
```

---

## Integration Guides

### Claude Code

**Recommended: stdio transport** (most reliable, subprocess-based):

Add to `~/.claude.json` (global, available in all projects):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp", "--stdio"],
      "type": "stdio"
    }
  }
}
```

Or add to `.claude/settings.json` (project-local):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp", "--stdio"],
      "type": "stdio"
    }
  }
}
```

**Note:** The `--stdio` flag is the recommended transport. It launches `leindex` as a
subprocess, reads JSON-RPC from stdin, and writes responses to stdout. Each response is
a single line of JSON (no double-newlines), which is required for the MCP protocol.

**HTTP transport** (alternative, requires running `leindex serve` separately):

```json
{
  "mcpServers": {
    "leindex": {
      "url": "http://localhost:3000/mcp",
      "type": "http"
    }
  }
}
```

**Project-specific override** (`.claude/settings.local.json`):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp", "--stdio"],
      "type": "stdio",
      "cwd": "/path/to/your/project"
    }
  }
}
```

### Cursor

Add to your Cursor MCP configuration:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp", "--stdio"]
    }
  }
}
```

### Generic MCP Client

**Python Example:**

```python
import json
import subprocess

def call_leindex_tool(tool_name, arguments):
    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        }
    }

    proc = subprocess.Popen(
        ["leindex", "mcp", "--stdio"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL
    )

    stdout, _ = proc.communicate(json.dumps(request).encode())
    return json.loads(stdout)

# Index a project
result = call_leindex_tool("leindex_index", {
    "project_path": "/home/user/my-project"
})
print(result)

# Search
result = call_leindex_tool("leindex_search", {
    "query": "error handling",
    "top_k": 5
})
print(result)
```

**HTTP Client Example:**

```python
import requests

def call_tool_http(url, tool_name, arguments):
    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        }
    }
    response = requests.post(f"{url}/mcp", json=request)
    return response.json()

result = call_tool_http("http://localhost:3000", "leindex_search", {
    "query": "authentication"
})
print(result)
```

---

## Advanced Usage Patterns

### Multi-Project Workflow

Every tool accepts an optional `project_path` parameter. The server automatically
switches context and auto-indexes on first use, so you can work across multiple
codebases without manually calling `leindex_index`:

```json
// Query project A
{ "name": "leindex_search", "arguments": { "project_path": "/home/user/project-a", "query": "auth" } }

// Switch to project B — auto-indexes transparently
{ "name": "leindex_search", "arguments": { "project_path": "/home/user/project-b", "query": "auth" } }

// Omit project_path to stay on the last-used project
{ "name": "leindex_search", "arguments": { "query": "database" } }
```

### Deep Analyze vs Phase Analysis — When to Use Each

| Scenario | Best Tool | Why |
|----------|-----------|-----|
| "How does auth work?" | `leindex_deep_analyze` | Follows PDG edges from search results to callers/callees/data flow |
| "Give me a project overview" | `leindex_phase_analysis` (all phases) | Runs 5 additive phases: file scan, symbol extraction, hotspot detection, dependency analysis, and risk assessment |
| "Analyze this one file deeply" | `leindex_phase_analysis` with `path` | Single-file deep dive with per-symbol PDG data (signatures, callers, cross-file deps) |
| "What calls this function?" | `leindex_symbol_lookup` | Direct caller/callee graph with impact radius |
| "Show me the architecture" | `leindex_project_map` | Annotated directory tree with complexity hotspots and dependency arrows |

**`leindex_deep_analyze` — Best for questions about behavior:**

```json
{
  "name": "leindex_deep_analyze",
  "arguments": {
    "query": "How does the error handling system propagate errors to the user?",
    "token_budget": 6000
  }
}
```

The tool finds relevant symbols via semantic search, then traverses the PDG to expand
context: callers, callees, data flow, and related symbols. Higher `token_budget` values
include more transitive context. Use 2000 for focused results, 6000-8000 for comprehensive
understanding.

**`leindex_phase_analysis` — Best for structural understanding:**

```json
// Full project analysis (all 5 phases)
{
  "name": "leindex_phase_analysis",
  "arguments": {
    "mode": "verbose",
    "include_docs": true,
    "docs_mode": "markdown"
  }
}

// Single file deep dive (returns per-symbol PDG data)
{
  "name": "leindex_phase_analysis",
  "arguments": {
    "path": "/project/src/auth/middleware.rs",
    "phase": "all",
    "mode": "verbose"
  }
}

// Phase 1 only: quick file scan + hotspot detection
{
  "name": "leindex_phase_analysis",
  "arguments": {
    "phase": 1,
    "mode": "ultra",
    "max_files": 500
  }
}
```

**Phase breakdown:**
1. **Phase 1 — File Scan**: Directory structure, file sizes, language distribution, hotspot detection
2. **Phase 2 — Symbol Extraction**: All symbols with signatures, complexity, and type classification
3. **Phase 3 — Dependency Analysis**: Import/call graphs, module coupling, circular dependency detection
4. **Phase 4 — Hotspot Analysis**: Complexity hotspots, most-referenced symbols, risk areas
5. **Phase 5 — Summary Report**: Architecture overview, key findings, recommendations

When `path` points to a single file, the response includes a `file_symbols` array
with per-symbol data: `signature`, `line_start`/`line_end`, `complexity`,
`caller_count`, `dependency_count`, and `cross_file_deps`.

### Batch Symbol Lookup

Look up multiple symbols in a single call (max 20). The token budget is divided evenly
across all symbols:

```json
{
  "name": "leindex_symbol_lookup",
  "arguments": {
    "symbols": ["handle_request", "authenticate", "StorageEngine", "parse_config"],
    "include_callers": true,
    "include_callees": true,
    "token_budget": 4000
  }
}
```

Response:
```json
{
  "batch": true,
  "count": 4,
  "results": [
    { "symbol": "handle_request", "file": "src/server.rs", "callers": [...], ... },
    { "symbol": "authenticate", "file": "src/auth.rs", "callers": [...], ... },
    ...
  ]
}
```

> **Design Note: `symbol` vs `symbols[]`** — The `leindex_symbol_lookup` tool accepts
> both `symbol` (string) for single lookups and `symbols` (array) for batch lookups.
> This dual interface is intentional: LLMs naturally use `symbol` for single lookups
> (the common case), while `symbols[]` enables efficient batch operations that reduce
> round trips. Providing `symbols` with a single element is equivalent to using `symbol`.
> If both are provided, `symbols[]` takes precedence.

### Pagination

Tools with large result sets support `offset` and `limit` parameters for pagination:

```json
// First page: 20 results
{ "name": "leindex_search", "arguments": { "query": "handler", "top_k": 20, "offset": 0 } }

// Second page: next 20 results
{ "name": "leindex_search", "arguments": { "query": "handler", "top_k": 20, "offset": 20 } }

// Project map with pagination
{ "name": "leindex_project_map", "arguments": { "offset": 0, "limit": 50 } }

// Grep symbols with pagination
{ "name": "leindex_grep_symbols", "arguments": { "pattern": "auth", "max_results": 10, "offset": 0 } }
```

Paginated responses include `offset`, `count`, and `has_more` fields.

### Safe Rename Workflow

The recommended workflow for cross-file renames is preview-first:

```bash
# Step 1: Preview the rename (default: preview_only=true)
leindex_rename_symbol { "old_name": "UserData", "new_name": "UserProfile" }

# Step 2: Review the diff, then apply
leindex_rename_symbol { "old_name": "UserData", "new_name": "UserProfile", "preview_only": false }

# Optional: Scope rename to a subdirectory
leindex_rename_symbol { "old_name": "UserData", "new_name": "UserProfile", "scope": "src/models/" }
```

Rename uses word-boundary detection: renaming `get` will NOT affect `get_user` or `widget`.

### Impact Analysis Before Refactoring

Before changing a function signature or removing a symbol, check transitive impact:

```json
{
  "name": "leindex_impact_analysis",
  "arguments": {
    "symbol": "StorageEngine",
    "change_type": "change_signature",
    "depth": 3
  }
}
```

Response includes `direct_callers`, `transitive_affected_symbols`,
`transitive_affected_files`, `risk_level` (low/medium/high), and a human-readable
`summary`. The `depth` parameter controls how many levels of transitive dependencies
to follow (1-5).

### Edit Preview + Apply Workflow

Always preview before applying edits:

```json
// Step 1: Preview
{
  "name": "leindex_edit_preview",
  "arguments": {
    "file_path": "/project/src/auth.rs",
    "changes": [
      { "type": "rename_symbol", "old_name": "authenticate", "new_name": "verify_identity" },
      { "type": "replace_text", "old_text": "pub fn old_helper", "new_text": "pub fn new_helper" }
    ]
  }
}

// Step 2: Review the diff, affected symbols, and risk level

// Step 3: Apply (or use dry_run=true for another preview)
{
  "name": "leindex_edit_apply",
  "arguments": {
    "file_path": "/project/src/auth.rs",
    "changes": [
      { "type": "rename_symbol", "old_name": "authenticate", "new_name": "verify_identity" }
    ],
    "dry_run": false
  }
}
```

### Boolean Parameter Flexibility

All boolean parameters accept multiple formats for LLM compatibility:
- Native JSON: `true`, `false`
- String: `"true"`, `"false"`, `"yes"`, `"no"`, `"1"`, `"0"`
- Numeric: `1`, `0`

```json
// All equivalent:
{ "force_reindex": true }
{ "force_reindex": "true" }
{ "force_reindex": "yes" }
{ "force_reindex": 1 }
```

### Incremental Indexing

LeIndex uses BLAKE3 file hashes for incremental indexing. On subsequent calls to
`leindex_index`, only changed, new, or deleted files are re-processed:

```json
// First call: full index (3-5 seconds for ~500 files)
{ "name": "leindex_index", "arguments": { "project_path": "/project" } }

// After editing 2 files: only those 2 files are re-parsed (~50ms)
{ "name": "leindex_index", "arguments": { "project_path": "/project" } }

// Force full re-index (useful after git checkout, branch switch, etc.)
{ "name": "leindex_index", "arguments": { "project_path": "/project", "force_reindex": true } }
```

### Scoring Methodology

Search results include a composite score (0.0-1.0) with three components:
- **Semantic (50%)**: TF-IDF cosine similarity between query and symbol tokens
- **Text match (30%)**: Direct token overlap ratio
- **Structural (20%)**: PDG centrality (how connected the symbol is in the dependency graph)

The scoring breakdown is included in every search response.

### Project Configuration

Create a `.leindex/config.toml` in your project root to customize indexing behavior:

```toml
[exclusions]
directory_patterns = [".git", "node_modules", "target", "dist", "build", "out"]
file_patterns = ["*.min.js", "*.min.css", "*.pb.go", "*.generated.rs"]
path_patterns = ["*/target/*", "*/node_modules/*"]

[extensions]
enabled = ["rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "cpp", "c", "h", "rb", "php"]

[analysis]
max_files = 5000
```

See [PROJECT_CONFIG_OVERRIDES.md](PROJECT_CONFIG_OVERRIDES.md) for full configuration reference.

### Supported Languages

LeIndex supports 17+ languages with full tree-sitter parsing:

| Language | Extensions | Status |
|----------|-----------|--------|
| Rust | `.rs` | Full support |
| Python | `.py` | Full support |
| JavaScript | `.js`, `.jsx` | Full support |
| TypeScript | `.ts`, `.tsx` | Full support |
| Go | `.go` | Full support |
| Java | `.java` | Full support |
| C | `.c`, `.h` | Full support |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp` | Full support |
| C# | `.cs` | Full support |
| Ruby | `.rb` | Full support |
| PHP | `.php` | Full support |
| Lua | `.lua` | Full support |
| Scala | `.scala`, `.sc` | Full support |
| Bash | `.sh`, `.bash` | Full support |
| JSON | `.json` | Full support |
| Swift | `.swift` | Parser available (disabled) |
| Kotlin | `.kt` | Parser available (disabled) |
| Dart | `.dart` | Parser available (disabled) |

---

## Protocol Details

### JSON-RPC 2.0 Format

All requests and responses follow the JSON-RPC 2.0 specification.

**Request:**

```json
{
  "jsonrpc": "2.0",
  "id": <string | number | null>,
  "method": "tools/call" | "tools/list",
  "params": {
    "name": "<tool_name>",
    "arguments": { ... }
  }
}
```

**Success Response:**

```json
{
  "jsonrpc": "2.0",
  "id": <request_id>,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "<json_result>"
      }
    ],
    "isError": false
  }
}
```

**Error Response:**

```json
{
  "jsonrpc": "2.0",
  "id": <request_id>,
  "error": {
    "code": <error_code>,
    "message": "<error_message>",
    "data": { ... }
  }
}
```

### Methods

| Method | Description |
|--------|-------------|
| `tools/list` | List all available tools |
| `tools/call` | Execute a tool |

---

## Error Handling

### Standard JSON-RPC Error Codes

| Code | Name | Description |
|------|------|-------------|
| -32700 | `PARSE_ERROR` | Invalid JSON was received |
| -32600 | `INVALID_REQUEST` | The JSON is not a valid request |
| -32601 | `METHOD_NOT_FOUND` | Method does not exist |
| -32602 | `INVALID_PARAMS` | Invalid method parameters |
| -32603 | `INTERNAL_ERROR` | Internal JSON-RPC error |

### LeIndex-Specific Error Codes

| Code | Name | Description |
|------|------|-------------|
| -32001 | `PROJECT_NOT_FOUND` | Project directory not found |
| -32002 | `PROJECT_NOT_INDEXED` | Project exists but not indexed |
| -32003 | `INDEXING_FAILED` | Project indexing failed |
| -32004 | `SEARCH_FAILED` | Search operation failed |
| -32005 | `CONTEXT_EXPANSION_FAILED` | Context expansion failed |
| -32006 | `MEMORY_LIMIT_EXCEEDED` | Memory limit exceeded |

### Error Response Format

Every error response includes a structured `data` field with:
- `error_type`: Machine-readable error classification
- `suggestion`: Actionable guidance for resolving the error
- Additional context fields specific to the error type

**Example: Project Not Indexed**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32002,
    "message": "Project not indexed — call leindex_index or pass project_path to auto-index",
    "data": {
      "project": "/home/user/my-project",
      "error_type": "project_not_indexed",
      "suggestion": "Pass project_path to any tool to auto-index on first use, or call leindex_index explicitly."
    }
  }
}
```

**Example: Search Failed**

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "error": {
    "code": -32004,
    "message": "Search failed: no results for query",
    "data": {
      "error_type": "search_failed",
      "suggestion": "Ensure the project is indexed. Try a different query or increase top_k."
    }
  }
}
```

**Example: Memory Limit Exceeded**

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "error": {
    "code": -32006,
    "message": "Memory limit exceeded",
    "data": {
      "error_type": "memory_limit_exceeded",
      "suggestion": "Reduce token_budget, use pagination (offset/limit), or re-index with a smaller scope."
    }
  }
}
```

---

## Timeouts and Performance

### Default Timeouts

- **HTTP Server**: 300 seconds (5 minutes) for all requests
- **Indexing**: No arbitrary timeout; use SSE streaming for progress
- **Search**: Typically completes in <1 second for most queries

### Memory Management

LeIndex automatically manages memory:
- Cache spilling to disk when memory budget is exceeded
- LRU eviction for infrequently accessed data
- Configurable memory limits

### Token Budget Recommendations

The `token_budget` parameter controls how much context is included in responses.
Choose your budget based on the task:

| Use Case | Recommended Budget | Rationale |
|----------|-------------------|-----------|
| Quick symbol lookup | 500–1000 | Just name, type, file, signature |
| File summary overview | 800–1500 | Symbol list + dependency counts |
| File summary with source | 2000–4000 | Full source code of key symbols |
| Symbol lookup with callers | 1500–2500 | Definition + caller/callee lists |
| Deep analysis (single question) | 3000–6000 | Multi-hop PDG traversal, data flow |
| Deep analysis (comprehensive) | 6000–10000 | Full transitive context expansion |
| Project map (small project) | 1500–2000 | Full directory tree |
| Project map (large project) | 3000–5000 | With `offset`/`limit` pagination |
| Batch symbol lookup (4 symbols) | 4000–6000 | ~1000–1500 per symbol |
| Edit preview | 1000–2000 | Diff + affected symbols |
| Impact analysis | 2000–4000 | Transitive dependency tree |

**Rules of thumb:**
- Start with the default and increase only if the response feels truncated
- For batch operations, multiply per-item budget by count
- Responses include a `truncated` flag when the budget was exhausted
- Higher budgets don't slow down the query — they only increase response size

### Diagnostics and Health Monitoring

The `leindex_diagnostics` tool returns enriched health information:

```json
{
  "name": "leindex_diagnostics",
  "arguments": { "project_path": "/project" }
}
```

Response includes:
- `pdg_loaded`: Whether the PDG is in memory
- `pdg_estimated_bytes`: Estimated in-memory size of the PDG (`nodes×200 + edges×64`)
- `search_index_nodes`: Number of nodes in the search index
- `index_health`: Overall health status (`"healthy"`, `"stale"`, or `"empty"`)
- `cache_entries`, `cache_bytes`: LRU cache utilization
- `rss_bytes`, `total_bytes`: Process and system memory
- `spilled_entries`, `spilled_bytes`: Disk-spilled cache entries

> **Note:** The PDG and search index are stored directly in the `LeIndex` instance,
> not in the LRU cache. This means `cache_bytes` may be 0 while the PDG is fully loaded.
> Use `pdg_estimated_bytes` and `search_index_nodes` for accurate in-memory data size.

### Performance Tips

1. **Index Once, Search Many**: Indexing is expensive; searches are fast
2. **Use `token_budget`**: Limit context expansion for large codebases (see table above)
3. **Incremental Re-index**: Set `force_reindex: false` to skip unchanged files
4. **SSE for Large Projects**: Use streaming for projects with 1000+ files
5. **Paginate Large Results**: Use `offset`/`limit` on `grep_symbols`, `project_map`, `search`
6. **Batch Symbol Lookups**: Use `symbols[]` array instead of multiple single calls
7. **Scope Your Queries**: Use `scope` on `grep_symbols` or `path` on `phase_analysis` to narrow results

---

## Authentication

LeIndex MCP server does not implement authentication. It is designed for local development use only.

**Security Recommendations:**
- Bind to `127.0.0.1` (default) for local-only access
- Use a reverse proxy with authentication if exposing remotely
- Run in a trusted network environment

---

## Design Decisions and Evaluated Proposals

### Distributed Indexing — Not Beneficial

**Evaluation:** Distributed indexing (partitioning parsing/analysis across multiple
machines) was evaluated and determined to be unnecessary for LeIndex's use case:

- **Indexing is already fast**: ~3-5 seconds for 500 files on a single machine
- **Incremental indexing eliminates re-work**: Only changed files are re-parsed (~50ms)
- **MCP is inherently single-process**: The stdio transport runs as a subprocess;
  adding distributed coordination would increase latency, not reduce it
- **No shared state benefits**: Each developer indexes their own project locally;
  there's no multi-user benefit from shared infrastructure
- **Complexity cost**: Distributed systems introduce failure modes (network partitions,
  coordinator availability) that are disproportionate to any throughput gain

**Recommendation:** Keep single-process architecture. If indexing speed becomes an
issue for very large monorepos (50k+ files), consider parallel parsing within the
process (already partially implemented via rayon) rather than cross-machine distribution.

### Advanced Analytics (Code Churn, Tech Debt) — Deferred

**Evaluation:** Code churn tracking and tech debt quantification were evaluated:

- **Code churn** requires `git log` analysis (commit frequency per file, author
  activity, change rate). This is orthogonal to tree-sitter parsing and would require
  a new data source (git2 integration already exists for other purposes)
- **Tech debt** quantification is inherently subjective and varies by team convention.
  Automated metrics (cyclomatic complexity, dependency depth, test coverage) are
  already partially covered by the existing `complexity` scores and hotspot detection
- **Existing coverage**: Phase analysis already identifies complexity hotspots,
  high-dependency modules, and potential risk areas — these serve the same purpose
  as tech debt metrics for most practical use cases

**Recommendation:** The existing complexity scoring and hotspot detection in
`leindex_phase_analysis` effectively address the need for tech debt awareness.
Code churn tracking could be added as a future `leindex_git_insights` tool using
the existing git2 dependency, but is not a priority for the MCP server.

### Cache Architecture — PDG Direct Storage

The PDG and search index are stored directly in the `LeIndex` instance fields, not
routed through the `CacheStore` LRU cache. This is by design:

- The PDG is the **primary data structure** — it's always needed and should never be
  evicted while the project is active
- The LRU cache is for **secondary/derived data** (analysis results, serialized
  snapshots for disk spilling)
- `cache_bytes = 0` in diagnostics is correct when no secondary data is cached;
  use `pdg_estimated_bytes` for actual in-memory data size

The warming strategies (All, PDGOnly, SearchIndexOnly, RecentFirst) handle the case
where the process restarts and needs to reload spilled data from disk.

---

## Versioning

The MCP server version matches the LeIndex crate version. Check version via:

```bash
leindex --version
```

Or via the health endpoint:

```bash
curl http://localhost:3000/health
# {"status":"ok","service":"leindex","version":"1.5.2"}
```
