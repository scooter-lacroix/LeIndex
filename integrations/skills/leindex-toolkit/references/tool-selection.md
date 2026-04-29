# LeIndex Tool Selection

Use this reference when deciding which LeIndex tool should replace a generic navigation action.

Common substitutions:

| If you were about to use | Prefer this LeIndex tool | Why |
|---|---|---|
| `Glob`, `find`, `fd`, `ls`, `tree` | `leindex_project_map` | Returns the indexed file tree with scope, sort, complexity, and pagination. |
| `Read` just to understand a file | `leindex_file_summary` | Gives structure, symbols, and dependencies without full-file token cost. |
| `Read` for exact file contents | `leindex_read_file` | Returns exact text plus line numbers and symbol/import overlays. |
| `Read` for one function/type/class | `leindex_read_symbol` | Reads only the symbol you need. |
| `grep`, `rg`, `git grep` for symbols | `leindex_grep_symbols` | Structural symbol search with type and complexity. |
| `grep`, `rg`, `git grep` for raw text | `leindex_text_search` | Exact matches with context and owning symbol information. |
| Broad codebase search | `leindex_search` | Semantic retrieval by intent. |
| “How does this feature work?” | `leindex_deep_analyze` | Combines semantic search with PDG traversal. |
| Manual caller/callee tracing | `leindex_symbol_lookup` or `leindex_context` | Returns structural relationships directly. |
| Manual change-risk estimation | `leindex_impact_analysis` | Computes blast radius and risk. |
| Grep + edit for rename | `leindex_rename_symbol` | Finds and updates reference sites atomically. |
| Hand-editing without review | `leindex_edit_preview` | Shows diff, breaking changes, and affected files first. |
| Raw `git status` during review | `leindex_git_status` | Maps changed files to affected symbols and impact. |

Recommended workflows:

1. Understand a feature
- `leindex_search`
- `leindex_read_symbol`
- `leindex_context` or `leindex_deep_analyze`

2. Map an unfamiliar project
- `leindex_project_map`
- `leindex_file_summary`
- `leindex_search`

3. Prepare a refactor
- `leindex_symbol_lookup`
- `leindex_impact_analysis`
- `leindex_edit_preview`
- `leindex_edit_apply` or `leindex_rename_symbol`

4. Investigate exact text/config usage
- `leindex_text_search`
- `leindex_read_file`

Use `project_path` whenever you need to pin the request to a specific repository. Use `scope` or `path` when a project is large and you want local context first.
