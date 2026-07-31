# PR #32 — Deferred CodeRabbit Findings: Remediation Plan

**Branch:** `codex/ci-quality-gates-remediation`  
**HEAD:** `4c71a201` (working tree clean)  
**Base for review diff:** `codex/release-1.8.4`  
**CodeRabbit review:** `coderabbit review --agent --base codex/release-1.8.4` (v0.7.1, completed: 14 findings, 119 files reviewed)  
**Date:** 2026-07-31

## Methodology Summary

1. Re-ran `coderabbit review --agent --base codex/release-1.8.4` to completion (14 findings emitted, review_completed event confirmed).
2. Read actual code at every cited location with current line numbers verified against HEAD `4c71a201`.
3. Traced data flow and callers using grep, code intelligence tools, and direct file reads.
4. Stated invariants explicitly for each change.
5. Specified concrete verification methods (tests, smoke commands).

## Reconciliation: Deferred Inventory vs Fresh Review

The fresh CodeRabbit review emitted **14 findings**, none of which overlap with the deferred inventory (A1–A7, B1, C1–C2, D1–D5). The deferred items were from an earlier review pass. All 14 new findings + all 16 deferred items are planned below. The deferred items are still researched and planned because they represent real defects verified against current code, even though CodeRabbit's current review scope no longer flags them.

**Coverage caveat (authoritative-source rule):** The 16-item inventory above was derived from the prior remediation session's deferred set. That session also deferred ~8 additional G7 "subtle pipeline" findings (review items 15, 22, 24, 26, 28, 31, 32, 35) that were judged high-regression-risk and are **not** individually enumerated in A1–D5. Rather than plan blind from a stale summary, the **fresh `coderabbit review` output is the authoritative source**: any prior deferred item that is still a live defect is re-surfaced by it, and any item it no longer flags is treated as **resolved**. Before closing this plan, the executor must confirm the fresh review emits no finding outside the set planned here; if it does, that finding is added to scope and planned with the same rigor.

---

## Category A — Architecture / Correctness Refactors

### A1. Neural batching must reuse the lexical admission decision

**Status:** VALID

**Problem:** `enrich_neural_embeddings` independently walks all PDG nodes and only excludes external nodes via `is_external_node_excluded(node)`. The lexical pass applies three gates: (1) external exclusion, (2) `ContentPruner::evaluate` (generated-code/low-information pruning), (3) `IndexingAdmissionGate::try_admit` (batch node/byte caps). Nodes pruned or shed from the lexical index still receive neural embeddings, causing wasted work and lexical/neural index divergence.

**Invariant at risk:** The neural index and lexical index must cover the same set of admitted nodes. If a node is pruned from lexical search, its neural embedding is orphaned — searchable via neural but absent from lexical, producing inconsistent hybrid results.

**Current location:**  
- `src/cli/index_builder/mod.rs:1290-1298` — neural loop admission gate (only `is_external_node_excluded`)  
- `src/cli/index_builder/mod.rs:1049-1065` — lexical admission (3 gates)  
- `src/cli/index_builder/mod.rs:885-925` — batch loop with ContentPruner + IndexingAdmissionGate  
- `src/cli/leindex/indexing/mod.rs:1511-1520` — call site

**Design:**

1. **Collect the admitted-node set during the lexical batch loop.** After `build_indexed_node` returns, if the outcome is `Indexed`, record the node's stable identifier (`node.id.clone()`) in a `HashSet<String>` declared before the batch loop. This set is the authoritative admitted set.

2. **Thread the set into `enrich_neural_embeddings`.** Change the signature:
   ```rust
   pub(crate) fn enrich_neural_embeddings(
       pdg: &ProgramDependenceGraph,
       embedder: &HybridEmbedder,
       file_cache: &mut FileReadCache,
       admitted_node_ids: &HashSet<String>,
   ) -> Vec<(String, Vec<f32>)>
   ```
   At `indexing/mod.rs:1513`, pass the collected set.

3. **Add a membership check in the neural loop.** After the external exclusion check at line 1296, add:
   ```rust
   if !admitted_node_ids.contains(&node.id) {
       continue;
   }
   ```
   This adds one branch; CCN of the loop body remains ≤ 15 (currently ~8).

4. **Preserve per-batch cap semantics.** The `IndexingAdmissionGate` resets per batch, but `ContentPruner::evaluate` is deterministic (purely file-path/content/name based). The admitted set collected from `Indexed` outcomes already encodes the final admission decision including all batch shedding. No replay of mutable gate state is needed.

**Affected callsites:**
- `src/cli/index_builder/mod.rs:1263` — signature change
- `src/cli/leindex/indexing/mod.rs:1513` — pass admitted set
- The lexical batch loop (`:905-955`) — collect admitted IDs

**CCN impact:** `enrich_neural_embeddings` loop gains one `if` branch (current ~8 → ~9, well within ≤15).

**Alternatives considered:**
- *Replay ContentPruner + AdmissionGate in neural pass:* Rejected because AdmissionGate state is mutable per-batch and not reproducible after the lexical loop completes. ContentPruner is reproducible but AdmissionGate shedding depends on cumulative batch state.
- *Rebuild admission decisions from PDG:* Rejected because it duplicates the lexical logic and risks divergence if either copy changes.

**Risk + verification:**
- **Risk:** A node admitted by lexical but missing from the set (if collection misses an `Indexed` outcome variant) would lose its neural embedding. 
- **Verification:** Add a unit test in `src/cli/index_builder/tests.rs` that builds a small PDG with a generated-code file (pruned by ContentPruner) and a normal file, runs the full indexing path, and asserts: (a) the generated-code node has no neural embedding, (b) the normal node does. Also assert the neural embedding count equals the lexical indexed count.
- **New test:** `test_neural_admission_matches_lexical_admission`

**Out of scope:** Changing the ContentPruner or AdmissionGate logic itself. Changing the neural embedding batch size or protocol.

---

### A2. enriched_node_content O(N×M) FileSummary rescan

**Status:** VALID

**Problem:** For every FileSummary node, `enriched_node_content` scans all `pdg.node_indices()` to collect up to 40 same-file symbol names. With M FileSummary nodes and N total PDG nodes, this is O(N×M).

**Invariant at risk:** Performance — no correctness issue. The output text must remain byte-identical.

**Current location:** `src/cli/index_builder/mod.rs:262-292` (FileSummary branch), consumers at `:1056`, `:1134`, `:1302`.

**Design:**

1. **Precompute a `file_path → Vec<String>` map once.** Create a struct:
   ```rust
   pub(crate) struct FileSummaryContext {
       /// file_path → names of non-FileSummary symbols in PDG iteration order
       file_symbols: HashMap<String, Vec<String>>,
   }
   ```
   Build it with a single pass over `pdg.node_indices()`, collecting every non-FileSummary node's name grouped by `file_path`. Do NOT cap at 40 globally — store all names (or at least 41) so the exclude-self + take-40 logic in `enriched_node_content` produces byte-identical output.

2. **Pass the context to `enriched_node_content`.** Change signature:
   ```rust
   pub(crate) fn enriched_node_content(
       pdg: &ProgramDependenceGraph,
       node_idx: petgraph::graph::NodeIndex,
       node: &crate::graph::pdg::Node,
       file_bytes: &[u8],
       connectivity_config: &crate::graph::pdg::TraversalConfig,
       file_summary_ctx: &FileSummaryContext,
   ) -> String
   ```
   The FileSummary branch replaces the `for ni in pdg.node_indices()` loop with:
   ```rust
   let items: Vec<String> = file_summary_ctx
       .file_symbols
       .get(&*node.file_path)
       .map(|names| names.iter()
           .filter(|n| *n != &node.name)
           .take(40)
           .cloned()
           .collect())
       .unwrap_or_default();
   ```

3. **Build the context once and share across all three consumers.** In the lexical indexing entry point (`build_search_index` or equivalent at `:885+`), build the context before `build_document_frequencies` and pass it to all three consumers. For `enrich_neural_embeddings`, pass it as an additional parameter.

   **Important:** The lexical pass and neural pass run in separate pipeline phases. The context must be rebuilt for the neural pass if the PDG has changed, or threaded from the lexical phase. Since `enrich_neural_embeddings` receives the same PDG, building the context once at the start of each phase is correct and cheap (single O(N) pass).

**Affected callsites:**
- `src/cli/index_builder/mod.rs:228` — signature change
- `:1056` — `build_indexed_node` call
- `:1134` — `build_document_frequencies` call
- `:1302` — `enrich_neural_embeddings` call
- `src/cli/leindex/indexing/mod.rs:1513` — pass context to neural enrichment

**CCN impact:** `enriched_node_content` FileSummary branch simplifies (loop → map lookup), reducing CCN. No function exceeds ≤15.

**Alternatives considered:**
- *Lazy per-file caching:* Rejected because the first FileSummary for each file still scans, and cache management adds complexity.
- *Cap at 40 in the map:* Rejected because exclude-self may remove one of the 40, yielding only 39 — not byte-identical to current behavior.

**Risk + verification:**
- **Risk:** Output text divergence (order, count, self-exclusion).
- **Verification:** Add a golden test `test_file_summary_content_byte_identical` that builds a PDG with multiple FileSummary and non-FileSummary nodes in the same file, calls `enriched_node_content` before and after the change, and asserts `==` on the output string. Also test the 40-cap boundary with 41+ same-file symbols.
- **New test:** `test_file_summary_content_byte_identical`, `test_file_summary_context_excludes_self_and_caps_at_40`

