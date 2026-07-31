# PR #32 — CodeRabbit Deferred Remediation: Complete Execution Ledger

**Branch:** `codex/ci-quality-gates-remediation`  
**Date started:** 2026-07-31  
**Last updated:** 2026-07-31  
**Status:** ✅ LOCAL IMPLEMENTATION/VALIDATION COMPLETE; OVERALL PLAN HAS AN UNCHECKED REINDEX GATE; CODERABBIT UNAVAILABLE (RATE-LIMITED)

This file is the authoritative ledger for the remediation work. Every implementation task records its status, edited files, current line anchors, and the invariant behind the change. Line ranges are current-source anchors and may shift after formatting.

---

## Status Summary

| Group | Scope | Status |
|---|---|---|
| 1 | Low-risk correctness fixes | ✅ Complete |
| 2 | Test isolation/infrastructure | ✅ Complete |
| 3 | Medium-risk persistence, freshness, setup, and tooling fixes | ✅ Complete |
| 4 | Higher-risk indexing/search/graph refactors | ✅ Complete |
| 5 | Documentation and product-rationale fixes | ✅ Complete |
| 6 | TASKLIST documentation reconciliation | ✅ Reviewed; no additional edit required |
| Final | Formatting, clippy, tests, feature gates, lizard, oversized-file gate, final code review, fresh CodeRabbit | ✅ Local gates and final code review complete; CodeRabbit unavailable due rate limit |

---

## Group 1 — Low-Risk Correctness Fixes

### 1.1 CR-F7 — Neural candidate over-fetch overflow

- **Status:** ✅ Complete
- **File/range:** `src/search/search/mod.rs:1102`
- **Edit:** Replaced `query.top_k * 10` with `query.top_k.saturating_mul(10).max(100)`.
- **Logic:** Prevents `usize` overflow while preserving the minimum neural candidate pool.
- **Verification:** Workspace clippy/tests passed in the prior validation cycle; final post-hardening cycle pending.

### 1.2 CR-F8 — Vector tombstone behavior after clear

- **Status:** ✅ Complete
- **File/range:** `src/search/search/vector_impl.rs:117`
- **Edit:** Added `!self.cleared` to the base-row tombstone condition.
- **Logic:** A cleared vector index must not report stale pre-clear rows as newly tombstoned.
- **Verification:** Workspace clippy/tests passed in the prior validation cycle; final cycle pending.

### 1.3 CR-F10 — ONNX response-frame alignment

- **Status:** ✅ Complete
- **File/range:** `src/search/onnx/client_config.rs:22`; client guard at `src/search/onnx/client.rs:93-96`.
- **Edit:** Aligned `MAX_RESPONSE_FRAME_SIZE` to 32 MiB, matching the worker’s doubled 16 MiB default.
- **Logic:** Client and worker must enforce the same maximum response frame.
- **Verification:** ONNX feature clippy passed in the prior validation cycle; final cycle pending.

### 1.4 CR-F11 — C++ decision-node coverage

- **Status:** ✅ Complete
- **File/range:** `src/parse/cpp.rs:447-449`.
- **Edit:** Added `for_range_loop`, `catch_clause`, and `conditional_expression` to `DECISION_KINDS`.
- **Logic:** Cyclomatic complexity must count all supported C++ decision constructs.
- **Verification:** Workspace clippy/tests passed in the prior validation cycle; final cycle pending.

### 1.5 CR-F13 — Duplicate ORT detection

- **Status:** ✅ Complete
- **File/range:** `src/cli/leindex/setup.rs:1106`.
- **Edit:** Derived `ort_installed` from the single `get_ort_version()` result.
- **Logic:** Avoids two identical Python/subprocess probes during setup checks.
- **Verification:** Workspace clippy/tests passed in the prior validation cycle; final cycle pending.

### 1.6 D2 — Admission-count accumulation

- **Status:** ✅ Complete
- **File/range:** `src/cli/index_builder/mod.rs:948-1021`.
- **Edit:** Added `total_admitted` accumulation across lexical batches and used it in diagnostics.
- **Logic:** Per-batch gate counters are reset; final metrics must represent all batches.
- **Verification:** Workspace clippy/tests passed in the prior validation cycle; final cycle pending.

### 1.7 CR-F9 — Neural-weight cache invalidation

- **Status:** ✅ Complete
- **File/range:** `src/search/search/mod.rs:206-212`.
- **Edit:** Clamp weight, detect meaningful changes, and clear cached search results/byte accounting when changed.
- **Logic:** Cached rankings scored with an old neural weight are invalid after configuration changes.
- **Verification:** Workspace clippy/tests passed in the prior validation cycle; final cycle pending.

---

## Group 2 — Test Infrastructure

### 2.1 D1 — Setup environment-variable isolation

- **Status:** ✅ Complete
- **File/range:** `src/cli/leindex/setup_test.rs:665-689` plus guarded test call sites around `705-925`.
- **Edit:** Added RAII `EnvVarGuard` and converted environment-mutating setup tests.
- **Logic:** Tests restore process-wide environment state even on panic and avoid cross-test leakage.
- **Verification:** Workspace tests and clippy passed in the prior validation cycle; final cycle pending.

### 2.2 D4 — Embed-runtime environment-variable isolation

- **Status:** ✅ Complete
- **File/range:** `crates/leindex-embed/src/runtime_test.rs:9-32` plus guarded tests around `73-187`.
- **Edit:** Added and applied `EnvVarGuard` to runtime configuration tests.
- **Logic:** Runtime tests must be order-independent despite process-global environment variables.
- **Verification:** Workspace tests and ONNX clippy passed in the prior validation cycle; final cycle pending.

---

## Group 3 — Medium-Risk Persistence, Freshness, Setup, and Tooling

### 3.1 A3 — TF-IDF persistence validation

- **Status:** ✅ Complete
- **File/range:** `src/cli/index_builder/tfidf.rs:17-24, 230-292`.
- **Edit:** Added persisted schema/version, dimension, and PDG-fingerprint fields; validated persisted state and freshness.
- **Logic:** A persisted TF-IDF model is reusable only when its schema, dimension, and PDG content match the current index.
- **Verification:** Workspace tests/clippy passed in the prior validation cycle; final cycle pending.

### 3.2 A6 — Phase-5 cache fallback persistence

- **Status:** ✅ Complete
- **File/range:** `src/phase/mod.rs:288-300, 332-385`.
- **Edit:** Added cache-first fallback helpers for phases 1–4, propagated cache-save errors, and retained phase-5 cache behavior.
- **Logic:** Phase 5 must reuse valid summaries, compute missing summaries once, and report persistence failures instead of silently discarding them.
- **Verification:** Workspace tests/clippy passed in the prior validation cycle; final cycle pending.

### 3.3 CR-F6 — Index-freshness skip semantics

