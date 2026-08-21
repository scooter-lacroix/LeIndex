# LeIndex Self-Hosted Stress Test — Final Report

**Date:** 2026-08-18 (session 2; supersedes the 2026-08-18 partial report and handoff)
**Test subject:** LeIndex 2.0.0 (branch `v2.0.0`, head `256bede1` + uncommitted remediation) indexing its own source tree
**Project:** `/mnt/WD-SSD/code_index_update/LeIndexer-release-1.8.4` (491 files, 11,147 symbols)
**Hardware:** CachyOS Linux, 62 GB RAM, AMD dual-GPU (ROCm 6.x / MIGraphX 2.15), NVMe SSD
**Outcome:** All 19 MCP tools exercised; 14 defects root-caused; 10 fixed in this session (+7 fixed by the prior remediation session, verified here); GPU neural search restored; warm operations brought under 100 ms.

---

## 1. Executive Summary

The stress test began against a build in which **11 of 17 tools were non-functional** (per the earlier report: every PDG-backed tool failed with `Failed to save PDG to storage`) and **semantic search took 101–161 seconds**. Root-causing produced a chain of five compounding defects, all now fixed or documented:

1. **PDG persistence corruption** (prior session, verified): a conditional `DO UPDATE … WHERE` guard in `save_pdg` silently dropped nodes from the id map, corrupting every index persist. Fixed; PDG-backed tools all work.
2. **GPU was never in use** (F-05/F-15): the embed worker could not load `libonnxruntime_providers_migraphx.so` because `/opt/rocm/lib/migraphx/lib` was missing from the worker's `LD_LIBRARY_PATH`. Every "GPU" run silently fell back to CPU inference of a 0.6B model — the true cause of the 100–200 s searches, 4.3 GB worker RSS, and 800% CPU burn. Fixed in `configure_worker_command`.
3. **MIGraphX probe defects**: the probe's smoke inference used hardcoded b1-s16 shapes that a statically exported b8-s128 graph rejects ("Got: 1, Expected: 8"), and its 20 s timeout was shorter than first-time compilation of the reranker. Both fixed (metadata-driven shapes; 120 s configurable timeout).
4. **Batch-size policy inversion**: `configured_onnx_inference_batch_size` forced batch 1 for the static model on MIGraphX, which can never match its b8-s128 compiled cache. Fixed (provider takes precedence); three tests encoding the wrong assumption were corrected with documented diagnosis.
5. **Reranker CPU trap**: `rerank_enabled=true` with a reranker that cannot compile within any sane probe budget turned every search into minutes of CPU cross-encoding. Disabled in config with documentation; needs a pre-compiled cache before re-enabling.

**Final measured state (installed binary):**

| Metric | Before | After |
|---|---|---|
| Semantic search (warm, steady state) | 101–161 s | **52–81 ms** (sfr-embedding-code-400m on GPU; 62–65 ms on qwen3) |
| Text/grep/read/lookup/context/impact/map/git tools (warm) | mixed / many failing | **0–7 ms** |
| `leindex.diagnostics` | 2.26 s, self-contradictory | **134–156 ms**, consistent |
| Full force re-index (incl. GPU embed of 11K symbols) | fails at persist | **146 s** (4.4 s core + ~100 s neural embed at 57–70% GPU utilization) |
| Search result snippets | `// name in path` echoes | real code lines |
| Tools working | 6/17 | **20/20** (19 `leindex.*` + native mirrors) |
| Project store on SSD (`.leindex`) | 18.60 GB, 98 unpruned generations | **0.92 GB**, 3-generation window (F-13 GC, follow-up session) |
| Slowest tool (any) | search 101–161 s; diagnostics 2.26 s | **deep-analyze 79 ms — every tool sub-100 ms** (follow-up session 3) |
| Graph fidelity | complexity 0 everywhere; empty callee/impact sets; import-noise symbols | complexity populated; impact = 54 dependents/6 files for `semantic_search`; noise symbols gone (follow-up session 3) |

Remaining gaps (documented, not fixed — see §7): 17 GB of unpruned generation snapshots, symbol-extraction noise, ~20 s cold start, diagnostics at ~140 ms.

---

## 2. Methodology

- Drove the MCP surface three ways: (a) the harness's MCP connection (`mcp__LeIndex__*`), (b) a persistent stdio JSON-RPC client scripted in Python against `leindex mcp` (id-matched responses, per-call wall timing, `_meta.timings` extraction), (c) `leindex tools run <tool> --args '{…}'` for isolated CLI-path timing.
- External observation: `/proc/<pid>/{stat,status,environ,task}` sampling, `ps` parentage trees, `rocm-smi --showuse --showmemuse --showpids` for true GPU engagement (KFD process list), daemon log at `~/.leindex/logs/leindex-embed-daemon.log`.
- Controlled mutation: scratch file `stress_scratch_test.rs` (created, edited via `leindex.edit-*`, renamed via `leindex.rename-symbol`, deleted). Git baseline captured before and verified after.
- Validation gates (per AGENTS.md): `cargo fmt --all --check` (clean), `cargo clippy --workspace --all-targets -- -D warnings` with and without `--features onnx` (0 errors), `cargo test --workspace --exclude memcheck` (all suites pass, exit 0).

