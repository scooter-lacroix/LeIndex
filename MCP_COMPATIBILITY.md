# LeIndex MCP Compatibility

Model Context Protocol (MCP) server integration guide for LeIndex v0.1.0

---

## Overview

LeIndex includes a built-in MCP server that provides AI assistants (like Claude Code, Cursor, Windsurf) with intelligent code search and analysis capabilities.

### Quick Start

```bash
# Recommended published MCP entrypoint
npx -y @leindex/mcp
```

If you installed the full Rust binary and want to run it directly, `leindex mcp` still works.

---

## Configuration

### Claude Code

Add to your Claude Code MCP configuration:

**Global:** `~/.claude/settings.json`
**Project-local:** `.claude/settings.json`

```json
{
  "mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["-y", "@leindex/mcp"],
      "type": "stdio"
    }
  }
}
```

Optional guidance pack:
- Shared skill: `integrations/skills/leindex-code-intelligence/`
- Reminder hook example: `integrations/claude-code/settings.example.json`

### Cursor

Add to Cursor settings (`settings.json`):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["-y", "@leindex/mcp"],
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
      "command": "npx",
      "args": ["-y", "@leindex/mcp"],
      "env": {}
    }
  }
}
```

---

## Available MCP Tools

LeIndex advertises **four** tools. A discriminator argument (`mode` for explore
and analyze, `action` for edit and manage) picks the operation and every other
argument is forwarded to it, which keeps the tool list small enough for every
client below and leaves the model one decision per call. `tools/list` returns a
flat union schema by default (some LLM APIs reject a top-level `oneOf`);
`LEINDEX_MCP_SCHEMA=oneof` switches to per-branch `oneOf` schemas, and
`leindex tools schema <tool>` prints that form. The per-branch argument
reference is the `leindex://tools/guide` resource.

| Tool | Discriminator | Operations |
|------|---------------|------------|
| `leindex_explore` | `mode` | `search` (by meaning), `find` (exact text / regex / symbol names, any path), `symbol_lookup`, `read_file`, `read_symbol`, `project_map`, `file_summary`, `context` |
| `leindex_analyze` | `mode` | `deep`, `impact`, `diagnostics`, `git_status`, `git_diff` |
| `leindex_edit` | `action` (required) | `preview`, `apply`, `rename`, `write` |
| `leindex_manage` | `action` | `index`, `phase` |

Every tool accepts `tier` (`l0` identity card, `l1` overview, `l2` full).
The original per-operation names (`leindex_search`, `leindex_deep_analyze`, ...)
remain callable but are not advertised; `LEINDEX_MCP_LEGACY_TOOLS=1` advertises
them.

### leindex_explore

**Common parameters:** `mode`, `project_path` (optional), `tier`.

- `mode=search`: `query` (required), `top_k`, `offset`, `scope`, `search_mode`.
- `mode=find`: `pattern` (required), `regex`, `word`, `case`, `target`
  (`text` | `symbols` | `auto`), `output` (`matches` | `files` | `count` |
  `symbols`), `scope`, `paths` (extra files or directories anywhere on disk; no
  index needed), `include_globs`, `exclude_globs`, `context_lines`, `limit`
  (`0` = unbounded), `offset`, `per_file_cap`, `timeout_ms`.
- `mode=symbol_lookup`: `symbol` or `symbols`, `depth`, `include_source`.
- `mode=read_file`: `file_path` (required), `start_line`, `end_line`, `include_symbol_map`.
- `mode=read_symbol`: `symbol` (required), `file_path`.
- `mode=project_map`: `path`, `depth`, `focus`, `group_by`, `sort_by`, `limit`.
- `mode=file_summary`: `file_path` (required).
- `mode=context`: `node_id` (required), `token_budget`.

```json
{ "name": "leindex_explore", "arguments": { "mode": "find", "pattern": "handle_request", "output": "files" } }
```

### leindex_analyze

- `mode=deep`: `query` (required), `token_budget`.
- `mode=impact`: `symbol` (required), `change_type`, `depth`.
- `mode=diagnostics`: index health, sizes, cache and memory statistics.
- `mode=git_status`: repository state enriched with structural impact.
- `mode=git_diff`: `ref`, `range`, `staged`, `scope`, `stat_only`,
  `include_patch`; changed hunks mapped to symbols, callers and affected files.

### leindex_edit

`action` is required (an ambiguous call must not guess at a mutation).

- `action=preview`: `file_path`, `old_text`, `new_text` (or `changes`); returns a `preview_token`.
- `action=apply`: same arguments, or `preview_token`; `dry_run`.
- `action=rename`: `old_name`, `new_name`, `scope`, `preview_only` (default `true`).
- `action=write`: `file_path`, `content`.

### leindex_manage

- `action=index`: `project_path` (required), `force_reindex`, `wait`. Returns a pollable job unless `wait=true`.
- `action=phase`: 5-phase architecture analysis: `phase`, `mode`, `path`, `include_docs`.

---

## Tool Compatibility Matrix

| AI Tool | leindex_explore | leindex_analyze | leindex_edit | leindex_manage |
|---------|-----------------|-----------------|--------------|----------------|
| **Claude Code** | ✅ Verified | ✅ Verified | ✅ Verified | ✅ Verified |
| **Cursor** | ✅ Verified | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending |
| **Windsurf** | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending |
| **Cline** | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending | ⚠️ Pending |

**Legend:**
- ✅ Verified - Tested and confirmed working
- ⚠️ Pending - Not yet tested

---

## Common Use Cases

### 1. Find Similar Code

**Scenario:** Find implementations of authentication logic

```json
{
  "name": "leindex_explore",
  "arguments": { "mode": "search", "query": "authenticate user token validation", "scope": "src" }
}
```

### 2. Find Every Use of a Name

**Scenario:** List the files that mention `validate_token`, then read the hits

```json
{
  "name": "leindex_explore",
  "arguments": { "mode": "find", "pattern": "validate_token", "word": true, "output": "files" }
}
```

To search a directory that is not part of the project, add `"paths": ["/other/dir"]`; no index is built.

### 3. Analyze a Function's Dependencies

**Scenario:** Understand what a function depends on and what depends on it

```json
{
  "name": "leindex_analyze",
  "arguments": { "mode": "deep", "query": "validate_token" }
}
```

### 4. Check the Blast Radius of a Change

```json
{
  "name": "leindex_analyze",
  "arguments": { "mode": "impact", "symbol": "validate_token" }
}
```

### 5. Index a Project

**Scenario:** Force a refresh (first use indexes automatically)

```json
{
  "name": "leindex_manage",
  "arguments": { "action": "index", "project_path": "/home/user/new-project", "force_reindex": true }
}
```

### 6. Check System Health

```json
{
  "name": "leindex_analyze",
  "arguments": { "mode": "diagnostics" }
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

# Opt in to the Engram query-embedding phrase-book (docs/MCP.md)
export LEINDEX_FEATURE_ENGRAM=1

# Custom memory budget
export LEINDEX_MEMORY_MB=4096
```

---

## Migration from Python v2.0.2

The MCP tool names have changed:

| Python v2.0.2 | Rust v2.0 | Notes |
|---------------|-----------|-------|
| `manage_project` | `leindex_manage` `action=index` | Same functionality |
| `search_content` | `leindex_explore` `mode=search` / `mode=find` | Semantic search; exact text and regex via `find` |
| `get_diagnostics` | `leindex_analyze` `mode=diagnostics` | Same functionality |
| N/A | `leindex_analyze` `mode=deep` | **NEW** - PDG analysis |
| N/A | `leindex_explore` `mode=context` | **NEW** - Context expansion |

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
