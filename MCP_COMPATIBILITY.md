# LeIndex MCP Compatibility

Model Context Protocol (MCP) server integration guide for LeIndex v0.1.0

---

## Overview

LeIndex includes a built-in MCP server that provides AI assistants (like Claude Code, Cursor, Windsurf) with intelligent code search and analysis capabilities.

### Quick Start

```bash
# Start the MCP server
leindex mcp
```

Configure your MCP client to connect to the LeIndex MCP server.

---

## Configuration

### Claude Code

Add to your Claude Code MCP configuration:

**macOS/Linux:** `~/.config/claude-code/mcp_servers.json`
**Windows:** `%APPDATA%\claude-code\mcp_servers.json`

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {}
    }
  }
}
```

### Cursor

Add to Cursor settings (`settings.json`):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {}
    }
  }
}
```

### Windsurf

Add to Windsurf MCP configuration:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {}
    }
  }
}
```

---

## Available MCP Tools

### leindex_index

Index a project for code search.

**Parameters:**
- `project_path` (string, required): Path to the project directory
- `config` (object, optional): Override configuration options

**Example:**
```json
{
  "project_path": "/home/user/my-project",
  "config": {
    "memory": {
      "total_budget_mb": 2048
    }
  }
}
```

**Returns:** Indexing status and statistics

---

### leindex_search

Perform semantic code search.

**Parameters:**
- `query` (string, required): Search query
- `project_path` (string, optional): Limit search to specific project
- `file_patterns` (array, optional): Filter by file patterns (e.g., `["*.py"]`)
- `exclude_patterns` (array, optional): Exclude file patterns
- `limit` (integer, optional): Maximum results (default: 10)

**Example:**
```json
{
  "query": "database connection handling",
  "file_patterns": ["*.rs", "*.toml"],
  "limit": 20
}
```

**Returns:** Search results with file paths, line numbers, and relevance scores

---

### leindex_deep_analyze

Perform deep code analysis with PDG (Program Dependence Graph).

**Parameters:**
- `file_path` (string, required): Path to the file to analyze
- `symbol_name` (string, optional): Specific symbol/function to analyze

**Example:**
```json
{
  "file_path": "/home/user/project/src/main.rs",
  "symbol_name": "process_request"
}
```

**Returns:** Analysis results including:
- Function dependencies
- Data flow information
- Control flow graph
- Related symbols

---

### leindex_context

Expand context around a code location.

**Parameters:**
- `file_path` (string, required): Path to the file
- `line_number` (integer, required): Line number
- `context_lines` (integer, optional): Number of context lines (default: 10)

**Example:**
```json
{
  "file_path": "/home/user/project/src/lib.rs",
  "line_number": 42,
  "context_lines": 20
}
```

**Returns:** Context window with surrounding code

---

### leindex_diagnostics

Get system health and diagnostic information.

**Parameters:** None

**Returns:** System status including:
- LeIndex version
- Memory usage
- Indexed projects
- Parser status
- Performance metrics

---

## Tool Compatibility Matrix

| AI Tool | leindex_index | leindex_search | leindex_deep_analyze | leindex_context | leindex_diagnostics |
|---------|---------------|----------------|----------------------|-----------------|---------------------|
| **Claude Code** | ✅ Verified | ✅ Verified | ✅ Verified | ✅ Verified | ✅ Verified |
| **Cursor** | ✅ Verified | ✅ Verified | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending |
| **Windsurf** | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending |
| **Cline** | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending |

**Legend:**
- ✅ Verified - Tested and confirmed working
- ⚠️ Pending - Not yet tested

---

## Common Use Cases

### 1. Find Similar Code

**Scenario:** Find implementations of authentication logic

```json
{
  "tool": "leindex_search",
  "arguments": {
    "query": "authenticate user token validation",
    "file_patterns": ["*.rs"]
  }
}
```

### 2. Analyze Function Dependencies

**Scenario:** Understand what a function depends on

```json
{
  "tool": "leindex_deep_analyze",
  "arguments": {
    "file_path": "/project/src/auth.rs",
    "symbol_name": "validate_token"
  }
}
```

### 3. Get Context for a Bug

**Scenario:** Get surrounding code for a line number

```json
{
  "tool": "leindex_context",
  "arguments": {
    "file_path": "/project/src/database.rs",
    "line_number": 156,
    "context_lines": 30
  }
}
```

### 4. Index New Project

**Scenario:** Index a project for AI assistant

```json
{
  "tool": "leindex_index",
  "arguments": {
    "project_path": "/home/user/new-project"
  }
}
```

### 5. Check System Health

**Scenario:** Verify LeIndex is working

```json
{
  "tool": "leindex_diagnostics",
  "arguments": {}
}
```

---

## Troubleshooting

### MCP Server Not Starting

**Problem:** `leindex mcp` command fails

**Solutions:**
1. Check LeIndex installation: `leindex --version`
2. Verify Rust binary: `ls -l target/release/leindex`
3. Check logs: `cat ~/.leindex/logs/leindex.log`

### Tools Not Available

**Problem:** MCP client doesn't show LeIndex tools

**Solutions:**
1. Verify MCP configuration syntax
2. Restart MCP client
3. Check MCP server is running: `leindex mcp`
4. Review client logs for connection errors

### Permission Errors

**Problem:** Cannot access project files

**Solutions:**
1. Check file permissions: `ls -la /path/to/project`
2. Ensure LeIndex has read access
3. Try running with appropriate permissions

---

## Performance Tips

### 1. Optimize Search

- Use `file_patterns` to limit search scope
- Set reasonable `limit` values (10-20 is usually sufficient)
- Be specific with queries

### 2. Optimize Indexing

- Exclude large directories (node_modules, target, etc.)
- Configure memory budget appropriately
- Use project-specific configuration

### 3. Cache Results

The MCP server maintains internal caches for:
- Search results
- Context windows
- Analysis results

---

## Security Considerations

### File Access

LeIndex MCP server has access to:
- All files in indexed projects
- Configuration files in `~/.leindex/`

**Recommendations:**
- Only index trusted projects
- Review file permissions
- Use exclude patterns for sensitive data

### Network

LeIndex MCP server:
- Does **not** make network requests
- Runs entirely locally
- Does not send data externally

---

## Advanced Configuration

### Custom MCP Endpoint

By default, the MCP server uses stdio. For HTTP transport, configure:

```bash
# Start MCP server with HTTP
leindex mcp --transport http --port 8080
```

### Environment Variables

```bash
# Custom config directory
export LEINDEX_HOME=/custom/leindex