- **Status:** ✅ Complete
- **File/range:** `src/cli/index_freshness.rs` (current diff range).
- **Edit:** Corrected fast-scan skip composition so an inconclusive skip cannot be treated as proof of freshness.
- **Logic:** Skipping a filesystem scan must not incorrectly suppress stale-index detection.
- **Verification:** Workspace tests/clippy passed in the prior validation cycle; final cycle pending.

### 3.4 CR-F14 — ORT runtime setup consolidation

- **Status:** ✅ Complete
- **File/range:** `src/cli/leindex/setup.rs` (current diff range around runtime preparation).
- **Edit:** Consolidated ORT detection and removed unreachable/redundant setup branching.
- **Logic:** Runtime preparation must use one authoritative ORT probe and preserve actionable failure paths.
- **Verification:** Workspace tests/clippy passed in the prior validation cycle; final cycle pending.

### 3.5 CR-F12 — Windows home-directory fallback

- **Status:** ✅ Complete
- **File/range:** `src/cli/leindex/setup_ort.rs:473`.
- **Edit:** Added `USERPROFILE` fallback after `HOME` when resolving the model/runtime home directory.
- **Logic:** Windows installations commonly provide `USERPROFILE` instead of `HOME`.
- **Verification:** ONNX clippy passed in the prior validation cycle; final cycle pending.

### 3.6 D5 — Numeric ORT library candidate ordering

- **Status:** ✅ Complete
- **File/range:** `src/cli/leindex/setup_ort.rs:545` and surrounding candidate-selection helpers.
- **Edit:** Made version candidate ordering numeric/deterministic rather than lexical.
- **Logic:** `libfoo.so.10` must sort newer than `libfoo.so.9`.
- **Verification:** ONNX clippy passed in the prior validation cycle; final cycle pending.

### 3.7 D3 — Memcheck entrypoint robustness

- **Status:** ✅ Complete
- **File/range:** `tools/memcheck/tests/diff_logic.rs` (current diff range).
- **Edit:** Reworked the test entrypoint invocation and asserted command success rather than relying on a hard-coded binary path.
- **Logic:** The test must work from Cargo’s supported build layout and fail loudly when the measurement command fails.
- **Verification:** Lizard/quality gate passed in the prior validation cycle; final cycle pending.

---

## Group 4 — Higher-Risk Indexing, Search, and Graph Refactors

### 4.1 A1 — Authoritative lexical admission set for neural enrichment

- **Status:** ✅ Complete
- **Files/ranges:**
  - `src/cli/index_job.rs:191-200, 390-397, 739-785`.
  - `src/cli/leindex/indexing/mod.rs:66-76, 889, 1320-1385, 1529-1535`.
  - `src/cli/index_builder/mod.rs:1327-1369`.
  - `src/cli/leindex/indexing/tests.rs:4-54`.
  - `tests/index_job_recovery_test.rs:74-78, 121-132`.
- **Edit:** Persisted `LexicalCheckpoint.admitted_node_ids` with `serde(default)` compatibility; restored the set on resume; passed it into neural enrichment; skipped nodes not admitted by lexical indexing; canonicalized IDs with sort/dedup at the durable writer boundary.
- **Logic:** Neural embeddings must exist only for nodes admitted by the authoritative lexical index, including after crash recovery. Checkpoint bytes/hashes must be independent of HashSet iteration order and duplicate inputs.
- **Regression coverage:** Tests cover legacy checkpoint decoding, helper restoration, empty fallback, production sorting, writer canonicalization/hash equality, and durable recovery.
- **Verification:** Focused tests and workspace tests passed before the final writer hardening; final post-hardening cycle pending.

### 4.2 A2 — FileSummary O(N×M) rescan removal

- **Status:** ✅ Complete
- **Files/ranges:** `src/cli/index_builder/mod.rs:229-261, 924, 1095-1176, 1355`; `src/cli/leindex/indexing/mod.rs:586-617`.
- **Edit:** Added `FileSummaryContext`, precomputed same-file names once, and threaded the context through lexical, incremental, and neural content generation.
- **Logic:** File-summary enrichment must preserve content while replacing repeated node-by-file rescans with bounded map lookups.
- **Verification:** Workspace tests/clippy passed in the prior validation cycle; final cycle pending.

### 4.3 A5 — Blocking text scan isolation

- **Status:** ✅ Complete
- **File/range:** `src/cli/mcp/text_search_handler.rs:103` and execute-path scan closure.
- **Edit:** Moved synchronous `scan_source_paths` work into `tokio::task::spawn_blocking`, preserving parameters, pagination, and partial-result behavior.
- **Logic:** Filesystem traversal and regex/text scanning must not block the async MCP executor.
- **Verification:** Workspace clippy/tests passed in the prior validation cycle; final cycle pending.

### 4.4 B1 — Project-wide phase1 metrics

- **Status:** ✅ Complete
- **Files/ranges:** `src/phase/phase1.rs:66-98, 145+ tests`.
- **Edit:** Derived parsed-file count from unique PDG file paths and signature count from `Function | Method | Variable` nodes; excluded structural and `FileSummary` nodes; added a focused metric test.
- **Logic:** Incremental phase summaries must describe the resident project graph, while synthetic summaries/classes/modules must not inflate parser-signature totals.
- **Verification:** Focused metric test and workspace tests passed before final hardening; final cycle pending.

### 4.5 C2 — Duplicate-name mapping and local target deduplication

- **Status:** ✅ Complete
- **Files/ranges:** `src/graph/extraction.rs` (current diff range); related extraction tests.
- **Edit:** Changed duplicate symbol mapping to first-wins insertion and deduplicated local call targets.
- **Logic:** A duplicate name must not silently replace the original node mapping, and one call site must not emit duplicate targets.
- **Verification:** Workspace tests/clippy passed in the prior validation cycle; final cycle pending.

---

## Group 5 — Documentation and Product-Rationale Fixes

### 5.1 A7 — Fail-fast source collection rationale

- **Status:** ✅ Complete
- **File/range:** `src/cli/index_builder/mod.rs` (source collection documentation comment; current diff range).
- **Edit:** Documented why source collection fails fast instead of silently indexing an incomplete inventory.
- **Logic:** Partial source inventories can produce misleading, destructive index generations; callers need an explicit failure.
- **Verification:** Workspace clippy passed in the prior validation cycle; final cycle pending.

### 5.2 C1 — Cross-file fan-out behavior and deduplication

- **Status:** ✅ Complete
- **File/range:** `src/graph/extraction_cross_file.rs` (resolver documentation and target sort/dedup range).
- **Edit:** Documented caller-to-callee fan-out and sorted/deduplicated resolved targets.
- **Logic:** Cross-file resolution may legitimately fan out, but output order and duplicates must be deterministic for stable graph/search artifacts.
- **Verification:** Workspace tests/clippy passed in the prior validation cycle; final cycle pending.

---

## Group 6 — TASKLIST Documentation Reconciliation

### 6.1–6.5 CR-F1 through CR-F5