**Out of scope:** Changing the enrichment text format. Changing the connectivity enrichment for non-FileSummary nodes.

---

### A3. TF-IDF persistence: schema version + validation + fingerprint freshness

**Status:** VALID

**Problem:** `TfIdfPersistedState` has no schema version, no dimension validation against `DEFAULT_EMBEDDING_DIMENSION`, no vocab/idf length mismatch check, and `is_fresh` keys only on node/edge counts (a rewrite with equal counts is treated as fresh).

**Invariant at risk:** A stale or structurally malformed TF-IDF embedder must be detected and rejected, not silently reused. The embedder's dimension must match what the search engine expects (768).

**Current location:**
- `src/cli/index_builder/tfidf.rs:12-19` — `TfIdfPersistedState` struct
- `:218-227` — `from_persisted_state` (no validation)
- `:239-245` — `is_fresh` (only node/edge counts)
- `:269-283` — `load_from_artifact_path`
- `src/search/search/mod.rs:29` — `DEFAULT_EMBEDDING_DIMENSION = 768`
- `src/cli/index_builder/mod.rs:1650-1711` — `pdg_search_fingerprint`
- `src/cli/leindex/indexing/load.rs:50-53` — loader (`.ok().flatten()`)
- `:110-117` — freshness check in snapshot hydration

**Design:**

1. **Add schema version and fingerprint to the persisted struct:**
   ```rust
   const TFIDF_SCHEMA_VERSION: u32 = 1;
   
   #[derive(Debug, Clone, Serialize, Deserialize)]
   struct TfIdfPersistedState {
       #[serde(default)]
       schema_version: u32,
       vocab: Vec<String>,
       idf: Vec<f32>,
       dimension: usize,
       pdg_nodes: usize,
       pdg_edges: usize,
       #[serde(default)]
       pdg_fingerprint: String,
   }
   ```

2. **Add validation in `from_persisted_state`:**
   ```rust
   fn from_persisted_state(state: TfIdfPersistedState) -> Option<Self> {
       if state.schema_version != TFIDF_SCHEMA_VERSION {
           warn!("Persisted TF-IDF schema version {} != current {}; discarding", 
                 state.schema_version, TFIDF_SCHEMA_VERSION);
           return None;
       }
       if state.dimension != DEFAULT_EMBEDDING_DIMENSION {
           warn!("Persisted TF-IDF dimension {} != expected {}; discarding",
                 state.dimension, DEFAULT_EMBEDDING_DIMENSION);
           return None;
       }
       if state.vocab.len() != state.idf.len() {
           warn!("Persisted TF-IDF vocab/idf length mismatch ({} != {}); discarding",
                 state.vocab.len(), state.idf.len());
           return None;
       }
       Some(Self { ... })
   }
   ```
   Change `load_from_artifact_path` to return `Ok(None)` when validation fails (matching the existing `.ok().flatten()` contract at the caller).

3. **Strengthen `is_fresh` to check fingerprint:**
   ```rust
   pub fn is_fresh(&self, pdg_node_count: usize, pdg_edge_count: usize, pdg_fingerprint: &str) -> bool {
       self.pdg_nodes == pdg_node_count 
           && self.pdg_edges == pdg_edge_count
           && !self.pdg_fingerprint.is_empty()
           && self.pdg_fingerprint == pdg_fingerprint
   }
   ```

4. **Update all `is_fresh` callsites** to pass the current fingerprint. The snapshot hydration at `load.rs:110-117` already computes `current_pdg_fingerprint`. Also update `save` to persist the fingerprint.

5. **Old artifacts with `schema_version: 0` (serde default)** are rejected by the version check → return `None` → treated as stale → rebuild. No crash.

**Affected callsites:**
- `tfidf.rs:12-19` — struct fields
- `:218-227` — `from_persisted_state` (return type `Option<Self>`)
- `:239-245` — `is_fresh` (new parameter)
- `:269-283` — `load_from_artifact_path` (handle None from validation)
- Save callsite (wherever the embedder is persisted) — add fingerprint
- `load.rs:110-117` — pass fingerprint to `is_fresh`
- `load.rs:186-188` — slow-path reuse must also check validation

**CCN impact:** `from_persisted_state` gains 3 if-checks (CCN ~4, well within ≤15). `is_fresh` gains 2 conditions (CCN ~4).

**Alternatives considered:**
- *Custom serde deserializer with rejection:* Rejected — `#[serde(default)]` + post-load validation is simpler and equally safe.
- *Hard error on mismatch:* Rejected — the existing loader contract converts errors to `None`, and a stale embedder should trigger rebuild, not crash.

**Risk + verification:**
- **Risk:** Old artifacts become stale (expected behavior, not a bug). Fingerprint computation must be deterministic and match `pdg_search_fingerprint`.
- **Verification:** Add test `test_tfidf_rejects_old_schema_version` (deserialize a struct with `schema_version: 0`, assert `None`). Add `test_tfidf_rejects_dimension_mismatch` (dimension 512, assert `None`). Add `test_tfidf_rejects_vocab_idf_mismatch` (vocab 10, idf 9, assert `None`). Add `test_tfidf_is_free_with_fingerprint_mismatch` (equal counts, different fingerprint, assert `false`).

**Out of scope:** Changing the embedding algorithm. Changing the fingerprint domain tag.

---

### A4. scan_git_project_files path normalization and duplicated logic

**Status:** PARTIAL

**Problem:** `scan_git_project_files` duplicates size-limit checks, manifest hashing, and canonicalization logic instead of reusing `check_file_size_limits` and `finalize_project_scan`. The path normalization concern is **invalid** — `git::source_inventory` already returns absolute paths via `inventory_paths(&root, ...)`, so no repo-relative paths enter.

**Invariant at risk:** Code maintainability — duplicated filtering/finalization logic can drift from the shared helpers.

**Current location:**
- `src/cli/index_builder/mod.rs:431-502` — `scan_git_project_files`
- `:657-672` — `check_file_size_limits`
- `:677-714` — `finalize_project_scan`

**Design:**

1. **Refactor `scan_git_project_files` to reuse `finalize_project_scan`.** After collecting `source_paths` and `manifest_paths` through the Git-specific extension/exclusion filtering (which is unique to Git), delegate the finalization:
   ```rust
   fn scan_git_project_files(project_path: &Path) -> Result<ProjectFileScan> {
       let project_config = crate::cli::config::ProjectConfig::load(project_path).unwrap_or_default();
       let limits = &project_config.indexing;
       let inventory = crate::cli::git::source_inventory(project_path)?;
       let mut source_paths = Vec::new();
       let mut manifest_paths = Vec::new();
       for path in inventory {
           // ... existing extension/exclusion/manifest classification ...
           // ... size/count/total limits inline (Git-specific break semantics) ...
       }
       Ok(finalize_project_scan(source_paths, manifest_paths, project_path))
   }
   ```

2. **Extract the inline size/count/total filtering** into a shared helper if the `break` semantics (early termination on limit) can be unified with `check_file_size_limits`. If the break-on-limit behavior differs (Git needs to stop iterating; the non-Git walker has different traversal), keep the inline loop but extract only `finalize_project_scan` reuse.

3. **No path normalization needed.** `source_inventory` returns absolute paths. Document this invariant with a comment.

**Affected callsites:**
- `src/cli/index_builder/mod.rs:477-501` — replace with `finalize_project_scan` call
- Verify `finalize_project_scan` signature accepts the same types

**CCN impact:** `scan_git_project_files` simplifies (removes ~20 lines of finalization), reducing CCN.

**Alternatives considered:**
- *Full unification with non-Git scanner:* Rejected — the Git-specific inventory iteration and manifest classification differ enough that forcing them into one function would increase complexity.

**Risk + verification:**
- **Risk:** `finalize_project_scan` might expect slightly different input (e.g., unsorted paths). Verify it sorts internally.
- **Verification:** Add test `test_scan_git_project_files_reuses_finalize` with a small Git repo, assert `ProjectFileScan` fields match. Compare manifest_hashes and canonical paths before/after.

**Out of scope:** Changing `source_inventory` or the Git inventory contract. Changing `check_file_size_limits` semantics.

---

### A5. text_search synchronous scan off the Tokio worker

**Status:** VALID

**Problem:** `scan_file` and `scan_source_paths` run synchronous file I/O and regex matching on the async Tokio worker thread, blocking the runtime under load. `live_source_inventory` correctly uses `spawn_blocking`.

**Invariant at risk:** Runtime responsiveness — other concurrent MCP requests stall while a text search scans files.

**Current location:**
- `src/cli/mcp/text_search_handler.rs:283-329` — `scan_file` (sync)
- `:331-362` — `scan_source_paths` (sync)
- `:453-473` — handler `execute` calls `scan_source_paths` directly
- `:100-133` — `live_source_inventory` (correctly uses `spawn_blocking`)

**Design:**

1. **Wrap `scan_source_paths` in `spawn_blocking`.** In `execute`, after resolving context:
   ```rust
   let source_paths = context.source_paths.clone();
   let scope = context.scope.clone();
   let params_clone = params.clone();
   let pdg_spans = context.pdg_spans.clone();
   let (results, partial) = tokio::task::spawn_blocking(move || {
       scan_source_paths(&source_paths, scope.as_deref(), &params_clone, &pdg_spans, started)
   })
   .await
   .map_err(|e| JsonRpcError::internal(format!("scan task failed: {e}")))?;
   ```

