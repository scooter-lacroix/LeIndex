---
name: leindex
description: AI-powered code search and analysis engine using semantic search and Program Dependence Graph traversal.
---

# LeIndex Skill

## Overview
LeIndex provides high-performance semantic search and deep code analysis by building a Program Dependence Graph (PDG) of your codebase.

## Core Commands
- `leindex index [path]` - Index a project for code search and analysis.
- `leindex search <query>` - Search indexed code using semantic search.
- `leindex analyze <query>` - Perform deep code analysis with context expansion.
- `leindex context <node_id>` - Expand context around a specific code node.
- `leindex diagnostics` - Get diagnostic information about the indexed project.

## Workflow
1. **Index**: Use `leindex index` to process your codebase. This is a one-time operation per project (incremental updates supported).
2. **Search**: Use `leindex search` to find relevant code snippets based on natural language queries.
3. **Analyze**: Use `leindex analyze` for complex questions about code behavior and relationships.
4. **Context**: Use `leindex context` when you need to understand the surroundings of a specific function or class found via search.

## MCP tools

When the LeIndex MCP server is connected, five routers cover the same ground: `leindex_explore` (`mode`: search, find, structural, symbol_lookup, read_file, read_symbol, project_map, file_summary, context), `leindex_analyze` (`mode`: deep, impact, diagnostics, phase), `leindex_edit` (`action`: preview, apply, rename, write), `leindex_manage` (`action`: index, state, projects, evict, lease, cleanup, all index lifecycle) and `leindex_git` (`action`: status, diff, log, show, blame, branch, remote, plus preview-only mutations that never apply). Use `mode=find` for exact text, regex and symbol names, including directories outside the project (`paths`); use `mode=structural` for AST patterns. See `integrations/skills/leindex-code-intelligence/` for the full guide.

## Integration
This skill works by calling the `leindex` CLI binary.

## Documentation
For more information, see the LeIndex repository at `https://github.com/scooter-lacroix/LeIndex`, and the shared skill pack at `integrations/skills/leindex-code-intelligence/`.
