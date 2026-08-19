# LeIndex MCP Stress Test — Session 5 Report

**Date:** 2026-08-18, 18:18–19:05 EDT
**Test subject:** LeIndex 2.0.0 (branch `v2.0.0`, head `256bede1` + uncommitted remediation from sessions 1–4), MCP surface driven against LeIndex's own source tree
**Project:** `/mnt/WD-SSD/code_index_update/LeIndexer-release-1.8.4` (491 files / ~11.5K signatures at healthy state)
**Harness:** Maestro Terminal (Splinky) → `leindex` MCP server (stdio), 17 tools × multiple modes/parameter matrices, 14 tool-call batches
**Binaries in play:** `~/.cargo/bin/leindex` and `target/release/leindex` (both built 17:10, contain sessions-1–4 fixes; source mtimes 14:06). Two concurrent `leindex mcp` servers observed (PIDs 2864240 PATH-binary, 2872915 target/release) + one `leindex-embed` GPU daemon (PID 2950061).
**Index lifecycle during test:** generation 2 (healthy) → generation 4 (corrupted by this session's edit-path incremental reindex) → CLI `--force` rebuild to generation 6 (healthy; 11,555 sigs / 18,981 PDG nodes / 120,419 edges / 3.27s).

---

## 1. Executive Summary

- **13 of 17 tools functional on their primary path**; 4 carry a failing or broken sub-path that matters: `index` (MCP stats payload + incremental corruption), `rename-symbol` (apply mode systemically blocked by its own validator), `edit-apply` (`dry_run=true` mode), `grep-symbols` (semantic mode returns nothing useful).
- **One systemic data-quality defect dominates everything else: duplicated PDG relationship entries.** Every tool that renders callers/callees (grep-symbols, read-symbol, symbol-lookup, impact-analysis, context) lists the same caller twice. `grep-symbols` even reports `caller_count: 6` beside a 5-entry list in one JSON payload. This is a render-layer dedup failure, not index corruption (the rebuild did not change it).
- **One new critical defect found and root-caused: rename-apply is impossible.** `validate_rename_plan` tags each change `EditType::Rename`, but `SemanticDriftAnalyzer::compare_signatures` (src/validation/drift.rs:276) never reads `edit_type()` and treats every rename as Removed(old)+Added(new) → `has_errors()` → hard `-32602` reject. Verified on two independent symbols. Preview mode works perfectly; apply mode can never succeed for any supported language.
- **The incremental index path corrupted the store during normal tool-driven editing:** after one `write` + one `edit-apply`, the auto-reindex path left the project at `status=failed` (`generation 4 already exists; refusing to overwrite an immutable snapshot`), `Total signatures: 7` (from 11,555), and a ~3.5× heap-size bloat (463 MB → 1,664 MB). CLI `--force` rebuild fully recovered it.
- **Warm performance is excellent and matches the prior sessions' claims** (sub-100 ms on every non-neural tool; semantic search 5 ms warm). **Cold neural start is still ~19 s** (`neural_ms: 18,878` on first search), and the first `diagnostics` call pays a ~2.1 s hydrate.
- **Cross-session verification:** F-04 (real RSS), F-07 (garbage-query "No results"), F-08 partial (no `Arc`/`*` in search results, but `write`'s inventory still shows `* [use]`-style noise), F-09 (real snippets), F-14b (batch limits), F-16 (impact direction), F-17 (rename scope) — all verified working in this session. New defects N-01…N-14 below are distinct from the prior register.

---

## 2. Methodology

- All calls through the live MCP connection with `project_path` pinned to the repo; per-call `_meta.timings` captured for latency attribution (handler vs neural vs hydrate).
- Parameter matrices exercised: `search_mode` (semantic/code), grep `mode` (exact/semantic) + `type_filter` + `include_source`, text-search `is_regex`/`case_sensitive`/`context_lines`/`include_globs`/`exclude_globs`/`scope`/pagination, read-file line ranges + `include_symbol_map`, read-symbol `file_path` disambiguation + token budgets, symbol-lookup batch mode, phase-analysis scoped path + modes, impact depth/change_type, edit preview/dry-run/real-apply, rename preview/apply, error paths (missing file, bad regex, non-matching old_text, inverted line range).
- Controlled mutation: disposable `stress_mcp_scratch.rs` (created via `write`, edited via `edit-apply`, rename attempts on two symbols, deleted; git-verified clean afterwards).
- Ground truth cross-checks: `ps` for server/daemon inventory, binary/source mtimes, `du`/`ls` on `.leindex`, CLI `leindex index --force` for recovery, prior sessions' report (`LEINDEX_STRESS_TEST_REPORT.md`) for regression verification.

---

## 3. Tool-by-Tool Verdicts

Latencies are from `_meta.timings.total_ms` on this session's calls.

### 3.1 `leindex.search` — **PASS (warm) / SLOW COLD START**
- Cold first semantic query: **18,923 ms** (`neural_ms: 18,878` — GPU embed daemon cold start). Identical warm query: **5 ms** (`neural_ms: 0`, cache). This is the documented ~20 s daemon spin-up, not a query-cost problem — but nothing in the response distinguishes "cold" from "slow"; a `cold_start: true` hint would save agents a retry loop.
- Relevance: top hit correct (`redact` @ 71% for the secret-redaction query, with its doc snippet). Result tail is weak: `Ok [variable]`, `Balanced [variable]`, `semantic [module]` at 49–50% — residual low-value hits ranking just under the floor. F-07's `low_signal` correctly did NOT fire (top score 0.71 > 0.25 floor) but there is no per-result floor, so junk still occupies slots 2–5.
- Garbage query (`zzz qwerty frobnicate xyzzy plugh`) → clean **"No results"** (F-07/F-08 behavior verified; would have returned noise-scored junk before session 3's fixes).
- Scoped search (`scope: src/cli/mcp`, `search_mode: code`) works: 375 ms, all hits in scope.

### 3.2 `leindex.text-search` — **PASS (best tool in the suite)**
- 22–123 ms across literal/regex/glob/scope/pagination variants. Output includes file:line, owning symbol name+type (`in_symbol`), before/after context, `total_matched`, `has_more`, offset pagination — everything grep gives you plus symbol attribution.
- Error paths exemplary: invalid regex → `-32602` with the regex parser's caret-positioned message; missing file → exact OS error. Fast-fail, informative, correct JSON-RPC code.
- Minor: `_meta.freshness` block (present on every call) reported `changed_unindexed_count: 43–44` while `status: fresh` — see N-11 (freshness semantics).

### 3.3 `leindex.grep-symbols` — **exact: PASS with data defects / semantic: FAIL**
- Exact mode: 6–21 ms, substring matching correct (`LogScrubber` matched its 5 test fns + struct; `log_scrubber` snake_case matched only snake_case names — expected under substring semantics, but agents should know there's no case-normalization or fuzzy name folding).
- `include_source=true` returns real source (verified on `stress_alpha`) — prior-session fix holding.
- **Defect N-02:** `caller_count: 6` with a 5-element `callers` array in the same payload (off-by-one between count and list).
- **Defect N-01 (visible here):** every caller listed twice when the symbol has a def-site + use-site edge pair.
- Semantic mode: query `"scrub secrets from logs"` with `type_filter: function` → **0 results** (766 ms, `neural_ms: 61`). It cannot surface `LogScrubber::redact` — the single most on-the-nose symbol in the codebase for that phrase. Semantic symbol mode is effectively non-functional; exact mode is the only usable path.

### 3.4 `leindex.project-map` — **PASS (cosmetic issues)**
- 6 ms. Complexity-sorted tree with per-file symbol counts and dependency arrows. Scoped fine at depth 3.
- **Defects (cosmetic but confusing):** "Files in scope: 393" vs 491 indexed elsewhere (scope-count basis unexplained); a stray root-level `extraction_rewrite.rs` entry with no directory prefix; `[143→4]`-style annotations with **no legend anywhere** in the output; inconsistent indent styles in the tree render.

### 3.5 `leindex.read-file` — **PASS**
- 43 ms; exact content with dual numbering (display│absolute), requested range honored.
- Parameter validation: inverted `start_line`/`end_line` → immediate `-32602` "end_line 60 precedes start_line 295" (my error, caught cleanly). Missing file → exact OS error.
- **Defect N-08:** `include_symbol_map=true` produced no symbol map in the rendered output — parameter silently ignored (or dropped by the renderer). Either honor it or reject it; silence is the worst option.

### 3.6 `leindex.read-symbol` — **PASS core / PDG enrichment noisy**
- 4 ms; exact source, line range, doc comment, complexity — excellent.
- `file_path` disambiguation works (`redact` in `src/observability.rs` resolved precisely; `truncate` resolved to `src/cli/mcp/output/mod.rs`).
- **Defect N-03:** `callees` for `redact` lists three unrelated project-local `truncate` fns — the extractor saw `String::truncate` (std) and resolved it by name to random namesakes, including one under `tests/fixtures/memcheck/small_repo/`. False precision: better to emit `String::truncate [external]` than to guess.
- Duplicate callers again (N-01).

### 3.7 `leindex.file-summary` — **WEAK (functional but misses its own contract)**
- 7 ms, correct file/lines/symbol-count/role.
- But: "Symbols: 82 … 81 more (truncated)" — the advertised ~380-token structural overview with complexity scores and cross-file deps is not visible in the rendered payload; `focus_symbol=LogScrubber` produced no focus effect. As shipped, a `text-search` for the file name delivers more signal per token.

### 3.8 `leindex.symbol-lookup` — **PASS core / batch works / conflation noise**
- 6–10 ms; callers/callees/complexity/impact correct for unique names; batch mode (`symbols: [stress_beta, stress_gamma, truncate]`) works in one call.
- **Defect N-04:** for common names the graph conflates every same-named symbol into one node's relationship list — `truncate` shows 14 "callers" spanning ≥5 distinct functions across the repo (including test files and `redact`, which actually calls std `String::truncate`). Directionally useful, individually unreliable.
- Duplicated entries again (N-01: `stress_gamma` twice as caller of `stress_beta`).

### 3.9 `leindex.context` — **PARTIAL**
- 9 ms. Expands a node into related symbols with source — the concept is right and the budget control works.
- **Defect N-05:** "Gravity traversal" pulls in symbols by name-affinity, not semantic relation: expanding `redact` dragged in all three `truncate` implementations (one from `tests/fixtures/`), a `From<UniqueProjectId> for String` impl, and `render_phase` — none call or are called by `redact`. ~80% of the returned context was noise for this node.
- Extraction artifacts visible in expansion output: split `impl` blocks rendered with stray closing braces (malformed snippet boundaries).

### 3.10 `leindex.deep-analyze` — **PARTIAL (good signal, sloppy edges)**
- 2,807 ms first call (includes neural warm-up: `neural_ms: 40`, rest is retrieval+assembly). Prior sessions measured 79 ms warm; this session's single call paid initialization.
- Quality: for "How does the MCP server dispatch tool calls to handlers and render responses?" it returned the genuine dispatch chain (`cmd_tools_impl` → `find_tool_handler` → `execute_tool_handler` → `render_tool_output`) with real source — the best single-answer retrieval in the suite. Tail degraded into `title()` accessor noise.
- **Defect N-06:** `tokens_used: 3,700` against `token_budget: 3,000` — budget overrun (~23%) with no truncation warning.

### 3.11 `leindex.phase-analysis` — **PASS (fast) with self-contradictory metadata**
- 151 ms for all 5 phases scoped to `src/cli/mcp` (38 files, 745 signatures, hotspots correctly identifying the `output/render/mod.rs` monsters at complexity 16–19). Impressive.
- **Defect N-07:** the payload contains two different generation identifiers (a 64-hex hash in the body, `generation=2` counter in the freshness footer — prior report flagged this as cosmetic, still open) and ends with `status=initializing` + "run force_reindex" advisory while every other tool reported fresh/complete. Confidence-eroding, not function-breaking.

### 3.12 `leindex.impact-analysis` — **PARTIAL**
- 5 ms. Post-F-16 direction is correct: "Direct callers" for `redact` are its dependents (the 5 tests), risk low, affected files 1 — right answer.
- **Defects:** direct callers duplicated (11 entries, 5 unique — N-01 again); `LogScrubber` (the impl block) listed as a caller of its own method — impl-block containment edges leak into call lists; "Transitive affected symbols (5)" identical to direct callers at `depth=3` — either transitivity is broken or depth has no effect for leaf symbols with no indication which.

### 3.13 `leindex.edit-preview` — **PASS**
- 50 ms. Side-by-side unified diff, `+1 -0` counts, risk level (low), affected files list. Did exactly what a preview should: showed the doc-comment insertion precisely. No issues found.

### 3.14 `leindex.edit-apply` — **PASS (real path) / FAIL (`dry_run=true`)**
- Real apply: 684 ms — applied exactly, echoed surrounding region, index generation auto-bumped 2→4 (incremental refresh triggered). Verified on disk. Error path (non-matching `old_text`): clean `-32602` with the first line of the missing text echoed. 
- **Defect N-09:** `dry_run=true` returns "Edit apply failed / Affected files: 1" with **no error detail** and `is_error=false`. A dry-run that reports failure without a reason is worse than no dry-run: agents can't tell validation-rejection from renderer bug. (Root suspicion: the dry-run path reuses the apply renderer, which expects a written-state payload.)
- **Defect N-10 (critical, from the same path):** the tool-triggered incremental reindex after the apply corrupted the store — "generation 4 already exists; refusing to overwrite an immutable snapshot" left the project `status=failed`, `Total signatures: 7`, heap estimate 463 MB → 1,664 MB. Recovery required CLI `--force`. The write itself was correct; the index bookkeeping around it was not.

### 3.15 `leindex.rename-symbol` — **preview: PASS (excellent) / apply: FAIL (systemic)**
- Preview: 44 ms, multi-hunk side-by-side diff catching the definition **and all 5 test call sites** with correct line numbers, scope honored. Best-in-class preview.
- **Defect N-00 (critical, root-caused):** apply mode is structurally impossible. Two independent attempts (`stress_alpha`→`stress_alpha_prime`, `stress_beta`→`stress_beta_renamed`) both rejected: "Semantic drift: 2 — Removed old / Added new". Root cause chain, verified in source:
  1. `validate_rename_plan` (src/cli/mcp/rename_symbol_handler.rs:248) builds `ResolvedEditChange`s correctly tagged `.with_edit_type(EditType::Rename)`.
  2. `LogicValidator::validate_changes` (src/validation/mod.rs:196) runs `analyze_semantic_drift` on them.
  3. `compare_signatures` (src/validation/drift.rs:276) name-keys original vs new signatures and emits Removed for anything only in the original, Added for anything only in the new — **it never reads `change.edit_type()`**. A rename is by definition remove+add by name, so every rename in every supported language produces exactly this rejection.
  - Fix direction: in `compare_signatures`, when `change.edit_type() == Rename`, pair a Removed(old) with an Added(new) whose signature bodies are identical modulo the name and emit either nothing or an informational `Rename` drift item; only unpaired removals/additions should error. `DriftItem` already has the type vocabulary for it.

### 3.16 `leindex.write` — **PASS**
- 6 ms; created the scratch file, returned structural context (7 symbols) and bumped the index. Cosmetic: symbol inventory shows return types as symbol types (`stress_alpha [u32]`) and a literal `* [use]` entry — the F-08 node filter fixed the graph, but the write-response inventory still renders the raw signature list.

### 3.17 `leindex.index` (MCP) — **FAIL (two distinct defects)**
- **Defect N-12:** returns an all-`null` stats payload (`files_parsed`, `pdg_nodes`, `total_signatures` … all null) on both `wait=false` and `wait=true`, with `handler_ms: 1–2`. The MCP index tool reports nothing about what it did. (CLI path reports correctly: 11,555 sigs / 18,981 nodes / 120,419 edges / 3,270 ms on `--force`.)
- **Defect N-10 (shared with edit-apply):** the incremental path it fronts produced the corrupt generation-4 state (see 3.14). Additionally, the failure surfaced only later via `_meta.freshness.last_failure` on unrelated tools — the index tool itself never reported the error at the time.
- Blocking full re-index via MCP still exceeds 30 s harness timeouts (prior report's note stands — use CLI for full rebuilds).

### 3.18 `leindex.diagnostics` — **PARTIAL**
- 59–63 ms warm (2,120 ms first call: `hydrate_ms: 2,080`). F-04 verified: first call showed `Memory RSS 463.56` vs `Index size 463.50` — genuinely distinct measurements. Later equality at 1664.55/1664.55 coincided with the bloated gen-4 heap (a server really holding 1.6 GB shows RSS≈heap); the field itself is honest.
- **Defect N-13:** `Total signatures: 7` while healthy (first call: 11,555 — correct; post-corruption: 7; post-rebuild: **still 7**). Two sub-causes: the corrupted gen-4 wrecked the count, and the running MCP server never re-hydrated after the external CLI rebuild — it served gen-4-era PDG stats (18,578 nodes / 111,149 edges) against a gen-6 reality (18,981 / 120,419) while stamping `Freshness: status=fresh, generation=6`. The freshness footer and the served snapshot disagree; the server trusts its in-memory state until something forces a hydrate.
- Index-size field (heap estimate) diverged from disk truth (1,664 MB estimate vs 821 MB `.leindex` on disk post-rebuild) — estimate basis undocumented.

### 3.19 `leindex.git-status` — **PASS (with internal contradictions)**
- 13 ms; matches `git status --porcelain` ground truth (36 modified / 7 untracked at time of call, including prior sessions' artifacts). Changed-symbols mapping per file is genuinely useful.
- Contradictions in one payload: every per-symbol line reads "N callers, **0 impact**" while the summary says "**500 affected symbols across 174 files**"; and "PDG Enrichment: ⚠ unavailable" while `diagnostics` says `PDG loaded: true`. Two different impact computations presented side by side with no reconciliation.

---

## 4. Cross-Cutting Defect Register (new this session)

| ID | Severity | Defect | Where seen | Root-cause status |
|---|---|---|---|---|
| N-00 | **CRITICAL** | rename-symbol apply impossible: drift validator flags every rename as Removed+Added | §3.15 | **Root-caused** — `compare_signatures` ignores `edit_type()` (drift.rs:276) |
| N-10 | **CRITICAL** | tool-triggered incremental reindex corrupts store ("generation N already exists"), leaving failed status, signatures=7, ~3.5× heap bloat | §3.14, §3.17 | Symptoms pinned; writer-side generation-allocation race suspected (two re-index triggers racing to create the same next generation after write+apply) |
| N-01 | HIGH | duplicated caller/callee entries in every relationship render | §3.3, §3.6, §3.8, §3.12 | def-site + use-site edge pairs both rendered; needs dedup at render or edge-materialization |
| N-02 | MED | `caller_count` disagrees with `callers.len()` (6 vs 5) | §3.3 | count includes an entry the list drops (likely the dedup'd duplicate) |
| N-03 | MED | std/external method calls resolved by name to unrelated project fns (incl. `tests/fixtures/`) | §3.6, §3.9 | name-based resolution with no externality marker; fixtures should also be excluded from the prod graph |
| N-04 | MED | same-name conflation: one symbol's relationship list merges ≥5 distinct functions | §3.8 | PDG keys relationships by short name, not qualified id |
| N-05 | MED | context "gravity traversal" pulls name-affinity noise (~80% irrelevant for `redact`) | §3.9 | ranking ignores call-graph distance vs lexical similarity |
| N-06 | LOW | deep-analyze token_budget overrun (3,700/3,000) without notice | §3.10 | budget checked after assembly |
| N-07 | LOW | phase-analysis dual generation ids + `status=initializing` footer contradicting `fresh` everywhere else | §3.11 | footer uses a different freshness source than body |
| N-08 | LOW | `include_symbol_map=true` silently no-op in rendered read-file | §3.5 | renderer drops field |
| N-09 | MED | edit-apply `dry_run=true` reports "failed" with no detail, `is_error=false` | §3.14 | dry-run path reuses apply renderer |
| N-11 | LOW | freshness metadata says `fresh` while `changed_unindexed_count: 43–44` on every call | §3.2 passim | "fresh" means phase-complete, not content-current — semantics undocumented, reads as a lie |
| N-12 | HIGH | MCP `index` returns all-null stats regardless of wait | §3.17 | stats never wired to the job handle |
| N-13 | HIGH | diagnostics serves stale in-memory snapshot after external rebuild (gen-4 stats + `signatures: 7` under a gen-6 fresh footer) | §3.18 | no generation-change detection / re-hydrate trigger |
| N-14 | LOW | project-map scope count (393) ≠ indexed count (491), no legend for annotations | §3.4 | count bases differ; undocumented |

**Deployment hygiene (not a code defect):** two concurrently-running `leindex mcp` servers (PATH binary + `target/release` binary) against the same project during this test. The prior report already documented the old-binary/new-binary daemon spawn race; the general rule stands — one server per project, restart servers after rebuilding the binary.

---

## 5. Intermittent / Bimodal Behaviors

1. **Semantic search latency is bimodal by design but invisible:** 18.9 s cold (daemon spawn) → 5 ms warm, with nothing in the payload marking which one you got. Agents will timeout-and-retry on the first call of every session.
2. **`grep-symbols` semantic mode worked for prior sessions (CPU path) but returned 0 results here** — the only tool whose behavior plausibly differed between the two live servers. Unreliable across restarts; treat exact mode as the contract.
3. **Diagnostics body vs footer flipped between `Stale: false / status=fresh` and `Stale: true / status=failed`** across the session — tracking actual index state correctly, but the intermediate states (`status=failed` with `Stale: true` and no issues list; then `fresh` with wrecked `signatures: 7`) show the two freshness sources can disagree for minutes.
4. **MCP tool calls occasionally surfaced as "cancelled" in the harness** (two `leindex.index` calls early in the session) — could not reproduce deterministically; flagged as a transport-level watch item, not charged to LeIndex.

---

## 6. Performance Profile & Poorly Optimized Areas

**Warm-path latencies (this session, `_meta.timings.total_ms`):**

| Tool | Warm | Notes |
|---|---|---|
| search (semantic) | 5 ms | 18,923 ms cold — daemon spawn |
| search (scoped, code) | 375 ms | |
| text-search | 22–123 ms | regex+globs+context |
| grep-symbols (exact) | 6–21 ms | semantic mode 766 ms → 0 results |
| project-map | 6 ms | |
| read-file | 43 ms | |
| read-symbol | 4 ms | |
| file-summary | 7 ms | |
| symbol-lookup | 6–10 ms | batch works |
| context | 9 ms | |
| impact-analysis | 5 ms | |
| deep-analyze | 2,807 ms (init-inclusive) | prior sessions: 79 ms warm |
| phase-analysis (scoped, 5 phases) | 151 ms | |
| git-status | 13 ms | |
| diagnostics | 59–63 ms | 2,120 ms first-call hydrate |
| edit-preview | 50 ms | |
| edit-apply (real) | 684 ms | includes incremental reindex trigger |
| write | 6 ms | |
| index (CLI --force) | 3,270 ms | full rebuild, core path |

**Poorly optimized / structurally weak areas, ranked by leverage:**

1. **Cold neural start (~19 s).** Every fresh server+daemon pair pays it; CLI one-shot invocations pay it per process (PDEATHSIG kills the daemon with the parent). Levers: persistent `leindexd` (already exists — adopt it as the default user-scoped daemon), model preload at MCP server start, fp16/quantized export (also attacks the 7 GB worker RSS from session 4).
2. **Per-edit full re-embed on the incremental path.** One `edit-apply` on a 12-line scratch file ballooned the heap estimate 463→1,664 MB — the incremental refresh re-embedded far more than the touched file, and then corrupted the generation allocation entirely (N-10). Levers: dirty-file-only embedding (the checkpoint architecture already exists), generation-allocation locking.
3. **Relationship rendering without dedup (N-01).** Doubles token cost of every caller/callee list and breaks count/list invariants — pure render-layer fix, high value per line.
4. **Name-based external resolution (N-03/N-04).** Wastes retrieval budget on conflation and fixture noise; an `external: true` marker + qualified-id keying fixes both lookup quality and context noise.
5. **`deep-analyze` assembly before budget check (N-06)** and its `title()`-accessor tail — rank by graph distance before assembly, then cut.
6. **Diagnostics double-reads state from two sources** (in-memory snapshot vs freshness footer) — one source of truth, plus a generation-watch that forces re-hydrate (N-13).

---

## 7. Recommendations (ranked)

1. **Fix rename-apply (N-00):** teach `compare_signatures` to read `edit_type()` and pair Removed/Added with identical bodies as a rename. One focused change in `src/validation/drift.rs` unblocks a currently-dead flagship tool. Add a regression test: rename any fixture symbol via the handler and assert apply succeeds.
2. **Fix the incremental generation race (N-10):** serialize next-generation allocation (project-scoped lock) and make the losing writer retry with the next generation instead of failing; surface the error from the `index`/`edit-apply` call that triggered it, not from later tools' freshness metadata.
3. **Dedup relationship renders (N-01/N-02)** across grep-symbols, read-symbol, symbol-lookup, impact-analysis, context — and make `caller_count` always equal the emitted list.
4. **Wire MCP `index` stats (N-12)** to the job handle so the payload isn't null; include `cold_start`/`neural_warm` hints on search responses.
5. **Re-hydrate on generation change (N-13):** diagnostics (and ideally all PDG-consuming handlers) compare persisted CURRENT generation to the hydrated one and refresh when they diverge; fix the `total signatures` counter source.
6. **Externality markers + qualified keys (N-03/N-04/N-05):** stop resolving std/external calls to project namesakes; key relationships by qualified symbol id; default-exclude `tests/fixtures/**` from the production graph.
7. **Make `dry_run` honest (N-09)** and `include_symbol_map` either real or rejected (N-08); unify phase-analysis/git-status freshness sources (N-07, N-11); document project-map counts/legend (N-14).
8. **Retire or repair grep-symbols semantic mode** — currently it returns emptiness with a straight face; at minimum fall back to exact-name matching when neural yields zero.
9. **Adopt `leindexd`** as the documented default for CLI-heavy workflows to amortize the 19 s cold start; document one-server-per-project and restart-after-rebuild as operational rules.
10. **file-summary needs to deliver its contract** (complexity table, cross-file deps, focus_symbol behavior) or be demoted in the skill docs until it does.

---

## 8. Verification Against Prior Sessions (1–4)

**Held up under this session's probing:** F-01/F-02 (PDG persist — no EdgeNodeMissing anywhere), F-03 (no stale/fresh body contradiction at healthy states), F-04 (RSS measured — 463.56 vs 463.50 on first call), F-05/F-15 (GPU active; neural_ms ≈ 0 warm, daemon on KFD), F-07 (garbage query → "No results"), F-08 (no `Arc`/`*` noise hits in search), F-09 (real code snippets), F-13 (post-rebuild `.leindex` = 821 MB — though 6 generations present, see N-10 note: the window is only enforced when `retention --gc` runs, not automatically), F-14b (batch lookup clean), F-16 (impact direction correct), F-17 (relative rename scope resolved correctly — preview found all sites from a relative scope).

**Not fully holding / regressed surface:** rename-apply now fails for a *new* reason (N-00 — the F-17 scope fix works, the drift validator behind it doesn't); diagnostics `Total signatures` is wrong (7) — a field the prior reports never exercised; MCP `index` stats null; dry_run failure rendering; duplicate-relationship entries visible in every render path (prior reports didn't cover render dedup).

---

## 9. Final State After Test

- Scratch file removed; `git status` verified free of my mutations (pre-existing session-1–4 artifacts untouched).
- Index force-rebuilt via CLI: generation 6, 491 files / 11,555 signatures / 18,981 PDG nodes / 120,419 edges / 3.27 s; `.leindex` = 821 MB, 6 generations on disk.
- Known residual: the long-lived MCP server still serves a gen-4-era in-memory snapshot until restarted or forced to re-hydrate (N-13) — restarting `leindex mcp` after this report is recommended.
- GPUs/daemon untouched; embed worker idle-evicted.

*Report ends. Same bat-channel, same bat-repo, next stress test brings the hammer again.*

---

## 10. Remediation Addendum (session 6, follow-up)

All session-5 defects triaged; fixes implemented, gated (fmt clean, `clippy -D warnings` clean on both feature sets, `cargo test --workspace --exclude memcheck` green), installed via `cargo install`, and live-verified:

| ID | Disposition | Evidence |
|---|---|---|
| **N-00** | **FIXED** — `compare_signatures` now reads `edit_type()`: under `EditType::Rename`, a Removed+Added pair with identical structure (param count, return type, method flag) is emitted as informational `DriftType::Renamed` (not an error); unpaired removals keep error semantics (a rename that also changes the signature still fails). 3 regression tests added. | Live repro: `stress_six_beta → stress_six_beta_renamed` applied through the MCP tool, **on-disk rename verified**, index stayed healthy. |
| **N-10** | **FIXED** — `publish_generation_snapshot` no longer bails on "generation N already exists": the number is allocated under contention (bump past on-disk dirs, retry on lost atomic-rename races), and the health record carries the actually-published number. | Live repro: `write` + `edit-apply` (session-5's exact corruption sequence) left the project `status=fresh`, health `healthy`, no failure phase. |
| **N-01/N-02** | **FIXED** — dedup at every relationship render source: `get_direct_callers` (helpers), `relation_nodes_to_json` (read-symbol), `summarize_nodes` (symbol-lookup), grep-symbols entry builders (`build_symbol_entry` + `enrich_catalog_results`); counts now always equal the emitted lists. | impact-analysis callers: **18 entries / 18 unique / 0 dupes** (was 11 entries / 5 unique); semantic grep entries `caller_count == callers.len()` across all results. |
| **N-12** | **FIXED** — `trim_index` handled job snapshots with the legacy IndexStats projection (every field null); it now surfaces the real `IndexJobSnapshot` fields. | MCP `leindex.index` returns `job_id / status / phase / generation / completed_units / total_units / published` (verified: `status: running, phase: scan, job_id: …`). |
| **N-13** | **FIXED** — `LeIndex` tracks `hydrated_generation` (atomic; set at hydration and publish); `registry.get_or_load` compares against the persisted `CURRENT` pointer and evicts + re-hydrates when an external rebuild advanced it. | Server hydrated at generation 15; after an external CLI `--force` rebuild its own diagnostics moved to **generation 19 without a restart** (previously it served gen-4 stats under a gen-6 footer). |
| **N-09** | **FIXED** — dry-run wraps the preview payload in an explicit envelope (`success: true, dry_run: true, message`), and the apply renderer labels it `Dry run (no changes written)` instead of `No-op (content identical)`. | Live: dry-run reports "Dry run: no changes written. See preview…". |
| Semantic grep (§3.3) | **Mitigated** — when semantic symbol mode returns zero matches, it falls back to exact/substring matching on the full phrase, then each informative token longest-first, with a `mode_note` stating the match basis. Non-empty-but-junk rankings (the `from`/`log` hits) remain part of the N-05-family ranking issue. | `scrub` token now resolves `LogScrubber` when the semantic path is empty. |
| §5.1 bimodal latency | **Mitigated** — search responses now carry `cold_start: true` + note when the call exceeded 5 s, so agents see the one-time model load instead of retrying blind. | Verified in payload. |
| N-03/N-04/N-05, N-06, N-07, N-08, N-11, N-14 | **Documented, open** — name-affinity resolution/conflation (needs qualified-id keying + externality markers + fixture exclusion), token-budget overrun, dual freshness sources, `include_symbol_map` no-op, freshness semantics, project-map legend. Ranked in §7. | — |

**Residual note on cold start:** daemon log confirms **no MIGraphX recompiles** across daemon spawns (the `.mxr` cache hits); the observed ~57 s first-search under a concurrently-reindexing dirty tree is 1.7 GB of model+cache I/O contending with generation writes on the same NVMe — worst case, not the steady state (15–20 s typical, 52–84 ms warm).

### §10.1 Session-7 addendum — name-affinity family + small items

**N-03 / N-04 / N-05 — FIXED and live-verified** (index rebuilt with the new semantics):
- **Std/external guard**: call targets in std/core/alloc namespaces or on prelude container types (`String`, `Vec`, `Option`, `Arc`, …) never resolve to project namesakes; they link to one shared `NodeType::External` marker node per distinct target (`external::String.truncate`), which relationship renders show as `[external]`. Live markers confirmed (`std.fs.*`).
- **Ambiguity rule**: bare last-segment and 2-3-segment-suffix resolution only fire when the name is UNAMBIGUOUS project-wide; an ambiguous short name resolves to none of its namesakes instead of all of them (unit-tested both directions).
- **Call-edge caller/callee semantics**: `get_direct_callers`/`get_direct_callees` (all render paths) now count **Call edges only** — data-flow heuristics (every function taking/returning `String` linked to every other) and containment edges (an impl block "calling" its own methods) no longer masquerade as calls.
- **`tests/fixtures/**` excluded** from the production graph in both scan paths (git inventory post-filter + walker prune).
- Verified with the report's own probes after a full reindex: `truncate` symbol-lookup **0 callers (was 14 across ≥5 distinct functions)**; `redact` callees **0 (was three unrelated namesakes incl. one in tests/fixtures)**; `redact` context **0 fixture-noise lines**; `LogScrubber`-as-caller-of-its-own-method **gone**. Known limit: receiver-style method calls (`result.truncate(...)`) are recorded by the parser without the receiver type, so they produce no marker — they are simply no longer mis-linked (typing the receiver needs full type inference, out of scope).

**N-06 — FIXED**: `expand_context` enforces the token budget DURING assembly (4 chars ≈ 1 token) and appends a visible `/* [context truncated at the requested token budget] */` marker instead of silently overrunning (was 3,700/3,000).

**N-08 — FIXED**: `render_read_file` now renders the `symbol_map` the handler builds (name[type]:line-range list under "Symbols in range") — `include_symbol_map=true` is no longer a silent no-op.

**N-07 — FIXED**: the phase payload's content hash is renamed `analysis_fingerprint` (serialized + trimmed + rendered as "Analysis fingerprint") — no longer masquerading as the store's generation counter that the freshness footer reports.

**N-11 — FIXED**: freshness meta gains `status_note` whenever `status=fresh` coexists with a nonzero `changed_unindexed_count` ("fresh = the last index run completed; N file(s) changed in the worktree since then").

**N-14 — FIXED**: project-map render gains a legend ("[N symbols] = indexed symbol count; [out→in] = outgoing→incoming dependencies") and states the "Files in scope" count basis (source files under the scoped path).

Gates: fmt clean, `clippy -D warnings` clean (both feature sets), `cargo test --workspace --exclude memcheck` green (incl. 4 new resolution-semantics regression tests). Reinstalled via `cargo install`. Graph after rebuild: 10,186 indexed nodes (fixtures out), 121K edges, external markers present.

---

## §10.2 Session-8 addendum — save-PDG heavy optimization + lifecycle defect wave

**Date:** 2026-08-19. All fixes below live in the `v2.0.0` working tree (uncommitted with the prior sessions' fixes), rebuilt and reinstalled (`~/.cargo/bin`, `--features onnx`). Gates: fmt clean, `clippy -D warnings` clean (default + onnx), `cargo test --workspace --exclude memcheck` 38/38 binaries green (1852 lib tests, +17 new regression tests this wave).

### A. Save-PDG stage — the "saving to storage" bottleneck (user-reported)

**Root cause (measured):** the index holds **20,454 nodes / 121,570 edges**. `save_pdg_inner` deleted and reinserted **every edge row on every save** — including one-file incremental deltas — ~242K row writes + PK-index churn + per-edge JSON serialization, plus a full trigram-blob rewrite and an unconditional 11.4MB `search_snapshot.bin` rewrite in the same perceived stage.

**Fixes:**
1. **Edge diffing** (`save_edges`): loads existing rows once, computes the desired set (deterministic JSON, last-wins on duplicate parallel edges preserved), deletes only stale rows (row-value `IN (VALUES …)` chunks — PK-indexed, depth-safe), upserts only new/changed rows. Bulk subquery-DELETE fallback when >50% of edges churn. Stale-node deletion moved after edge deletion (FK-safe ordering).
2. **`BEGIN IMMEDIATE`** for the save transaction: the diff's read-before-write pattern opened a deferred read transaction, and SQLite returns `SQLITE_BUSY` immediately on a read→write upgrade without honoring `busy_timeout` — a contention regression caught by the existing `test_save_pdg_retries_after_busy_timeout_expires`. Immediate transactions restore busy-timeout waiting.
3. **Trigram blob hash-skip**: `content_hash` column (ALTER TABLE migration) + blake3 compare; unchanged index ⇒ no multi-MB rewrite.
4. **Search-snapshot identity sidecar**: `SearchSnapshot` has no volatile fields; a fingerprint sidecar skips the 11.4MB rewrite when identity matches.
5. Save stats (`edges_written/deleted/skipped`, per-stage ms) logged at INFO for live verification.

**Measured result (release, this repo):** identical-content force rebuild now saves in **296–313 ms with 1–7 edges written, 121,56x skipped** (was: delete+reinsert all 121,570). A transient SQLI `Expression tree is too large` failure found on the way (chained-OR depth limit) was fixed by the row-value form.

### B. Lifecycle defects (audit register)

1. **Stale-server `-32008` (CRITICAL) — FIXED.** Reproduced live: a competing writer holding the DB longer than the open-retry budget made every tool call fail `-32008` after ~16s, with remediation text telling the user to **delete a valid database**. Four sub-fixes:
   - `open_storage_with_retry` 3→6 attempts, backoff capped at 2s (≈36s budget ≈ full external rebuild);
   - lock-class failures render `error_type: storage_locked`, `retryable: true`, "data is intact — retry" instead of "Delete .leindex/";
   - `detect_corruption` classifies lock contention as Healthy (was `Severe` → destructive `restore_latest_generation` renamed the live DB under the running writer);
   - `restore_latest_generation` removes stale `leindex.db-wal`/`-shm` after swapping (mismatched WAL = permanent unopenable store = persistent -32008).
   Verified: fresh server's first call under a finite 20s exclusive lock **waits out the writer and succeeds** (31.1s); infinite-lock failure now shows the honest retry message.
2. **Symbol-lookup silent zero-impact — FIXED.** Responses now carry `index_freshness` always and an `impact_note` when relations come back empty under a degraded graph (no call edges), a stale index, or latency-budget truncation — precedence: budget > degraded graph > stale. Renderer prints `⚠ Note` + `Index freshness`; trimmer preserves all three fields (it also used to drop `pdg_status`).
3. **Dry-run rendered as "No-op (content identical)" — FIXED.** `trim_edit` dropped the `dry_run` flag, killing the renderer's dry-run branch. Live-verified: renders `Dry run (no changes written)` + handler message.
4. **Incremental churn — NOT REPRODUCIBLE on current code** (audit ran against the older binary): 3 consecutive fresh-server spawns + an mtime-only touch all hold generation steady; DB inventory clean (456 rows, zero missing files, no fixture phantoms). The convergence machinery (parse-plan hash skip + `delete_file_data` pruning in both reindex paths + Session-7 fixture exclusion) is working; documented rather than changed.
5. **Cancelled index task poisoned freshness — FIXED.** JoinError cancellation no longer calls `mark_index_failure` (panic still does); discriminator unit-tested both ways.
6. **`total_signatures` misleading on incremental runs — FIXED.** `IndexStats.signature_scope` = full|delta (serde-defaulted for legacy snapshots); set from parse coverage; surfaced in diagnostics JSON + rendered as an explicit scope note; CLI prints "(changed files only…)" for deltas. The audit's "Total signatures: 2 (down from 11,555)" is now labeled as a delta. **`Self` return-type display** in write structural context is a parser-level artifact (resolving it needs impl-block type inference) — documented as a bounded limitation, same family as the receiver-type limit.

### C. Verification matrix this wave
- 17 new regression tests (edge-diff ×4, trigram skip, snapshot sidecar skip, corruption-under-lock, init_failed text ×2, WAL-sidecar cleanup, JoinError ×2, impact-note ×5, trim keeps degradation fields, trim keeps dry_run).
- Live: force rebuild gen 38 fresh; no-op saves ~300 ms; dry-run render correct; `save_pdg` lookup 27 symbols/15 files; search ranked results intact; `retention --gc` reclaimed 472 MB of test generations.

---

## §10.3 Session-9 addendum — the "saving to storage" tail: 85.7s → 5.0s (17x) + RAM-safety wave

**Attribution (from the 14:06Z run's artifacts):** the stall after "Indexing: saving to storage…" was 85.7 s: ~26 s embed-daemon cold start (spawn + ORT/MIGraphX init + SFR model load incl. one first-batch collapse retry), 58.3 s ONNX inference of all 10,211 rows at b8/s128 (~5.7 ms/row), ~1.4 s persist/publish. The reported `Time: 4083ms` stopped at lexical persist — the entire neural tail was silent and unaccounted.

**Root cause of the recurring inference:** the WS10 global embedding cache was worker-complete but client-unwired (`cache_keys: vec![]` at both call sites; telemetry 0/0). Embedding is a deterministic function of (model, tokenizer, text) — the cache keys hash exactly that — so every re-index recomputed what it already had.

**Fixes (commits 4e707862, d28cb88c, 4eb617b4, f7d86c85, 17cf4df7):**
1. **Client-side cache probe + miss-only dispatch** (`embed_cache_frontend`): hits apply without touching the worker — an all-hit batch never spawns the multi-GiB daemon. Keys mirror worker semantics (last-token pooling, L2, streamed model/tokenizer digests).
2. **`put_batch`**: the per-row `put` (fsync + full `model_index.json` rewrite per row) turned a 58 s phase into 287 s at 10K rows; the batch variant budgets once, writes fsync-free (rows are re-hash-verified; torn rows read as misses), persists metadata once.
3. **FileSummary list canonicalization**: per-file symbol lists followed parallel-parse completion order, so summary cache keys changed every run (~450 permanent misses). Sorted now.
4. **Lazy CPU-fallback guard**: `cpu_fallback_reason` eagerly spawned the daemon on every index start — 26 s of pure waste once the cache answers everything. The quality gate moved to embed time. Zombie-aware `daemon_pid_alive` + stale-lock cleanup stop 20 s readiness polls against dead sockets.
5. **Daemon RAM lifecycle** (OOM evidence: Maestro cgroup 26.4 GB RAM + 11.4 GB swap, two resident daemons, rust-analyzer killed): single-daemon policy with superseded-GC, default `LEINDEX_WORKER_MAX_RSS_MB=10240` (measured SFR peak 8.06 GiB — an 8 GiB cap killed the worker mid-run), sibling-RSS-aware load floor, socket idle 600 s → 180 s, CLI keeps the daemon warm for bursts (opt-outs documented).
6. **Registry byte budget** (1.5 GiB default) + honest `estimated_memory_bytes` (token sets now counted); **streamed mmap writer** (no whole-file heap buffer; layout unchanged); progress narration + true wall time in the CLI output.

**Measured outcomes (release, this repo, sfr-400m @ MIGraphX b8):**
- No-change force index: **85.7 s → 5.0 s (17x)**; 41/41 batches cache-hit; zero worker dispatches; zero daemons resident. Requirement was ≥7x.
- Delta run (source edited since last index): only changed content embeds; with a warm daemon the tail stays seconds.
- First-ever index (cold cache): unchanged full pass (~85 s; b32 `.mxr` remains the future lever for that one-time case).
- Search: ranked, relevant results (72–75% on the pipeline query), ~4 s CLI cold, no daemon residency in steady state.
- Gates: fmt clean, `clippy -D warnings` clean (default + onnx), `cargo test --workspace --exclude memcheck` 38/38 binaries green (incl. 6 new tests this wave; trace-harness tests serialized after a process-global-hook flake was root-caused).

**Known bounds documented:** the two `20260811_*_Fork__.md` scratch files remain untracked (unrelated prior engagement); `madvise` after matrix scans deliberately skipped (page-cache accounting makes it cosmetic; re-fault costs latency).