- **Status:** ✅ Reviewed; no additional edit required in this remediation pass.
- **File:** `TASKLIST.md`.
- **Finding:** Existing TASKLIST entries already report the current oversized-file, lizard, format, clippy, workspace-test, and reindex-gate state. No contradictory status caused by the edits in this pass required a TASKLIST change.
- **Evidence:** `TASKLIST.md` records all Rust files below 2,000 lines, lizard passing, format/clippy/tests passing, and the serialized reindex gate intentionally unchecked because no final serialized refresh was run in this pass. The current oversized-file command returned no files over 2,000 lines.

---

## Complete Changed-File Ledger

The following inventory is the final `git status --short` accounting. It includes every tracked modified file and every untracked entry; source files include the current logic area/rationale:

1. `.github/workflows/quality.yml` — jscpd/Lizard workflow syntax and command gate.
2. `TASKLIST.md` — current gate/reindex/task-status reconciliation.
3. `crates/leindex-embed/src/runtime_test.rs` — D4 environment guards.
4. `crates/leindex-embed/src/worker_main.rs` — socket-worker complexity split.
5. `src/cli/index_builder/hybrid.rs` — embedding cardinality/readiness validation and TF-IDF documentation.
6. `src/cli/index_builder/mod.rs` — admission metrics, neural admission plumbing, summary precompute, source-collection rationale.
7. `src/cli/index_builder/tests.rs` — indexing regressions.
8. `src/cli/index_builder/tfidf.rs` — persisted-state schema/freshness validation.
9. `src/cli/index_freshness.rs` — conservative freshness skip semantics.
10. `src/cli/index_job.rs` — durable admission metadata and canonical checkpoint persistence.
11. `src/cli/leindex/indexing/helpers.rs` — importer boundaries, reuse continuation, helper regression.
12. `src/cli/leindex/indexing/load.rs` — persisted fingerprint/freshness integration.
13. `src/cli/leindex/indexing/mod.rs` — admission restore and summary-context integration.
14. `src/cli/leindex/indexing/tests.rs` — admission sorting/restoration regressions.
15. `src/cli/leindex/query.rs` — deterministic event-loop candidates.
16. `src/cli/leindex/setup.rs` — ORT setup and invalid model-name diagnostics.
17. `src/cli/leindex/setup_models.rs` — model validation, atomic staging/rollback, unique install paths.
18. `src/cli/leindex/setup_ort.rs` — Windows home fallback and numeric library ordering.
19. `src/cli/leindex/setup_test.rs` — setup environment guards and model regressions.
20. `src/cli/mcp/grep_symbols_handler.rs` — regex/partial-result/cache handling.
21. `src/cli/mcp/output/render/mod.rs` — rendering complexity split.
22. `src/cli/mcp/output/trim_test.rs` — safe trim assertions.
23. `src/cli/mcp/phase_handler.rs` — phase-analysis complexity split.
24. `src/cli/mcp/prompts_resources.rs` — prompt/resource contract fixes.
25. `src/cli/mcp/search_handler.rs` — normalized scoped pagination and absolute/relative scope matching.
26. `src/cli/mcp/server.rs` — bounded socket framing, first-frame timeout selector, test extraction.
27. `src/cli/mcp/text_search_handler.rs` — blocking scan isolation and pagination.
28. `src/cli/registry.rs` — indexing job lifecycle/published health.
29. `src/global/registry.rs` — checked persisted file-count conversion and regression.
30. `src/graph/extraction.rs` — first-wins duplicate mapping and target deduplication.
31. `src/graph/extraction_cross_file.rs` — deterministic call fan-out and exact flow-key resolution.
32. `src/graph/extraction_test.rs` — exact-flow negative and duplicate-node regressions.
33. `src/parse/cpp.rs` — decision coverage and range-loop CFG dispatch/regression.
34. `src/parse/csharp.rs` — decision-node coverage.
35. `src/parse/javascript.rs` — parser import/decision fixes.
36. `src/parse/ruby.rs` — parser complexity expectation/fix.
37. `src/phase/mod.rs` — cache-first fallback/error propagation.
38. `src/phase/phase1.rs` — project-wide PDG metrics and regression.
39. `src/search/onnx/client_config.rs` — response-frame/PID cleanup hardening.
40. `src/search/search/mod.rs` — overflow-safe candidate sizing, cache invalidation, mmap hydration.
41. `src/search/search/pruner.rs` — normalized generated-path matching.
42. `src/search/search/vector_impl.rs` — clear/tombstone guard.
43. `src/storage/schema.rs` — read-only schema validation.
44. `tests/index_job_recovery_test.rs` — durable admission checkpoint/recovery coverage.
45. `tools/memcheck/tests/diff_logic.rs` — robust test entrypoint/status assertion.

Untracked entries: `Tracker.md` — this complete execution ledger; `docs/plans/` — existing remediation plan artifact; `src/cli/mcp/server_test.rs` — extracted MCP socket tests.

**Final working-tree snapshot:** 45 tracked files modified plus 3 untracked entries = 48 status entries. Final tracked diff stat: 45 files changed, 1,822 insertions, 1,034 deletions. Earlier 39/42-entry snapshots are historical and superseded.

---

## Regression Tests Added or Updated

| Test | Location | Purpose |
|---|---|---|
| `lexical_checkpoint_admitted_ids_are_backward_compatible` | `src/cli/index_job.rs` | Legacy JSON without the new field decodes to an empty set. |
| `lexical_checkpoint_writer_canonicalizes_admitted_ids` | `src/cli/index_job.rs` | Different orderings and duplicates produce the same persisted hash and sorted unique IDs. |
| `lexical_checkpoint_payload_is_deterministic_when_ids_are_sorted` | `src/cli/index_job.rs` | Stable JSON payload for canonical IDs. |
| `admitted_node_ids_are_sorted_for_checkpoint_payloads` | `src/cli/leindex/indexing/tests.rs` | Production sorting helper round-trips through checkpoint JSON. |
| `admitted_node_ids_restore_from_lexical_checkpoint` | `src/cli/leindex/indexing/tests.rs` | Resumes the exact persisted admission set. |
| `missing_lexical_checkpoint_restores_empty_admission_set` | `src/cli/leindex/indexing/tests.rs` | Safe legacy/missing-checkpoint fallback. |
| `pdg_signature_count_excludes_structural_and_summary_nodes` | `src/phase/phase1.rs` | Pins B1 signature semantics. |
| `test_resume_each_phase` | `tests/index_job_recovery_test.rs` | Durable lexical admission IDs survive checkpoint read/write and canonicalization. |

---

## Historical Remaining Finalization Work (Superseded)

> Historical pre-edit checklist. Parser, prompt, framing, Ruby, daemon-safety, and ledger items listed below were either implemented in this pass or remain unrelated queued work; the authoritative final evidence is recorded later in this file.

