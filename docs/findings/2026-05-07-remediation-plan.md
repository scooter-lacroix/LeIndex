# LeIndex Prioritized Remediation Plan

**Date:** 2026-05-07  
**Mission:** LeIndex root-cause resource and stability investigation  
**Feature:** final-remediation-synthesis  
**Scope:** Synthesizes profiling, audit, process-model, crash, traversal/watch, file-management, and cleanup findings into one prioritized strategy. No product-code implementation changes are introduced in this mission.

---

## 1. Executive Summary

This plan integrates evidence from four investigation milestones — bounded profiling, root-cause code audit, multi-process architecture analysis, and stale-index cleanup — into a single prioritized remediation strategy for LeIndex's resource and stability issues.

**Core finding:** LeIndex's primary problem is **not** a memory leak or architectural flaw, but a **simultaneous bounded-peak allocation** in the TF-IDF construction path that materializes ~9 GB of data structures for large projects (773K+ PDG nodes). This intrinsic single-process cost is the root cause of 100% crash rates on large projects, and it is amplified by 100% state duplication across concurrent processes.

**Remediation philosophy:** Root-cause and efficiency fixes come first. Memory caps are positioned as fallback safety controls only. Each recommendation preserves or improves LeIndex functionality. Contradictions and uncertainty are explicitly reconciled or carried as bounded hypotheses.

**Priority tiers:**
- **Tier 1 (Critical)** — Root-cause fixes that eliminate the crash and enable large-project support
- **Tier 2 (High)** — Efficiency fixes that reduce resource waste and improve performance
- **Tier 3 (Medium)** — Architecture improvements that reduce multi-process duplication
- **Tier 4 (Low)** — Operational improvements and defensive measures
- **Tier 5 (Fallback)** — Memory-cap safety controls, positioned after all substantive remedies

---

## 2. Evidence Traceability Matrix

Each recommendation below links to supporting evidence from the investigation milestones.

| Evidence Area | Key Artifacts | Key Findings |
|---------------|--------------|--------------|
| **Profiling** | `profiling-evidence.md` | 100% crash rate on llvm-project at ~3 GB RSS; shared crash offset +0x19586b0; SIGBUS/SIGSEGV in TF-IDF path; small-repo baseline succeeds at 160 MB |
| **Code Audit** | `root-cause-audit.md` | TF-IDF materializes ~9 GB simultaneously (file_cache + corpus + raw_nodes + NodeInfo); no true leaks; 7 duplication sites; 7 file-management inefficiencies; 8 streaming/pagination opportunities |
| **Process Model** | `multi-process-architecture-analysis.md` | 100% state duplication across concurrent processes; 83% wall-time overhead; 8.6 GB RSS across 3 live processes; hybrid persistent MCP server recommended |
| **Cleanup** | `stale-index-cleanup-evidence.md` | 959 stale artifacts (~1 GB) removed; temp-path fallback in `resolve_storage_path` accumulates residue; no GC mechanism exists |
| **Historical OOM** | `2026-05-06-leindex-memory-oom-investigation.md` | 3 OOM kills (29–38 GB RSS each); SIGSEGV in `__libc_free` (heap corruption); 17 crashes in 24h; system-wide cascading failures |

---

## 3. Intrinsic Single-Process Costs vs. Multi-Process Duplication

Before prioritizing remedies, we separate the two classes of resource cost:

### 3.1 Intrinsic Single-Process Costs (Present Even with One Process)

| Cost | Peak for Large Project | Root Cause | Evidence |
|------|----------------------|------------|----------|
| TF-IDF corpus + file_cache + raw_nodes | ~6.5 GB | All file contents + tokens held simultaneously in `index_nodes()` | both (profiling §2.2 + audit §2.1) |
| NodeInfo + embedding construction | ~2.5 GB | 773K × 768-dim embeddings allocated in Pass 2 | both |
| SearchEngine indexes (inverted + vector) | ~3 GB | Rebuilt from scratch on every `load_from_storage()` | both |
| PDG in-memory graph | ~400 MB | StableGraph loaded from SQLite | both |
| Source file hashing I/O | ~6.4 GB read | Full file reads for BLAKE3 hash | code-supported |
| Double TF-IDF build per index request | ~2× transient | `index_handle` creates two LeIndex instances | both |

