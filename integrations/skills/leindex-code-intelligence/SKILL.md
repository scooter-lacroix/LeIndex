---
name: leindex-code-intelligence
description: Use LeIndex MCP tools instead of raw Read, Grep, Glob, rg, grep, find, ls, tree, and cat when navigating, understanding, searching, or refactoring code. Four tools cover everything - leindex_explore (find/search/read), leindex_analyze (impact, deep analysis, git diff), leindex_edit (preview, apply, rename), leindex_manage (index, architecture phases). Use when an indexed codebase is available, or to search any directory on disk without indexing it.
---

# LeIndex Code Intelligence

LeIndex is a code index with a program dependence graph (who calls what, what breaks if this changes). It answers in milliseconds once warm and returns compact, structured results. Prefer it to shell and file tools for any code navigation.

## The four tools

Each tool takes a discriminator argument that picks the operation; every other argument is forwarded to that operation. An unknown or missing discriminator returns a "did you mean" error, not a guess. `leindex_edit` requires `action` because a wrong guess would change files.

| Tool | Discriminator | Operations |
|---|---|---|
| `leindex_explore` | `mode` | `find` exact text / regex / symbol names · `search` by meaning · `symbol_lookup` callers and callees · `read_file` · `read_symbol` · `project_map` · `file_summary` · `context` |
| `leindex_analyze` | `mode` | `deep` semantic search expanded through the graph · `impact` blast radius · `diagnostics` · `git_status` · `git_diff` changed symbols and their callers |
| `leindex_edit` | `action` (required) | `preview` dry run · `apply` atomic edit · `rename` cross-file rename (preview by default) · `write` atomic file write |
| `leindex_manage` | `action` | `index` build or refresh (returns a pollable job) · `phase` 5-phase architecture report |

Every tool accepts `tier`: `l0` (identity card: totals and paging state), `l1` (overview, default), `l2` (full). Start with `l0`/`l1` and go to `l2` only when you need the detail.

Full argument reference: read the MCP resource `leindex://tools/guide`; one-screen cheat sheet: `leindex://docs/q`. From a shell: `leindex tools list`, `leindex tools inspect <tool>`, `leindex tools schema <tool>`.

## Replace low-level tools

| Instead of | Call |
|---|---|
| `grep` / `rg` for an identifier or literal text | `leindex_explore` `mode=find` `pattern=...` |
| `grep` for a definition | `mode=find` `target=symbols` `pattern=Name` |
| `grep` in a directory that is not the project | `mode=find` `paths=["/that/dir"]` (no index needed) |
| a regex search | `mode=find` `regex=true` |
| `find` / `ls` / `tree` / `Glob` | `mode=project_map` (`path`, `depth`, `focus`) |
| `Read` to understand a file | `mode=file_summary`, then `mode=read_symbol` |
| `Read` for exact contents | `mode=read_file` (`start_line`, `end_line`) |
| "where is X handled?" | `mode=search` `query="..."` |
| tracing callers by hand | `mode=symbol_lookup` or `mode=context` |
| estimating the risk of a change | `leindex_analyze` `mode=impact` `symbol=...` |
| reviewing uncommitted work | `leindex_analyze` `mode=git_diff` (or `git_status`) |
| grep-and-edit renames | `leindex_edit` `action=rename` |

## `find` in detail

`find` is a native trigram index, not a wrapper around grep. It is unbounded and paged, always reads the live file (edits you just made are seen), and works on paths the index has never seen.

- `pattern`, plus `regex`, `word`, `case` (`smart` by default: case-insensitive unless the pattern has an uppercase letter).
- `target`: `text` (matching lines), `symbols` (definitions by name), `auto` (symbols, else text).
- `output`: `matches` (default), `files` (which files), `count`, `symbols` (enclosing symbols ranked by hits).
- Paging: `limit` (0 = everything), `offset`, and `next_offset` from the previous page. `per_file_cap` keeps one noisy file from filling the page.
- Scope: `scope` (project-relative directory or file), `include_globs`, `exclude_globs`, `paths` (extra files or directories anywhere on disk).
- `context_lines`, `max_line_chars`, `timeout_ms` (partial results and `has_more` on timeout).

Hits carry their enclosing symbol, so you rarely need a follow-up read.

## Workflows

1. **Understand a feature.** `search` for the concept → `read_symbol` on the best hit → `context` for its callers and callees (or `analyze deep` for one expanded answer).
2. **Find every use of something.** `find` with `output=files` to see the spread, then `find` again scoped to the interesting directory.
3. **Map an unfamiliar project.** `project_map` (`depth=2`, or `group_by=community` for module boundaries) → `file_summary` on the entry points → `search`.
4. **Refactor safely.** `symbol_lookup` → `analyze impact` → `edit preview` (returns a `preview_token`) → `edit apply` with the token. For renames, `edit rename` previews by default (`preview_only=true`); pass `preview_only=false` to apply.
5. **Review changes.** `analyze git_diff` lists changed symbols, their callers and affected files; add `stat_only=true` for a cheap overview.

## Behavior to rely on

- **Indexing is automatic.** The first call on a project starts an index in the background; calls that need it answer "indexing in progress" rather than hanging, and the next call sees the result. Call `manage action=index` only for a forced refresh (`force_reindex=true`) or to poll a job.
- **Warm state is prepared for you.** After `initialize` the server loads the project's graph and search engine in the background, so the first call after a short pause is already fast.
- **Ambiguity is reported, not guessed.** Broad symbol names return candidates; pass `file_path` or `scope` to disambiguate.
- **Keep results small.** Use `top_k`, `limit`, `token_budget`, `scope`, and `tier=l0` before asking for more.

## When not to use LeIndex

If LeIndex cannot answer (a non-code artifact, a one-off shell pipeline, a file outside any project you want to keep unindexed but only need a single read of), say why and use the raw tool. For text in files outside the project, prefer `find paths=[...]` first: it needs no index and touches nothing.