| Task | Planned files | Intended logic/invariant | Status |
|---|---|---|---|
| Add parser regressions | `src/parse/javascript.rs`, `src/parse/ruby.rs` | Pin import filtering and decision-node complexity semantics introduced by the current fixes. | ⏳ Planned |
| Add prompt regression | `src/cli/mcp/prompts_resources.rs` | Require a non-empty string query for `investigation_workflow`; preserve successful prompt generation. | ⏳ Planned |
| Add MCP framing regressions | `src/cli/mcp/server.rs` | Verify bounded newline reads reject oversized input and preserve clean EOF/line framing. | ⏳ Planned |
| Reconcile ledger and task list | `Tracker.md`, `TASKLIST.md` | Record every expanded-scope file, current line anchors, rationale, and final command evidence without claiming unverified gates. | ⏳ Planned |
| Correct Ruby regression expectation | `src/parse/ruby.rs:431` | The focused test observed five decisions (the method body conditional plus two modifier forms and ternary); assert the measured behavior rather than undercounting it. | ⏳ Planned |
| Harden/clarify daemon cleanup safety | `src/search/onnx/client_config.rs`, `Tracker.md` | Re-check ownership immediately before escalation and document fail-closed non-Linux behavior; investigate pidfd/platform support before final status. | ⏳ Planned |
| Expand MCP framing regression | `src/cli/mcp/server.rs` | Exercise actual socket Content-Length and newline framing transitions in addition to the bounded-line helper. | ⏳ Planned |
| Run final review and validation | repository-wide | Review changes, then run format, clippy, workspace tests, feature gates, quality gates, and fresh CodeRabbit; fix every finding. | ⏳ Planned |

## Historical CodeRabbit Review Scope — 16 Findings (Superseded Status Snapshot)

> Historical planning snapshot. These findings were resolved in the remediation work; the final local gates are recorded in the Final Validation Evidence section below.

Fresh review command: `coderabbit review --agent --base codex/release-1.8.4` (2026-07-31). These findings are now part of the active ledger and must be resolved or explicitly documented with evidence:

| ID | File | Planned edit / invariant | Status |
|---|---|---|---|
| CR16-01 | `.github/workflows/quality.yml` | Restore the multiline `run: |` declaration so the jscpd job is valid YAML and executes its command. | ✅ Complete; fresh YAML parse passed |
| CR16-02 | `src/search/search/mod.rs` | Preserve mmap-backed neural embeddings during snapshot hydration; avoid per-node cloning while retaining scoring behavior. | ✅ Complete; fresh review did not re-flag |
| CR16-03 | `src/search/search/pruner.rs` | Normalize Windows separators before generated-path matching and remove redundant patterns. | ✅ Complete; fresh review did not re-flag |
| CR16-04 | `src/search/search/mod.rs` | Return no semantic vector results when a semantic query has no embedding. | ✅ Complete; fresh review did not re-flag |
| CR16-05 | `src/search/search/mod.rs` | Make cache byte/count accounting correct across count-capacity eviction and byte-budget eviction. | ✅ Complete; fresh review did not re-flag |
| CR16-06 | `src/parse/csharp.rs` | Count `do_statement` decisions and route its CFG through loop handling. | ✅ Complete; fresh review did not re-flag |
| CR16-07 | `src/search/onnx/client_config.rs` | Validate PID sidecar identity immediately before signaling; preserve fail-closed cleanup. | ✅ Complete; fresh review did not re-flag |
| CR16-08 | `src/cli/mcp/output/trim_test.rs` | Make ellipsis assertion diagnostics safe for empty, short, and multibyte strings. | ✅ Complete; fresh review did not re-flag |
| CR16-09 | `src/cli/mcp/grep_symbols_handler.rs:58` | Report dependency count from the complete callee ID set. | ✅ Verified already fixed |
| CR16-10 | `src/cli/mcp/grep_symbols_handler.rs` | Cache live file bytes/freshness per path during symbol processing to avoid repeated I/O. | ✅ Complete; fresh review did not re-flag |
| CR16-11 | `src/cli/mcp/prompts_resources.rs` | Correct documented `LEINDEX_PORT` default to match `DEFAULT_MCP_PORT`. | ✅ Complete; fresh review did not re-flag |
| CR16-12 | `src/cli/mcp/server.rs` | Bound socket reads/incremental framing work with timeouts to prevent slow-peer resource retention. | ✅ Complete; fresh review did not re-flag |
| CR16-13 | `src/cli/mcp/prompts_resources.rs` | Serialize resource MIME fields as MCP-compatible `mimeType`. | ✅ Complete; fresh review did not re-flag |
| CR16-14 | `src/storage/schema.rs` | Validate schema version with a read-only SELECT before accepting a read-only storage handle. | ✅ Complete; fresh review did not re-flag |
| CR16-15 | `src/cli/mcp/prompts_resources.rs:136-146` | Validate `investigation_workflow.query` as present, string, and non-whitespace. | ✅ Verified already fixed |
| CR16-16 | `src/cli/leindex/query.rs` | Borrow cached file bytes during reranking instead of cloning complete buffers. | ✅ Complete; fresh review did not re-flag |

## Historical CodeRabbit Review Scope — 4 Findings (Superseded Status Snapshot)

> Historical planning snapshot. These findings were resolved and locally revalidated; statuses below are retained for audit history only.

Fresh review rerun after CR16 work (2026-07-31) reported four findings:

| ID | File | Planned edit / invariant | Status |
|---|---|---|---|
| CR17-01 | `src/search/onnx/client_config.rs` | Reconcile response-frame constant with the documented worker-side 32 MiB protocol limit. | ✅ Complete; feature clippy passed |
| CR17-02 | `src/cli/mcp/grep_symbols_handler.rs` | Do not byte-prefilter regex-shaped symbol queries; regex matching must remain authoritative. | ✅ Complete; workspace tests passed |
| CR17-03 | `src/cli/mcp/grep_symbols_handler.rs` | Treat unreadable live files as stale/missing entries under partial-result semantics instead of aborting the entire symbol search. | ✅ Complete; workspace tests passed |
| CR17-04 | `src/cli/leindex/query.rs` | Preserve deterministic event-loop candidate ordering; HashSet iteration must not affect fuzzy results. | ✅ Complete; workspace tests passed |

## Historical Finalization Batch — Completed and Superseded

> Historical pre-edit plan. All listed implementation slices were completed; the final validation evidence below supersedes the old in-progress markers.

This batch is recorded before any source edit. Each item will be marked complete only after the edit, review, and validation evidence are available.

