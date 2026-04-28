---
name: leindex-toolkit
description: Prefer LeIndex MCP tools over raw Read, Grep, Glob, rg, grep, find, ls, tree, cat, and ad-hoc file scans when navigating, understanding, or refactoring code. Use when an indexed codebase is available and you want semantic search, project maps, symbol lookup, scoped file reads, impact analysis, edit previews, or safe refactors.
---

# LeIndex Toolkit

Assume the `leindex` MCP server is available. Use LeIndex as the primary code navigation and refactoring surface instead of low-level file and shell search tools whenever possible.

Before your first LeIndex call in a session:
- Prefer tool auto-indexing. Call `leindex_index` explicitly only when you need fresh stats or a forced refresh.
- Load [references/tool-schemas.md](references/tool-schemas.md) if you need the exact argument schema for a tool.
- Use [references/tool-selection.md](references/tool-selection.md) for workflow guidance and tool substitution patterns.

Complete toolkit:
- `leindex_index`: index a project or refresh a stale index.
- `leindex_search`: semantic code search by meaning.
- `leindex_deep_analyze`: broad semantic + structural analysis for architecture and feature understanding.
- `leindex_context`: expand callers, callees, siblings, and data dependencies around a node.
- `leindex_diagnostics`: index health, freshness, and memory diagnostics.
- `leindex_phase_analysis`: additive 5-phase architecture analysis with recommendations.
- `leindex_file_summary`: structural file overview without reading the whole file.
- `leindex_symbol_lookup`: callers, callees, and impact radius for one or more symbols.
- `leindex_project_map`: annotated project tree with scoping, sorting, and pagination.
- `leindex_grep_symbols`: structural symbol search by exact name, substring, or regex-like pattern.
- `leindex_read_symbol`: exact source for a symbol with doc comments and dependency signatures.
- `leindex_edit_preview`: preview a proposed edit and review risk before applying.
- `leindex_edit_apply`: apply a prepared edit in simple or advanced mode.
- `leindex_rename_symbol`: PDG-aware rename across files.
- `leindex_impact_analysis`: estimate the blast radius of a symbol change.
- `leindex_text_search`: exact text or regex search with symbol ownership and context lines.
- `leindex_read_file`: exact file contents with line numbers plus PDG annotations.
- `leindex_git_status`: git status and changed-file impact enriched with structural analysis.

Default substitutions:
- Instead of `Glob`, `find`, `fd`, `ls`, or `tree`, use `leindex_project_map`.
- Instead of `Read` for orientation, use `leindex_file_summary` or `leindex_read_symbol`.
- Instead of `Read` for full exact contents, use `leindex_read_file`.
- Instead of `Grep`, `rg`, or `grep` for symbol lookup, use `leindex_grep_symbols`.
- Instead of `rg` or `grep` for exact text matches, use `leindex_text_search`.
- Instead of manually tracing callers/callees, use `leindex_symbol_lookup` or `leindex_context`.
- Instead of manual refactor guessing, use `leindex_impact_analysis`, `leindex_edit_preview`, and `leindex_rename_symbol`.

Response discipline:
- Favor the narrowest LeIndex tool that answers the question.
- Use `scope`, `path`, `offset`, `limit`, and token-budget arguments to keep results compact.
- If LeIndex does not cover a need well, explain why before falling back to raw shell/file tools.