2. **Ensure all captured values are `Send + 'static`.** Verify:
   - `Vec<PathBuf>` — Send ✓
   - `Option<String>` — Send ✓
   - `TextSearchParams` — must be `Clone + Send`. Check if it contains `Regex` (which is Send+Sync but not Clone by default — may need `Arc<Regex>` or recompile).
   - `PdgSpans` — must be `Clone + Send`. Verify its fields.
   - `Instant` — Send ✓

3. **If `TextSearchParams` contains non-Clone fields**, either:
   - Make `Regex` an `Arc<Regex>` so it's `Clone + Send`.
   - Or move the regex compilation into the blocking closure.

4. **Preserve pagination behavior.** The `results` vector is still paginated with `skip(offset).take(max_results)` after the blocking task returns. The `partial` flag and `has_more` semantics are unchanged.

**Affected callsites:**
- `text_search_handler.rs:453-473` — wrap in `spawn_blocking`
- Possibly `TextSearchParams` definition — make `Clone + Send`

**CCN impact:** `execute` gains one `.await` + error mapping (~1 branch, well within ≤15).

**Alternatives considered:**
- *Spawn per-file blocking tasks:* Rejected — too many task spawns for large directories; one blocking task for the whole scan is simpler and sufficient.

**Risk + verification:**
- **Risk:** If `TextSearchParams` or `PdgSpans` is not `Clone + Send`, compilation fails. Must verify and fix types first.
- **Verification:** Add a Tokio test `test_text_search_does_not_block_runtime` that spawns a text search on a large file and concurrently runs a simple async timer task; assert the timer completes while the search is running. Add `test_text_search_pagination_after_blocking` to verify pagination still works.

**Out of scope:** Changing the scan algorithm itself. Changing `live_source_inventory`.

---

### A6. run_phase5 does not persist computed phase1–4 summaries

**Status:** VALID

**Problem:** When `run_phase5` computes fallback phase1–4 summaries via `unwrap_or_else(|| phaseN::run(...))`, it does not save them to `PhaseCache`. A later request recomputes them. Current CCN of `run_phase5` is 8.

**Invariant at risk:** Cache efficiency — computed summaries should be available for future requests. Correctness is not at risk (summaries are recomputed correctly), but performance degrades.

**Current location:** `src/phase/mod.rs:265-314` — `run_phase5`, fallback at `:286-297`.

**Design:**

1. **Extract a helper to compute and persist a fallback phase summary:**
   ```rust
   fn compute_and_cache_phase<P, S>(
       cache: &PhaseCache,
       context: &PhaseExecutionContext,
       options: &PhaseOptions,
       phase: u8,
       supplied: Option<&S>,
       compute: P,
   ) -> Result<S>
   where
       P: FnOnce(&PhaseExecutionContext, &PhaseOptions) -> S,
       S: serde::Serialize + serde::de::DeserializeOwned + Clone,
   {
       if let Some(s) = supplied {
           return Ok(s.clone());
       }
       let key = options_hash_for_phase(phase, options);
       // Check cache first
       if let Some(cached) = cache.load_with_options::<S>(
           &context.project_id, &context.generation_hash, phase, key.as_deref(),
       )? {
           return Ok(cached.payload);
       }
       let summary = compute(context, options);
       cache.save_with_options(
           &context.project_id, &context.generation_hash, phase, key.as_deref(), &summary,
       )?;
       Ok(summary)
   }
   ```

   **Note:** Phase 1 and 2 don't use `options_hash_for_phase` (they use `cache.load`/`cache.save` without options). The helper must handle both paths — either with two variants or by checking `phase <= 2`.

2. **Replace the fallback block in `run_phase5`:**
   ```rust
   let phase1_summary = compute_and_cache_phase(cache, context, options, 1, phase1_summary, |ctx, _| phase1::run(ctx))?;
   let phase2_summary = compute_and_cache_phase(cache, context, options, 2, phase2_summary, |ctx, _| phase2::run(ctx))?;
   let phase3_summary = compute_and_cache_phase(cache, context, options, 3, phase3_summary, |ctx, opts| phase3::run(ctx, opts))?;
   let phase4_summary = compute_and_cache_phase(cache, context, options, 4, phase4_summary, |ctx, opts| phase4::run(ctx, opts))?;
   ```

3. **Never re-save caller-owned supplied summaries.** The helper returns the supplied value directly without saving.

4. **Keep `run_phase5` CCN ≤ 15.** With the helper extraction, `run_phase5` itself simplifies to ~CCN 5-6.

**Affected callsites:**
- `src/phase/mod.rs:286-297` — replace with helper calls
- New helper function in `mod.rs`

**CCN impact:** `run_phase5` CCN decreases (8 → ~5). Helper is ~CCN 6.