| Task | Planned files | Planned line anchors | Logic/invariant | Status |
|---|---|---|---|---|
| CR17-01 response-frame contract | `src/search/onnx/client_config.rs` | `17-22`, related daemon protocol constants | Keep the client response limit exactly aligned with the worker’s documented 32 MiB maximum; do not silently accept a larger allocation. | 🔄 In progress |
| CR17-02 live regex prefilter | `src/cli/mcp/grep_symbols_handler.rs` | `295-315`, `525-535` | Byte prefiltering is only an optimization for literal queries. Regex-shaped queries must reach authoritative regex matching and never be discarded by a literal substring heuristic. | 🔄 In progress |
| CR17-03 unreadable live-file handling | `src/cli/mcp/grep_symbols_handler.rs` | `235-241`, live-candidate fallback | A single unreadable/deleted file must be treated as stale/missing under partial-result semantics; one filesystem error must not abort the complete symbol search. | 🔄 In progress |
| CR17-04 deterministic event-loop candidates | `src/cli/leindex/query.rs` | `980-998` | Hash-based candidate collection must be sorted/deduplicated before scoring so fuzzy results are reproducible across runs. | 🔄 In progress |
| MCP framing regression | `src/cli/mcp/server.rs` | socket loop `1224-1393`, tests `1570+` | Exercise actual Unix-socket Content-Length framing, newline framing, malformed headers, incomplete headers, and timeout/close behavior. | 🔄 In progress |
| Lizard split: MCP socket | `src/cli/mcp/server.rs` | `handle_socket_connection` around `1224-1393` | Extract framing/header/payload/response helpers without changing protocol behavior; reduce CCN below 16. | 🔄 In progress |
| Lizard split: phase analysis | `src/cli/mcp/phase_handler.rs` | `execute_phase_analysis` around `261-331` | Extract target/configuration and phase execution/report assembly helpers while preserving phase selection and output schema. | 🔄 In progress |
| Lizard split: embed socket worker | `crates/leindex-embed/src/worker_main.rs` | `run_socket_worker` around `201-283` | Extract initialization/accept/retry lifecycle helpers without changing cleanup, readiness, or idle-shutdown semantics. | 🔄 In progress |
| Daemon PID safety documentation/test | `src/search/onnx/client_config.rs`, `Tracker.md` | PID ownership/kill path around `300-390` | Preserve immediate ownership recheck and document platform limitations; stale cleanup remains fail-closed when identity cannot be proven. | 🔄 In progress |
| Reconcile final ledger | `Tracker.md`, `TASKLIST.md` | final sections | Update all statuses, edited-file inventory, line ranges, and command outputs from the final post-edit state only. | ⏳ Planned |

## Historical CodeRabbit Review — 13 Findings After Final Server Split (Resolved)

> Historical pre-remediation inventory. CR18-01 through CR18-13 were resolved in the subsequent CR18 batches; the current statuses are summarized in the final evidence section.

Fresh review command: `coderabbit review --agent --base codex/release-1.8.4` (latest final-validation rerun). These findings are authoritative for the next remediation batch and remain open until individually fixed and revalidated.

| ID | File | Planned edit / invariant | Status |
|---|---|---|---|
| CR18-01 | `src/cli/index_builder/hybrid.rs` | Update `tfidf_dimension` documentation to describe the configured TF-IDF dimension rather than claiming a fixed 768. | ⏳ Planned |
| CR18-02 | `src/cli/index_builder/hybrid.rs` | Validate decoded embedding-vector length against `texts.len()` in `EmbedResult::Success`; do not trust `response.count` as the storage-length invariant. | ⏳ Planned |
| CR18-03 | `src/cli/leindex/indexing/helpers.rs` | Replace raw importer `contains` matching with path-boundary-aware relative-path matching to avoid false importer edges. | ⏳ Planned |
| CR18-04 | `src/cli/leindex/indexing/helpers.rs` | Treat parse checkpoint/source-read failures in the reuse loop as non-reusable entries and continue parsing rather than aborting the whole resume. | ⏳ Planned |
| CR18-05 | `src/cli/index_builder/hybrid.rs` | Derive `HybridLocal::is_onnx_loaded` from `EmbeddingClient` neural availability/readiness rather than variant presence alone. | ⏳ Planned |
| CR18-06 | `TASKLIST.md` | Reconcile contradictory workspace-test claims in required gates and finalization history without erasing historical evidence. | ⏳ Planned |
| CR18-07 | `TASKLIST.md` | Reconcile jscpd percentages/threshold statements so authoritative current status is distinct from historical snapshots. | ⏳ Planned |
| CR18-08 | `TASKLIST.md` | Reconcile Rust line-count gate references with the current zero-over-2,000 result and historical entries. | ⏳ Planned |
| CR18-09 | `src/parse/scala.rs` | Remove duplicate `match_expression` decision-kind entry; complexity must count each construct once. | ⏳ Planned |
| CR18-10 | `tests/index_job_recovery_test.rs` | Assert the first snapshot is `Running` before issuing the concurrent second `start_index_job` call. | ⏳ Planned |
| CR18-11 | `src/cli/leindex/setup_models.rs` | Reject model names matching neither the dynamic filename nor the supported legacy stem before proceeding. | ⏳ Planned |
| CR18-12 | `src/cli/mcp/server.rs` | Prevent the initial socket read timeout from prematurely terminating an idle client before its first request while retaining bounded slow-peer protection. | ⏳ Planned |
| CR18-13 | `src/cli/leindex/setup_models.rs` | Stage downloaded model sources into temporary sibling paths and atomically replace destinations; never remove a valid destination before replacement succeeds. | ⏳ Planned |

## CR18 Implementation Batch 1 — Planned Before Source Edits

| Task | Files/ranges | Logic/invariant | Status |
|---|---|---|---|
| Hybrid response validation/readiness | `src/cli/index_builder/hybrid.rs:149-151,268-273,358-364,406-420, is_onnx_loaded` | Decoded vector count must match input count; ONNX-loaded state must reflect worker readiness; documentation must describe configured TF-IDF dimension. | ✅ Complete; fmt/check/clippy and focused tests passed |
| Resume/importer correctness | `src/cli/leindex/indexing/helpers.rs:83-92,283-308` | Importer matching must respect normalized path boundaries; one unreadable checkpoint/source artifact must skip reuse and allow normal parsing to continue. | ✅ Complete; fmt/check/clippy and focused tests passed |
| Scala/recovery regressions | `src/parse/scala.rs:315-322`, `tests/index_job_recovery_test.rs:411-457` | Count each Scala match construct once; prove the first coalesced job is Running before the second request. | ✅ Complete; Scala already had one match entry; recovery regression passed |

## Historical CR18 Implementation Batch 2 — Completed

> Historical pre-edit plan. Model validation/atomic installation, initial socket timeout handling, and TASKLIST reconciliation were implemented and locally validated.

| Task | Files/ranges | Logic/invariant | Status |
|---|---|---|---|
| Model-name validation and atomic installation | `src/cli/leindex/setup_models.rs:275-298,506-526` plus setup tests | Accept only dynamic or supported legacy model names; stage each downloaded file beside its destination and replace only after staging succeeds, preserving the prior destination on staging failure. | 🔄 In progress |
| MCP initial socket timeout | `src/cli/mcp/server.rs:1237-1358`, `src/cli/mcp/server_test.rs` | Give an idle client a longer first-request grace period while retaining the 30-second slow-peer timeout for subsequent frames and payload/header reads. | 🔄 In progress |
| TASKLIST reconciliation | `TASKLIST.md:1-16,107-114` | Make current gate status authoritative and label historical snapshots without deleting evidence; align test, jscpd, and line-count claims. | 🔄 In progress |

