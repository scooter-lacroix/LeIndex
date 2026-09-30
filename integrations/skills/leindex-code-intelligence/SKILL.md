---
name: leindex-code-intelligence
description: >-
  LeIndex code intelligence layer. Activates on any code navigation, search,
  editing, or analysis task within a LeIndex-equipped project. Provides
  precedence rules over native tools, the four-tool surface with complete
  argument reference, and fallback policy.
---

<Purpose>
LeIndex is the preferred code-intelligence layer when it is configured, available,
correctly scoped to the active repository, and returning valid results. It provides
semantic search, exact/regex/symbol search over a native trigram index, program
dependence graph (PDG) traversal, structural analysis, context-aware editing,
structured git diff, and token-efficient file reading. Native file and search tools
remain valid bounded fallbacks when LeIndex is unavailable, incorrectly scoped,
unhealthy, or unsuitable for the operation.
</Purpose>

<Use_When>
- Any code navigation, search, reading, editing, or analysis task in a project with LeIndex configured
- User asks to find, understand, search, read, edit, rename, or analyze code
- Agent would normally reach for Grep, Read, Glob, or Edit on source code
- Need caller/callee graphs, data flow analysis, or impact assessment
- Need git diff/status between arbitrary refs with PDG enrichment
- Need to understand project structure or module architecture
- Need to search files OUTSIDE the project (`find` with `paths`, no index required)
</Use_When>

<Do_Not_Use_When>
- Running shell commands, tests, or build steps — use native shell
- Creating a brand new file from scratch when no symbol feedback is wanted — use native file write
- Reading binary files or images — use native read
- No LeIndex MCP server is configured for the project
</Do_Not_Use_When>

<Precedence_Rule>
Use LeIndex first for code navigation, search, structural analysis, impact
analysis, git diff/status, and previewed edits when:

- the MCP server is configured and available;
- the project path resolves to the intended repository;
- the index is healthy or can be initialized successfully; and
- the operation is supported by the exposed LeIndex schema.

Use native `Read` or `Edit` when:

- the file is already known and a direct native operation is simpler;
- the LeIndex tool is unavailable in the current harness;
- LeIndex failed after one corrected retry and one alternative structured call;
- the file is generated, binary, or otherwise unsuitable for LeIndex; or
- no preview action is exposed and a safe exact edit is still required.

STRICT PROHIBITION — DO NOT USE BASH/GREP FOR WHAT LEINDEX PROVIDES:

Native `Grep`, `Bash grep`, `rg`, `find`, and any shell-based text search or
file-reading command are PROHIBITED as first-resort tools when LeIndex is
connected. This is not a preference — it is a hard requirement because:

1. `leindex_explore mode=find` is faster than grep (a native memory-mapped
   trigram index: about a millisecond on a 700-file repository, unbounded and
   paged) and returns the enclosing symbol with every hit.
2. LeIndex enforces the index's own ignore rules, skip dirs, and gitlink
   boundaries — shell grep does not.
3. Results are always read from the live file, so edits made a moment ago are
   seen; nothing is stale.