**Alternatives considered:**
- *Call `run_phase1` through `run_phase4` instead of `phaseN::run`:* Rejected because `run_phaseN` functions have different signatures (some take `options`, some don't) and would add option-passthrough complexity. The helper approach is cleaner.
- *Always save without checking cache:* Rejected — would overwrite existing cache entries unnecessarily.

**Risk + verification:**
- **Risk:** The generic helper must handle the phase 1/2 (no options key) vs phase 3/4 (options key) difference correctly.
- **Verification:** Add test `test_run_phase5_caches_fallback_summaries`: create a PhaseCache, call `run_phase5` with no supplied summaries and empty cache. Then call again with no supplied summaries. Assert the second call hits cache (no recomputation — verify via a mock/computed flag or timing). Add `test_run_phase5_does_not_resave_supplied_summaries`: pass supplied summaries, assert cache does not contain them (or contains the same value without re-saving).

**Out of scope:** Changing phase computation logic. Changing the cache key scheme.

---

### A7. collect_source_files_with_hashes error handling (product decision)

**Status:** VALID — but recommendation is to **retain fail-fast** (triage the CodeRabbit recommendation as INVALID for this indexer).

**Problem:** A single unreadable source file aborts the entire collection via `?` propagation. CodeRabbit suggests skip-and-warn.

**Invariant at risk:** **Index completeness and change detection correctness.** Hashes drive freshness/change detection. If an unreadable file is silently skipped, it's omitted from the hash map. On the next run, the file's absence from `indexed_files` means:
- If the file becomes readable again, it's treated as "new" (not "changed") — it will be indexed, but the prior generation's index silently lacked it.
- If the file remains unreadable, it's never detected as needing indexing — the index is permanently incomplete.
- The `parse_plan` logic compares current hashes to `indexed_files`; a missing entry forces a parse. But if the file was skipped from `source_files_with_hashes`, it may also be skipped from the scan itself depending on where the error occurs.

**Current location:** `src/cli/index_builder/mod.rs:721-739`. Callers: `indexing/mod.rs:307`, `:800`, `mod.rs:583`.

**Design (recommendation: retain fail-fast):**

The correct behavior for an indexer is **fail-fast**. A successful generation must have a complete hash for every admitted inventory source file. If a file cannot be read, the generation fails explicitly, and the next run retries. This makes incomplete generations visible rather than silently producing partial indexes.

**Evidence:**
1. The hash map is the authoritative change-detection mechanism. Missing entries are indistinguishable from "file was deleted" — but the file still exists on disk, creating a false signal.
2. `scan_checkpoint` at `indexing/helpers.rs:199-204` converts every pair into a `FileFingerprint`. The aggregate input hash controls checkpoint/resume reuse. A missing file silently changes the checkpoint, potentially skipping resume work.
3. `parse_plan` at `indexing/helpers.rs:229-246` treats a missing prior entry as "needs parsing" — but only if the file appears in the current scan. If the file was skipped from the scan entirely, it's never scheduled.

**If skip-and-warn is mandated instead**, the plan must:
1. Log the skipped path at `warn!`.
2. Persist an "unhashed" sentinel in the hash map so `is_fresh` always returns false until the file is successfully hashed.
3. Ensure the file appears in `source_files_with_hashes` with a sentinel hash that never matches any future hash, forcing a re-parse every run until readable.

**Recommendation:** Reject CodeRabbit's skip-and-warn suggestion. Document the fail-fast rationale in a comment.

**Affected callsites:** None (no change).

**Verification:** Existing behavior is correct. Add a comment documenting the rationale. No new test needed — the existing fail-fast behavior is the correct contract.

**Out of scope:** Adding retry logic for transient I/O errors. Changing the hash algorithm.

---

## Category B — Incremental Pipeline

### B1. refresh_persisted_graph parse_results scope

**Status:** PARTIAL

**Problem:** `refresh_persisted_graph` replaces `self.parse_results` and `self.signatures_by_file` with changed-file-only results. Phases 2–5 read from the full PDG (correct), but **phase 1** uses `parse_results` for `parsed_files`, `parse_failures`, `signatures`, and parser-completeness metrics — these reflect only the changed subset, not the full project.

**Invariant at risk:** Phase 1 summary accuracy in incremental mode. `parsed_files` and `signatures` underreport the project total. `parser_completeness` is language-complete but not project-complete within each changed language (via `merge_completeness_with_pdg`).

**Current location:**
- `src/phase/context.rs:177-181` — replacement
- `src/phase/phase1.rs:44-73` — uses `parse_results` for metrics
- `src/phase/phase1.rs:49-59` — `merge_completeness_with_pdg`

**Research findings:**
- Phase 2: reads only PDG edges (`phase2.rs:19-61`) — **correct, full project**.
- Phase 3: reads only PDG nodes/traversal (`phase3.rs:18-61`) — **correct, full project**.
- Phase 4: reads only PDG nodes/traversal (`phase4.rs:28-93`) — **correct, full project**.
- Phase 5: reads summaries + storage/global symbols (`phase5.rs:20-96`) — **correct**.
- Phase 1: **mixed** — language distribution from PDG (full), but `parsed_files`/`parse_failures`/`signatures` from `parse_results` (changed-only).

**Design:**

1. **Rename fields to clarify scope.** Change `PhaseExecutionContext`:
   ```rust
   /// Parse results for the current (changed) batch only. Empty on cache hit.
   pub changed_parse_results: Vec<ParsingResult>,
   /// Signatures for the current (changed) batch only.
   pub changed_signatures_by_file: HashMap<String, (String, Vec<Signature>)>,
   ```
   This makes the changed-only scope explicit in the type system. All references update accordingly.

2. **Update phase 1 to derive project-wide metrics from the PDG.** For `parsed_files` and `signatures`, count unique files and total signatures from PDG nodes:
   ```rust
   let total_files = pdg.node_indices()
       .filter_map(|n| pdg.get_node(n))
       .map(|n| n.file_path.clone())
       .collect::<HashSet<_>>()
       .len();
   let total_signatures = pdg.node_indices()
       .filter_map(|n| pdg.get_node(n))
       .filter(|n| !matches!(n.node_type, NodeType::FileSummary))
       .count();
   ```
   Keep `parse_failures` from the changed batch (failures are inherently per-run).

3. **Keep `merge_completeness_with_pdg`** as-is for language completeness, but ensure the "changed" score doesn't override the PDG-derived count for languages present in both.

4. **Update all references** to `parse_results` and `signatures_by_file` in `context.rs` to use the renamed fields.

**Affected callsites:**
- `src/phase/context.rs:36-38` — field declarations
- `:75-76` — initialization
- `:108-110` — cold path (unchanged, still populates all files)
- `:179-181` — incremental path (renamed)
- `src/phase/phase1.rs:44-73` — use renamed fields + PDG-derived counts

**CCN impact:** Phase 1 `run` gains a few lines for PDG counting but stays ≤15 (currently ~10).

**Alternatives considered:**
- *Merge old parse results:* Rejected — old parse results are not persisted/loaded; they're only available in-memory during the same session. Merging would require loading them from somewhere that doesn't exist.
- *Leave as-is and document:* Rejected — the metrics are factually wrong in incremental mode (underreporting project totals).
- *Fix phase1 metric derivation WITHOUT the rename (RECOMMENDED lower-risk path):* The actual defect is step 2 — phase1 reads changed-only `parse_results` for metrics. Step 1 (renaming `parse_results → changed_parse_results` across `context.rs`, the cold path `:108-110`, incremental path `:179-181`, and every phase reader) is **cosmetic/defensive only** and does not change correctness. The rename touches ~5 sites and risks breaking external consumers for no behavior change. **Drop step 1; keep step 2 + step 3 alone.** If the executor still wants the rename for clarity, do it as a **separate follow-up commit** after the step-2 fix is verified green, so a rename regression can be isolated from the correctness fix. The verification test (`test_incremental_summaries_cover_full_project`) is identical either way.

**Risk + verification:**
- **Risk:** Renaming public fields may break external consumers. Verify with grep that no code outside the phase module reads these fields directly.
- **Verification:** Add end-to-end test `test_incremental_summaries_cover_full_project`: create a project with files A and B, index fully, modify file A, run incremental refresh, assert phase 1 summary reports total_files=2, total_signatures covers both files. Assert deleted symbols are absent from the PDG.

**Out of scope:** Changing phases 2–5 (they already use the PDG). Changing the incremental refresh mechanism itself.

---

## Category C — Graph Edge Correctness

### C1. Cross-file caller→callee binding

**Status:** VALID (with nuance)

**Problem:** Cross-file call resolution fans out to every matching candidate rather than selecting a unique callee. A call to `process()` binds to every cross-file `process` node. Unresolved/common names are dropped via `COMMON_CALL_NAMES`.

**Invariant at risk:** Edge precision — over-connecting call edges inflates the PDG's call graph, causing data-flow and impact analysis to report false positives (too many reachable nodes). Under-connecting (dropped bindings) causes false negatives.

**Current location:**
- `src/graph/extraction_cross_file.rs:33-42` — `resolve_cross_file_call_edges_for_files`
- `:44-50` — `CrossFileCallIndexes` (all maps use `Vec<NodeId>`)
- `:387-423` — caller binding and target resolution
- `:292-333` — `resolve_cross_file_call_targets` (extends with all matches)
- `src/graph/extraction.rs:970` — `ordered_resolution_candidates`

**Design:**

This finding requires a **product decision**: is fan-out to all matching candidates the intended behavior, or should resolution prefer a single best match?

**Current behavior analysis:**
- The `qname_file_to_node` map correctly binds callers to their source file, preventing cross-file caller confusion.
- The target resolution uses ordered candidates (exact → suffix → last-segment) and extends the target set with all matches.
- `COMMON_CALL_NAMES` suppresses last-segment fallback for very common names, reducing noise.
- `existing_edges` prevents duplicate caller-target edges.

**Recommendation:** The fan-out is **intentionally conservative** — connecting to all possible targets ensures no real call edge is missed. The cost is false-positive connectivity, which is preferable to false-negative missed edges for a code-intelligence tool.

**Plan:** 
1. **Document the fan-out behavior** with a comment in `resolve_cross_file_call_targets` explaining that connecting to all matches is deliberate (conservative over-connection).
2. **Add deduplication** of targets within a single resolution (currently targets can contain the same NodeId multiple times through different candidate paths). The `existing_edges` check prevents duplicate edges, but the intermediate vector may be larger than needed.
3. **Do NOT change the fan-out semantics** unless a specific failing case is constructed.

**Required test additions:**
- `test_cross_file_call_fans_out_to_all_matches`: two files each defining `process()`, a call from file A, assert edges to both targets.
- `test_cross_file_call_drops_common_names`: a call to `do()` (in `COMMON_CALL_NAMES`), assert no edge created via last-segment fallback.
- `test_cross_file_qualified_call_binds_correctly`: `Foo.bar()` call, assert edge only to `Foo.bar` target, not to bare `bar`.

**Affected callsites:** `extraction_cross_file.rs:292-333` — add dedup, add comment.

**CCN impact:** Minimal (adding `targets.sort(); targets.dedup();` is 2 statements).

**Alternatives considered:**
- *Select unique best match:* Rejected without a ranking heuristic — there's no reliable way to pick one target over another without import resolution, which the PDG doesn't currently model.
- *Require exact qualified name only:* Rejected — would miss many real cross-file calls in languages with loose naming.

**Risk + verification:**
- **Risk:** Adding dedup changes edge count in tests that expect duplicates. Run existing graph tests.
- **Verification:** Run `cargo test -p leindex --lib graph::extraction` and all cross-file tests. Add the three tests above.

**Out of scope:** Implementing import resolution. Changing `COMMON_CALL_NAMES`. Changing `ordered_resolution_candidates`.

---

### C2. Duplicate-name node mapping

**Status:** PARTIAL

**Problem:** The `LocalNodeIds` map (`HashMap<String, Vec<NodeId>>`) correctly retains all duplicates. However, a separate single-value `node_ids: HashMap<String, NodeId>` at line 33-34 overwrites duplicates, retaining only the last-inserted ID. Operations using `node_ids` (inheritance, source-level flow, import/class inference) observe only one duplicate.

**Invariant at risk:** Edge correctness for duplicate qualified names — some edge types may miss valid targets.

**Current location:**
- `src/graph/extraction.rs:19` — `LocalNodeIds` type
- `:33-34` — `node_ids` (single-value) and `local_node_ids` (multi-value)
- `:44-59` — node creation (both maps populated, `node_ids` overwrites)
- `:1007` — `local_call_targets` (uses multi-value maps, no sort/dedup)

**Design:**

1. **Audit all uses of `node_ids` (single-value map).** Grep for `node_ids` in `extraction.rs`. For each use, determine whether it should use `local_node_ids` (multi-value) instead.

2. **For inheritance and source-level flow:** If these operations semantically need all duplicates, migrate them to `local_node_ids`. If they intentionally pick one (e.g., "the" class definition), document why and keep `node_ids` but make the selection deterministic (e.g., first-inserted, not last).

3. **Add deduplication to `local_call_targets`** return value: `targets.sort(); targets.dedup();` before returning.

4. **Make `node_ids` selection explicit:** If kept, change `insert` to `entry().or_insert()` to retain the **first** insertion (more intuitive than last):
   ```rust
   node_ids.entry(sig.qualified_name.clone()).or_insert(nid);
   ```

**Affected callsites:**
- `src/graph/extraction.rs:55` — change `insert` to `or_insert`
- All uses of `node_ids` — audit and potentially migrate to `local_node_ids`
- `local_call_targets` at `:1007` — add dedup

**CCN impact:** Negligible.

**Alternatives considered:**
- *Remove `node_ids` entirely:* Rejected without confirming all uses can migrate — some may rely on single-value semantics.
- *Keep as-is:* Rejected — the overwrite behavior is a latent bug for any operation that needs all duplicates.

**Risk + verification:**
- **Risk:** Changing `insert` to `or_insert` changes which node is selected by single-value consumers — may break tests expecting the last-inserted.
- **Verification:** Run all graph extraction tests. Add `test_duplicate_qname_inheritance_assigns_to_all` or `test_duplicate_qname_inheritance_assigns_to_first` depending on the semantic decision. Add `test_local_call_targets_deduplicated`.

**Out of scope:** Changing the duplicate ID disambiguation scheme (byte-range suffixes). Changing cross-file resolution (covered by C1).

---

## Category D — Test-Only / Low-Value

### D1. setup_test env-mutating tests need RAII cleanup

**Status:** VALID

**Problem:** 10 tests in `setup_test.rs` mutate environment variables (`LEINDEX_HOME`, `PIP_BIN`, `ROCM_PATH`, `CUDA_PATH`) under a mutex with `lock().unwrap()`. If an assertion panics, the env var is left mutated. If the mutex is poisoned, all subsequent tests panic.

**Current location:** `src/cli/leindex/setup_test.rs` — 10 tests (see list below).

**Affected tests:**
1. `test_model_checksum_status_missing_for_clean_dir` (LEINDEX_HOME)
2. `test_find_pip_honors_pip_bin_with_split` (PIP_BIN)
3. `test_find_pip_honors_pip_bin_single_token` (PIP_BIN)
4. `test_find_pip_empty_pip_bin_falls_through` (PIP_BIN)
5. `test_detect_amd_gpu_no_false_positive_on_clean_system` (ROCM_PATH)
6. `test_detect_amd_gpu_honors_existing_rocm_path` (ROCM_PATH)
7. `test_detect_nvidia_gpu_with_cuda_path_env` (CUDA_PATH)
8. `test_ensure_home_writable_succeeds_for_writable_leindex_home` (LEINDEX_HOME)
9. `test_ensure_home_writable_uses_leindex_home_location` (LEINDEX_HOME)
10. `test_ensure_home_writable_fails_for_read_only_dir` (LEINDEX_HOME)

**Design:**

1. **Create an `EnvVarGuard` struct** (test-only, in `setup_test.rs` or a shared test helper):
   ```rust
   struct EnvVarGuard {
       key: String,
       original: Option<OsString>,
   }
   
   impl EnvVarGuard {
       fn set(key: &str, value: &str) -> Self {
           let original = std::env::var_os(key);
           unsafe { std::env::set_var(key, value); }
           Self { key: key.to_string(), original }
       }
       fn remove(key: &str) -> Self {
           let original = std::env::var_os(key);
           unsafe { std::env::remove_var(key); }
           Self { key: key.to_string(), original }
       }
   }
   
   impl Drop for EnvVarGuard {
       fn drop(&mut self) {
           match &self.original {
               Some(val) => unsafe { std::env::set_var(&self.key, val); },
               None => unsafe { std::env::remove_var(&self.key); },
           }
       }
   }
   ```
   **Note:** In Rust 2024 edition, `set_var`/`remove_var` are `unsafe`. Use `unsafe` blocks as shown.

2. **Replace manual `set_var`/`remove_var` with guards.** Each test becomes:
   ```rust
   let _guard = EnvVarGuard::set("LEINDEX_HOME", &temp_dir.path().display().to_string());
   let _lock = PIPE_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
   // ... test body ...
   // _guard restores in Drop
   ```

3. **Fix mutex poisoning:** Change `PIPE_ENV_LOCK.lock().unwrap()` to `PIPE_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())` in all 10 tests.

4. **Declare guard before lock** so guard drops (restoring env) before lock releases — though order doesn't strictly matter since Drop is per-variable.

**Affected callsites:** All 10 tests listed above.

**CCN impact:** N/A (test code).

**Alternatives considered:**
- *Use `std::sync::Mutex` poisoning recovery only:* Rejected — doesn't solve the env var leak on panic.

**Risk + verification:**
- **Risk:** `unsafe` blocks for env mutation in Rust 2024. Verify the crate's edition.
- **Verification:** Add `test_env_var_guard_restores_on_normal_exit` and `test_env_var_guard_restores_on_panic` (using `catch_unwind`). Run all 10 affected tests twice to verify no env leakage between runs.

**Out of scope:** Refactoring the test logic itself. Changing non-env-mutating tests.

---

### D2. Admission-count accumulation across batches

**Status:** VALID

**Problem:** `admission_gate.reset()` is called at the start of each batch (`:907`), and `nodes_admitted()` is logged after the loop (`:963`). The logged `admitted` value reflects only the last batch, while `pruned`/`shed`/`hoisted`/`external_skipped` accumulate across all batches.

**Current location:**
- `src/cli/index_builder/mod.rs:907` — `admission_gate.reset()`
- `:956-965` — log with `admission_gate.nodes_admitted()`

**Design:**

1. **Accumulate `nodes_admitted()` before each reset:**
   ```rust
   let mut total_admitted: usize = 0;
   for batch in node_indices.chunks(batch_size) {
       nodes.clear();
       // ... batch processing ...
       total_admitted += admission_gate.nodes_admitted();
       admission_gate.reset();
       // ... append_nodes etc ...
   }
   // After loop: also add final batch's count (if reset is at loop start)
   ```

   **Important:** The `reset()` is at line 907 (loop start), so the pattern is:
   - Before reset: accumulate `total_admitted += admission_gate.nodes_admitted()`
   - After loop: the last batch's count is already accumulated before the last reset.

   Actually, since `reset()` is at the **start** of each iteration, the flow is:
   - Iteration 1: reset → process batch 1
   - Iteration 2: reset → process batch 2
   - ...
   - After loop: `nodes_admitted()` = last batch's count only.

   Fix: accumulate **after processing, before the next reset**:
   ```rust
   let mut total_admitted: usize = 0;
   for batch in node_indices.chunks(batch_size) {
       admission_gate.reset();
       // ... process batch ...
       total_admitted += admission_gate.nodes_admitted();
   }
   ```

2. **Use `total_admitted` in the log:**
   ```rust
   admitted = total_admitted,
   ```

3. **Fix the comment** at line 956 from "per-batch stats" to "run-total stats".

**Affected callsites:** `:907`, `:956-965`.

**CCN impact:** Negligible (one addition).

**Alternatives considered:** None — the fix is straightforward.

**Risk + verification:** Low risk (info-level logging only). Verify by running indexing with a multi-batch dataset and checking the log shows a cumulative count.

**Out of scope:** Changing admission gate logic. Changing batch size.

---

### D3. memcheck xtask entrypoint test robustness

**Status:** VALID

**Problem:** `test_val_measure_012_xtask_memcheck_entrypoint` hardcodes `target/debug/xtask`, ignores `CARGO_TARGET_DIR`, may use a stale binary, doesn't assert process success, and only checks `--help` output.

**Current location:** `tools/memcheck/tests/diff_logic.rs:343-370`, `workspace_root()` at `:14-23`.

**Design:**

1. **Replace hardcoded binary with `cargo run`:**
   ```rust
   #[test]
   fn test_val_measure_012_xtask_memcheck_entrypoint() {
       let root = workspace_root();
       let output = std::process::Command::new("cargo")
           .args(["run", "--quiet", "-p", "xtask", "--", "memcheck", "--help"])
           .current_dir(&root)
           .output()
           .expect("failed to run cargo xtask memcheck --help");
       
       assert!(output.status.success(), 
               "xtask memcheck --help failed: {}", 
               String::from_utf8_lossy(&output.stderr));
       
       let stdout = String::from_utf8_lossy(&output.stdout);
       assert!(
           stdout.contains("memcheck"),
           "xtask memcheck --help should mention memcheck"
       );
   }
   ```

2. **This respects `CARGO_TARGET_DIR`, platform executable suffix, and always uses a fresh build.** `cargo run` handles compilation, target directory, and executable naming automatically.

3. **Use `memcheck --help` instead of `--help`** to verify the subcommand actually dispatches, not just that the top-level help mentions it.

4. **Assert `status.success()`** to catch silent failures.

5. **Keep `workspace_root()`** as-is (the `CARGO_MANIFEST_DIR.parent().parent()` derivation is correct for the current layout). Add a validation check:
   ```rust
   assert!(root.join("Cargo.toml").exists(), "workspace root not found");
   ```

**Affected callsites:** `tools/memcheck/tests/diff_logic.rs:343-370`.

**CCN impact:** N/A (test code).

**Alternatives considered:**
- *Use `CARGO_BIN_EXE_xtask`:* Rejected — not available for cross-package test binaries in Cargo.
- *Keep hardcoded path but add CARGO_TARGET_DIR support:* Rejected — `cargo run` is simpler and handles all edge cases.

**Risk + verification:**
- **Risk:** `cargo run` is slower than running a pre-built binary. Acceptable for a test.
- **Verification:** Run `cargo test -p memcheck --test diff_logic test_val_measure_012` and verify it passes.

**Out of scope:** Changing the xtask binary itself. Changing memcheck logic.

---

### D4. runtime_test.rs improvements

**Status:** VALID

**Problem:** 6 of 27 tests in `runtime_test.rs` mutate environment variables (`ONNX_INFERENCE_BATCH_SIZE_ENV`, `ONNX_SEQUENCE_LEN_ENV`) under `ENV_LOCK.lock().unwrap()`. Cleanup is not panic-safe and doesn't restore prior values.

**Current location:** `crates/leindex-embed/src/runtime_test.rs` — 6 env-mutating tests.

**Affected tests:**
1. `onnx_inference_batch_size_defaults_to_fixed_batch_safe_value` (removes ONNX_INFERENCE_BATCH_SIZE_ENV)
2. `onnx_inference_batch_size_uses_positive_env_override` (sets/removes)
3. `onnx_inference_batch_size_rejects_zero_and_bad_values` (sets/removes multiple times)
4. `onnx_sequence_len_defaults_and_clamps_env_override` (removes, sets, removes)
5. `dynamic_qwen_uses_batched_inference_by_default` (removes)
6. `migraphx_uses_one_stable_batch_shape_by_default` (removes)

**Design:**

1. **Apply the same `EnvVarGuard` pattern as D1.** Create the guard in `runtime_test.rs` (or share via a test helper module).

2. **For tests that mutate a variable multiple times** (e.g., test 3 sets to "0", then "nope", then removes), use a single guard that captures the **original** value and restores it on drop. Intermediate mutations within the test are fine — the guard only restores the original at scope exit.

3. **Fix lock poisoning:** Change `ENV_LOCK.lock().unwrap()` to `ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())`.

4. **Preserve `no_compile_config()`** — do not alter the test configuration that prevents expensive MIGraphX JIT/OOM.

5. **Do not allow tests to discover/use real model assets.** Preserve the existing isolation.

**Affected callsites:** All 6 tests listed above.

**CCN impact:** N/A (test code).

**Risk + verification:**
- Add `test_env_var_guard_restores_original_value` in `runtime_test.rs`.
- Run all 27 runtime tests twice to verify no env leakage.

**Out of scope:** Changing runtime configuration logic. Changing test assertions.

---

### D5. setup_ort version-candidate sorting

**Status:** VALID

**Problem:** `scan_dir_for_ort_lib` sorts ORT library candidates lexicographically as `PathBuf`s and picks the last (`pop()`). This mishandles version numbers: `.so.9` sorts after `.so.10` (because `'9' > '1'`), incorrectly selecting version 9 as "newest."

**Current location:** `src/cli/leindex/setup_ort.rs:538-556`.

**Design:**

1. **Implement numeric version comparison.** Parse version components from the filename and compare numerically:
   ```rust
   fn ort_lib_version_key(path: &Path) -> Vec<u64> {
       let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
       // Extract version suffix: e.g., "libonnxruntime.so.1.20.0" → [1, 20, 0]
       let parts: Vec<&str> = name.rsplitn(4, '.').collect();
       parts.iter()
           .rev()
           .filter_map(|s| s.parse::<u64>().ok())
           .collect()
   }
   
   fn scan_dir_for_ort_lib(dir: &Path) -> Option<PathBuf> {
       let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
           .ok()?
           .filter_map(Result::ok)
           .map(|e| e.path())
           .filter(|p| p.file_name()
               .and_then(|n| n.to_str())
               .map(is_ort_runtime_lib_name_for_setup)
               .unwrap_or(false))
           .collect();
       // Sort by numeric version key, then by path for deterministic tie-breaking
       matches.sort_by(|a, b| {
           ort_lib_version_key(a).cmp(&ort_lib_version_key(b))
               .then_with(|| a.cmp(b))
       });
       matches.pop()
   }
   ```

2. **Preserve exact-name preference.** `find_ort_lib_in_dir` at `:529-536` already checks exact names first via `ort_lib_names()`. The numeric sort only applies when no exact name is found.

3. **Handle platform variations:**
   - Linux: `libonnxruntime.so.X.Y.Z`
   - macOS: `libonnxruntime.X.Y.Z.dylib`
   - Windows: `onnxruntime.dll` (usually unversioned — exact name match handles this)

4. **Malformed lookalikes** (e.g., `libonnxruntime.so.abc`) → `parse::<u64>()` fails → empty version key → sorts first → never selected over real versions.

**Affected callsites:** `src/cli/leindex/setup_ort.rs:538-556`.

**CCN impact:** `scan_dir_for_ort_lib` stays ≤15 (simple sort_by with custom comparator, ~CCN 4).

**Alternatives considered:**
- *Use `semver` crate:* Rejected — ORT version strings don't follow semver exactly, and adding a dependency for this is overkill.
- *Parse filename with regex:* Rejected — simpler to split on `.` and parse.

**Risk + verification:**
- **Risk:** Version parsing must handle all platform filename patterns. Test with actual filenames.
- **Verification:** Add tests:
  - `test_ort_lib_sort_prefers_10_over_9`: create temp files `libonnxruntime.so.10` and `libonnxruntime.so.9`, assert `.10` is selected.
  - `test_ort_lib_sort_prefers_higher_patch`: `.1.20.0` vs `.1.19.5`, assert `.1.20.0` selected.
  - `test_ort_lib_sort_malformed_sorts_last`: `.so.abc` vs `.so.1.0.0`, assert `.1.0.0` selected.
  - `test_ort_lib_sort_deterministic_tiebreak`: two copies of `.so.1.0.0` with different parent paths, assert deterministic selection.

**Out of scope:** Changing `is_ort_runtime_lib_name_for_setup`. Changing `ort_lib_names()`.

---

## New CodeRabbit Findings (Fresh Review)

### CR-F1 through CR-F5. TASKLIST.md consistency findings

**Status:** VALID (all 5)

**Problem:** TASKLIST.md contains contradictory status entries where checklist items are marked complete (`[x]`) but the evidence/notes contradict the claim, or items are marked complete when gates are actually failing.

**Current location:** `TASKLIST.md:10-16` and related evidence lines.

**Findings:**

| # | Severity | Lines | Issue |
|---|----------|-------|-------|
| CR-F1 | minor | 10, 71, 109 | Oversized-file status: line 10 says `[x]` all files <2000 lines, line 109 confirms 0 files >2000, but line 71 still has `[~]` listing four files >2000 (search/search.rs 5801, index_builder.rs 3717, render.rs 2992, indexing.rs 2280). Contradiction between completed gate and active work item. |
| CR-F2 | major | 12, 72, 110 | jscpd status: line 12 says `[~]` with "actual: 7.25%" but line 110 reports "8.58%" — the actual current value exceeds the 8% threshold. Line 12's claim of 7.25% is stale. The gate is failing. |
| CR-F3 | ~~major~~ **invalid** | 14, 42, 111 | ONNX validation: line 14 says `[x]` clippy passes, but lines 42 and 111 claim the onnx-feature build is "broken pre-existingly." **Verified against HEAD `4c71a201`: `cargo check -p leindex-embed --features onnx` → exit 0 (clean), and `cargo check -p leindex --features onnx,remote-embeddings` → exit 0 (clean).** The "broken" premise is stale; the onnx feature compiles. Line 14's `[x]` is accurate → CR-F3 is a non-issue. **No TASKLIST edit required** (do not downgrade a gate that passes). |
| CR-F4 | major | 16, 103, 112 | Reindex gate: line 16 says `[x]` reindex succeeds, but line 103 says `[~]` "one final serialized refresh remains required" and line 112 says "do not commit, push, or open a PR unless separately requested." Contradiction. |
| CR-F5 | major | 15, 111 | Workspace test status: line 15 says `[x]` all tests pass (1769/0), but line 111 reports "1200 pass / 1 fail" with a failing graph extraction test. Contradiction. |

**Design:**

1. **For each finding, verify the actual current state** by running the relevant command, then update TASKLIST.md to reflect reality:
   - CR-F1: Run `find src crates -name '*.rs' | xargs wc -l | awk '$1>2000'`. If empty, mark line 71 as historical (add "Historical: resolved" label). If non-empty, update line 10 to `[~]`.
   - CR-F2: Run the jscpd command. If >8%, change line 12 to `[ ]` with actual percentage. If ≤8%, update the percentage.
   - CR-F3: Run `cargo clippy -p leindex-embed --features onnx -- -D warnings` AND `cargo clippy -p leindex --features onnx,remote-embeddings -- -D warnings`. **Both verified clean at HEAD `4c71a201` (exit 0).** If both pass (expected), CR-F3 is invalid — **make no TASKLIST edit**, since line 14's `[x]` is already accurate. Only if a future run actually fails should line 14 change to `[~]`.
   - CR-F4: Verify the reindex status. If not finalized, change line 16 to `[~]`.
   - CR-F5: Run `cargo test --workspace`. If failures, change line 15 to `[~]` with actual pass/fail counts.

2. **Rule:** Only mark a gate `[x]` when the corresponding command actually passes at the time of the edit. Historical snapshots should be explicitly labeled as historical.

**Verification:** After editing TASKLIST.md, re-run `coderabbit review --agent --base codex/release-1.8.4` and confirm these 5 findings no longer appear.

**Out of scope:** Fixing the underlying gate failures (those are separate engineering tasks). Only TASKLIST.md documentation consistency is in scope here.

---

### CR-F6. index_freshness: should_skip_fast_file_stat_scan bypass

**Status:** VALID

**Problem:** `indexed_files_are_stale` at `src/cli/index_freshness.rs:290` composes `should_skip_fast_file_stat_scan(len) || ...`, meaning projects >10,000 files are always declared stale by the fast check. The predicate is meant to **bypass** the expensive per-file stat scan, not to declare staleness.

**Invariant at risk:** Large projects are forced through the slow freshness path every time, even when nothing has changed.

**Current location:** `src/cli/index_freshness.rs:285-299`, predicate at `:47-51`.

**Design:**

1. **Change the composition so skip returns non-stale (inconclusive):**
   ```rust
   fn indexed_files_are_stale(
       ctx: &FreshnessContext<'_>,
       indexed_files: &HashMap<String, String>,
       db_time: std::time::SystemTime,
   ) -> bool {
       if should_skip_fast_file_stat_scan(indexed_files.len()) {
           return false; // Skip fast stat scan; defer to authoritative hash check
       }
       indexed_files.keys().any(|indexed_path| {
           match std::fs::metadata(ctx.project_path.join(indexed_path)) {
               Ok(metadata) => metadata
                   .modified()
                   .map_or(true, |modified| modified >= db_time),
               Err(_) => true,
           }
       })
   }
   ```

2. **Verify the caller's behavior for BOTH return paths (load-bearing — trace before changing code).** `is_stale_fast` at `:166-236` calls `indexed_files_are_stale` as one of several `||` checks. Two cases must be confirmed by reading `is_stale_fast` and its own caller (e.g. `index_freshness.rs:65-92` `check_freshness` and whatever invokes `is_stale_fast`):
   - **`false` path:** If `indexed_files_are_stale` returns `false`, the other `||` checks still run; if all fast checks pass, the caller proceeds to the authoritative `check_freshness` hash-based path. So `false` = "this fast check found no staleness," not "definitely fresh." ✅ Safe.
   - **`true` path (the one the current `skip → true` relies on):** Determine what the caller does when `is_stale_fast` returns `true`. If `true` **short-circuits into a reindex that bypasses the authoritative hash check**, then the current code makes large projects reindex on every run (wasteful but correct), and the fix converts that into a defer-to-authoritative (the intended optimization) — confirm the authoritative path still runs so freshness is still enforced. If `true` merely triggers the authoritative path anyway, the change is behavior-neutral for large projects. **Either way the change is safe, but the executor must state which case holds and cite the call site.** Do not merge with this trace left blank.

3. **Preserve metadata-error stale behavior** for scanned projects (the `Err(_) => true` branch remains for projects below the threshold).

**Affected callsites:** `src/cli/index_freshness.rs:285-299`.

**CCN impact:** `indexed_files_are_stale` gains one `if` (CCN ~3, well within ≤15).

**Verification:**
- `test_indexed_files_stale_skips_large_project`: 10,001 indexed files, all unchanged metadata, assert `false`.
- `test_indexed_files_stale_scans_small_project`: 5,000 files, one modified, assert `true`.
- `test_indexed_files_stale_metadata_error_small_project`: 5,000 files, one missing metadata, assert `true`.
- `test_large_project_proceeds_to_authoritative_check`: verify caller behavior when fast check returns false.
- **Caller `true`-path trace (manual, load-bearing):** read `is_stale_fast` and its caller; document which case holds (short-circuit-reindex vs authoritative-always-runs) with the cited call site. Required before merge — see step 2.

**Out of scope:** Changing `should_skip_fast_file_stat_scan` threshold. Changing other stale checks.

---

### CR-F7. Neural candidate over-fetch saturating multiplication

**Status:** VALID

**Problem:** `query.top_k * 10` at `src/search/search/mod.rs:1097` can overflow/panic for large `usize`. The same file already uses `saturating_mul` at line 1296.

**Current location:** `src/search/search/mod.rs:1097`.

**Design:**

1. **Replace with saturating multiplication:**
   ```rust
   .search(q_emb, query.top_k.saturating_mul(10).max(100))
   ```

**Affected callsites:** `src/search/search/mod.rs:1097`.

**CCN impact:** None.

**Verification:** Existing search tests. Add `test_large_top_k_does_not_overflow` with `top_k = usize::MAX / 2` (verify no panic, results still returned).

**Out of scope:** Changing the over-fetch factor (10×).

---

### CR-F8. VectorImpl clear/remove tombstone bug

**Status:** VALID

**Problem:** After `MmapVectorIndex::clear()`, `rows` still contains pre-clear IDs. Calling `remove(pre_clear_id)` finds the ID in `rows` and inserts an unnecessary tombstone, returning `true` misleadingly. Tombstone growth causes memory bloat.

**Current location:** `src/search/search/vector_impl.rs:109-120`.

**Design:**

1. **Guard `remove` against cleared state:**
   ```rust
   fn remove(&mut self, node_id: &str) -> bool {
       let removed_delta = self.delta.remove(node_id).is_some();
       let removed_base = !self.cleared
           && self.rows.contains_key(node_id)
           && self.tombstones.insert(node_id.into());
       removed_delta || removed_base
   }
   ```

   This is exactly CodeRabbit's suggested patch. It adds `!self.cleared` as a precondition for base-row tombstoning.

**Affected callsites:** `src/search/search/vector_impl.rs:115-120`.

**CCN impact:** Negligible (one `&&` added).

**Verification:**
- `test_remove_after_clear_returns_false`: clear, then remove a pre-clear ID, assert `false`.
- `test_remove_after_clear_no_tombstone_growth`: clear, remove several pre-clear IDs, assert `tombstones.is_empty()`.
- `test_remove_delta_after_clear`: add to delta, clear, remove delta ID, assert `false` (delta was cleared).

**Out of scope:** Changing `clear()` itself. Changing search behavior (already correct via `cleared` flag).

---

### CR-F9. set_neural_weight cache invalidation

**Status:** VALID

**Problem:** `Searcher::set_neural_weight` sets `neural_weight` without clearing `search_cache`. Cached results scored with the old weight are returned for queries after the weight changes. The cache key doesn't include `neural_weight`.

**Current location:** `src/search/search/mod.rs:206-208`.

**Design:**

1. **Clear cache when the effective weight changes:**
   ```rust
   pub fn set_neural_weight(&mut self, weight: f32) {
       let clamped = weight.clamp(0.0, 1.0);
       if (clamped - self.neural_weight).abs() > f32::EPSILON {
           self.search_cache.clear();
           self.search_cache_bytes = 0;
       }
       self.neural_weight = clamped;
   }
   ```

   This matches CodeRabbit's suggested patch. Only clears when the value actually changes (after clamping).

**Affected callsites:** `src/search/search/mod.rs:206-208`.

**CCN impact:** +2 branches (CCN ~4, well within ≤15).

**Verification:**
- `test_set_neural_weight_clears_cache_on_change`: search (populates cache), change weight, search again (assert cache miss/recomputation).
- `test_set_neural_weight_preserves_cache_when_unchanged`: search, set same weight (after clamping), search again (assert cache hit).

**Out of scope:** Including `neural_weight` in the cache key (clearing is sufficient and simpler).

---

### CR-F10. MAX_RESPONSE_FRAME_SIZE alignment

**Status:** VALID

**Problem:** The constant is 64 MiB but the comment says it mirrors the worker-side guard of `max_frame_size * 2 = 32 MiB`. The worker default is `DEFAULT_MAX_FRAME_SIZE = 16 MiB`, so `2× = 32 MiB`. The client should be 32 MiB, not 64 MiB.

**Current location:**
- `src/search/onnx/client_config.rs:17-22` — constant (64 MiB)
- `crates/leindex-embed/src/runtime.rs:57-58` — `DEFAULT_MAX_FRAME_SIZE = 16 MiB`
- `:913-914` — `max_incoming_frame = config.max_frame_size.saturating_mul(2)` = 32 MiB

**Design:**

1. **Change the constant to 32 MiB:**
   ```rust
   pub(super) const MAX_RESPONSE_FRAME_SIZE: u32 = 32 * 1024 * 1024; // 32 MiB
   ```

2. **Update the comment** to accurately reflect the worker-side guard:
   ```rust
   /// Maximum response frame size in bytes.
   ///
   /// Mirrors the worker-side incoming-frame guard (`max_frame_size * 2` = 32 MiB
   /// with the default 16 MiB max_frame_size). A response larger than this is
   /// rejected with a clear protocol error.
   ```

**Affected callsites:** `src/search/onnx/client_config.rs:22`.

**CCN impact:** None.

**Verification:** Existing ONNX client tests. Add `test_max_response_frame_size_matches_worker_guard` asserting `MAX_RESPONSE_FRAME_SIZE as usize == DEFAULT_MAX_FRAME_SIZE * 2`.

**Out of scope:** Changing `DEFAULT_MAX_FRAME_SIZE`. Changing the worker-side guard formula.

---

### CR-F11. C++ DECISION_KINDS missing node types

**Status:** VALID

**Problem:** C++ `DECISION_KINDS` omits `for_range_loop`, `catch_clause`, and `conditional_expression` — all verified tree-sitter-cpp 0.23.4 node types. Other languages include their language-specific decision forms.

**Current location:** `src/parse/cpp.rs:441-447`.

**Design:**

1. **Add the three verified node types:**
   ```rust
   const DECISION_KINDS: &[&str] = &[
       "if_statement",
       "for_statement",
       "while_statement",
       "do_statement",
       "case_statement",
       "for_range_loop",
       "catch_clause",
       "conditional_expression",
   ];
   ```

2. **Grammar verification done:** tree-sitter-cpp 0.23.4 `node-types.json` contains all three at confirmed line numbers.

**Affected callsites:** `src/parse/cpp.rs:441-447`.

**CCN impact:** None (constant array).

**Verification:** Add test `test_cpp_range_for_complexity` with `for (int x : vec) { ... }`, assert `cyclomatic > 1`. Add `test_cpp_catch_complexity` with `try { } catch (...) { }`. Add `test_cpp_ternary_complexity` with `int x = cond ? 1 : 0;`.

**Out of scope:** Changing other languages' DECISION_KINDS. Changing the complexity calculation algorithm.

---

### CR-F12. setup_ort USERPROFILE fallback for leindex_home

**Status:** VALID

**Problem:** `discover_ort_path_fallback` checks `LEINDEX_HOME` then `HOME/.leindex` but doesn't check `USERPROFILE` on Windows where `HOME` may be absent.

**Current location:** `src/cli/leindex/setup_ort.rs:468-471`.

**Design:**

1. **Add USERPROFILE fallback after HOME:**
   ```rust
   let leindex_home = std::env::var("LEINDEX_HOME")
       .map(PathBuf::from)
       .or_else(|_| {
           std::env::var("HOME")
               .or_else(|_| std::env::var("USERPROFILE"))
               .map(|home| PathBuf::from(home).join(".leindex"))
       })
       .ok();
   ```

   This matches CodeRabbit's suggested patch. Priority: `LEINDEX_HOME` → `HOME/.leindex` → `USERPROFILE/.leindex`.

**Affected callsites:** `src/cli/leindex/setup_ort.rs:468-471`.

**CCN impact:** Negligible.

**Verification:** Add test `test_discover_ort_path_userprofile_fallback` (set `USERPROFILE`, remove `HOME` and `LEINDEX_HOME`, place a mock ORT lib under `USERPROFILE/.leindex/lib/`, assert discovery finds it). Use `EnvVarGuard` from D1.

**Out of scope:** Changing other fallback paths in `discover_ort_path_fallback`.

---

### CR-F13. run_check duplicate ORT detection

**Status:** VALID

**Problem:** `run_check` calls `check_ort_installed()` (which internally calls `get_ort_version()`) and then separately calls `get_ort_version()` — two redundant Python subprocess probes. `ort_installed` should be derived from the single `get_ort_version()` call.

**Current location:** `src/cli/leindex/setup.rs:1107-1108`.

**Design:**

1. **Derive `ort_installed` from `live_version`:**
   ```rust
   let live_version = get_ort_version();
   let ort_installed = live_version.is_some();
   ```
   Remove the separate `check_ort_installed()` call at line 1107.

2. **Preserve all status-reporting behavior:** `ort_version` still prefers live over configured:
   ```rust
   let ort_version = live_version.or_else(|| config.neural.ort_version.clone());
   ```

**Affected callsites:** `src/cli/leindex/setup.rs:1107-1108`.

**CCN impact:** None (removes a line).

**Verification:** Existing setup tests (78 tests). Verify `run_check` output is unchanged in a test that mocks both probes.

**Out of scope:** Changing `check_ort_installed()` or `get_ort_version()` definitions.

---

### CR-F14. prepare_neural_runtime consolidate ORT detection

**Status:** VALID

**Problem:** `prepare_neural_runtime` makes redundant ORT detection calls: `check_ort_installed()` at line 607, again at 614 (neural-disabled return), `get_ort_version()` at 649, `get_ort_version()` at 712, `check_ort_installed()` at 717. The `else if pre_existing_version.is_none()` branch at 703 is unreachable under consistent probe results.

**Current location:** `src/cli/leindex/setup.rs:603-738`.

**Design:**

1. **Consolidate initial detection:**
   ```rust
   let pre_existing_version = get_ort_version();
   let initial_ort_installed = pre_existing_version.is_some();
   ```

2. **Reuse `initial_ort_installed` in neural-disabled return** (line 614): replace `check_ort_installed()` with `initial_ort_installed`.

3. **Remove the `get_ort_version()` call at line 649** — use `pre_existing_version` directly (already computed).

4. **Remove the unreachable `else if pre_existing_version.is_none()` branch** at lines 703-708. Under consistent detection, `initial_ort_installed == true` implies `pre_existing_version.is_some()`, so this branch is dead.

5. **Consolidate post-install detection:**
   ```rust
   let post_install_version = get_ort_version();
   let ort_installed = post_install_version.is_some();
   let ort_version = post_install_version.or(pre_existing_version);
   ```
   Remove `check_ort_installed()` at line 717.

6. **Preserve all install/provider-selection behavior:** partial-state messages, version compatibility checks, provider availability checks, install/upgrade decisions, model validation.

**Affected callsites:** `src/cli/leindex/setup.rs:607, 614, 649, 703-708, 712, 717`.

**CCN impact:** `prepare_neural_runtime` decreases (removes dead branch + redundant calls).

**Verification:** Existing 78 setup tests. Add `test_prepare_neural_runtime_single_detection` verifying `get_ort_version` is called exactly once when neural is disabled, and exactly twice (before + after install) when enabled.

**Out of scope:** Changing install_ort, check_provider_available, or version compatibility logic.

---

## Final Coverage Matrix

| Finding ID | Plan Section | Status | Verification |
|---|---|---|---|
| **Deferred inventory** | | | |
| A1 | A1 | VALID | `test_neural_admission_matches_lexical_admission` |
| A2 | A2 | VALID | `test_file_summary_content_byte_identical` |
| A3 | A3 | VALID | `test_tfidf_rejects_old_schema_version`, `test_tfidf_is_fresh_with_fingerprint_mismatch` |
| A4 | A4 | PARTIAL | `test_scan_git_project_files_reuses_finalize` |
| A5 | A5 | VALID | `test_text_search_does_not_block_runtime` |
| A6 | A6 | VALID | `test_run_phase5_caches_fallback_summaries` |
| A7 | A7 | VALID (retain fail-fast) | Document rationale; no code change |
| B1 | B1 | PARTIAL | `test_incremental_summaries_cover_full_project` |
| C1 | C1 | VALID (conservative by design) | `test_cross_file_call_fans_out_to_all_matches` |
| C2 | C2 | PARTIAL | `test_duplicate_qname_inheritance_assigns_to_first` |
| D1 | D1 | VALID | `test_env_var_guard_restores_on_panic` |
| D2 | D2 | VALID | Multi-batch indexing log check |
| D3 | D3 | VALID | `cargo test -p memcheck --test diff_logic test_val_measure_012` |
| D4 | D4 | VALID | Run 27 runtime tests twice |
| D5 | D5 | VALID | `test_ort_lib_sort_prefers_10_over_9` |
| **Fresh review findings** | | | |
| CR-F1 | CR-F1 | VALID | Verify with `find ... wc -l` command |
| CR-F2 | CR-F2 | VALID | Verify with jscpd command |
| CR-F3 | CR-F3 | **INVALID (stale premise)** | Verified clean at `4c71a201`: both onnx + onnx,remote-embeddings build. **No action required** — gate already passes. |
| CR-F4 | CR-F4 | VALID | Verify reindex status |
| CR-F5 | CR-F5 | VALID | Verify with `cargo test --workspace` |
| CR-F6 | CR-F6 | VALID | `test_indexed_files_stale_skips_large_project` |
| CR-F7 | CR-F7 | VALID | `test_large_top_k_does_not_overflow` |
| CR-F8 | CR-F8 | VALID | `test_remove_after_clear_returns_false` |
| CR-F9 | CR-F9 | VALID | `test_set_neural_weight_clears_cache_on_change` |
| CR-F10 | CR-F10 | VALID | `test_max_response_frame_size_matches_worker_guard` |
| CR-F11 | CR-F11 | VALID | `test_cpp_range_for_complexity` |
| CR-F12 | CR-F12 | VALID | `test_discover_ort_path_userprofile_fallback` |
| CR-F13 | CR-F13 | VALID | Existing 78 setup tests |
| CR-F14 | CR-F14 | VALID | `test_prepare_neural_runtime_single_detection` |

## Validation Gates (Post-Implementation)

After implementing any changes, the following must remain green:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy -p leindex-embed --features onnx -- -D warnings
cargo clippy -p leindex --features onnx,remote-embeddings -- -D warnings  # remote-embeddings is a leindex (root) feature, NOT leindex-embed
lizard src/ crates/leindex-embed/src/ -C 15 -x"*/tests/*"
find src crates -name '*.rs' | xargs wc -l | awk '$1>2000'  # must be empty
```

All functions touched must end at CCN ≤ 15. No `// lizard: off` suppressions. No `#[allow(...)]` suppressions for lint findings.

## Execution Priority

1. **Low-risk, high-confidence fixes first:** CR-F7 (saturating mul), CR-F8 (clear/remove), CR-F10 (frame size), CR-F11 (DECISION_KINDS), CR-F13 (run_check dedup), D2 (admission count), CR-F9 (cache invalidation).
2. **Test infrastructure:** D1, D4 (EnvVarGuard) — enables safe testing of other changes.
3. **Medium-risk refactors:** A3 (TF-IDF persistence), A6 (phase5 cache), CR-F6 (freshness skip), CR-F14 (prepare_neural_runtime), CR-F12 (USERPROFILE), D5 (ORT sorting), D3 (memcheck test).
4. **Higher-risk refactors:** A1 (neural admission), A2 (FileSummary precompute), A5 (spawn_blocking), B1 (parse_results metric fix — **drop the rename, see B1 alternatives**), C2 (node_ids fix).
5. **Product decisions:** A7 (retain fail-fast — no code change), C1 (document fan-out — minimal code change).
6. **Documentation:** CR-F1, CR-F2, CR-F4, CR-F5 (TASKLIST consistency — verify each gate, then update). **CR-F3 excluded** — verified invalid (onnx builds clean); make no edit.