## Historical CR18 Batch 2 Corrections — Completed

> Historical correction plan. Rollback diagnostics, pre-side-effect validation, and boundary/timeout regressions were implemented and locally validated.

| Task | Files/ranges | Logic/invariant | Status |
|---|---|---|---|
| Rollback-safe model replacement | `src/cli/leindex/setup_models.rs:275-330` | If destination replacement fails after staging, restore the prior destination from a sibling backup; staging failures must never remove a valid installed model. | 🔄 In progress |
| Validate model name before filesystem side effects | `src/cli/leindex/setup_models.rs:506-526` | Reject unsupported names before resolving/creating the model directory. | 🔄 In progress |
| Reconcile stale TASKLIST historical test claim | `TASKLIST.md:111` | Label the 1200/1 result as historical and point to Tracker for current evidence. | 🔄 In progress |
| Add boundary/timeout regressions | `src/cli/leindex/indexing/helpers.rs`, `src/cli/mcp/server_test.rs` | Pin normalized component-boundary matching and initial-vs-subsequent socket timeout contracts. | 🔄 In progress |

## Historical CR18 Final Corrections and CR19 Review Batch — Completed

> Historical pre-edit plan. The correction rows below are retained as an audit trail; final status and evidence are recorded later in this ledger.

### Verified CR18 corrections

| Task | File/range | Invariant | Status |
|---|---|---|---|
| Move helper tests after production items | `src/cli/leindex/indexing/helpers.rs:392+` | Keep `#[cfg(test)]` module last so strict clippy `items_after_test_module` remains clean. | 🔄 Planned |
| Report backup-restore failure | `src/cli/leindex/setup_models.rs:300-330` | Never silently discard failure to restore the prior model destination. | 🔄 Planned |
| Improve invalid-model diagnostic | `src/cli/leindex/setup_models.rs:555+` | Reject unsupported names before filesystem side effects with a meaningful model directory/context. | 🔄 Planned |
| Exercise socket timeout behavior | `src/cli/mcp/server_test.rs` | Test the framing timeout policy rather than only comparing constants. | 🔄 Planned |

### Fresh CodeRabbit Review — CR19 findings

Fresh review command: `coderabbit review --agent --base codex/release-1.8.4` (latest rerun after CR18). These findings are authoritative and are planned before source edits:

| ID | File | Planned edit / invariant | Status |
|---|---|---|---|
| CR19-01 | `src/parse/cpp.rs` | Route `for_range_loop` through CFG loop dispatch wherever `DECISION_KINDS` counts it. | ⏳ Planned |
| CR19-02 | `src/cli/mcp/search_handler.rs` | Retry scoped search until filtered results cover `offset + top_k`, not merely one non-empty page. | ⏳ Planned |
| CR19-03 | `src/cli/mcp/search_handler.rs` | Match scope descendants and dotted directory names using normalized path boundaries. | ⏳ Planned |
| CR19-04 | `src/global/registry.rs` | Convert persisted `file_count` with checked `i64`→`usize`; return a mapping error on negative/out-of-range values. | ⏳ Planned |
| CR19-05 | `TASKLIST.md` | Keep serialized reindex checklist unchecked until a final refresh is actually run. | ⏳ Planned |
| CR19-06 | `TASKLIST.md` | Use one authoritative current jscpd result and keep the 8% gate incomplete while current result is 8.58%. | ⏳ Planned |
| CR19-07 | `TASKLIST.md` | Separate written-plan completion from design approval. | ⏳ Planned |
| CR19-08 | `TASKLIST.md` | Keep workspace-test checklist status tied to the final-tree test run and record exact current evidence. | ⏳ Planned |
| CR19-09 | `TASKLIST.md` | Remove contradictory clippy claims and identify the latest authoritative command. | ⏳ Planned |
| CR19-10 | `TASKLIST.md` | Reconcile file-size inventory against one final-tree inventory command. | ⏳ Planned |
| CR19-11 | `TASKLIST.md` | Reconcile Lizard status against the exact final-tree command. | ⏳ Planned |
| CR19-12 | `src/graph/extraction_cross_file.rs` | Remove name-only fallback in file-scoped cross-file flow resolution; use exact normalized keys. | ⏳ Planned |

## Final Validation Gates

### Finalization correction — socket regression delimiter literals

- **Status:** ✅ Complete
- **File/range:** `src/cli/mcp/server.rs:1639-1714` (formatted anchors may shift)
- **Edit:** Corrected the regression-test byte/string literals so newline and Content-Length tests transmit actual `\n` and `\r\n` delimiters rather than literal backslash characters.
- **Logic/invariant:** The tests now exercise the real transport framing implementation; production protocol behavior was unchanged.
- **Verification:** `cargo fmt --all --check` passed; both focused socket regressions passed (`1 passed, 0 failed` each).

### Finalization correction — oversized server module and header bound

- **Status:** ✅ Complete; final Lizard/oversized-file/workspace validation passed
- **Files/ranges:** `src/cli/mcp/server.rs:1250`, `src/cli/mcp/server.rs:1570-2178`, new `src/cli/mcp/server_test.rs`
- **Planned edit:** Preserve the existing payload-sized bound for the first line because newline-delimited JSON is itself a complete payload, keep the 10 KiB bound for Content-Length header lines, and move the final test module into an out-of-line child test file without changing test visibility or production APIs.
- **Logic/invariant:** Newline-framed payloads may be as large as the configured 10 MiB payload limit; header/control lines must remain bounded by the 10 KiB header limit; extracting tests must reduce the production module below the 2,000-line repository policy while preserving `super::*` private access and all test behavior.
- **Verification planned:** `cargo fmt --all --check`, focused socket tests, strict workspace clippy, workspace tests, ONNX clippy, Lizard, oversized-file check, and diff hygiene.

These commands are being rerun after the last checkpoint-writer/test hardening edits. Update statuses only from their final outputs:

| Gate | Command | Status |
|---|---|---|
| Format | `cargo fmt --all --check` | ✅ Passed after CR19 edits |

| Clippy | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ Final corrected run passed |




| Workspace tests | `cargo test --workspace` | ✅ Final corrected run passed; 0 failures (exact totals from final command output recorded below) |


| ONNX clippy | `cargo clippy -p leindex-embed --features onnx -- -D warnings` | ✅ Passed |

| ONNX + remote clippy | `cargo clippy -p leindex --features onnx,remote-embeddings -- -D warnings` | ✅ Final corrected run passed |

| Lizard | `lizard src/ crates/leindex-embed/src/ -C 15 -x '*/tests/*'` | ✅ Passed; zero violations |

| Oversized files | `find src crates -name '*.rs' -print0 | xargs -0 wc -l | awk '$1>2000'` | ✅ Passed; zero files over 2,000 lines |


