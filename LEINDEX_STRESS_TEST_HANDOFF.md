# LeIndex Self-Stress-Test — Handoff (COMPLETED)

- **Status:** COMPLETE — see [`LEINDEX_STRESS_TEST_REPORT.md`](./LEINDEX_STRESS_TEST_REPORT.md) for the full final report.
- **Date completed:** 2026-08-18 (session 2)

## What happened to the open items from the previous handoff

All 19 tools from the previous plan (§5 of the old handoff) were exercised with verbatim evidence; the findings register F-01…F-14 was resolved or extended to F-21. Summary of dispositions:

- **F-05 (100 s+ searches): root-caused and FIXED.** The embed worker never ran on GPU: `libonnxruntime_providers_migraphx.so` could not resolve `libmigraphx_tf.so.2015000` because `/opt/rocm/lib/migraphx/lib` was missing from the worker's loader path (now prepended by `configure_worker_command`). Compounding fixes: metadata-driven MIGraphX probe shapes, provider-precedence batch policy (b8 for MIGraphX), probe timeout 20 s → 120 s (env-overridable), reranker disabled (cannot compile; CPU fallback was minutes/query). Warm semantic search is now 65–83 ms with the neural query embed (43–46 ms) running on GPU (verified via rocm-smi KFD process listing).
- **F-01/F-02 (PDG persist): fixed by the prior remediation session; verified here** (generation 97+ persists; all PDG tools work).
- **F-03 (staleness contradiction), F-04 (RSS == index size), F-09 (snippet echoes), F-16 (impact direction/summary), F-17 (rename relative-scope filter): FIXED** this session.
- **F-13 (disk growth): FIXED (follow-up session).** Root cause: `cleanup_project_store`/`retention_report_cli` returned empty for stores without `cas/` — this project uses the legacy full-copy generation layout. Added `retain_generations_no_cas()` (safe window pruning: legacy generations are self-contained), made `cleanup --store` and `retention --report` handle legacy stores, added **`leindex retention --gc [--max-generations N] [--dry-run]`** (default 3), extended job-completion heuristics to checkpoint-style stores, 7 new tests. **Ran it: `.leindex` 18.60 GB → 0.92 GB** (96 generations + 599 MB stale jobs reclaimed), index verified healthy afterwards, all tool latencies unchanged (search 64–70 ms).
- **F-07, F-08, F-14, F-18 + diagnostics latency: FIXED (follow-up session 3).** Diagnostics 136→62 ms (the ORT version lookup spawned Python + imported onnxruntime per call — now config-first with a process cache; and sysinfo `refresh_processes` RSS reads — now a /proc read). F-18: the `StreamingPdg` route (production default) built PDGs from an unfinished skeleton that hardcoded complexity 0 and emitted **no intra-file edges**; it now uses the real extractor — complexity populated, `semantic_search` impact = 54 dependents / 6 files. F-08: `use`-import signatures (the source of `Arc`/`*`/`Lazy` "functions") and blank names no longer become nodes. F-07: `top_score` + `low_signal` warning (floor 0.25, rendered). F-14: fast-path timings were a benchmark artifact (timings attach to every response); batch lookup >20 now rejected. **Final suite: every tool sub-100 ms (max: deep-analyze 79 ms).**
- **RAM pillar: executed (follow-up session 4).** Provisioned the sfr-embedding-code-400m tokenizer (WordPiece 30,522 from `Salesforce/SFR-Embedding-Code-400M_R`), added per-model tokenizer resolution (`<model>-tokenizer.json` preferred — previously a model switch silently kept the old vocabulary), switched `neural.model_name` to sfr-embedding-code-400m (dim 1024 per the repo eval catalog), force re-embedded on GPU (fresh 879 MB `.mxr`; the model collapses batch-8 and the runtime's b1 recovery handles it). Verified: worker active RSS 8.9 → 7.0 GB (idle-evicted as before), KFD GPU entry, steady-state searches **52–81 ms (faster than qwen3's 62–65 ms)** across varied query lengths, deep-analyze 4–5 ms, every tool ≤ 59 ms warm on the final installed-binary suite. Multi-second events are daemon cold starts (~15 s, same class as qwen3) and one first-query shape compile per daemon lifetime. Remaining lever documented: fp16/quantized export (MIGraphX runtime, not weights, dominates worker RSS).
- **F-21 (AppImage env quirk): documented** — `env -u APPDIR -u APPIMAGE` required for cargo in this harness.

## Final state

- Build: `cargo fmt` / `clippy -D warnings` (both feature sets) / `cargo test --workspace --exclude memcheck` all green.
- Installed to `~/.cargo/bin/{leindex,leindex-embed,leindexd}` via `cargo install --path . --features onnx --bins --force`.
- Config: model `qwen3-embed-0.6b` (warm b8-s128 MIGraphX cache), `rerank_enabled=false` (documented in `~/.leindex/config/leindex.toml`).
- All changes are uncommitted in the working tree (16 + 10 modified files, 2 new benches) — review and commit when ready.

---

# Session-8 addendum (2026-08-19) — save-PDG optimization + lifecycle wave

- **Save-PDG ("saving to storage") heavily optimized:** edge-level diffing in `save_pdg` (was: delete+reinsert all 121,570 edges every save), `BEGIN IMMEDIATE` transaction (restores busy-timeout under writer contention), trigram blob hash-skip, search-snapshot identity sidecar. Identical-content force rebuild now saves in ~300 ms with 1–7 edges written (was 242K row writes). Watch the `save_pdg diff summary` INFO line for live counters.
- **Stale-server -32008 fixed** (4 sub-fixes: 6-attempt open retry, honest lock remediation instead of "delete DB", `detect_corruption` treats locks as Healthy, `restore_latest_generation` clears stale WAL/SHM sidecars).
- **Symbol-lookup honest degradation** (`index_freshness` + `impact_note`), **dry-run renders "Dry run (no changes written)"** (trim kept dropping `dry_run`), **cancelled index tasks no longer poison freshness**, **`total_signatures` scope-labeled** (full|delta).
- **Incremental churn: verified not reproducible** on current code (consecutive spawns + mtime-touch hold generation; inventory clean).
- Gates green; 17 new regression tests; binaries rebuilt with `--features onnx` and installed to `~/.cargo/bin`. Still uncommitted — commit before branch ops. Known bounded limits (documented): `Self` return-type display, receiver-style external calls need type inference, semantic-grep quality floor when non-empty-but-junk.
