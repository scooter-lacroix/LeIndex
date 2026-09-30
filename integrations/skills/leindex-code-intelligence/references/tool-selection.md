# LeIndex Tool Selection

Use this reference when deciding which LeIndex call should replace a generic navigation action. Every call is one of four tools with a discriminator argument (`mode` or `action`).

| If you were about to use | Call | Why |
|---|---|---|
| `Glob`, `find`, `fd`, `ls`, `tree` | `leindex_explore` `mode=project_map` | Indexed tree with scope, sort, complexity, pagination and `focus` ranking. |
| `Read` to understand a file | `leindex_explore` `mode=file_summary` | Structure, symbols and dependencies without full-file token cost. |
| `Read` for exact contents | `leindex_explore` `mode=read_file` | Exact text with line numbers; `include_symbol_map=true` adds graph annotations. |
| `Read` for one function, type or class | `leindex_explore` `mode=read_symbol` | Reads only that symbol. |
| `grep`, `rg`, `git grep` for text or a regex | `leindex_explore` `mode=find` | Millisecond trigram search, unbounded and paged, always live, enclosing symbol per hit. |
| `grep` for a definition by name | `leindex_explore` `mode=find` `target=symbols` | Definitions, exact matches first. |
| `grep -r` in a directory outside the project | `leindex_explore` `mode=find` `paths=[...]` | Parallel live scan; no index, no side effects. |
| Broad "where is this handled?" | `leindex_explore` `mode=search` | Ranked semantic + structural retrieval. |
| "How does this feature work?" | `leindex_analyze` `mode=deep` | Semantic search expanded through the dependence graph. |
| Manual caller/callee tracing | `leindex_explore` `mode=symbol_lookup` or `mode=context` | Structural relationships returned directly. |
| Estimating change risk | `leindex_analyze` `mode=impact` | Transitive blast radius and risk. |
| `git diff` / `git status` during review | `leindex_analyze` `mode=git_diff` / `git_status` | Changed hunks mapped to symbols, callers and affected files. |
| Grep + sed for a rename | `leindex_edit` `action=rename` | Reference sites found and updated together; previews first. |
| Hand-editing without review | `leindex_edit` `action=preview`, then `action=apply` | Diff, breaking changes and affected files first; the token binds apply to the preview. |
| Writing a whole file | `leindex_edit` `action=write` | Atomic write. |
| Checking or forcing the index | `leindex_analyze` `mode=diagnostics`, `leindex_manage` `action=index` | Freshness, sizes, job polling. |
| Architecture overview | `leindex_manage` `action=phase` | Cached 5-phase report; near-instant when nothing changed. |

## Recommended workflows

1. **Understand a feature** — `explore search` → `explore read_symbol` → `explore context` (or `analyze deep`).
2. **Map an unfamiliar project** — `explore project_map` → `explore file_summary` → `explore search`.
3. **Prepare a refactor** — `explore symbol_lookup` → `analyze impact` → `edit preview` → `edit apply` (or `edit rename`).
4. **Investigate exact text or config usage** — `explore find` (`output=files` first if the spread is unknown) → `explore read_file`.
5. **Review a change set** — `analyze git_diff` → `analyze impact` on the riskiest symbol.

Pin a request to a repository with `project_path`. Narrow with `scope`, `path`, `limit`, `top_k` and `tier=l0` before asking for more.