---

## Final Review Corrections — Implemented and Validated

The final reviewer found three hardening items after the otherwise green validation cycle. These are recorded before edits:

1. **Scope representation:** Implemented canonical absolute/relative matching with separator normalization, component boundaries, and relative/Windows regression coverage.
2. **Model staging uniqueness:** Implemented a per-process atomic sequence in staging and backup sibling names, preventing same-process install collisions.
3. **Exact flow contract:** Documented and tested exact normalized qualified-key resolution; the positive exact-key and negative unrelated bare-name regressions pass.

## Completion Log

| # | Completed task/edit | Files and current anchors | Logic/invariant | Verification state |
|---:|---|---|---|---|
| 1 | Group 1 correctness fixes | `src/search/search/mod.rs`, `vector_impl.rs`, `client_config.rs`, `parse/cpp.rs`, `setup.rs`, `index_builder/mod.rs` | Overflow safety, frame alignment, parser coverage, setup deduplication, cumulative metrics, cache invalidation | Final fmt/clippy/tests/feature gates passed |
| 2 | Group 2 environment isolation | `setup_test.rs:665+`; `runtime_test.rs:9+` | No process-global env leakage between tests | Final workspace tests/clippy passed |
| 3 | A3/A6/CR-F6/CR-F12/CR-F14/D3/D5 | `tfidf.rs`, `phase/mod.rs`, `index_freshness.rs`, `setup*.rs`, `diff_logic.rs` | Durable cache correctness, freshness correctness, portable runtime setup, robust tooling | Final clippy/tests/quality gates passed |
| 4 | A1/A2/A5/B1/C1/C2/A7 | `index_job.rs`, indexing modules/tests, `index_builder/mod.rs`, `text_search_handler.rs`, phase/graph modules | Lexical/neural consistency, bounded summary enrichment, non-blocking scans, project-wide metrics, deterministic graph output | Final workspace tests/clippy passed |
| 5 | Durable checkpoint hardening | `src/cli/index_job.rs`, `tests/index_job_recovery_test.rs` | Sort/dedup at persistence boundary; resume admission set exactly | Final focused/workspace tests passed |
| 6 | Tracker replacement | `Tracker.md` | Complete audit trail for planned/made edits and validation | This edit |

---

**Final validation evidence (post-hardening):**