# Custom log level
export RUST_LOG=debug

# Custom memory budget
export LEINDEX_MEMORY_MB=4096
```

---

## Migration from Python v2.0.2

The MCP tool names have changed:

| Python v2.0.2 | Rust v0.1.0 | Notes |
|---------------|-------------|-------|
| `manage_project` | `leindex_index` | Same functionality |
| `search_content` | `leindex_search` | Same functionality |
| `get_diagnostics` | `leindex_diagnostics` | Same functionality |
| N/A | `leindex_deep_analyze` | **NEW** - PDG analysis |
| N/A | `leindex_context` | **NEW** - Context expansion |

**Configuration:** No changes needed - binary name is still `leindex`

---

## Version Compatibility

| LeIndex Version | MCP Protocol | Status |
|-----------------|--------------|--------|
| 0.1.0 | 1.0 | ✅ Current |
| 2.0.2 (Python) | 1.0 | ⚠️ Deprecated |

---

## Support

### Issues

Report MCP-related issues:
- GitHub: [https://github.com/scooter-lacroix/leindex/issues](https://github.com/scooter-lacroix/leindex/issues)
- Include: Client name, OS, error messages

### Documentation

- [Installation](INSTALLATION_RUST.md) - Setup guide
- [Architecture](ARCHITECTURE.md) - System design
- [Migration](MIGRATION.md) - From Python v2.0.2

---

**Happy indexing with AI!** 🤖

*Last Updated: 2025-01-26*
*LeIndex v0.1.0*