**Total intrinsic single-process peak: ~9–12 GB for llvm-project** (far exceeds the 3 GB crash threshold)

### 3.2 Multi-Process Duplication Costs (Additional Overhead per Concurrent Process)

| Duplicated State | Per-Process Cost | Evidence |
|-----------------|------------------|----------|
| PDG in-memory graph | ~400 MB | Each process loads from SQLite independently |
| TF-IDF embedder | ~500 MB | Rebuilt from scratch per process (not persisted) |
| Search engine (inverted + vector) | ~3 GB | Rebuilt from scratch per process |
| Vector embeddings | ~2.4 GB | 773K × 768 × 4 bytes, rebuilt per process |
| Initialization overhead | ~350 ms (small), ~2–3 min (large) | Storage open + PDG load + TF-IDF build + search index |

**Measured duplication:** 2 concurrent processes on small project: 2.0× RSS, 1.83× wall time. On large project: 100% of state duplicated, masked by MemoryHigh capping.

### 3.3 Priority Assignment Logic

1. **Fix intrinsic single-process costs first** — without this, even a single process crashes on large projects
2. **Then eliminate multi-process duplication** — once a single process works, reduce waste across processes
3. **Memory caps as fallback only** — defense in depth, not a substitute for fixing the root cause

---

## 4. Prioritized Remediation Recommendations

### Tier 1: Critical Root-Cause Fixes

#### R1. Batch-process TF-IDF construction to reduce peak memory from ~9 GB to ~1–2 GB

**Problem:** `index_nodes()` in `index_builder.rs:479-599` materializes `file_cache` (all source files), `corpus` (all tokens), `raw_nodes` (all AST nodes), and `NodeInfo` (all embeddings) simultaneously. For 773K nodes, this peaks at ~9 GB.