- `cargo fmt --all --check` — passed.
- `cargo check --workspace` — passed.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo clippy -p leindex-embed --features onnx -- -D warnings` — passed.
- `cargo clippy -p leindex --features onnx,remote-embeddings -- -D warnings` — passed.
- `cargo test --workspace` — passed; 1,760 passed, 0 failed, 35 ignored.
- Focused search, registry, C++, and exact-flow regressions — passed.
- `lizard src/ crates/leindex-embed/src/ -C 15 -x '*/tests/*'` — passed; zero violations.
- Oversized Rust-file check — passed; zero files over 2,000 lines.
- Workflow YAML parse and `git diff --check` — passed.
- `code-reviewer-luna` — final review completed; reported hardening items were implemented and revalidated.
- Fresh CodeRabbit — attempted repeatedly but unavailable due provider rate limiting/seat policy; no clean CodeRabbit result is claimed.

**Current implementation conclusion:** CR16–CR19 remediation, final hardening, regression coverage, local validation, and final code review are complete. The serialized reindex refresh remains intentionally unchecked because it was not run in this pass; other unchecked TASKLIST items are unrelated queued product work and were not silently marked complete.

## Final Changed-File Hunk-Range Appendix

The following current-source ranges come from the final `git diff --unified=0` audit. They supplement the task ledger above and provide an explicit line-range record for every tracked modified file.

- `.github/workflows/quality.yml`: 16, 33-42
- `TASKLIST.md`: 10, 12, 15-16, 49, 59, 71, 110-111
- `crates/leindex-embed/src/runtime_test.rs`: 8-44, 72-73, 87-88, 93, 98, 100-101, 107-108, 112, 155-156, 158, 160, 162, 164, 166, 168-169, 174-175, 186-187
- `crates/leindex-embed/src/worker_main.rs`: 206, 216-217, 232, 246-256, 344, 484-495
- `src/cli/index_builder/hybrid.rs`: 149, 269-274, 276-281, 366-371, 373-378, 421-423, 427-428, 487
- `src/cli/index_builder/mod.rs`: 225-251, 261, 292-303, 747-758, 924-931, 948, 969, 1000, 1003, 1010, 1021, 1095, 1105-1112, 1176, 1191-1198, 1266, 1331, 1355, 1365-1369, 1373-1380
- `src/cli/index_builder/tests.rs`: 846-850
- `src/cli/index_builder/tfidf.rs`: 5-6, 16-17, 23-24, 44-45, 77, 151, 229-254, 260-261, 266, 272, 279-289, 327
- `src/cli/index_freshness.rs`: 290-308
- `src/cli/index_job.rs`: 195-199, 384-387, 730-788
- `src/cli/leindex/indexing/helpers.rs`: 46-54, 98-99, 303-324, 325, 402-426
- `src/cli/leindex/indexing/load.rs`: 113, 187-188, 202, 208
- `src/cli/leindex/indexing/mod.rs`: 66, 70-81, 124, 598, 617, 889, 1269-1275, 1326, 1380, 1385, 1539-1542, 1544, 1547
- `src/cli/leindex/indexing/tests.rs`: 3-48
- `src/cli/leindex/query.rs`: 135, 137, 994-995
- `src/cli/leindex/setup.rs`: 607-608, 615, 649-650, 713-715, 1104, 1106, 1509-1513, 1672-1679
- `src/cli/leindex/setup_models.rs`: 2-8, 289-308, 310-360, 362, 395-399, 583-595
- `src/cli/leindex/setup_ort.rs`: 4, 471-475, 543-555, 560-562, 570-576
- `src/cli/leindex/setup_test.rs`: 289, 291-293, 664-701, 704-706, 720-722, 730-732, 861-863, 868-871, 876, 878, 882, 889, 891-892, 902, 904, 906, 910, 915, 918, 925, 934, 1027, 1081-1098
- `src/cli/mcp/grep_symbols_handler.rs`: 224-227, 229-232, 236-254, 273-275, 312, 314-320
- `src/cli/mcp/output/render/mod.rs`: 185
- `src/cli/mcp/output/trim_test.rs`: 245
- `src/cli/mcp/phase_handler.rs`: 261-265, 267, 269, 271-272, 276, 278-280, 283, 286-291, 293-311, 313-324, 330
- `src/cli/mcp/prompts_resources.rs`: 141-146, 177, 191, 313, 332, 368-401
- `src/cli/mcp/search_handler.rs`: 55, 59-73, 75-84, 95, 97, 105-106, 111, 115-118, 120-127, 283, 292, 362-365, 372-405
- `src/cli/mcp/server.rs`: 35-48, 1237-1387, 1395, 1397, 1399, 1401, 1403-1416, 1420, 1423, 1430-1431, 1433-1435, 1437-1438, 1588-1589
- `src/cli/mcp/text_search_handler.rs`: 26, 37, 66, 73, 462-478
- `src/cli/registry.rs`: 121-127, 551-564
- `src/global/registry.rs`: 257-263, 577-601
- `src/graph/extraction.rs`: 58-61, 1039-1040
- `src/graph/extraction_cross_file.rs`: 292-301, 342-343, 476-480, 490, 507, 535, 567, 570
- `src/graph/extraction_test.rs`: 293-364
- `src/parse/cpp.rs`: 447-449, 469, 607-644
- `src/parse/csharp.rs`: 563
- `src/parse/javascript.rs`: 489-491, 863-865, 1101-1137
- `src/parse/ruby.rs`: 319-331, 429-447
- `src/phase/mod.rs`: 286-301, 327-409
- `src/phase/phase1.rs`: 1, 64-66, 72, 79-104, 171-200
- `src/search/onnx/client_config.rs`: 19-22, 299-348, 368, 372-376, 382-386, 393-395
- `src/search/search/mod.rs`: 207-212, 816, 1101, 1125-1127, 1132-1135, 1137-1142, 1436-1437, 1440, 1442, 1453-1457, 1495, 1512-1527
- `src/search/search/pruner.rs`: 63, 75, 110
- `src/search/search/vector_impl.rs`: 117-119, 134-148, 261-275
- `src/storage/schema.rs`: 88, 90-103
- `tests/index_job_recovery_test.rs`: 78-82, 125-130, 382, 384-385, 427-432, 445-462, 558, 560-561
- `tools/memcheck/tests/diff_logic.rs`: 345-347, 349, 351-353, 355-361, 366

Untracked line spans: `Tracker.md:1-631` (this ledger), `src/cli/mcp/server_test.rs:1-620` (extracted socket tests), and `docs/plans/` (directory artifact; no single line span).


## CR19 Remediation Batch — Completed and Validated

| Task | Files/ranges | Logic/invariant | Status |
|---|---|---|---|
| CR18 correction: helper-test ordering | `src/cli/leindex/indexing/helpers.rs:391+` | Keep the test module after all production items so strict clippy remains clean. | ⏳ Planned |
| CR18 correction: model rollback diagnostics | `src/cli/leindex/setup_models.rs:300-330` | Report both replacement and restoration failures; never silently discard a failed restore. | ⏳ Planned |
| CR18 correction: invalid model diagnostic | `src/cli/leindex/setup.rs`, `src/cli/leindex/setup_models.rs`, setup tests | Reject unknown model names with an explicit, actionable diagnostic before model-directory resolution. | ⏳ Planned |
| CR18 correction: timeout-policy testability | `src/cli/mcp/server.rs`, `src/cli/mcp/server_test.rs` | Test the timeout selector used by framing code, not only duplicated constants. | ⏳ Planned |
| CR19-01 C++ CFG dispatch | `src/parse/cpp.rs:465-471` | A range-based loop counted by complexity must also use loop CFG handling. | ⏳ Planned |
| CR19-02/03 scoped search | `src/cli/mcp/search_handler.rs:55-106` | Normalize both separators, use component boundaries for dotted directories, and fetch until the requested page is covered or the result source is exhausted. | ⏳ Planned |
| CR19-04 registry conversion | `src/global/registry.rs:247-263` plus regression test | Convert persisted signed counts with checked `i64`→`usize`; reject negative/out-of-range values as row-conversion errors. | ⏳ Planned |
| CR19-05–11 TASKLIST truthfulness | `TASKLIST.md:10-16,46,71-72` and final gate sections | Keep incomplete gates/checklists incomplete until final evidence exists; distinguish current evidence from historical snapshots and written plans from approvals. | ⏳ Planned |
| CR19-12 exact flow targets | `src/graph/extraction_cross_file.rs:486-590` plus regression test | File-scoped cross-file flow resolution must use normalized qualified keys, not a bare-name fallback that creates unrelated edges. | ✅ Implemented; focused validation pending |
| Review correction: exhaustive model error handling | `src/cli/leindex/setup.rs`, `src/cli/leindex/setup_test.rs` | Format the new invalid-name error and assert the new variant at the call site. | ⏳ Planned |
| Review correction: timeout selector regression | `src/cli/mcp/server_test.rs` | Exercise `socket_read_timeout(true/false)` so the production policy is covered, not just constants. | ⏳ Planned |
| Review correction: file-scope semantics | `src/cli/mcp/search_handler.rs` | Normalize separators while retaining exact-file scopes and treating dotted directory names as directories when they exist. | 🔄 In progress; clippy identified a nonminimal boolean and review identified extensionless-file classification risk |
| Review regression coverage | `src/cli/mcp/search_handler.rs`, `src/graph/extraction_cross_file.rs`, `src/parse/cpp.rs` | Pin extensionless/dotted scope handling, exact-vs-bare flow resolution, and C++ range-loop CFG dispatch. | 🔄 In progress; scope/graph regressions added, C++ test traversal correction pending |
| Validation correction: tree-sitter test traversal | `src/parse/cpp.rs:617-630` | Replace unavailable `Node::descendants` usage with the repository-compatible cursor/BFS traversal while preserving the range-loop CFG assertion. | ✅ Implemented; focused C++ tests pass |
| Validation correction: exact-flow regression lookup | `src/graph/extraction_test.rs:300-350` | Use the actual merged node identity/qualified-name invariant rather than assuming a fixed `file:name` ID; preserve the negative assertion against unrelated bare-name targets. | ✅ Implemented; exact-flow regression and workspace tests pass |
| Final review correction: scope path representation | `src/cli/mcp/search_handler.rs`, `src/cli/mcp/helpers.rs` | Match canonical absolute scopes against both canonical absolute and project-relative result paths without widening component boundaries. | ✅ Implemented; focused/full tests and clippy pass |
| Final review correction: unique model staging names | `src/cli/leindex/setup_models.rs:275-335` | Prevent concurrent installs in one process from sharing a PID-only sibling staging/backup path. | ✅ Implemented; final workspace gates pass |
| Final review correction: exact flow-key contract | `src/graph/extraction_cross_file.rs`, `src/graph/extraction_test.rs` | Confirm exact normalized qualified keys are the intentional file-scoped contract; retain positive unqualified exact-key coverage and reject unrelated bare-name fallback. | ✅ Implemented; exact-flow regression and workspace tests pass |
| Final validation and review | repository-wide | Run format, strict clippy, workspace tests, feature gates, quality checks, fresh CodeRabbit, and final code review; fix all actionable findings. | ✅ Local gates and final code review complete; CodeRabbit unavailable/rate-limited |

**Final working-tree accounting:** 45 tracked files modified plus 3 untracked entries (`Tracker.md`, `docs/plans/`, `src/cli/mcp/server_test.rs`) = 48 status entries. This supersedes all earlier 39/42-entry snapshots.