4. Shell commands can hang on pagers or interactive terminals (the exact
   problem LeIndex's git tools are designed to eliminate).

`find` also covers the case grep was used for last: files that are NOT in the
index. Pass `paths=["/any/dir"]` and it scans them live in parallel with no
index and no side effects. There is no reason to shell out for text search.

If LeIndex fails after one corrected retry and one alternative structured
call, fall back to native tools as documented in <Failure_Policy>. Do not
use bash as a first resort. Do not use bash "just this once." Do not use
bash for convenience. Use bash only after LeIndex has demonstrably failed.

Similarly, `git status`, `git diff`, `git log`, and `git -c` commands run in
bash are PROHIBITED when `leindex_analyze mode=git_status` or
`leindex_analyze mode=git_diff` can serve the same purpose. LeIndex's git
tools never trigger pagers, never require `q` keypresses, return structured
JSON (not raw terminal output), and include PDG-enriched symbol/impact data
that native git cannot provide.
</Precedence_Rule>

<Tool_Naming>
Skill documentation uses logical LeIndex names. Harnesses may expose them with
transport-specific names, for example `mcp__leindex__leindex_explore`. Always
inspect and use the exact name and schema from the live tool manifest. Do not
guess a wire name from this document.
</Tool_Naming>

<Tool_Inventory>
LeIndex advertises FOUR tools. Each is a router: a discriminator argument
(`mode` for explore and analyze, `action` for edit and manage) picks the
operation and every other argument is forwarded to it. All accept optional
`project_path` (auto-indexes on first use) and `tier`.

- `tier`: `l0` identity card (totals and paging state only), `l1` overview
  (default), `l2` full detail. Start at `l0`/`l1`; ask for `l2` only when needed.
- A missing or unknown discriminator returns a "did you mean" error, never a
  guess. `leindex_edit` REQUIRES `action` because a wrong guess would change files.
- Per-branch arguments: read the MCP resource `leindex://tools/guide`. One-screen
  cheat sheet: `leindex://docs/q`. From a shell: `leindex tools list`,
  `leindex tools inspect <tool>`, `leindex tools schema <tool>`.
- The original per-operation names (`leindex_search`, `leindex_text_search`,
  `leindex_edit_apply`, ...) still work as direct calls but are not advertised.
  `leindex_grep_symbols` and `leindex_text_search` are folded into `find`.

<Explore>
`leindex_explore` — find and read code. `mode` (default `search`):

- `find` — exact text, regex, or symbol names, ANYWHERE on disk. Args: `pattern`
  (required), `regex`, `word`, `case` (`smart` default: ignore case unless the
  pattern has an uppercase letter), `target` (`text` matching lines | `symbols`
  definitions by name, exact first | `auto` symbols else text), `output`
  (`matches` default | `files` | `count` | `symbols` enclosing symbols ranked by
  hits), `scope` (project-relative dir/file), `paths` (extra files/dirs anywhere
  on disk, no index needed), `include_globs`, `exclude_globs`, `kind`
  (function|class|struct|... for `target=symbols`), `context_lines`,
  `max_line_chars`, `limit` (per page, `0` = all), `offset` (use the response's
  `next_offset`), `per_file_cap`, `timeout_ms` (partial results + `has_more`).
- `search` — ranked semantic + structural search by meaning. Args: `query`
  (required), `top_k`, `offset`, `scope`, `search_mode`
  (`code`|`prose`|`auto`|`exact`|`semantic`), `task_context`. Repeat queries are
  served from a result cache.
- `symbol_lookup` — callers, callees, data dependencies, impact radius from the
  PDG. Args: `symbol` | `symbols` (batch, max 20), `depth`, `include_callers`,
  `include_callees`, `include_source`, `scope`, `token_budget`.
- `read_file` — exact contents with line numbers; PRIMARY file reader for indexed
  files. Args: `file_path` (required), `start_line`, `end_line`, `max_lines`,
  `include_symbol_map` (PDG annotations).
- `read_symbol` — exact source of one function/class/method with doc comments and
  dependency signatures. Args: `symbol` (required), `file_path` (disambiguate),
  `include_dependencies`, `token_budget`.
- `project_map` — annotated project tree with complexity hotspots. Args: `path`,
  `depth`, `focus` (rank files by relevance to a topic), `group_by` (`tree` |
  `community` for module boundaries), `sort_by`, `include_symbols`, `limit`,
  `offset`, `token_budget`.
- `file_summary` — structural overview (symbol inventory, complexity,
  cross-file dependencies) at ~380 tokens instead of ~2000. Args: `file_path`
  (required), `focus_symbol`, `include_source`, `token_budget`.
- `context` — expand PDG context around a node: callers, callees, data deps,
  siblings. Args: `node_id` (required; short name or full id), `token_budget`.
</Explore>

<Analyze>
`leindex_analyze` — graph analysis and repository state. `mode` (default `deep`):

- `deep` — semantic retrieval expanded through the PDG with data flow across
  files. Args: `query` (required), `token_budget`, `task_context`.
- `impact` — transitive blast radius of changing a symbol. Args: `symbol`
  (required), `change_type` (`modify`|`remove`|`rename`|`change_signature`),
  `depth`.
- `diagnostics` — index health, sizes, cache and memory statistics.
- `git_status` — structured status with PDG-enriched symbol/impact data per
  changed file. Never triggers a pager. Replaces `git status`.
- `git_diff` — structured diff, rename-aware, changed hunks mapped to PDG symbols
  with callers and affected files, untracked files included. Args: `ref` (one
  commit against its parent), `range` (`A..B` / `A...B`), `staged`, `scope`,
  `stat_only` (per-file numstat only), `include_patch`, `max_patch_chars`,
  `enrich_pdg`. Replaces `git diff`, `--cached`, `--stat`, `HEAD~N`, `a..b`.
</Analyze>

<Edit>
`leindex_edit` — context-aware editing. `action` is REQUIRED:

- `preview` — dry run: unified diff, affected symbols, risk level, and a
  `preview_token`. Args: `file_path`, `old_text` + `new_text` | `changes[]`.
  Run BEFORE every non-trivial edit.
- `apply` — atomic edit with impact analysis; PRIMARY editor for indexed source.
  Args: `file_path`, `old_text` + `new_text` (aliases `old_str`/`new_str`) |
  `changes[]` | `preview_token`, `dry_run`.
- `rename` — atomic cross-file symbol rename using the PDG to find every
  reference. PREVIEWS BY DEFAULT: `preview_only=true`; pass `preview_only=false`
  to apply. Args: `old_name`, `new_name`, `scope`.
- `write` — create or overwrite a file atomically and get its symbols back.
  Auto-creates parent directories. Args: `file_path`, `content`.
</Edit>

<Manage>
`leindex_manage` — index lifecycle and architecture reports. `action` (default
`index`):

- `index` — build or refresh. Incremental by default; `force_reindex=true` after
  a branch switch or major refactor. Returns a pollable job snapshot;
  `wait=true` blocks. Args: `project_path` (required), `force_reindex`, `wait`.
- `phase` — 5-phase architecture report (scan → symbols → dependencies →
  hotspots → recommendations). Args: `phase` (1-5 | `all`), `path` (file or
  directory), `mode` (`ultra`|`balanced`|`verbose`), `include_docs`,
  `docs_mode`, `top_n`. Cached: near-instant when nothing changed.
</Manage>

STRICT PROHIBITION — DO NOT USE BASH GIT COMMANDS FOR CODEBASE STATE:

Running `git status`, `git diff`, `git diff --cached`, `git diff --stat`,
`git diff HEAD~N`, `git diff branch1..branch2`, or any `git -c` variant in
bash when LeIndex's git modes are available is PROHIBITED. Reasons:

1. Bash git commands frequently trigger pagers that require `q` keypresses
   to exit — agents hang indefinitely on this.
2. LeIndex returns structured JSON with file paths, symbol counts, forward
   impact (blast radius), and PDG enrichment — raw git output has none of this.
3. `git_diff` includes untracked files with sizes (native git diff ignores them
   without `--no-index`).

Only use bash git commands for operations LeIndex does NOT cover:
- `git commit`, `git push`, `git checkout`, `git branch`, `git merge`, `git rebase`
- `git stash`, `git cherry-pick`, `git revert`, `git reset`
- Repository mutation commands (LeIndex is read-only for codebase state)
</Tool_Inventory>

<Replacement_Map>
Every common native action mapped to its LeIndex replacement:

| Native action | Use instead | Why |
|---|---|---|
| Text or regex search | `leindex_explore mode=find pattern=...` (`regex=true`) | ms latency, always live, enclosing symbol per hit |
| Grep for a definition | `leindex_explore mode=find target=symbols` | exact definitions first, with kind and location |
| Grep outside the project | `leindex_explore mode=find paths=["/dir"]` | live parallel scan, no index |
| "Which files mention X?" | `leindex_explore mode=find output=files` | file list with counts, no line noise |
| Read indexed source | `leindex_explore mode=read_file` | content + optional symbol map |
| Read to understand structure | `leindex_explore mode=file_summary` | ~380 tokens vs ~2000 for full read |
| Read a specific function | `leindex_explore mode=read_symbol` | exact source, zero file noise |
| Glob / find / ls / tree | `leindex_explore mode=project_map` | annotated tree in one call |
| Grep to find callers | `leindex_explore mode=symbol_lookup` | full PDG graph, not text heuristics |
| Concept search | `leindex_explore mode=search` | semantic + structural ranking |
| "How does X work?" | `leindex_analyze mode=deep` | PDG traversal with data flow |
| Edit indexed source | `leindex_edit action=preview` then `action=apply` | impact report + diff before disk |
| Rename across files | `leindex_edit action=rename` | atomic, PDG-driven, previewed |
| Unknown blast radius | `leindex_analyze mode=impact` | transitive dependency tree with risk |
| `git status` | `leindex_analyze mode=git_status` | structured JSON + PDG enrichment, no pager |
| `git diff` | `leindex_analyze mode=git_diff` | numstat + optional patches + PDG impact |
| `git diff --cached` | `leindex_analyze mode=git_diff staged=true` | same enrichment |
| `git diff HEAD~2..main` | `leindex_analyze mode=git_diff range="HEAD~2..main"` | arbitrary ref ranges |
| `git diff --stat` | `leindex_analyze mode=git_diff stat_only=true` | numstat summary |
| Module architecture | `leindex_manage action=phase` | 5-phase report, cached |
</Replacement_Map>

<Decision_Tree>
Finding code:
- Know the exact text, identifier, or a regex -> explore mode=find
- Know a definition's name -> explore mode=find target=symbols
- Know the concept, not the words -> explore mode=search
- Want project layout -> explore mode=project_map depth=2
- Text lives outside the project -> explore mode=find paths=[...]

Understanding code:
- Read a file -> explore mode=read_file
- File structural overview -> explore mode=file_summary
- Single function/class -> explore mode=read_symbol
- End-to-end data flow -> analyze mode=deep
- Caller/callee graph -> explore mode=symbol_lookup
- Context around a node -> explore mode=context
- Module deep-dive -> manage action=phase path="src/module" mode=verbose

Changing code (always preview first):
- Simple edit -> edit action=preview, then edit action=apply
- Cross-file rename -> edit action=rename (preview by default), then preview_only=false
- Before a non-trivial change -> analyze mode=impact

Git codebase state (always LeIndex first):
- What changed? -> analyze mode=git_status
- Diff between refs -> analyze mode=git_diff range="A..B"
- Staged diff -> analyze mode=git_diff staged=true
- Patch for one file -> analyze mode=git_diff include_patch=true scope="path/to/file"
- Blast radius of changes -> analyze mode=git_diff (enriched by default)
</Decision_Tree>

<Failure_Policy>
1. Verify that `project_path` points at the intended repository.
2. Verify index health with `analyze mode=diagnostics`, or initialize the index
   with `manage action=index` if necessary.
3. Retry the failed MCP operation once with corrected arguments.
4. Try one alternative structured operation:
   - `find target=symbols` instead of `search`, or the reverse;
   - `find` instead of `symbol_lookup` for a name the graph does not know;
   - direct native Read instead of indexed file read;
   - native Edit instead of LeIndex edit when preview/apply is unavailable.
5. If structured alternatives fail, use the narrowest native or shell fallback.
6. Report the failed tool, failure class, and fallback used.

A suspiciously empty result must be checked against project path, index count,
scope filters, and regex/literal mode before it is treated as authoritative.
`find` reads live files, so an empty `find` is trustworthy for text; an empty
`search` or `symbol_lookup` may mean the index is still building (the response
says so) — poll `manage action=index` or retry.

Do not retry the same failing operation more than twice. Do not block completion
solely because LeIndex is unavailable when a safe native fallback exists.
</Failure_Policy>

<Anti_Patterns>
- Do not use shell grep as the first search mechanism when a healthy, correctly
  scoped LeIndex search is available. `find` is faster.
- Do not use bash `git status`, `git diff`, `git log`, or `git -c` when
  LeIndex's git modes are connected.
- Do not read an entire large file merely to locate one known symbol; use
  `read_symbol` or `file_summary`.
- Do not edit through `sed`, `perl`, Python replacement scripts, `awk`, or shell
  redirection when LeIndex edit or native Edit is available.
- Do not claim that an empty search proves absence without checking project path
  and index state.
- Do not omit `action` on `leindex_edit`; it has no default by design.
- Do not apply a rename without reading its preview.
- Do not repeatedly call a malformed or failing structured tool.
- Do not guess transport-specific MCP tool names.
- Do not page `find` results by re-running with a bigger `limit` when
  `next_offset` is returned; follow `next_offset`.
</Anti_Patterns>

<Token_Budget>
| Context | Budget |
|---|---|
| Quick symbol lookup | 500-1000 |
| `find` (default page) | 500-1500 |
| File overview | 800-1500 |
| Function + callers | 1500-2500 |
| Deep analysis | 3000-6000 |
| Project map | 2000-3000 |
| Symbol with deps | 4000-8000 |
| Git diff (stat_only) | 500-1000 |
| Git diff (full + patches) | 3000-10000 |

`tier=l0` returns just the identity card (totals, paging state) and is the
cheapest way to size a result before pulling it.
</Token_Budget>

<Quick_Reference>
leindex_explore mode=find     pattern, regex?, word?, case?, target?, output?, scope?, paths?, include_globs?, exclude_globs?, context_lines?, limit?, offset?
leindex_explore mode=search   query, top_k?, scope?, search_mode?
leindex_explore mode=symbol_lookup   symbol|symbols, depth?, include_callers?, include_callees?, include_source?
leindex_explore mode=read_file       file_path, start_line?, end_line?, max_lines?, include_symbol_map?
leindex_explore mode=read_symbol     symbol, file_path?, include_dependencies?
leindex_explore mode=project_map     path?, depth?, focus?, group_by?, sort_by?, include_symbols?
leindex_explore mode=file_summary    file_path, focus_symbol?, include_source?
leindex_explore mode=context         node_id, token_budget?
leindex_analyze mode=deep            query, token_budget?
leindex_analyze mode=impact          symbol, change_type?, depth?
leindex_analyze mode=diagnostics
leindex_analyze mode=git_status      scope?
leindex_analyze mode=git_diff        ref? | range? | staged?, scope?, stat_only?, include_patch?
leindex_edit action=preview   file_path, old_text+new_text | changes[]
leindex_edit action=apply     file_path, old_text+new_text | changes[] | preview_token, dry_run?
leindex_edit action=rename    old_name, new_name, scope?, preview_only? (default true)
leindex_edit action=write     file_path, content
leindex_manage action=index   project_path, force_reindex?, wait?
leindex_manage action=phase   phase?, path?, mode?, include_docs?
(all: project_path?, tier? = l0|l1|l2)
</Quick_Reference>
