# LeIndex MCP Usage Skill

A comprehensive guide for using LeIndex effectively with MCP (Model Context Protocol).

## Overview

LeIndex provides semantic code search and analysis capabilities through MCP tools. This skill document explains when to use each tool and how to combine them for effective code investigation.

## Tool Selection Guide

### Quick Reference Table

| What you want to do | Tool to use | Why |
|---------------------|-------------|-----|
| Find code by meaning | `leindex_search` | Semantic search understands intent |
| Find symbols by name | `leindex_grep_symbols` | Fast structural search |
| Understand a symbol deeply | `leindex_deep_analyze` | Full PDG + semantic analysis |
| See how a symbol is used | `leindex_context` | Shows callers, callees, dependencies |
| Read a file with context | `leindex_read_file` | PDG-annotated file contents |
| Get file overview | `leindex_file_summary` | Structural summary without full content |
| Find where a symbol is defined | `leindex_symbol_lookup` | Direct symbol navigation |
| See project structure | `leindex_project_map` | Annotated project tree |
| Check for impacts | `leindex_impact_analysis` | Transitive dependency analysis |
| Preview edits | `leindex_edit_preview` | See changes before applying |
| Apply edits | `leindex_edit_apply` | Safe code modifications |
| Rename symbols | `leindex_rename_symbol` | Cross-file renaming |
| Check git status | `leindex_git_status` | PDG-aware git operations |

### When to use `leindex_search` vs `leindex_grep_symbols`

**Use `leindex_search`** when:
- You know what you want to find but not the exact name
- You're exploring unfamiliar code
- Your query is conceptual ("how is authentication handled")
- You want semantic similarity, not exact matches

**Use `leindex_grep_symbols`** when:
- You know the exact symbol name
- You want fast structural lookup
- You're searching for patterns in symbol names
- You need precise symbol navigation

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
     "name": "leindex_grep_symbols",
     "arguments": {
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