---

## 3. Tool-by-Tool Verdicts

Legend: PASS = correct + fast. All latencies are warm-path through a persistent MCP server with the installed build.

| # | Tool | Verdict | Warm latency | Notes |
|---|---|---|---|---|
| 1 | `leindex.search` (semantic) | **PASS** (was FAIL: 101–161 s) | 65–83 ms | neural_ms 43–46 ms via GPU daemon; cold start ≈ 20 s (§7.4) |
| 2 | `leindex.search` (code) | PASS w/ caveat | ~60 ms | no score floor on garbage queries (F-07, documented) |
| 3 | `leindex.text-search` | **PASS** | < 1 ms | regex, scope, case, context lines, pagination, `in_symbol` attribution all verified; freshness metadata attached |
| 4 | `leindex.grep-symbols` (exact) | **PASS** | < 1 ms | `include_source=true` now returns real source (was empty — fixed via `read_source_snippet_resolved` + project-root resolution) |
| 5 | `leindex.grep-symbols` (semantic) | PASS | — | exercised pre-GPU-fix on CPU path; now benefits from GPU worker |
| 6 | `leindex.project-map` | PASS | < 1 ms | depth/scope/sort verified; minor: mixed indent styles in tree render at depth ≤ 2 |
| 7 | `leindex.read-file` | **PASS** | < 1 ms | line ranges, symbol map, dual numbering verified |
| 8 | `leindex.read-symbol` | **PASS** | < 1 ms | disambiguation via `file_path` works (`SearchEngine` — old report's failure — now resolves); doc comments, callers, budgets verified |
| 9 | `leindex.file-summary` | PASS w/ caveat | < 1 ms | works; symbol lists include noise entries (`*`, `Arc`) — F-08 |
| 10 | `leindex.symbol-lookup` | **PASS** | < 1 ms | callers/callees/impact verified; batch of 20 works; batch of 21 silently processes 20 (F-14b) |
| 11 | `leindex.context` | **PASS** | 5–7 ms | short + full node ids, source expansion verified |
| 12 | `leindex.deep-analyze` | PARTIAL | ~77 ms (fast) | fast but ranked name-substring matches over semantic retrieval when the neural path was cold; quality improved with warm GPU embeddings but remains the weakest retrieval tool |
| 13 | `leindex.phase-analysis` | **PASS** | < 1 ms (scoped) | phase 1/5, modes, docs_mode verified; full-repo runs remain expensive — use CLI |
| 14 | `leindex.impact-analysis` | **PASS** (fixed) | < 1 ms | direction bug fixed (§4 F-16): affected set now = dependents (backward traversal); summary counts consistent with listed callers |
| 15 | `leindex.edit-preview` | **PASS** | — | unified diff + risk on un-indexed scratch file (live fallback); clean `-32602` on missing `old_text` |
| 16 | `leindex.edit-apply` | **PASS** | — | applied + verified on disk; index generation auto-bumped (incremental refresh works) |
| 17 | `leindex.rename-symbol` | **PASS** (fixed) | — | indexed symbols: found all 3 files / all sites for `cosine_similarity`; relative-scope bug fixed (§4 F-17) |
| 18 | `leindex.write` | PASS | — | created scratch file; structural context returned |
| 19 | `leindex.git-status` | **PASS** | < 1 ms | matches `git status --porcelain` ground truth (16 M / 6 untracked); enrichment lists still show blank-named symbols (F-08) |
| 20 | `leindex.index` | PASS w/ caveat | — | works via CLI/auto-index; MCP `wait=false` returns job state incl. `last_known_state` on failure (prior session fix, verified); blocking MCP call still exceeds 30 s harness timeouts for full re-index — use CLI |
| 21 | `leindex.diagnostics` | **PASS** (fixed) | 134–156 ms | `Stale` no longer contradicts `Freshness`; RSS is a real measurement; `execution_provider_active` now reported (§4 F-03/F-04/F-15) |
| — | Native mirrors (`project_map`, `read_symbol`, `search_codebase`, …) | PASS | 0–5 ms | behavior consistent with `leindex.*` twins |

**Concurrency (old F-12):** batches of 3+ parallel cheap calls survive; the earlier "parallel calls both died" was the old server's failed-auto-index path, not a concurrency defect. Long neural calls still serialize behind the daemon (by design, single model session).

---

## 4. Findings Register (with root causes and disposition)

Numbering continues the prior handoff (F-01…F-14) and extends it.

### Fixed this session

**F-15 (CRITICAL) — GPU never active despite `Execution provider: migraphx` reported.**
Root cause chain, each link verified empirically:
- `libonnxruntime_providers_migraphx.so` (mlstack ORT 1.27.1 capi dir) NEEDED `libmigraphx_tf.so.2015000` etc., installed only under `/opt/rocm/lib/migraphx/lib`, which was absent from the worker env → EP load failed → "falling back to CPU".
- `collect_ort_diagnostics` reported the *configured* provider, masking the fallback.
- Consequence: qwen3-embed 0.6B on CPU = 125–200 s/search, 4.3 GB RSS worker at 800% CPU.
Fix: `configure_worker_command` (src/search/onnx/client.rs) prepends `$ROCM_PATH|HIP_PATH|/opt/rocm` + `lib/migraphx/lib` to the daemon/probe child's `LD_LIBRARY_PATH` (dedup, existing dirs only). Verification: manual probe rc=0; `rocm-smi --showpids` lists `leindex-embed` as a KFD compute process at 57–70% GPU use during embedding.

**F-05 (CRITICAL, prior F-number retained) — 100–200 s semantic searches.** Root cause = F-15's CPU fallback (neural_ms was small only when the whole neural layer bailed to TF-IDF; when it ran, it ran on CPU). Additional amplifier: the registry auto-index path re-indexed inside tool handlers (mitigated by prior session's `INDEX_ATTEMPT_COOLDOWN`; fresh-process runs still hydrate/re-index once). After F-15's fix: warm 65–83 ms.

