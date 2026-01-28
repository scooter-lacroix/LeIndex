# LeIndex Skill

AI-powered code search and analysis engine.

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

## Integration
This skill works by calling the `leindex` CLI binary or communicating with the LeIndex MCP server.
