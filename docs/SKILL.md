# LeIndex MCP Usage Skill

A comprehensive guide for using LeIndex effectively with MCP (Model Context Protocol).

## Overview

LeIndex provides semantic code search and analysis capabilities through MCP tools. This skill document explains when to use each tool and how to combine them for effective code investigation.

## Tool Selection Guide

LeIndex exposes **four tools**; a `mode` (explore, analyze) or `action` (edit,
manage) argument picks the operation and every other argument is forwarded to it.
The rest of this guide names operations by their original tool names
(`leindex_context`, `leindex_edit_apply`, …) — those names still work as direct
calls, and each is a branch of one of the four:

| Original name | Call it as |
|---|---|
| `leindex_search` | `leindex_explore` `mode=search` |
| `leindex_grep_symbols`, `leindex_text_search` | `leindex_explore` `mode=find` |
| `leindex_symbol_lookup`, `_read_file`, `_read_symbol`, `_project_map`, `_file_summary`, `_context` | `leindex_explore` `mode=…` |
| `leindex_deep_analyze`, `_impact_analysis`, `_diagnostics`, `_git_status` | `leindex_analyze` `mode=deep` / `impact` / `diagnostics` / `git_status` |
| *(new)* PDG-enriched diff | `leindex_analyze` `mode=git_diff` |
| `leindex_edit_preview`, `_edit_apply`, `_rename_symbol`, `_write` | `leindex_edit` `action=preview` / `apply` / `rename` / `write` |
| `leindex_index`, `leindex_phase_analysis` | `leindex_manage` `action=index` / `phase` |

Full argument reference: the `leindex://tools/guide` resource; one-screen cheat
sheet: `leindex://docs/q`.

### Quick Reference Table

| What you want to do | Call | Why |
|---------------------|------|-----|
| Find code by meaning | `leindex_explore` `mode=search` | Semantic search understands intent |
| Find exact text, a regex, or a symbol by name | `leindex_explore` `mode=find` | Millisecond, always live; works on any path, even unindexed |
| Understand a symbol deeply | `leindex_analyze` `mode=deep` | Full PDG + semantic analysis |
| See how a symbol is used | `leindex_explore` `mode=context` | Shows callers, callees, dependencies |
| Read a file with context | `leindex_explore` `mode=read_file` | PDG-annotated file contents |
| Get file overview | `leindex_explore` `mode=file_summary` | Structural summary without full content |
| Find where a symbol is defined | `leindex_explore` `mode=symbol_lookup` | Direct symbol navigation |
| See project structure | `leindex_explore` `mode=project_map` | Annotated project tree |
| Check for impacts | `leindex_analyze` `mode=impact` | Transitive dependency analysis |
| Review what changed | `leindex_analyze` `mode=git_diff` | Changed symbols and their callers |
| Preview edits | `leindex_edit` `action=preview` | See changes before applying |
| Apply edits | `leindex_edit` `action=apply` | Safe code modifications |
| Rename symbols | `leindex_edit` `action=rename` | Cross-file renaming |
| Check git status | `leindex_analyze` `mode=git_status` | PDG-aware git operations |

### When to use `search` vs `find`

**Use `search`** (semantic) when:
- You know what you want to find but not the exact name
- You're exploring unfamiliar code
- Your query is conceptual ("how is authentication handled")
- You want semantic similarity, not exact matches

**Use `find`** when:
- You know the exact text, identifier, or a regex
- You want every occurrence, not the top-k (`limit=0`, or page with `offset`)
- You need definitions by name (`target=symbols`), or matches grouped by enclosing symbol
- The files are outside the project — pass `paths: ["/any/dir"]`; no index needed

**Example workflow:**
```
1. User: "How does authentication work?"
   → Use: leindex_search with query "authentication flow"

2. Found: User::authenticate method
   → Use: leindex_deep_analyze on "User::authenticate"

3. Need to see all callers
   → Use: leindex_context on "User::authenticate"
```

### When to use `leindex_deep_analyze` vs `leindex_context`

**Use `leindex_deep_analyze`** when:
- You need comprehensive understanding of a symbol
- You want semantic summary + structural data + PDG
- You're investigating complex logic
- You need recommendations for next steps

**Use `leindex_context`** when:
- You want to expand from a specific symbol
- You need callers, callees, and dependencies
- You're tracing data flow
- You want focused, targeted information

**Example workflow:**
```
1. User: "Explain the error handling in User::login"
   → Use: leindex_deep_analyze on "User::login"

2. Found: Several error conditions
   → Use: leindex_context on specific error handling methods

3. Want to see error definitions
   → Use: leindex_read_symbol on error types
```

## Auto-Indexing Behavior

LeIndex **automatically indexes projects on first use**. You don't need to manually index before searching.

### How it works:
1. First tool call on a project path triggers indexing
2. Index is cached for subsequent calls
3. Use `force_reindex: true` to refresh the index

### Best practices:
- Let auto-indexing work - don't manually index unless necessary
- Use `force_reindex` after major code changes
- Check `leindex_diagnostics` for index status

## Recommended Investigation Workflows

