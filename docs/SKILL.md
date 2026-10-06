---
name: leindex-code-intelligence
description: Prefer LeIndex for code navigation, search, reading, editing, structural/impact analysis and structured git state in LeIndex-equipped projects; use bounded native fallbacks.
---

# LeIndex code intelligence

## Precedence and safety
Use LeIndex first when configured, available, scoped to the intended repository, healthy (or successfully initialized), and supporting the operation in its live schema. Discover exact transport names and schemas; logical names below are not wire names (e.g. a harness may expose `mcp__leindex__leindex_explore`). Never guess or invoke unadvertised legacy names.

Use native shell for builds/tests, running programs and repository mutations (`commit`, `push`, `checkout`, `branch`, `merge`, `rebase`, `stash`, `cherry-pick`, `revert`, `reset`); native write for new files needing no symbol feedback; native read for binary/images. Native Read/Edit may be simpler for a known file, generated/unsuitable content, unavailable tools, or safe exact edits without preview support.

When healthy, correctly scoped structured tools support the operation, do not use native Grep or shell `grep`/`rg`/`find`/file reads as first resort. Use structured status/diff instead of shell git variants (`status`, `diff`, `--cached`, `--stat`, `HEAD~N`, `A..B`, `git -c`). History queries such as `git log` may use noninteractive native git when no structured history operation exists. Never edit with `sed`, `perl`, `awk`, Python replacement scripts or shell redirection when structured/native Edit exists. Do not read a large file to locate a known symbol: use read_symbol or file_summary.

Before nontrivial changes: impact analysis, then preview, inspect the diff, then apply. Prefer preview/apply even for simple edits when available. Never apply a rename without reading its preview; edit action is mandatory.

## Four routers
`mode` selects explore/analyze; `action` selects edit/manage. Other arguments are forwarded to the selected branch. Common optional arguments: `project_path` (auto-indexes on first use), `tier=l0|l1|l2` (l1 default). l0 returns identity totals/paging state; start l0/l1, request l2 only for needed detail. Unknown selectors error with suggestions; omitted selectors use documented defaults, except edit requires action.

Live manifest is authoritative. Branch guide: `leindex://tools/guide`; short guide: `leindex://docs/q`. CLI discovery: `leindex tools list`, `leindex tools inspect <tool>`, `leindex tools schema <tool>`. Legacy operation names (`leindex_search`, `leindex_text_search`, `leindex_edit_apply`, etc.) are compatibility names, not advertised discovery targets; `leindex_grep_symbols`/`leindex_text_search` are folded into find.

Notation: `*` required; otherwise optional. Always check live schema before calling.

### leindex_explore (mode defaults to search)
- **find**: live exact/regex/symbol search, including anywhere on disk. `pattern*`, `regex`, `word`, `case=smart|sensitive|insensitive` (smart default: insensitive unless pattern has uppercase), `target=text|symbols|auto` (text lines; symbols definitions, exact first; auto symbols then text), `output=matches|files|count|symbols` (matches default; symbols enclosing definitions ranked by hits), `scope` (project-relative dir/file), `paths` (extra files/dirs; no index or indexing side effects), `include_globs`, `exclude_globs`, `kind` (function/class/struct/etc. for symbols), `context_lines`, `max_line_chars`, `limit` (per page; 0 requests ceiling, currently 10000, NOT unlimited), `offset`, `per_file_cap` (0 disables; matches beyond cap appear on no page), `timeout_ms` (partial results + has_more). Follow returned next_offset; do not grow limit to re-fetch earlier pages. Live file reads reflect recent edits and respect index ignore rules, skip dirs and gitlinks. Use output=files for “which files mention X?”; paths for outside-project search.
- **search**: ranked semantic/structural concept search. `query*`, `top_k`, `offset`, `scope`, `search_mode=code|prose|auto|exact|semantic`, `task_context`, `allow_partial`, `max_latency_ms`. Repeated queries use a result cache. Optional persistent, content-addressed Engram phrase-book (`LEINDEX_FEATURE_ENGRAM=1`) reuses neural embeddings across processes/projects/reindexes without waking the embedder; enable only with permission to change environment.
- **symbol_lookup**: PDG callers/callees, data dependencies and impact. `symbol` OR `symbols` (batch ≤20), `depth`, `include_callers`, `include_callees`, `include_source`, `scope`, `token_budget`, `allow_partial`, `max_latency_ms`.
- **read_file**: primary indexed reader; exact numbered contents with optional symbol map. `file_path*`, `start_line`, `end_line`, `max_lines`, `include_symbol_map`.
- **read_symbol**: exact function/class/method source, doc comments and dependency signatures. `symbol*`, `file_path` (disambiguation), `include_dependencies`, `token_budget`, `allow_partial`, `max_latency_ms`.
- **project_map**: tree/layout, complexity hotspots and dependencies; replaces supported Glob/find/ls/tree navigation. `path`, `depth`, `focus` (topic relevance), `group_by=tree|community` (community = module boundaries), `sort_by`, `include_symbols`, `limit`, `offset`, `token_budget`. Example layout request: depth=2. Never call when user prohibits project maps.
- **file_summary**: symbols, complexity and cross-file dependencies without full-source noise. `file_path*`, `focus_symbol`, `include_source`, `token_budget`, `allow_partial`, `max_latency_ms`.
- **context**: PDG callers/callees, data dependencies and siblings. `node_id*` (short name or full ID), `token_budget`, `allow_partial`, `max_latency_ms`.