**F-16 (HIGH) — impact-analysis self-contradiction ("Direct callers (10)" vs "affects 0 symbols in 0 files").**
Root cause: the affected set used `forward_impact` (callees), which returns 0 for many nodes because cross-struct method calls are unresolved in the PDG; the correct semantics for "what breaks if I change X" is dependents (backward). Fix: `impact_analysis_handler.rs` now uses `backward_impact` for the affected set, risk, and summary; `transitive_callers` kept for renderer compatibility.

**F-17 (HIGH) — rename-symbol silently reported "Files affected: 0" for a symbol with call sites in scope.**
Root cause: PDG file paths are absolute while the `scope` argument was compared with `str::starts_with` — a relative scope (`"stress_scratch_test.rs"`) filtered out every file including the definition. Fix: relative scopes resolve against the project root; comparison is component-wise (`Path::starts_with`), which also prevents `src/a.rs` matching `src/a.rs.bak`.

**F-09 (MED) — search result snippets were `// name in path` echoes.**
Root cause: `extract_signature_from_content` (src/search/search/mod.rs) skipped exactly one header line and filtered only `// [`-prefixed lines, so the enrichment header `// <name> in <path>` passed as the "signature". Fix: skip all `//`-prefixed lines; first code line wins. Verified: results now show `fn similarity(&self, node_id: &str, query: &[f32]) -> Option<f32> {`.

**F-03 (MED) — diagnostics said `Stale: true` while `Freshness: status=fresh` in the same payload.**
Root cause: `is_stale` ORed git-dirt (modified+staged+untracked vs HEAD) into the staleness flag; the indexer indexes the working tree, so uncommitted files are fully indexed. Fix: `stale` derives only from persisted health status; git-dirty count surfaced separately as `freshness.uncommitted_git_files`.

**F-04 (MED) — "Memory RSS" always equaled "Index size".**
Root cause: both derived from `diagnostics.memory_usage_bytes` (index-heap estimate) — a copy, not a measurement. Fix: `memory_rss_mb` now from `crate::cli::memory_report::current_rss_bytes()` (real /proc VmRSS). Verified: RSS varies per call (166→206 MB) and no longer tracks index size.

**F-19 (HIGH, new) — MIGraphX probe could never pass for the static model.**
(a) Probe smoke inference hardcoded batch 1 / seq 16; the static qwen3-embed-0.6b graph is b8-s128 → "Got: 1, Expected: 8" → probe failure → CPU fallback on every daemon start. Fix: probe derives shapes from session input metadata (dynamic dims keep small defaults).
(b) Batch policy forced batch 1 for non-dynamic models on MIGraphX; the compiled cache is keyed b8-s128. Fix: provider precedence in `configured_onnx_inference_batch_size` (migraphx/rocm ⇒ fixed batch 8 for all models; the batch loop pads partial batches). Three tests encoding the old assumption updated with the empirical diagnosis in comments.
(c) Probe error output was discarded (`process::exit(1)` silently) — now printed to stderr.
(d) Probe timeout 20 s < reranker first-compile time; raised to 120 s default, `LEINDEX_MIGRAPHX_PROBE_TIMEOUT_SECS` override.

**F-20 (MED, new) — compile-die-respawn loop on cold cache.** Before the cache existed, the daemon began compiling (~7.8 GB RSS, single core), died at ~115 s, client respawned, forever. Contributors: probe timeout kills + PDEATHSIG daemon teardown. Resolved in practice by the warm b8-s128 cache (loads in ~8 s) and the timeout raise; the structural cold-compile experience is documented in §7.4.

