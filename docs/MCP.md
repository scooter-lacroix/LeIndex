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

Search indexed code using semantic search. Returns the most relevant code snippets matching your query.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Search query (e.g., 'authentication', 'database connection')"
    },
    "top_k": {
      "type": "integer",
      "description": "Maximum number of results to return (default: 10)",
      "default": 10,
      "minimum": 1,
      "maximum": 100
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

Perform deep code analysis with context expansion. Uses semantic search combined with Program Dependence Graph traversal to provide comprehensive understanding.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Analysis query (e.g., 'How does authentication work?', 'Where is user data stored?')"
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

Expand context around a specific code node using Program Dependence Graph traversal. Useful for understanding code relationships.

**Parameters:**

```json
{
  "type": "object",
  "properties": {
    "node_id": {
      "type": "string",
      "description": "Node ID to expand context around"
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
  "properties": {},
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

Add to your `.claude/settings.json`:

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

Or for HTTP mode:

```json
{
  "mcpServers": {
    "leindex": {
      "url": "http://localhost:3000/mcp"
    }
  }
}
```

**Project-specific configuration** (`.claude/settings.local.json`):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp", "--stdio"],
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

### Error Response Example

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32002,
    "message": "Project not indexed",
    "data": {
      "project": "/home/user/my-project",
      "suggestion": "Run leindex_index first"
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

### Performance Tips

1. **Index Once, Search Many**: Indexing is expensive; searches are fast
2. **Use `token_budget`**: Limit context expansion for large codebases
3. **Incremental Re-index**: Set `force_reindex: false` to skip unchanged files
4. **SSE for Large Projects**: Use streaming for projects with 1000+ files

---

## Authentication

LeIndex MCP server does not implement authentication. It is designed for local development use only.

**Security Recommendations:**
- Bind to `127.0.0.1` (default) for local-only access
- Use a reverse proxy with authentication if exposing remotely
- Run in a trusted network environment

---

## Versioning

The MCP server version matches the LeIndex crate version. Check version via:

```bash
leindex --version
```

Or via the health endpoint:

```bash
curl http://localhost:3000/health
# {"status":"ok","service":"leindex-mcp-server","version":"0.1.0"}
```