### Workflow 1: Understanding a Feature

**Goal:** Understand how a feature works end-to-end

**Steps:**
1. **Search for entry points**
   ```json
   {
     "name": "leindex_search",
     "arguments": {
       "query": "feature X entry point API"
     }
   }
   ```

2. **Analyze main component**
   ```json
   {
     "name": "leindex_deep_analyze",
     "arguments": {
       "query": "FeatureXController"
     }
   }
   ```

3. **Trace data flow**
   ```json
   {
     "name": "leindex_context",
     "arguments": {
       "symbol_id": "FeatureXController::process"
     }
   }
   ```

4. **Read key files**
   ```json
   {
     "name": "leindex_read_file",
     "arguments": {
       "path": "/path/to/feature_x.rs"
     }
   }
   ```

### Workflow 2: Debugging an Issue

**Goal:** Find the root cause of a bug

**Steps:**
1. **Search for error location**
   ```json
   {
     "name": "leindex_explore",
     "arguments": {
       "mode": "find",
       "pattern": "error|exception|panic",
       "language": "rust"
     }
   }
   ```

2. **Analyze error handling**
   ```json
   {
     "name": "leindex_deep_analyze",
     "arguments": {
       "query": "ErrorHandler::handle"
     }
   }
   ```

3. **Check impact**
   ```json
   {
     "name": "leindex_impact_analysis",
     "arguments": {
       "symbol_id": "ErrorHandler::handle"
     }
   }
   ```

4. **Read relevant code**
   ```json
   {
     "name": "leindex_read_symbol",
     "arguments": {
       "symbol_id": "suspect_function"
     }
   }
   ```

### Workflow 3: Code Review

**Goal:** Review changes and their impact

**Steps:**
1. **Check git status**
   ```json
   {
     "name": "leindex_git_status",
     "arguments": {
       "project_path": "/path/to/project"
     }
   }
   ```

2. **Analyze changed symbols**
   ```json
   {
     "name": "leindex_deep_analyze",
     "arguments": {
       "query": "changed_symbol_name"
     }
   }
   ```

3. **Check impact of changes**
   ```json
   {
     "name": "leindex_impact_analysis",
     "arguments": {
       "symbol_id": "changed_symbol"
     }
   }
   ```

4. **Preview any fixes**
   ```json
   {
     "name": "leindex_edit_preview",
     "arguments": {
       "path": "/path/to/file.rs",
       "old_string": "old code",
       "new_string": "new code"
     }
   }
   ```

## Advanced Techniques

### Combining Tools

**Pattern: Search → Analyze → Context → Read**
```
1. leindex_search (find candidates)
2. leindex_deep_analyze (understand best candidate)
3. leindex_context (expand understanding)
4. leindex_read_file (read implementation)
```

**Pattern: Symbol → Impact → Edit**
```
1. leindex_symbol_lookup (find symbol)
2. leindex_impact_analysis (check effects)
3. leindex_edit_preview (plan change)
4. leindex_edit_apply (apply change)
```

### Using Phase Analysis

For comprehensive project understanding, use the 5-phase analysis:

```json
{
  "name": "leindex_phase_analysis",
  "arguments": {
    "project_path": "/path/to/project",
    "phases": ["phase1", "phase2", "phase3", "phase4", "phase5"]
  }
}
```

**Phases explained:**
- **Phase 1:** File discovery and metadata
- **Phase 2:** Symbol extraction and indexing
- **Phase 3:** Cross-reference resolution
- **Phase 4:** Semantic analysis and embeddings
- **Phase 5:** Documentation generation

## Common Pitfalls

### 1. Over-searching
Don't search repeatedly with slight variations. Use `leindex_context` to expand from good results.

### 2. Ignoring auto-indexing
Don't manually index unless necessary. Trust auto-indexing for most use cases.

### 3. Not using context
Always use `leindex_context` after finding a relevant symbol to understand how it's used.

### 4. Reading whole files
Use `leindex_file_summary` first to understand structure, then `leindex_read_file` for specific sections.

## Tips for Effective Use

1. **Start broad, then narrow:** Use search first, then specific tools
2. **Follow the PDG:** Program Dependence Graph shows true code relationships
3. **Use semantic queries:** Natural language works better than exact patterns
4. **Check diagnostics:** Use `leindex_diagnostics` to verify system health
5. **Let it cache:** Don't force reindex unless code has changed significantly

## MCP Prompts

Use these prompts for quick assistance:

- **`quickstart`** - Get started with LeIndex basics
- **`investigation_workflow`** - Step-by-step investigation guide

## MCP Resources

Access these resources for detailed information:

- **`leindex://docs/quickstart`** - Quickstart guide
- **`leindex://docs/server-config`** - Server configuration reference

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `LEINDEX_HOME` | Storage directory | `~/.leindex` |
| `LEINDEX_PORT` | Server port | `47268` |

## Getting Help

- Use the `quickstart` prompt for immediate help
- Read the `leindex://docs/quickstart` resource
- Check tool descriptions with `tools/list`
- Use `leindex_diagnostics` to troubleshoot issues