### leindex_analyze (mode defaults to deep)
- **deep**: end-to-end semantic retrieval plus PDG/data flow across files. `query*`, `token_budget`, `task_context`, `allow_partial`, `max_latency_ms`.
- **impact**: transitive blast radius. `symbol*`, `change_type=modify|remove|rename|change_signature`, `depth`.
- **diagnostics**: health, sizes, memory/cache statistics, Engram entries/bytes/limits/hits/misses and embed-cache hits/misses; counters are per process.
- **git_status**: structured changed files with PDG symbol/impact enrichment, no pager. `scope`, `allow_partial`, `enrich_pdg`, `max_latency_ms`.
- **git_diff**: rename-aware diff, changed hunks → PDG symbols/callers/affected files, including untracked file sizes; no pager. `ref` (commit vs parent), `range` (A..B or A...B, e.g. HEAD~2..main), `staged` (--cached), `scope`, `stat_only` (per-file numstat; --stat), `include_patch`, `max_patch_chars`, `enrich_pdg`, `allow_partial`, `max_latency_ms`. Use include_patch + scope for one-file patches; enriched diff for change blast radius.

### leindex_edit (action REQUIRED)
- **preview**: dry-run unified diff, affected symbols, risk and preview_token. `file_path*`, `old_text` + `new_text` OR `changes[]`.
- **apply**: atomic indexed edit with impact analysis. `file_path*`, `old_text` + `new_text` (aliases old_str/new_str) OR `changes[]` OR `preview_token`, `dry_run`.
- **rename**: atomic PDG-based cross-file rename. `old_name*`, `new_name*`, `scope`, `preview_only=true` default; inspect preview before setting false.
- **write**: atomic create/overwrite, parent creation, returned symbols. `file_path*`, `content*`.

### leindex_manage (action defaults to index)
- **index**: incremental build/refresh, pollable job; `project_path` (identify intended repository), `force_reindex`, `wait` (true blocks), `status_only` (poll without starting work). Force only when required after branch switch/major refactor; never re-index an already warm/current index merely to navigate, or when user prohibits it.
- **phase**: cached five-phase scan → symbols → dependencies → hotspots → recommendations. `phase=1..5|all`, `path` (file/dir), `mode=ultra|balanced|verbose`, `include_docs`, `docs_mode`, `top_n`, `max_chars`, `max_files`, `max_focus_files`. Module deep dive: path=src/module, mode=verbose.

## Failure/empty-result policy
1. Check intended project_path; for suspicious emptiness also check index count, scope filters and regex/literal mode.
2. Check diagnostics if health is unknown; initialize only if necessary and permitted. A known warm/current index needs no refresh.
3. Retry failed operation once with corrected arguments, then one alternative structured operation: find target=symbols ↔ search; find for unknown graph name; native Read for indexed read failure; native Edit when preview/apply unavailable.
4. Only if structured alternatives fail, use the narrowest native/shell fallback; report tool, failure class and fallback. Never repeatedly retry malformed calls (at most two attempts of the same operation); unavailable LeIndex alone must not block safe completion.
5. Empty live find supports text absence only within checked scope and after accounting for truncation/timeouts/errors. Empty search/symbol_lookup may indicate a building index; inspect response, poll index with status_only=true or retry, rather than treating it as absence.

## Response budgets (tokens; guidance, not measured guarantees)
Quick symbol lookup 500–1000; find page 500–1500; file overview 800–1500; function+callers 1500–2500; deep analysis 3000–6000; project map 2000–3000; symbol+deps 4000–8000; git stat_only 500–1000; git full patches 3000–10000. Use l0 to size before pulling detail. Source benchmarks (~1 ms/700 files for find; ~380 vs ~2000 tokens for file summary/read) are historical estimates, not universal performance contracts.