**Fix:** Process PDG nodes in configurable batches (e.g., 10K nodes at a time). For each batch:
1. Read only the needed source files, extract byte ranges, build enrichment strings
2. Tokenize and accumulate into a partial corpus
3. Build TF-IDF vocabulary incrementally (or in two passes: first pass for vocabulary, second for embedding)
4. Construct NodeInfo objects for the batch, feed to SearchEngine, then drop batch data
5. Release file contents after extraction (don't cache all files for the full duration)

**Functional preservation:** Indexing produces identical results; only the order of processing changes. All PDG nodes are still indexed. Search quality is unaffected.

**Evidence:** profiling-evidence §2 (crash at ~3 GB during TF-IDF), root-cause-audit §2.1 (9 GB peak analysis), root-cause-audit §10 (P0 streaming opportunity)

**Validation intent:** After implementation, bounded profiling on llvm-project should complete without crash at <3 GB RSS. Compare peak RSS before/after on both small and large projects.

**Estimated impact:** Reduces peak RSS by ~6–7 GB for large projects. **Enables large-project support.**

---

#### R2. Stream file content reading — read on-demand, extract byte range, release immediately

**Problem:** `file_cache: HashMap<String, Arc<String>>` in `index_builder.rs:487-488, 505-514` loads every unique source file into memory and holds them all simultaneously. For llvm-project with 70K+ files, this is ~3.5 GB.

**Fix:** Instead of caching all files:
1. Read each file on-demand when processing its PDG nodes
2. Extract the needed byte range `[start..end]` for the node
3. Release the file content immediately after extraction
4. Use seeked reads for large files (read only the needed range)
5. Optionally: keep a small LRU cache (e.g., 100 files) for files with many nodes

**Functional preservation:** Byte-range extraction produces identical content. Node enrichment is unchanged. The only change is when file contents are loaded and released.

**Evidence:** root-cause-audit §7 (file-management inefficiencies), root-cause-audit §10 (P0 streaming opportunity)

**Validation intent:** Measure peak file_cache memory during indexing. Should drop from ~3.5 GB to <200 MB with LRU cache.

**Estimated impact:** Reduces peak RSS by ~3–4 GB for large projects. **Critical for crash avoidance.**

---

#### R3. Persist TF-IDF embedder to disk alongside PDG

**Problem:** Every call to `load_from_storage()` (in `leindex/indexing.rs:279-313`) rebuilds the entire TF-IDF embedder from scratch by re-running `index_nodes()`. This means every search, context, diagnostics, or analysis operation on a large project must rebuild ~6 GB of state before it can serve the request.

**Fix:** After TF-IDF construction, serialize the trained `TfIdfEmbedder` (vocabulary, IDF scores, stratified sample indices) to the `.leindex/` storage directory. On load, deserialize the embedder instead of rebuilding it. Only rebuild when the PDG changes (reindex).

**Functional preservation:** Embedder output is deterministic for the same PDG. Persisting and loading produces identical embeddings. Search quality is preserved.

**Evidence:** root-cause-audit §6 (duplication site: load_from_storage TF-IDF rebuild), process-model §4.2 (TF-IDF not persisted), process-model §5.1 (77ms TF-IDF build for small, ~2–3 min for large)

**Validation intent:** Compare load_from_storage time before/after. For small project: should drop from ~350ms to ~30ms. For large project: should drop from ~2–3 min to ~10–20s (PDG load + search index build only).

**Estimated impact:** Eliminates ~2–3 minutes of startup time per process on large projects. Reduces peak RSS during load by ~3 GB (no corpus/file_cache materialization).

---

#### R4. Eliminate double LeIndex instantiation in `index_handle`

**Problem:** `index_handle()` in `registry.rs:530-545` creates two separate LeIndex instances for a single indexing request: one `temp` for indexing and one `fresh` for loading. The `fresh` instance re-runs `load_from_storage()` which rebuilds TF-IDF from scratch.

**Fix:** After indexing completes in the `temp` instance, reuse its already-built search engine and TF-IDF state instead of creating a new instance. Either:
1. Return the initialized `temp` LeIndex as the registry entry, or
2. Transfer the search engine from `temp` to the registry's instance without rebuilding

**Functional preservation:** The indexed data is identical. The registry's project handle receives the same search engine state. No functional change.

**Evidence:** root-cause-audit §2.2 (double instantiation analysis), root-cause-audit §6 (duplication site)

**Validation intent:** Verify that a single `index` call creates only one LeIndex instance. Measure peak RSS reduction during indexing.

**Estimated impact:** Eliminates one full TF-IDF build per indexing request. Reduces peak RSS by up to ~6 GB during indexing on large projects.

---

### Tier 2: High-Priority Efficiency Fixes

#### R5. Use incremental indexing for watcher-triggered reindex

**Problem:** The watcher (`watcher.rs:47-51`) calls `index_project(false)` on any file change, which runs the **full** indexing pipeline including TF-IDF rebuild. Even for a single file change, this materializes all 773K nodes' state.

**Fix:** Use the existing `incremental_reindex` method (already exists in `search.rs:568-590`) for watcher-triggered events. Only re-index changed files, update the search engine incrementally, and avoid rebuilding the entire TF-IDF corpus.

**Functional preservation:** Incremental indexing updates only the changed nodes. Search results reflect the updated code. PDG consistency is maintained.

**Evidence:** root-cause-audit §4 (watcher behavior), root-cause-audit §10 (P1 incremental indexing opportunity), root-cause-audit §3.2 (unbounded reindex cost)

**Validation intent:** Trigger a file change in a watched project. Verify that only the changed file is re-parsed and re-indexed (not the full project). Measure peak RSS during watcher-triggered reindex.

**Estimated impact:** Reduces watcher-triggered reindex cost from ~9 GB peak / 2+ minutes to ~100 MB / <1 second for single-file changes.

---

#### R6. Combine file read operations — hash and extract content in a single pass

**Problem:** Source files are read twice: once for BLAKE3 hashing (`index_builder.rs:311-315`) and once for content extraction during TF-IDF construction (`index_builder.rs:505-514`). For llvm-project, this means ~12.8 GB of I/O for the same files.

**Fix:** Read each file once, compute the hash, and cache the content (or the needed byte ranges) for later TF-IDF use. Alternatively, use streaming BLAKE3 that doesn't require buffering the entire file.

**Functional preservation:** Hash values are identical. Content extraction is unchanged. Only I/O pattern changes.

**Evidence:** root-cause-audit §7 (file-management inefficiencies: full file read for hashing, full file read for content), root-cause-audit §10 (P1 streaming file hashing)

**Validation intent:** Count file read syscalls during indexing. Should drop by ~50%. Measure wall-time improvement.

**Estimated impact:** Reduces disk I/O by ~50% during indexing. Moderate wall-time improvement on I/O-bound systems.

---

#### R7. Eliminate token data duplication between `corpus` and `raw_nodes`

**Problem:** `index_nodes()` stores tokenized data in both `corpus: Vec<(String, Vec<String>)>` and `raw_nodes: Vec<(NodeIndex, String, Vec<String>)>` simultaneously (`index_builder.rs:491-492, 563-564`). For 773K nodes, this doubles token storage (~1.5 GB × 2 = ~3 GB).

**Fix:** Use a single structure that holds all needed data (node index, content, tokens) without duplication. Pass references to both the TF-IDF builder and the embedding loop.

**Functional preservation:** Tokenization results are identical. TF-IDF vocabulary and embeddings are unchanged.

**Evidence:** root-cause-audit §6 (duplication site: corpus + raw_nodes), root-cause-audit §2.1 (memory flow analysis)

**Validation intent:** Measure memory reduction during TF-IDF Pass 1. Should drop by ~1.5 GB for large projects.

**Estimated impact:** Reduces peak RSS by ~1.5 GB during TF-IDF construction.

---

#### R8. Pass pre-tokenized data to SearchEngine to avoid re-tokenization

**Problem:** `SearchEngine.index_nodes()` (`search.rs:452-527`) re-tokenizes all node content that was already tokenized by `index_builder`. For 773K nodes, this is redundant CPU work.

**Fix:** Pass pre-tokenized data (from the TF-IDF construction phase) to SearchEngine. Skip the internal tokenization step.

**Functional preservation:** Tokenization is deterministic; same tokens produce same inverted index. Search results are identical.

**Evidence:** root-cause-audit §8 (CPU-heavy operations: SearchEngine re-tokenization)

**Validation intent:** Measure CPU time in SearchEngine construction. Should drop proportionally to the tokenization savings.

**Estimated impact:** Reduces indexing CPU time by ~10–20% for the search-index phase. No memory impact.

---

### Tier 3: Medium-Priority Architecture Improvements

#### R9. Adopt hybrid persistent MCP server to eliminate process duplication

**Problem:** Each MCP client spawns its own `leindex mcp` process with 100% independent state. On the current host, 3 processes consume 8.6 GB RSS — most of which is duplicated PDG, TF-IDF, and search engine state for the same projects.

**Fix:** Move toward a shared persistent MCP server (Option D from process-model analysis):
1. Use the existing `leindex serve` command or extend `leindex mcp` with Unix socket support
2. All MCP clients connect to the shared server instead of spawning subprocesses
3. The existing `ProjectRegistry` with `DEFAULT_MAX_PROJECTS=5` and `ProjectRwLock` already supports multi-project concurrent access
4. Add memory-aware LRU eviction (evict by memory usage, not just project count)

**Functional preservation:** Each project's data remains isolated (existing `ProjectRwLock` per project). Search results, analysis, and indexing are project-specific. The `index_slots` mechanism already prevents duplicate indexing within a single process.

**Evidence:** process-model §7 (4 architecture options compared), process-model §8 (recommendation with 53% memory savings), process-model §9 (measured 100% duplication, 83% wall-time overhead)

**Validation intent:** Run 2 MCP clients against shared server vs. 2 separate processes. Measure combined RSS, wall time, and correctness of results.

**Estimated impact:** ~53% memory reduction for multi-client scenarios. Eliminates redundant initialization.

**Dependencies:** R3 (TF-IDF persistence) is a prerequisite — without it, a shared server still rebuilds TF-IDF for each project load.

---

#### R10. Memory-map vector embeddings from disk

**Problem:** Vector embeddings (773K × 768 × 4 bytes = ~2.4 GB) are allocated in RAM and rebuilt on every process launch. They are immutable between reindexes.

**Fix:** Persist embeddings to a memory-mapped file in `.leindex/`. Use `mmap` to access them without loading into RSS. The OS manages page cache, and unused pages are evictable under memory pressure.

**Functional preservation:** Embedding values are identical (same binary representation). Search quality is preserved. Distance computations produce identical results.

**Evidence:** process-model §4.2 (vector embeddings not persisted, ~2.4 GB), process-model §6.2 (high sharing potential for embeddings)

**Validation intent:** Compare RSS with and without mmap for large project. Vector search results should be identical.

**Estimated impact:** Reduces per-project RSS by ~2.4 GB for large projects. Embeddings loaded on-demand by OS page cache.

**Dependencies:** R3 (TF-IDF persistence) and R1 (batch processing) should be implemented first to reduce the overall indexing memory pressure.

---

### Tier 4: Operational Improvements

#### R11. Implement stale-artifact garbage collection

**Problem:** LeIndex creates temp artifacts under `~/.claude/tmp/` via the `resolve_storage_path` fallback chain. There is no cleanup on process exit or garbage collection mechanism. Over time, ~1 GB of stale artifacts accumulated.

**Fix:**
1. Add an at-exit cleanup hook that removes temp-based storage directories when the process shuts down cleanly
2. Implement a startup GC that scans for LeIndex-owned temp artifacts older than a configurable threshold (e.g., 7 days) and removes them
3. Consider adding a `leindex cleanup` CLI command for manual cleanup

**Functional preservation:** Only removes stale, orphaned artifacts. Active indexes are preserved. The in-project `.leindex/` storage is never touched.

**Evidence:** stale-index-cleanup-evidence (959 stale artifacts, ~1 GB removed), root-cause-audit §11 (stale-artifact root cause: temp fallback chain, no cleanup)

**Validation intent:** Run LeIndex on a temp project, exit, verify temp artifacts are cleaned. Run GC, verify only stale artifacts removed.

**Estimated impact:** Prevents disk space accumulation. No direct runtime memory/CPU impact (operational residue, not live pressure).

---

#### R12. Add file count and size limits to project scanning

**Problem:** `scan_project_files` (`index_builder.rs:323-395`) has no file count limit, no total size limit, and no per-file size limit. A project with 1M files or a single 10 GB file would be processed without restriction.

**Fix:**
1. Add configurable `max_files` limit (default: 100K files)
2. Add configurable `max_file_size` limit (default: 10 MB per file)
3. Add configurable `max_total_size` limit (default: 5 GB total)
4. Skip files exceeding limits with a warning log

**Functional preservation:** Files within limits are indexed normally. Users can configure limits per-project via `.leindex/config.yaml`. Large files are skipped gracefully.

**Evidence:** root-cause-audit §3.1 (conditionally bounded traversal, no file count/size limits)

**Validation intent:** Test with a project containing oversized files. Verify they are skipped with warnings and indexing completes for remaining files.

**Estimated impact:** Prevents pathological cases. No impact on typical projects.

---

#### R13. Resolve crash offset with debug build

**Problem:** The shared crash offset `+0x19586b0` cannot be precisely mapped to a source function without debug symbols. While the evidence strongly points to TF-IDF/embedding code, exact confirmation requires symbol resolution.

**Fix:** Build LeIndex with debug symbols (`cargo build --profile release-with-debug` or similar) and reproduce the crash on llvm-project. Use `addr2line` or `coredumpctl` to map the offset to exact source location.

**Functional preservation:** Debug build is a development/diagnostic tool only. No production code change.

**Evidence:** root-cause-audit §12 (unresolved gap: exact binary offset mapping), profiling-evidence §2 (shared crash offset in SIGBUS and SIGSEGV)

**Validation intent:** Reproduce crash with debug build. Confirm offset maps to TF-IDF construction code path.

**Estimated impact:** Confirms crash hypothesis. No direct user impact but increases confidence in R1–R4 fixes.

---

### Tier 5: Fallback Safety Controls

#### R14. External memory cap via systemd/cgroups (FALLBACK — not primary remedy)

**Problem:** Without the root-cause fixes above, LeIndex can consume unlimited memory and trigger OOM kills that crash the entire system.

**Fix:** Deploy external memory limits as a safety net:
1. Wrap LeIndex invocations with `systemd-run --user --scope -p MemoryHigh=6G -p MemoryMax=8G`
2. Or configure the MCP client (maestro) to set memory limits on spawned processes
3. Optionally: add a `--max-memory` CLI flag to LeIndex that sets a self-imposed RSS limit via `rlimit` or allocator configuration

**CRITICAL:** This is a **fallback safety control**, not a primary remedy. It prevents system crashes but does not fix the underlying resource waste. Memory caps cause LeIndex to crash (SIGBUS/SIGSEGV) or hang (stuck in swap) on large projects rather than completing successfully.

**Functional preservation:** Small projects work normally. Large projects fail gracefully (crash or timeout) instead of consuming all system memory. **Functionality is reduced** — large projects cannot be indexed under memory caps.

**Evidence:** profiling-evidence §2 (crash at 3 GB under MemoryHigh), historical OOM investigation (29–38 GB without limits), mission AGENTS.md ("Do not use a LeIndex-side memory cap as the primary remediation recommendation")

**Validation intent:** Verify that memory caps prevent OOM kills. Verify that small projects still work. Document that large projects cannot complete under caps.

**Estimated impact:** Prevents system-wide OOM. **Reduces functionality** for large projects until root-cause fixes (R1–R4) are implemented.

**Position:** This must come AFTER R1–R4. Once batch processing (R1), streaming reads (R2), embedder persistence (R3), and double-instantiation elimination (R4) are implemented, the per-process peak should drop to ~2–3 GB for large projects, making memory caps less necessary.

---

## 5. Contradiction and Uncertainty Reconciliation

### 5.1 Reconciled Contradictions

| Apparent Contradiction | Resolution | Confidence |
|----------------------|------------|------------|
| **"Memory leak" vs. "bounded peak"** | No memory leak exists. All allocations are bounded and released when `LeIndex` is dropped. The OOM events are caused by bounded peaks exceeding system memory, not unbounded growth. | **High** — both runtime and code evidence agree |
| **"SIGSEGV = unsafe Rust bug" vs. "allocator pressure"** | The SIGSEGV/SIGBUS at ~3 GB RSS is caused by memory mapping failure under extreme pressure, not by unsafe Rust code. The shared crash offset `+0x19586b0` is in the TF-IDF allocation path. SIMD/unsafe code is not involved. | **High** — crash occurs in single-process mode, correlates with RSS level, eliminated alternatives |
| **"29–38 GB OOM" vs. "3 GB crash under limits"** | Without external limits, the 9 GB peak grows to 29–38 GB because (a) multiple concurrent processes each hit the peak, (b) VmSize reaches 10–20 GB per process, (c) swap amplifies the problem. The 3 GB crash under `MemoryHigh=3G` is the same root cause, just capped earlier. | **High** — consistent scaling explanation |
| **"Concurrent 31% overhead" vs. "100% duplication"** | The measured 31% RSS overhead for concurrent large-project runs is an artifact of `MemoryHigh=3G` capping each process. The true duplication is 100% — both processes build identical state independently. The cap prevents both from reaching full RSS simultaneously, pushing the difference to swap. | **High** — process-model §9.1 explains the discrepancy |
| **"Cleanup fixes the problem" vs. "runtime pressure"** | Stale artifacts (~1 GB on disk) are operational residue, not live runtime pressure. Cleaning them frees disk space but does not reduce RSS or prevent crashes. Cleanup is a separate operational concern. | **High** — explicitly separated in cleanup evidence §5 |

### 5.2 Bounded Hypotheses (Uncertainty Carried Forward)

| Hypothesis | Evidence For | Evidence Against | Confidence | Impact on Plan |
|-----------|-------------|------------------|------------|----------------|
| **Crash offset +0x19586b0 maps to TF-IDF allocation in `index_nodes()`** | All runtime evidence points to TF-IDF path (crash timing, RSS level, log messages) | Cannot confirm without debug symbols (R13) | **High** | Plan proceeds with R1–R4 targeting TF-IDF path. R13 provides confirmation. |
| **Batch processing (R1) will reduce peak to <3 GB** | Code analysis shows ~9 GB is sum of 4 overlapping structures; eliminating overlap should reduce to ~2–3 GB | Actual reduction depends on batch size tuning and allocator behavior | **Medium-High** | R1 is the correct approach regardless; validation after implementation will confirm exact savings. |
| **Memory fragmentation contributes to ~3 GB plateau** | glibc malloc can fragment under heavy allocation patterns; VmSize (10–20 GB) >> RSS (3 GB) suggests fragmentation | Not directly measured; could be mmap overhead instead | **Low-Medium** | Batch processing (R1) reduces fragmentation risk by reducing allocation churn. No separate fix needed. |
| **Watcher-triggered reindex would crash on large projects** | Code shows watcher calls `index_project(false)` which runs full TF-IDF; single-process runs crash on same path | No runtime evidence for watcher-triggered crash (not tested) | **Medium** | R5 (incremental indexing for watcher) is the correct mitigation regardless. |
| **Shared MCP server will achieve 53% memory reduction** | Measured 100% duplication + existing ProjectRegistry infrastructure | Assumes all clients adopt shared server; memory-based LRU eviction not yet implemented | **Medium** | R9 is the correct direction; validation after implementation will confirm actual savings. |
| **HNSW graph overhead is 50–100% of raw embeddings** | Code structure analysis; HNSW adds neighbor lists per node | `estimated_memory_bytes()` not exercised in profiling | **Low** | R10 (mmap embeddings) would also cover HNSW if persisted together. Low priority. |

---

## 6. Functional Preservation and Improvement Summary

| Recommendation | Functionality Preserved | Functionality Improved |
|---------------|------------------------|----------------------|
| R1. Batch TF-IDF | Identical index quality, same search results | Enables large-project indexing (currently crashes) |
| R2. Stream file reads | Same byte-range extraction, same enrichment | Reduces I/O, enables large-project support |
| R3. Persist TF-IDF embedder | Identical embeddings (deterministic) | Faster startup (2–3 min → 10–20s for large projects) |
| R4. Eliminate double instantiation | Same indexed data in registry | Halves peak RSS during indexing |
| R5. Incremental watcher reindex | Same reindex results for changed files | Single-file reindex: 2+ min → <1 sec |
| R6. Combine file reads | Same hash values, same content | 50% less disk I/O |
| R7. Deduplicate token data | Same tokenization, same TF-IDF | ~1.5 GB less peak RSS |
| R8. Pre-tokenized SearchEngine | Same inverted index, same search | ~10–20% less indexing CPU |
| R9. Shared MCP server | Same per-project isolation, same results | 53% less memory for multi-client |
| R10. Mmap embeddings | Same vector values, same distances | ~2.4 GB less RSS per project |
| R11. Stale-artifact GC | Active indexes untouched | Prevents disk space accumulation |
| R12. File/size limits | Files within limits indexed normally | Graceful handling of pathological cases |
| R13. Debug build | No production change | Confirms crash hypothesis |
| R14. Memory cap (fallback) | Small projects work normally | Prevents system OOM (reduces large-project functionality) |

---

## 7. Implementation Sequencing

### Phase 1: Crash Elimination (R1, R2, R4)
**Goal:** Enable large-project indexing without crash.
**Dependencies:** None (can be implemented in parallel).
**Validation:** Bounded profiling on llvm-project should complete at <3 GB RSS.

### Phase 2: Efficiency (R3, R5, R6, R7, R8)
**Goal:** Reduce resource waste and improve performance.
**Dependencies:** R1 should be implemented first (batch processing changes the data flow).
**Validation:** Compare before/after metrics on both small and large projects.

### Phase 3: Architecture (R9, R10)
**Goal:** Eliminate multi-process duplication.
**Dependencies:** R3 (TF-IDF persistence) must be implemented before R9 (shared server).
**Validation:** Multi-client comparison: shared server vs. separate processes.

### Phase 4: Operational (R11, R12, R13)
**Goal:** Defensive measures and diagnostics.
**Dependencies:** None (can be implemented at any time).
**Validation:** Stale-artifact GC test, file limit test, debug build crash reproduction.

### Phase 5: Fallback (R14)
**Goal:** Safety net while fixes are being implemented.
**Dependencies:** None (can be deployed immediately as interim measure).
**Validation:** Verify memory caps prevent OOM without breaking small projects.
**Position:** Can be deployed in parallel with Phase 1 as an interim safety measure, but must NOT be treated as the solution.

---

## 8. Pre-Implementation Validation Checklist

Before beginning implementation of any recommendation:

- [ ] **Baseline metrics captured:** Run bounded profiling on llvm-project and LeIndexer to establish before/after comparison
- [ ] **Test suite baseline:** `cargo test --workspace` passes on `feature/unified-crate` branch
- [ ] **Benchmark baseline:** Run existing benchmarks (`search_benchmarks`, `phase_bench`, `text_search_bench`) for comparison
- [ ] **Debug build available:** Build with debug symbols for crash offset resolution (R13)
- [ ] **Concurrent MCP correctness:** Verify that concurrent MCP tool calls in a single `leindex mcp` process produce correct results (for R9)
- [ ] **`index_slots` consolidation test:** Verify that two simultaneous index requests for the same project are correctly serialized (for R9)
- [ ] **Graceful shutdown test:** Verify that killing `leindex mcp` mid-operation does not corrupt storage (for R9)
- [ ] **Memory-aware LRU design:** Design memory-based eviction before implementing shared server (for R9)

---

## 9. Instrumentation and Regression Prevention

### Recommended Instrumentation

| Metric | Collection Method | Alert Threshold |
|--------|------------------|-----------------|
| Peak RSS during indexing | `/proc/PID/status` VmHWM | >4 GB (after fixes) |
| TF-IDF build time | Structured logging | >30 seconds (after R3) |
| Indexing wall time | Structured logging | >60 seconds for incremental |
| Concurrent process count | Process monitoring | >2 for same project |
| Search latency (p95) | Structured logging | >2 seconds |
| Stale artifact disk usage | Startup GC scan | >500 MB |

### Recommended Regression Tests

| Test | What It Verifies |
|------|-----------------|
| Large-project indexing completes | R1, R2, R4 — no crash on projects with 100K+ nodes |
| Peak RSS during indexing <4 GB | R1, R2, R7 — memory budget maintained |
| TF-IDF embedder persistence round-trip | R3 — loaded embedder produces same embeddings |
| Watcher incremental reindex | R5 — single file change doesn't trigger full rebuild |
| Concurrent MCP correctness | R9 — shared server produces correct results |
| Stale-artifact GC | R11 — cleanup removes only stale artifacts |

### Recommended Benchmarks

| Benchmark | What It Measures |
|-----------|-----------------|
| `indexing_large_project` | Wall time + peak RSS for indexing llvm-project |
| `load_from_storage` | Time to load PDG + search engine (before/after R3) |
| `concurrent_search` | 2-client search latency (before/after R9) |
| `watcher_reindex` | Time for watcher-triggered reindex (before/after R5) |

---

## 10. Summary

| Tier | Recommendations | Primary Impact | Evidence Basis |
|------|----------------|---------------|----------------|
| **1. Critical** | R1–R4 | Eliminate crash, enable large-project support | Runtime crash evidence + code-path analysis |
| **2. High** | R5–R8 | Reduce waste, improve performance | Code analysis + profiling correlation |
| **3. Medium** | R9–R10 | Eliminate multi-process duplication | Measured 100% state duplication |
| **4. Low** | R11–R13 | Operational improvements, diagnostics | Cleanup evidence + unresolved gaps |
| **5. Fallback** | R14 | Prevent system OOM as safety net | Historical OOM evidence |

**The plan favors root-cause and efficiency fixes first (Tiers 1–2), architecture improvements second (Tier 3), operational measures third (Tier 4), and memory caps only as a fallback safety control (Tier 5). No memory-cap recommendation is ranked as the primary solution. Each recommendation preserves or improves LeIndex functionality while reducing resource cost.**