**F-10-part (LOW) — ORT version string.** `ORT version: 1.28.0` vs loaded dylib 1.27.1: mlstack capi ships two `libonnxruntime.so` versions; discovery picks the newest file name while config pins 1.27.1 (the only one with a migraphx provider). Now largely moot — with the loader-path fix the 1.27.1 provider loads and diagnostics reports both path and version; residual mismatch documented.

**Bench hygiene (prior session's uncommitted benches)** — `worker_batch_bench.rs` dead field, `gpu_embed_bench.rs` redundant `.ok()`: fixed; clippy `-D warnings` clean on both feature sets.

### Fixed by the prior remediation session (verified working here)

**F-01/F-02-part — PDG persist corruption** (`save_pdg` `DO UPDATE … WHERE` guard dropping nodes → `EdgeNodeMissing`): fixed with unconditional upsert + returned-id count assertion; WAL pragmas re-asserted; busy/locked bounded retry; legacy-schema column migration (`content_hash`, `created_at`, `updated_at`). Verified: index persists (generation 97+), all PDG tools functional.
**F-01-part — index failure returns partial data**: `last_known_state` attached on failed jobs.
**F-11-part — registry no longer serializes failed full re-index behind every call**: `INDEX_ATTEMPT_COOLDOWN`.
**grep-symbols empty `include_source`**: `read_source_snippet_resolved` + project-root resolution.
**read-symbol disambiguation** (`SearchEngine` case): works.

### Follow-up session 4 — RAM pillar: model switch to sfr-embedding-code-400m

Executed the documented worker-RAM reduction path end-to-end:
- **Tokenizer provisioned**: downloaded the WordPiece tokenizer (vocab 30,522, `token_type_ids` family — matching the model's declared inputs) from `Salesforce/SFR-Embedding-Code-400M_R` to `~/.leindex/models/sfr-embedding-code-400m-tokenizer.json`.
- **Per-model tokenizer resolution** added: `ModelResolver::resolve_tokenizer` now prefers `<model>-tokenizer.json` beside the model (the same convention the reranker always used) before the shared `tokenizer.json` — previously the shared file meant switching models silently kept the previous model's vocabulary (qwen3 BPE on a BERT-family model = plausible-looking garbage embeddings).
- **Model switched** (`neural.model_name = "sfr-embedding-code-400m"`, dimensions 1024 per the repo's own eval catalog) and **force re-embedded on GPU**: fresh MIGraphX compile (one 879 MB `.mxr`); the model collapses batch-8 inputs and the runtime's existing collapsed-batch recovery retries per-sequence at batch 1 (logged, correct outputs).
- **Verified**: worker active-inference RSS **8.9 GB → 7.0 GB** (fp32 weights 873 MB vs 1.19 GB) with idle eviction unchanged; worker listed as an AMD KFD GPU process during embedding; steady-state semantic searches **52–81 ms — faster than qwen3's 62–65 ms** — stable across ten varied-length queries (inputs are padded to the fixed s128 shape, so no query-time recompiles); deep-analyze settles at 4–5 ms; full suite with the installed binary: every tool ≤ 59 ms warm (deep-analyze's first call after a daemon start pays a one-time 1.2 s shape compile; daemon cold start itself is ~15 s, same class as qwen3). Quality spot-check passes (top hit for "how does the semantic search embedding pipeline work" is the search engine's `search` method).
- Remaining honest note: the MIGraphX runtime (compiled program + ORT arena), not the model weights, now dominates worker RSS; an fp16/quantized export or MIGraphX memory tuning is the next lever if sub-2 GB is ever required. GPU neural search, sub-100 ms tools, SSD ≤ 1 GB, and bounded generations are all in place.

### Follow-up session 3 — latency + fidelity fixes

**Diagnostics 136–156 ms → 62 ms.** Two root causes, both found by measurement: (a) `collect_ort_diagnostics` live-queried the ORT version by **spawning Python and importing onnxruntime on every call** (~110–130 ms alone) — now the config-recorded version is preferred (VAL-SETUP-020 existed for exactly this) and the discovery result is cached per process; (b) `MemoryManager::get_rss_bytes` used sysinfo's `refresh_processes`, which scans `/proc` including per-thread task dirs (~40 ms × 2 calls) — now a direct `/proc/self` read (~0.1 ms) with sysinfo fallback.

**F-18 (PDG fidelity) — FIXED.** The `StreamingPdg` feature flag (production default ON) routed full-index PDG construction through `build_fragment_from_parsed`, an unfinished **skeleton** that flattened signatures to name/kind/bytes, hardcoded `complexity: 0`, and emitted **no intra-file edges at all** (its own comment said "for the streaming skeleton we leave them empty") — which is why every streaming-built index showed complexity 0, empty callee lists, and an `impact-analysis` that reported nothing. The route now feeds the real extractor (`extract_pdg_from_signatures` → `fragment_from_pdg`, the skeleton's documented production realization); the skeleton and its adapter were removed, and the test pinning the skeleton's node-shape contract was rewritten for the real one (with diagnosis). After reindex: `semantic_search` shows **Complexity 3** (was 0) and impact-analysis reports **54 dependent symbols in 6 files, risk high** (was "0 symbols in 0 files").

**F-08 (symbol noise) — FIXED.** Root cause: `extract_import_signature` deliberately turns every Rust `use` declaration into a signature (marker `return_type: "use"`), and the graph builder materialized each as a **Function node** named by the last path segment — indexing `Arc`, `Lazy`, `Ok`, `*` and module names as searchable "functions" (they were also the top-scoring hits for garbage queries). Use-marker signatures and blank/`*` names are now skipped at graph-node creation (they carry no calls/params/imports, so no edge references them; the parser-level import API is unchanged and its test untouched). After reindex: `grep-symbols "Arc"` returns **0 function hits** (was polluted), and the garbage-query top score dropped from 0.79 to 0.17 because the noise symbols no longer win.

**F-07 (low-signal) — FIXED.** Search responses now always carry `top_score` and, when the top composite score is below a conservative 0.25 floor, `low_signal: true` plus a reformulation suggestion — carried through the LLM payload trimmer and rendered as `⚠ low signal (0.17): results may be coincidental token overlap…`. Verified: garbage query flagged, normal semantic queries unflagged.

**F-14 — resolved as filed:** (a) fast-path timings were a **benchmark artifact** — the transport attaches `_meta.timings` to every response; the earlier "missing timings" observation came from my own probe script using underscore tool names (`leindex.grep_symbols` vs the actual `leindex.grep-symbols`), which returned instant `Method not found` errors. Correct-name benchmarks show all fast paths 2–28 ms with timings attached. (b) batch `symbol-lookup` >20 symbols now returns a clean `-32602` "at most 20 entries" error instead of silently dropping the tail.

**Final latency suite (installed binary, warm):** semantic search 62–65 ms · text-search 27 ms · read-file 28 ms · grep-symbols 8 ms · symbol-lookup 6 ms · context 9 ms · impact-analysis 7 ms · project-map 7 ms · **diagnostics 62 ms** · git-status 18 ms · **deep-analyze 79 ms** · phase-analysis 14 ms — **every tool under 100 ms**. Gates: fmt clean, clippy `-D warnings` clean (both feature sets), full test suite green. Index after fixes: 18,981 PDG nodes (class containment restored), 120,402 edges, 10,423 searchable symbols (−724 import-noise entries).

### Follow-up session 2 — F-13 generation-store GC

**F-13 — RESOLVED (follow-up session): generation-store GC implemented and run.** The 17 GB / 97-generation accumulation had a precise root cause: `cleanup_project_store` and `retention_report_cli` bailed out with an empty report whenever `cas/` was missing — and this project uses the **legacy full-copy generation layout** (self-contained `generations/<N>/` dirs, no CAS, no manifest), so no shipped tool could ever prune it. Fixes landed:
- New `retain_generations_no_cas()` (src/storage/generation/retention.rs): prunes legacy generation directories to a current-anchored window (safe without CAS — legacy generations share nothing); job-pruning heuristics extended to recognize checkpoint-style stores (`<gen>`-named dirs, `lexical/pdg/neural.complete` phase markers).
- `cleanup_project_store` now runs the no-CAS prune for legacy stores instead of returning empty; `retention --report` reports them too.
- New **`leindex retention --gc [--max-generations N] [--dry-run]`** command (N defaults to 3: current + two rollback points), dispatching to the CAS sweep (`retain_after_publish`, blob GC with manifest pins) or the legacy prune as appropriate.
- 7 new tests (window retention, dry-run, missing-CURRENT fallback, never-remove-current, completed-job pruning, legacy `cleanup_project_store` integration, CLI arg parsing).

**Result on this project: `.leindex` 18.60 GB → 0.92 GB (96 generations + 599 MB of stale jobs reclaimed; 3 generations retained),** plus removal of the regenerable 2.1 GB MIGraphX compile cache for the unused sfr model. Verified after GC: index healthy (491 files / 11,147 symbols / generation fresh), all tool latencies unchanged (search 64–70 ms, read tools 0–6 ms, diagnostics 136 ms), embed worker idle-evicts so its 8.9 GB active-inference footprint is transient. Steady-state `.leindex` is bounded at ~1 GB (3-generation window + 128 MiB job cap). Future reindexes stay bounded because each GC run (or `cleanup --store`) prunes to the window.

**F-08 (MED) — FIXED in follow-up session 3** (see above): use-import signatures and blank/`*` names no longer materialize as nodes; `Arc [function]` and star-symbol hits are now zero, and the garbage-query top score fell from 0.79 to 0.17.

**F-07 (MED) — FIXED in follow-up session 3** (see above): `top_score` always reported; `low_signal` + rendered warning below the 0.25 floor.

**F-14 (LOW-MED) — RESOLVED in follow-up session 3** (see above): (a) fast-path timings were a benchmark artifact — timings are attached to every transport response; (b) batch `symbol-lookup` >20 entries now rejected with `-32602`. The `phase-analysis` "Generation" hash-vs-counter naming inconsistency remains cosmetic and open.

**F-18 (MED) — FIXED in follow-up session 3** (see above): the streaming PDG skeleton (edgeless, complexity-0) was bypassed for the real extractor; complexity and dependent impact are now populated (54 dependents / 6 files for `semantic_search`).

**F-21 (LOW, new) — environment quirk.** The ZCode AppImage exports `APPDIR`/`APPIMAGE`, which breaks rustup proxy dispatch ("unknown proxy name: 'zcode'") — all cargo invocations in this environment need `env -u APPDIR -u APPIMAGE`. Also clap's usage line shows `zcode.appimage` as bin name when APPIMAGE is set. Not a LeIndex defect; documented because it will bite every cargo invocation from this harness.

---

## 5. Performance Detail

### 5.1 Warm-path latencies (installed binary, final state — every tool sub-100 ms)

| Operation | Latency |
|---|---|
| `leindex.search` semantic (incl. 43–46 ms GPU query embed) | **62–65 ms** |
| `leindex.text-search` | 27 ms |
| `leindex.read-file` | 28 ms |
| `leindex.grep-symbols` | 8 ms |
| `leindex.read-symbol` / `file-summary` | 3–5 ms |
| `leindex.symbol-lookup` / `context` / `impact-analysis` / `project-map` | 6–9 ms |
| `leindex.diagnostics` | **62 ms** (was 136–156 ms) |
| `leindex.git-status` | 18 ms |
| `leindex.deep-analyze` | **79 ms** (was 140 ms) |
| `leindex.phase-analysis` (scoped) | 14 ms |

### 5.2 Cold paths

- **Daemon cold start ≈ 20 s** (spawn + ORT dylib + fp32 model 1.19 GB + warm .mxr cache + probe ≈ 8 s). Paid once per daemon lifetime. First search after any server start: ~20 s (`neural_ms ≈ 18 000`).
- **Full force re-index: 146 s wall** — core parse/PDG/persist 4.4 s; the rest is GPU embedding of ~11 K symbols at batch 8 (57–70% GPU utilization, worker ~8.3 GB RSS). Incremental (post-edit) re-index: seconds.
- **CLI `tools run leindex_search` (fresh process): ~20 s every call** — PDEATHSIG kills the daemon when the CLI exits, so the cold start is repaid per process. Long-lived servers (MCP/serve/leindexd) share the daemon and stay warm. Recommendation: `leindexd` as the user-scoped persistent daemon for CLI-heavy workflows.

### 5.3 Resource footprint (after F-13 GC — follow-up session)

| Item | Before GC | After GC | Assessment |
|---|---|---|---|
| `.leindex` total | **18.60 GB** | **0.92 GB** | ✅ reclaimed; bounded at ~1 GB by the 3-generation window + 128 MiB job cap |
| `.leindex/generations` | 17 GB / 97–98 dirs | 754 MB / 3 dirs | ✅ current + 2 rollback points |
| `.leindex/jobs` | 739 MB | 165 MB | ✅ completed/stale jobs reclaimed, byte-capped |
| `~/.leindex/cache/migraphx` | 5.5 GB | 2.3 GB | active model's compiled programs only; stale-model caches removed |
| `~/.leindex/models` | 7.6 GB | 7.6 GB | user data — unused variants (qwen3 ×3, sfr, rerankers) left in place; deleting them is a user decision |
| `leindex.db` | 121 MB | 121 MB | reasonable |
| MCP server RSS (warm, hydrated) | ~440–711 MB | same | index resident in RAM |
| Embed worker RSS (active GPU inference) | 4.3 GB (CPU fallback era) / 8.9 GB (qwen3 fp32 GPU) | **7.0 GB** (sfr-400m fp32 GPU; weights 873 MB — MIGraphX runtime now dominates), **idle-evicted** | session 4; fp16/quantized export is the next lever |

Remaining drastic-reduction levers (documented): fp16/quantized export of the embed model (the MIGraphX runtime, not weights, dominates the 7 GB worker RSS); pre-compile or drop the reranker models (−2.3 GB of model files). The sfr tokenizer provisioning and per-model resolution landed in session 4; unused qwen3 model variants remain user data.

### 5.4 Reliability

- Steady-state repeat variance across 10+ warm searches: 64–83 ms (tight).
- Parallel cheap calls: no failures. Expensive calls serialize on the daemon (single model session) — expected.
- Post-edit incremental refresh works (generation bumps; freshness metadata consistent).
- One residual intermittency: a *second* daemon from an old-binary server can win the spawn race after mass daemon kills — operational note: restart MCP servers after upgrading the binary (done here).

---

## 6. Changes Made (this session, uncommitted)

| File | Change |
|---|---|
| `src/search/onnx/client.rs` | `migraphx_loader_dirs()` + LD_LIBRARY_PATH augmentation in `configure_worker_command`; re-export `daemon_active_provider` |
| `src/search/onnx/client_config.rs` | `daemon_active_provider()` live health probe (50 ms budget) |
| `src/search/onnx/mod.rs` | re-export |
| `src/embed/runtime.rs` | metadata-driven probe shapes; probe timeout 120 s + env override; probe error printed |
| `src/embed/runtime_env.rs` | provider-precedence batch policy (migraphx/rocm ⇒ fixed 8) |
| `src/embed/runtime_test.rs` | 3 tests updated (were encoding the disproven b1 assumption), 1 assertion added |
| `src/embed/worker_main.rs` | probe failure reason printed to stderr |
| `src/cli/mcp/diagnostics_handler.rs` | real RSS; stale from health only; `uncommitted_git_files`; `execution_provider_active` |
| `src/cli/cli.rs` | `collect_ort_diagnostics` 4-tuple incl. active provider |
| `src/cli/mcp/output/render/mod.rs` | "Provider fallback" line when active ≠ requested |
| `src/cli/mcp/impact_analysis_handler.rs` | affected set = backward traversal; consistent summary/risk |
| `src/cli/mcp/rename_symbol_handler.rs` | scope resolution (absolute vs relative, component-wise compare) |
| `src/search/search/mod.rs` | `extract_signature_from_content` skips all comments |
| `benches/worker_batch_bench.rs`, `benches/gpu_embed_bench.rs` | clippy fixes |
| `src/storage/generation/retention.rs` | `retain_generations_no_cas()` legacy-store pruning; checkpoint-store job-completion heuristics (follow-up session) |
| `src/cli/cleanup.rs` | `cleanup_project_store` + `retention_report_cli` handle no-CAS legacy stores; `retention_gc_cli()`; legacy-store integration test (follow-up session) |
| `src/cli/cli.rs` | `retention --gc [--max-generations N] [--dry-run]` command + parsing tests; per-process ORT-diagnostics cache with config-first version (follow-up session 3) |
| `src/cli/memory.rs` | `get_rss_bytes` procfs fast path (sysinfo fallback) (follow-up session 3) |
| `src/graph/extraction.rs` | skip use-import / blank / `*` name nodes at graph build (F-08) (follow-up session 3) |
| `src/cli/leindex/indexing/mod.rs` | streaming PDG route uses the real extractor; skeleton adapter removed (F-18) (follow-up session 3) |
| `src/cli/leindex/indexing/streaming/pdg.rs` | edgeless/complexity-0 skeleton `build_fragment_from_parsed` removed (F-18) (follow-up session 3) |
| `src/cli/leindex/indexing/tests.rs` | route test rewritten for the real-extractor contract with diagnosis (follow-up session 3) |
| `src/cli/mcp/search_handler.rs` | `top_score` + `low_signal` (F-07) (follow-up session 3) |
| `src/cli/mcp/output/trim.rs`, `render/mod.rs` | low-signal fields survive trimming; rendered warning (follow-up session 3) |
| `src/cli/mcp/symbol_lookup_handler.rs` | batch >20 rejected with `-32602` (F-14b) (follow-up session 3) |
| `src/embed/model_path.rs` | per-model tokenizer resolution: `<model>-tokenizer.json` preferred over shared `tokenizer.json` (follow-up session 4) |
| `~/.leindex/models/sfr-embedding-code-400m-tokenizer.json` | provisioned from `Salesforce/SFR-Embedding-Code-400M_R` (WordPiece 30,522) (follow-up session 4) |
| `src/storage/generation/retention_test.rs` | 5 legacy-prune tests (follow-up session) |
| `~/.leindex/config/leindex.toml` | model → `qwen3-embed-0.6b` (warm b8-s128 cache); `rerank_enabled=false` (documented reason) |

Installed via `cargo install --path . --features onnx --bins --force` → `~/.cargo/bin/{leindex,leindex-embed,leindexd}`.

Validation: `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets` clean with and without `onnx`; `cargo test --workspace --exclude memcheck` all green.

## 7. Ranked Recommendations

1. ~~Generation-store GC~~ **DONE (follow-up session 2)** — `leindex retention --gc` prunes both CAS and legacy stores; this project reclaimed 17.6 GB.
2. ~~Symbol-noise filter~~ **DONE (follow-up session 3)** — use-import/blank/star nodes no longer indexed.
3. ~~Low-signal warning~~ **DONE (follow-up session 3)** — `top_score` + `low_signal` with rendered warning.
4. ~~PDG complexity/callee fidelity~~ **DONE (follow-up session 3)** — streaming route uses the real extractor.
5. ~~Cold-start reduction~~ partially addressed (session 4: sfr-400m loads faster); remaining: persistent `leindexd`, model preload at server start, or fp16 export (would also cut the 7 GB active-inference worker RSS — MIGraphX runtime dominates).
6. **Reranker cache pre-compile or removal** — currently disabled; re-enable only with a compiled .mxr.
7. `phase-analysis` Generation-field naming consistency (hash vs counter); ranking-weight retuning now that noise symbols are gone.

## 8. W6 Addendum — deterministic agent-task benchmark + external validation (2026-08-20)

Roadmap W6 complete. Two new eval modules extend the WS11 harness (no new harness):

- `src/eval/agent_tasks.rs` — deterministic agent-task suite: 29 tasks over 3
  fixture repositories (leindex-self-mirror, polyglot-checkout, docs-corpus),
  ground-truth recall@10 / MRR@10 / nDCG@10, token cost (chars/4) and
  tool-call count vs a naive ls+grep+Read emulation. Doc-section ground
  truths measure the docs tier; deep-section placement in a 12-section
  architecture doc reproduces the whole-file-read failure mode.
- `src/eval/external_suite.rs` + `src/eval/corpus/cosqa/` — external
  validation on CoSQA (ACL 2021): every-8th-record subset (63 records) of the
  official `cosqa-retrieval-test-500.json` split vendored under C-UDA 1.0
  with canonical license text and provenance README. Landscape survey
  (CodeSearchNet, CodeXGLUE, CodeQueries, CoSQA+) documented in
  `docs/baselines/AGENT_TASKS_METHODOLOGY.md` with an out-of-band protocol
  for the suites that cannot be vendored.

Gated results (tests/agent_tasks_benchmark_test.rs, ws11 report pattern):

| suite | backend | recall@10 | MRR@10 | nDCG@10 | tokens | calls |
|---|---|---:|---:|---:|---:|---:|
| internal (29 tasks) | leindex lexical | 1.000 | 0.970 | 0.974 | 400 | 1.0 |
| internal | naive | 0.897 | 0.702 | 0.751 | 1181 | 4.8 |
| doc sections (9) | leindex | 1.000 | 1.000 | 1.000 | 400 | 1.0 |
| CoSQA real (63) | leindex lexical | 0.905 | 0.621 | 0.689 | 400 | 1.0 |
| CoSQA real | naive | 0.683 | 0.585 | 0.610 | 264 | 4.9 |

Reports: `docs/baselines/2026-08-20-w6-agent-tasks.md`,
`docs/baselines/2026-08-20-w6-cosqa-external.md`. README parity (root +
pypi + npm) now states the 37-language/100+-goal breadth, docs tier, Leiden
communities, SCIP precision tier, and links the methodology. Benchmark
development itself surfaced and fixed two emulation-fidelity defects
(cosine-only fusion under-ranked real web queries; unrealistically small
fixture files hid the token-cost gap) — both fixed by matching the production
ranking shape and realistic file sizes rather than by relaxing gates.

## 9. Head-to-head vs real indexers + precision default-on (2026-08-21)

`LEINDEX_FEATURE_PRECISION_INGEST` now defaults ON (silent Tier-0 fallback
keeps indexer-less machines unchanged; `=false` remains the rollout kill).
Verified live: diagnostics reports SCIP precision enabled with no env vars.

Real head-to-head (`tools/headtohead/headtohead.py`; same corpora, queries,
ground truth, and metric formulas for every system; zoekt 2026-08-18 build
installed for the run then uninstalled):

| corpus | system | recall@10 | MRR@10 | avg tokens |
|---|---|---:|---:|---:|
| CoSQA real queries (63) | **leindex (hybrid)** | **0.921** | **0.702** | **317** |
| | ripgrep 15 (aider/cline/kilo/roo backend) | 0.857 | 0.633 | 250 |
| | universal-ctags 6.2 | 0.603 | 0.392 | 172 |
| | zoekt AND (native) | 0.000 | 0.000 | 555 |
| | zoekt OR (parity) | 0.079 | 0.015 | 1253 |
| LeIndex repo (12 curated) | **leindex (hybrid)** | **0.833** | **0.533** | **310** |
| | zoekt AND (native) | 0.750 | 0.438 | 14,561 |
| | universal-ctags 6.2 | 0.750 | 0.381 | 73,084 |
| | ripgrep 15 | 0.417 | 0.069 | 43,727 |
| | zoekt OR (parity) | 0.083 | 0.012 | 1,474,876 |

Reading: LeIndex led every system on both corpora. On the real repo the cost
gap dominates — LeIndex answered in ~310 tokens where the indexer-backed
alternatives burned 14.6k–73k and pure grep 43.7k for worse ranking. zoekt's
0.000 on CoSQA is its native AND semantics over long natural-language queries
(match nothing); its OR mode floods ranking — both are real characteristics,
reported side by side. Report: docs/baselines/2026-08-21-headtohead-indexers.md.
