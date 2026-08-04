# LeIndex Single-Daemon, Sub-1-GB Resource Architecture

**Date:** 2026-08-04  
**Status:** Approved design  
**Scope:** MCP runtime, project residency, indexing, TF-IDF, PDG, content-hash/fragment embeddings, neural embeddings, reranking, model serving, evaluation, migration, and rollout

## 1. Purpose

LeIndex must support several agent harnesses operating across several projects without multiplying resident engines, embedding runtimes, indexing jobs, CPU pools, or allocator arenas. Requests should appear concurrent because each bounded operation completes or yields quickly, not because every client receives a heavyweight process.

Target runtime:

- One user-scoped `leindexd` daemon.
- One shared `leindex-embed` worker.
- Tiny MCP stdio shims for clients that require stdio transport.
- One globally budgeted scheduler.
- Project contexts backed by immutable, memory-mapped generations.
- Streaming, bounded indexing stages.
- Retrieval quality and embedding accuracy guarded by corpus-specific evaluation.

Target resources, aggregated across daemon, worker, and all shims:

- Steady-state system RAM: at most 1 GiB under representative multi-project use.
- Worst-case full-index system RAM: at most 1 GiB after the redesigned pipeline and selected model profile are production-ready.
- Idle CPU: statistically indistinguishable from zero beyond event handling.
- CPU/GPU work: short, efficient spikes only when requests or changed source require it.
- Index wall time and common-tool latency: no regression from the accepted baseline.
- Retrieval quality: no material regression in aggregate or protected query categories.

The 1 GiB goal is a system design constraint, not a suggestion to add an out-of-memory check around the current architecture.

## 2. Governing Tenets

### 2.1 Anti-cheat charter

The target must not be “met” by transferring, hiding, skipping, or redefining cost. Any implementation using one of the following tactics fails even if its headline RSS number is below 1 GiB:

1. Disabling neural, TF-IDF, PDG, content-hash/fragment, reranking, or other accepted retrieval behavior without evaluation proving equal or better end-to-end quality.
2. Indexing fewer files, nodes, languages, fragments, or source bytes than the baseline workload.
3. Serving stale generations to avoid indexing work.
4. Reducing embedding dimensions, precision, context, prompt information, or candidate count without passing the quality gates.
5. Moving anonymous RAM to swap and reporting RSS only.
6. Moving host RAM to uncontrolled GPU allocations and reporting CPU RSS only.
7. Converting heap allocations into aggressively faulted mmap pages and excluding resident file pages from the total.
8. Moving inference to a remote service and excluding server resources, network latency, privacy impact, or monetary cost.
9. Serializing work so severely that throughput or p95/p99 tool latency regresses.
10. Adding hard caps that make valid repositories fail, silently shed work, or repeatedly restart.
11. Increasing disk churn enough to trade RAM savings for material indexing or query slowdown.
12. Benchmark-specific branches, warmed-only reporting, reduced corpora, excluded failures, or selective metric reporting.
13. Claiming model equivalence from generic public benchmarks without testing LeIndex’s fused retrieval workload.
14. Keeping a legacy heavyweight path enabled by default while measuring only the optimized path.

Resource reports must include:

- RSS, PSS, anonymous RSS, file RSS, and swap for daemon, worker, and shims.
- GPU VRAM and utilization where available.
- CPU-seconds, wall time, thread count, context switches, and idle utilization.
- Mmap artifact sizes and measured resident pages.
- Disk bytes read/written during indexing and query workloads.
- Cold and warm starts.
- Success, failure, cancellation, and recovery paths.

### 2.2 Optimization discipline

At each stage, implementers must pause before editing and answer:

1. What data is essential to the stage’s output?
2. What is the shortest lifetime that data can have?
3. Can an existing persistent artifact, iterator, mmap, database cursor, or compact index replace a heap copy?
4. Can the stage stream or incrementally merge while preserving deterministic output?
5. Can work be deduplicated across nodes, projects, generations, or clients?
6. Can a better algorithm remove work instead of merely limiting concurrency?
7. Does the proposed saving increase latency, I/O, complexity, or quality risk elsewhere?
8. What measurement will disprove the proposal?

Prefer deletion, ownership simplification, streaming algorithms, immutable sharing, zero-copy access, and work avoidance over knobs and cleanup calls. Allocator tuning is containment, not the primary architecture.

### 2.3 Quality-first efficiency

Optimization order:

1. Remove duplicate processes and duplicate work.
2. Shorten allocation lifetimes.
3. Replace project-wide materialization with streaming or bounded external-memory algorithms.
4. Share immutable artifacts.
5. Deduplicate embeddings by content and model identity.
6. Select better data structures and representations.
7. Tune batching, threads, provider arenas, and allocator behavior.
8. Consider quantization or smaller models only behind measured quality gates.

No sacrifice is accepted merely because it is small. Every sacrifice must be measured, documented, and explicitly accepted against the end-to-end retrieval objective.

## 3. Observed Baseline and Root-Cause Evidence

Measurements on the current 1.9.5 binary and this repository established:

- Three live agent harnesses produced three separate `leindex mcp` processes.
- Observed RSS values included approximately 15.6 GiB, 10.0 GiB, and 1.2 GiB concurrently. Earlier snapshots showed approximately 19.6 GiB and 7.7 GiB for the two largest processes.
- The oldest process had approximately 17–19 GiB of swap in sampled `/proc` reports and a peak virtual size above 54 GiB.
- Large-process memory was overwhelmingly private anonymous memory, not resident index files.
- Processes had 20–35 threads. Their maps contained many approximately 128 MiB anonymous regions plus multi-gigabyte heap regions, consistent with high-thread-count glibc arena growth and retained transient allocations.
- No `leindex-embed` process and no ROCm KFD process was live during the largest sampled MCP memory use. Main MCP/index ownership alone can therefore explain the immediate incident.
- The indexed project had roughly 419 files, 26k PDG nodes, 145k PDG edges, and about 401 MiB reported normal index residency.
- Root mutable artifacts were approximately 63 MiB database, 39 MiB neural vectors, 30 MiB TF-IDF vectors, and 10 MiB search snapshot. Artifact scale does not explain 10–20 GiB private anonymous heaps.
- The runtime Qwen3 embedding ONNX file is approximately 1.19 GiB FP16. The enabled Qwen3 reranker artifact is also approximately 1.19 GiB. Both cannot satisfy the final host-memory budget as independently resident FP16 sessions.
- `.leindex/jobs` occupied roughly 2 GiB across many historical jobs. This is a disk-retention defect and evidence of repeated generation work, though not the direct 25 GiB RSS source.

Confirmed architectural multipliers:

1. Stdio MCP lifecycle creates one heavyweight server per harness.
2. Each process owns its own `ProjectRegistry`, `LeIndex`, search engine, Tokio pool, caches, and indexing orchestration.
3. Cross-process project locking prevents simultaneous writes but does not share engines, queues, or memory.
4. `IndexPipelineState` carries source hashes, multiple path collections, parse results, checkpoints, PDG state, admitted IDs, and caches across phases.
5. Lexical indexing performs a document-frequency pass and then a second enriched-content/token/vector pass.
6. Neural enrichment builds all output rows in `Vec<(String, Vec<f32>)>` before installation/persistence.
7. Fragment sync loads persisted fragment embedding rows into a heap `HashMap<String, Vec<f32>>` and accumulates new rows.
8. High transient parallel allocation across many runtime threads allows allocator arenas to retain memory after logical objects drop.

Hypotheses requiring profiler confirmation before implementation claims:

- Exact retained-byte split among enriched content, tokens, parse signatures, PDG/checkpoint serialization, vector staging, fragment rows, and allocator fragmentation.
- Exact provider host-memory behavior for Qwen FP16, quantized Qwen, static/dynamic shapes, and reranker sessions on CPU, CUDA, and MIGraphX.
- Quality contribution and cost of current reranking relative to fused PDG + TF-IDF + dense + fragment retrieval.

The implementation plan must begin with measurements capable of confirming or rejecting these hypotheses.

## 4. Target Runtime Architecture

```text
Agent harness A ─┐
Agent harness B ─┼─> tiny stdio MCP shims ─> user-scoped leindexd
Agent harness C ─┘                              │
                                                ├─ request router
                                                ├─ fair bounded scheduler
                                                ├─ shared project registry
                                                ├─ mmap generation manager
                                                ├─ bounded indexing pipeline
                                                └─ one IPC channel
                                                        │
                                                        v
                                                shared leindex-embed
```

### 4.1 Stdio shim

Responsibilities:

- Parse and emit MCP/JSON-RPC framing.
- Discover compatible daemon endpoint.
- Start daemon under a cross-process startup lock when absent.
- Forward request, cancellation, progress, and response frames.
- Preserve client request IDs and error semantics.
- Reconnect once after daemon restart when safe.
- Reject incompatible protocol versions with actionable instructions.

Non-responsibilities:

- No project registry.
- No SQLite connection.
- No PDG/search index.
- No file watcher.
- No model/runtime.
- No Tokio worker pool sized from host CPUs.

Target: 5–15 MiB RSS per shim. A direct Unix-socket MCP transport may eliminate shims for clients that support it.

Illustrative Rust boundary:

```rust
pub struct DaemonClient {
    socket: UnixStream,
    protocol: ProtocolVersion,
}

impl DaemonClient {
    pub async fn forward(&mut self, frame: ClientFrame) -> io::Result<ServerFrame> {
        write_frame(&mut self.socket, &frame).await?;
        read_frame(&mut self.socket).await
    }
}
```

This is a boundary example, not a mandate to create these exact names if existing protocol types can be reused.

### 4.2 `leindexd`

One daemon runs per OS user and LeIndex protocol-major version. Endpoint identity must prevent cross-user access. Unix permissions should be user-only. Windows uses an equivalent user-scoped named pipe.

Daemon responsibilities:

- Own project registry and generation leases.
- Own request scheduler and global resource budget.
- Coordinate index/watch jobs.
- Serve all tools.
- Own one embedding-worker client.
- Maintain local metrics.
- Exit after configurable global inactivity when no clients, jobs, or worker lease remain.

Daemon startup must not eagerly load any project or model.

### 4.3 Shared project contexts

Project identity uses canonical path plus repository/worktree identity where required. Each context contains:

- Compact generation metadata.
- Read-only artifact mappings.
- Bounded query/result caches.
- Per-project mutation lock.
- Generation lease/reference counts.
- Watch state only while subscribed.

It must not contain a second heap mirror of mmap vectors or serialized snapshots. PDG/search metadata should progressively move to compact mmap/DB-backed forms. During migration, the scheduler admits fewer hot contexts to preserve the global budget.

Immutable generations allow:

- Concurrent reads during indexing.
- Atomic publication.
- Safe old-generation leases until in-flight reads finish.
- Deterministic rollback.
- Shared OS page cache across processes during transitional compatibility.

### 4.4 Shared embedding worker

One worker owns one active embedding runtime profile. It receives requests from the daemon only. It provides:

- Token-aware bounded batching.
- Query/document prompt role.
- Model/tokenizer/config digest reporting.
- Health and measured memory state.
- Explicit provider/thread profile.
- Cancellation between inference batches.
- Crash isolation and restart.

The worker must not spawn per project or per client. Reranking must not silently create a second unbudgeted runtime.

## 5. Scheduling Model

The system simulates low-resource parallelism through fast bounded execution and fair interleaving.

### 5.1 Work classes

- **Interactive read:** read file/symbol, exact text/symbol search, diagnostics.
- **Interactive compute:** semantic search, context, impact, deep analysis.
- **Mutation:** edit preview/apply, project metadata changes.
- **Index chunk:** scan, parse, PDG merge, lexical, neural, persist sub-steps.
- **Maintenance:** compaction, stale-generation cleanup, cache cleanup.

### 5.2 Fairness

Recommended starting policy: deficit round robin by client/project with class weights and aging.

- Interactive work receives short latency preference.
- Index work receives guaranteed quanta and cannot starve.
- Each heavy operation yields after a time, byte, or item quantum.
- Same-project duplicate index requests coalesce by target source generation.
- New changes arriving during an index schedule one follow-up generation rather than restarting current work repeatedly.
- Cancellation is observed at every yield point.

Do not use an unbounded Tokio task per request. Admission occurs before expensive allocations.

Illustrative contract:

```rust
pub struct WorkBudget {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_items: usize,
    pub max_cpu_time: Duration,
}

pub enum Step<T> {
    Yield(T),
    Complete,
}

pub trait BoundedJob {
    fn step(&mut self, budget: WorkBudget) -> Result<Step<JobProgress>>;
    fn estimated_next_bytes(&self) -> usize;
}
```

Existing job/checkpoint types should be adapted where practical; avoid a speculative framework detached from real phases.

### 5.3 Global admission

Before scheduling a heavy step:

1. Read current process/worker memory state.
2. Add conservative next-step estimate.
3. Consider active mmap resident set and provider reserve.
4. Admit, defer, or reduce concurrency/batch shape.
5. Never reduce semantic scope or embedding quality automatically.

A memory cap should prevent overlapping peaks, not convert valid work into errors.

## 6. Streaming Index Architecture

### 6.1 Scan

Current project-wide path/hash collections should be reduced to compact metadata and persisted early.

Algorithm:

1. Walk eligible files lazily using existing ignore rules.
2. Hash each file through a fixed buffer.
3. Record `(path, hash, size, language, mtime advisory data)` in staging storage.
4. Diff via sorted merge or indexed DB lookup.
5. Emit bounded changed/deleted work units.

Do not cache source bodies during scan. Re-reading changed files once during parse is cheaper than retaining hundreds of source allocations across phases, especially under a 1 GiB hard objective.

### 6.2 Parse

- Bound by both file count and aggregate bytes.
- Keep parser concurrency small and measured.
- Read source into a job-owned buffer.
- Parse and extract compact signatures/edges.
- Persist per-file intermediate immediately.
- Drop syntax tree and source before next chunk.
- Route oversized files through a single-file lane.
- Reuse parser instances per worker where safe, but reset retained trees/buffers.

A successful parse checkpoint should reference durable per-file artifacts, not clone all signatures into another phase-wide map.

### 6.3 PDG

Build/update per-file graph fragments, then resolve cross-file relationships from compact symbol indices.

Preferred shape:

- Stable node IDs.
- Compact interned strings or table IDs.
- Per-file node/edge segments.
- Global symbol lookup table.
- External edge resolution in bounded batches.
- Mmap/DB-backed adjacency after publication.

Avoid repeated whole-PDG clone/serialize cycles. Where deterministic global algorithms require broad state, use compact ID arrays and external merge passes rather than rich cloned node objects.

### 6.4 TF-IDF

Preserve scoring semantics while eliminating corpus materialization.

Two-pass external-memory algorithm:

1. Stream admitted enriched documents; update document frequencies.
2. Freeze vocabulary/IDF.
3. Stream documents again; tokenize and write each final row directly to staged vector storage.

Potential improvement: persist compact token-frequency records from pass 1 when benchmarked disk I/O is cheaper than regenerating enriched content. Selection must be measured, not assumed.

Dense TF-IDF rows may remain if required by current scoring, but heap mirrors must not. If sparse scoring produces identical ranking and improves latency/memory, it may replace dense rows after an equivalence test.

### 6.5 Content-hash and fragment embeddings

Content hash is an identity and deduplication mechanism, not an unmeasured semantic substitute.

Cache key:

```text
(model_digest,
 tokenizer_digest,
 prompt_role_and_version,
 pooling_and_normalization,
 output_dimensions,
 content_hash)
```

Workflow:

1. Stream fragments from changed files.
2. Compute content hash.
3. Probe persistent global embedding cache.
4. Queue misses under token/byte budget.
5. Write returned vectors directly to staged row locations.
6. Reference shared cached vector where format permits.
7. Publish fragment manifest/root atomically.
8. Compact unreferenced cache rows asynchronously under maintenance budget.

Do not load all previous fragment vectors into `HashMap<String, Vec<f32>>` to determine misses. Use an indexed metadata table and mmap rows.

### 6.6 Neural enrichment

Replace all-row accumulation with direct staged writes.

Current anti-pattern:

```rust
pub(crate) fn enrich_neural_embeddings(...) -> Vec<(String, Vec<f32>)>
```

Target behavior:

```rust
pub(crate) fn enrich_neural_embeddings<W: NeuralRowWriter>(
    source: &mut dyn Iterator<Item = Result<NeuralInput>>,
    embedder: &HybridEmbedder,
    writer: &mut W,
    budget: BatchBudget,
) -> Result<NeuralStats>;
```

Implementation may use concrete iterators/generics instead of trait objects if faster and simpler. Required property: only one bounded input batch and output batch exist on heap; final rows go directly to staged storage.

Batch limits must include:

- Text count.
- UTF-8 bytes.
- Estimated tokens.
- Maximum sequence length.
- Output vector bytes.
- Provider profile.

Count-only `batch_size = 500` is insufficient because 500 tiny symbols and 500 long enriched documents have radically different memory/compute shapes.

### 6.7 Publication

1. Flush staged tables/vector mappings.
2. Validate row counts, dimensions, model identity, hashes, and graph/search fingerprints.
3. Sync required metadata.
4. Atomically publish generation pointer.
5. Release job-owned transient state.
6. Purge allocator arenas where supported and measured useful.
7. Delete superseded jobs/generations only after leases expire.

Avoid building whole artifact byte vectors before writes. Use streaming serializers or fixed-layout writers.

## 7. Memory Budget Ledger

Initial design budget, subject to baseline validation:

| Component | Steady target | Index-peak target | Notes |
|---|---:|---:|---|
| MCP shims, three clients | 45 MiB | 45 MiB | 15 MiB each ceiling |
| `leindexd` base/runtime | 100 MiB | 120 MiB | 2–4 runtime workers |
| Project metadata, two projects | 150 MiB | 150 MiB | mmap-first; bounded caches |
| Resident mmap working set | 150 MiB | 150 MiB | measured, not artifact-size fiction |
| Index transient buffers | 0–25 MiB | 200 MiB | one heavy chunk globally |
| Embed worker host memory | 350 MiB | 350 MiB | requires quantized/compact validated profile |
| Safety/unclassified reserve | 229 MiB | 9 MiB | peak budget must be refined |
| **Total** | **≤1,024 MiB** | **≤1,024 MiB** | aggregate target |

This ledger exposes a real constraint: FP16 Qwen3 and FP16 reranker sessions do not fit the final budget. Architecture work proceeds independently, but production model profile must use validated quantization, a validated compact model, removal/replacement of reranking after quality proof, provider-side weight residency outside host RAM with fully counted GPU cost, or an explicitly revised target. It is forbidden to pretend the conflict does not exist.

The peak ledger has little reserve. Each workstream must replace estimates with measured values and recover reserve through algorithmic reductions.

## 8. CPU, GPU, Thread, and Allocator Policy

### 8.1 Threads

- Daemon Tokio workers: start at 2; benchmark 2–4.
- Blocking/parser pool: explicit small bound.
- Embedding intra-op/inter-op threads: explicit provider profile.
- No host-CPU-count defaults.
- Disable runtime/provider busy spinning unless benchmark proves latency benefit within idle-CPU gate.

### 8.2 Allocator

Immediate containment for glibc deployments:

- Measure `MALLOC_ARENA_MAX=2` and potentially `MALLOC_TRIM_THRESHOLD_`.
- Consider jemalloc/mimalloc only after allocator traces prove net benefit across supported platforms.
- Invoke platform trim after large job teardown only if measured latency cost is acceptable.

Allocator changes do not excuse excessive object lifetimes.

### 8.3 GPU

- GPU work occurs only for admitted neural/rerank batches.
- Worker remains idle without polling/spinning.
- Report VRAM allocations and provider compile caches.
- Cache compiled provider artifacts where safe.
- Prefer static-shape profiles when representative sequence/batch coverage and latency beat dynamic shape.
- Avoid padding mixed batches to pathological longest input; bucket by token length under fairness limits.
- CPU fallback is functional and separately benchmarked.

## 9. Model and Reranker Evaluation

### 9.1 Candidates

Embedding baseline and candidates:

1. Qwen3-Embedding-0.6B FP16 baseline.
2. Qwen3-Embedding-0.6B INT8.
3. Qwen3-Embedding-0.6B Q4 where runtime/provider support is correct.
4. EmbeddingGemma 300M.
5. CodeRankEmbed 137M.
6. Jina embeddings v2 base-code 137M.
7. Existing SFR code 400M artifact.

Evaluate reranking independently:

- Current Qwen3 reranker baseline.
- Compact cross-encoder candidates supported by deployment targets.
- No-reranker fused retrieval.
- Conditional reranking only on ambiguous score margins, if quality-equivalent and deterministic.

Public MTEB/CodeSearchNet numbers shortlist candidates; they do not select production defaults.

### 9.2 LeIndex corpus

Include labeled cases for:

- Natural-language intent to symbol.
- Exact and partial identifier.
- Concept to implementation.
- Error/log string to origin.
- Caller/callee/data-flow retrieval.
- Interface to implementation.
- Configuration/docs to code.
- Similar algorithms with different names.
- Same names with different behavior.
- Changed/deleted files and freshness.
- Large files and generated-looking distractors.
- Rust, TypeScript/JavaScript, Python, Go, Java, C/C++, and other supported languages.
- Cross-language conceptual queries.
- Hard negatives sharing syntax, names, or comments.

Use repository-held labels and fixed splits. Add real anonymized query patterns where privacy permits.

### 9.3 Metrics

Quality:

- Recall@1, @5, @10.
- MRR@10.
- nDCG@10.
- Relevant-file and relevant-symbol recall.
- Per-category metrics and confidence intervals.
- Fused-ablation metrics for TF-IDF, PDG, dense, fragments/hash reuse, and reranker.

Performance:

- Index wall time and CPU-seconds.
- Nodes/source MiB per second.
- Search p50/p95/p99.
- Cold/warm model load.
- Batch throughput across token-length distributions.
- Aggregate memory ledger.
- GPU utilization/energy where measurable.

### 9.4 Acceptance

Before running candidate tests, record allowed statistical noise bands. Suggested starting gate:

- No statistically meaningful aggregate MRR@10 or Recall@10 regression.
- No protected category regression greater than 1 percentage point without explicit review.
- No new zero-result or stale-result cases.
- Search p95 no worse than baseline.
- Index wall time no worse than baseline.
- Resource target met under identical corpus/hardware/conditions.

Exact gates should use baseline variance from repeated runs. Do not pick tolerances after seeing candidate outcomes.

Likely evaluation order:

1. Quantized Qwen3, because it preserves architecture/training family.
2. EmbeddingGemma, because it offers a strong compact/on-device profile and reported code capability.
3. CodeRankEmbed/Jina, because code specialization may outperform general models inside fused LeIndex retrieval.
4. Existing SFR model, because artifact/integration cost may be lowest.

Winner is end-to-end fused retrieval under target resources, not standalone embedding score.

## 10. Persistence and Cache Design

### 10.1 Global embedding cache

A user-level content-addressed cache can deduplicate identical source across projects/worktrees. It must include full model/config identity and maintain project-generation references.

Requirements:

- Transactional metadata.
- Fixed-layout or mmap vector rows.
- Corruption detection.
- Byte-budgeted compaction.
- Privacy remains local.
- No source text required after hashing unless explicitly needed for debugging.
- Model upgrade creates new namespace; no accidental mixed vectors.

### 10.2 Generation retention

- Keep current generation.
- Keep previous known-good generation for rollback.
- Keep leased generations until readers finish.
- Remove abandoned staging/jobs after verified timeout and lock ownership checks.
- Bound retained job bytes, not just job count.
- Surface cleanup stats in diagnostics.

### 10.3 Cache policy

Every cache needs:

- Byte accounting.
- Maximum bytes.
- Entry-size rejection.
- Eviction policy.
- Hit/miss/eviction telemetry.
- Generation/model invalidation key.

Count-only cache limits are prohibited where entry sizes vary materially.

## 11. Failure and Recovery Semantics

### 11.1 Daemon failure

- Clients reconnect/restart daemon.
- Last published generation remains valid.
- Incomplete staging remains unpublished.
- Startup verifies and removes only abandoned staging with dead ownership.

### 11.2 Worker failure

- Current inference batch fails clearly or retries once when idempotent.
- Worker restarts under rate limit/backoff.
- Index checkpoint resumes at batch boundary.
- Search may use already-published lexical/PDG generation according to existing explicit fallback semantics, but diagnostics must report neural availability.
- Failure must not spawn multiple workers racing to recover.

### 11.3 Memory pressure

- Stop admitting new heavy chunks.
- Evict idle project heap caches.
- Release unleased generation mappings.
- Reduce concurrent parser/inference work within validated profiles.
- Continue interactive reads where budget permits.
- Never silently omit nodes, shrink embeddings, or serve partial publication.

### 11.4 Cancellation

- Job-owned transient buffers drop at bounded yield point.
- Durable checkpoint identifies last complete unit.
- Publication cannot be cancelled halfway through atomic commit.
- Coalesced clients detach independently; underlying job cancels only when no requester/watch target needs it.

## 12. Compatibility and Migration

### 12.1 Protocol

Introduce daemon protocol version separate from MCP schema version. Handshake includes:

- LeIndex version.
- Daemon protocol version.
- Artifact format version.
- Worker protocol version.
- Supported capabilities.

### 12.2 Artifact migration

- New artifact formats use explicit magic/version/checksum.
- Read old generation where feasible.
- Build new generation beside old.
- Switch only after full validation.
- Preserve rollback generation.
- Model/vector identity mismatch forces rebuild, never silent reuse.

### 12.3 Rollout phases

1. Measurement-only baseline.
2. Immediate safe containment defaults.
3. Daemon opt-in with legacy artifacts.
4. Shared project registry and scheduler.
5. Streaming phase conversion one stage at a time.
6. Shared worker/global embedding cache.
7. Model-profile bake-off.
8. Daemon default-on.
9. Legacy heavyweight mode fallback period.
10. Legacy removal after reliability/resource gates pass.

Each phase must preserve an independently runnable rollback point.

## 13. Verification Matrix

Required scenarios:

1. Three harnesses, two projects, mixed tools.
2. Same-project simultaneous index requests.
3. Different-project simultaneous index requests.
4. Searches during indexing.
5. Repeated identical index requests.
6. Changes arriving mid-index.
7. Worker cold and warm starts.
8. Worker crash during batch.
9. Daemon crash during every publication phase.
10. Cancellation during every index phase.
11. Huge source file.
12. Large repository.
13. 100 repeated reindexes; verify no monotonic RSS growth.
14. One-hour and 24-hour idle tests.
15. CPU-only provider.
16. MIGraphX provider.
17. CUDA provider where CI/hardware exists.
18. Memory-pressure/cgroup test.
19. Corrupt/missing DB, mmap, checkpoint, and model artifacts.
20. Model/tokenizer/config migration.
21. Old shim against new daemon and new shim against old daemon.
22. Multiple worktrees sharing substantial content.
23. Query cancellation/client disconnect storms.
24. Filesystem watcher event storms.

For every scenario capture correctness, latency, CPU, memory, thread, swap, GPU, and disk-I/O evidence.

## 14. Pre-Implementation Baseline Protocol

Do not optimize from one `ps` snapshot. Build a repeatable harness that:

1. Starts from cold daemon/worker state.
2. Records hardware, kernel, allocator, provider, model digests, config, git revision, and corpus hash.
3. Runs full index three or more times where variance requires.
4. Runs incremental no-op, one-file, burst, and delete workloads.
5. Runs fixed query suite cold and warm.
6. Runs three-client/two-project contention workload.
7. Samples `/proc` or platform equivalents by process tree.
8. Samples GPU state.
9. Captures heap/allocation profiles at phase boundaries.
10. Produces machine-readable JSON plus concise report.

Required phase markers:

- daemon start
- project open
- scan start/end
- parse chunk boundaries/end
- PDG merge/resolution/end
- TF-IDF pass 1/pass 2/end
- fragment/hash sync
- neural batches/end
- rerank model load/query
- publication
- teardown/idle

The harness must count descendant processes and survive worker restarts.

## 15. Workstreams and Agent Execution Contract

Detailed implementation plans should split work into these ordered workstreams:

1. Baseline profiler and stress harness.
2. Safe containment: thread/arena/batch/idle defaults.
3. User-scoped daemon and stdio shim.
4. Shared project registry and generation leases.
5. Fair bounded scheduler and admission controller.
6. Streaming scan and parse.
7. Incremental/compact PDG persistence and access.
8. Streaming TF-IDF and mmap row writing.
9. Streaming content-hash/fragment/neural enrichment.
10. Shared worker and global content-addressed embedding cache.
11. Model/reranker conversion and bake-off.
12. Migration, compatibility, cleanup, and rollback.
13. Multi-client soak, fault injection, and default-on rollout.

Every local-agent guide must contain:

- Outcome and why it matters.
- Non-negotiable invariants, including anti-cheat rules.
- Prerequisite workstreams and artifacts.
- Exact code paths/symbols to inspect first.
- Current behavior and intended ownership change.
- Minimal architecture boundary.
- Verbatim implementation skeletons where they reduce ambiguity.
- Tests to write before behavior changes.
- Benchmark commands and expected evidence files.
- Explicit pass/fail criteria.
- Failure/rollback behavior.
- Common shortcuts that are forbidden.
- Files likely touched, with instruction to verify rather than blindly edit.
- Completion checklist.
- Handoff summary format for the next agent.

Agent prompt preamble:

```text
Do not optimize by disabling, skipping, shrinking, staling, offloading, or
hiding work. Preserve accepted retrieval quality and throughput. Measure
aggregate daemon + worker + shim resources, including swap and GPU memory.
Before editing, trace current ownership and capture baseline evidence. Prefer
removing duplicate work, streaming state, immutable sharing, compact data, and
better algorithms over adding limits. At each stage, pause and compare at
least two viable approaches. Choose the smallest architecture that can satisfy
all invariants. If a target conflicts with model physics or measured quality,
report the conflict with evidence; never manufacture a passing metric.
```

Agent handoff format:

```text
Workstream:
Revision:
Invariant status:
Files changed:
Tests run/results:
Benchmark artifact paths:
Before/after resource table:
Before/after quality table:
Unverified assumptions:
Known risks:
Rollback procedure:
Next workstream prerequisites:
```

## 16. Full Acceptance Gates

Architecture is complete only when all gates pass:

### Resource

- Aggregate steady RAM at or below 1 GiB in three-client/two-project workload.
- Aggregate full-index peak RAM at or below 1 GiB in final production model profile.
- Zero monotonic RSS/swap growth across 100 reindexes.
- Idle CPU near zero over long soak.
- Thread counts remain within explicit budgets.
- GPU memory/utilization reported and within approved profile.

### Performance

- Common tool p50/p95/p99 no worse than accepted baseline.
- Search remains responsive during indexing.
- Full and incremental index wall time no worse than accepted baseline.
- CPU-seconds per indexed MiB/node improve materially.
- No pathological cold-start loop or model reload churn.

### Quality

- Aggregate retrieval metrics meet predeclared equivalence gates.
- Protected categories meet per-category gates.
- No stale, omitted, or partial index behavior.
- Model/prompt/pooling/normalization identity is reproducible.
- Reranker removal/replacement, quantization, truncation, and smaller models each pass independent ablation.

### Reliability

- Crash/cancel tests preserve last valid generation.
- No duplicate daemon/worker under startup races.
- No cross-project context confusion.
- Protocol/artifact mismatches fail safely.
- Cleanup never removes leased/current/rollback generations.

### Repository quality

Run the full required suite:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All warnings/errors must be investigated and fixed under repository policy.

## 17. Out-of-Scope Until Measured

- Distributed/multi-machine daemon.
- Remote embedding service as default.
- New vector database dependency.
- Custom allocator as architectural foundation.
- Fine-tuning a new model before existing candidates are benchmarked.
- Approximate graph/vector algorithms that alter ranking without quality evidence.
- Multiple simultaneously resident model workers.

These may become justified only when measured evidence shows the simpler approved architecture cannot satisfy a gate.

## 18. Decision Summary

Approved direction:

- Build one shared, user-scoped daemon from the whole-system view.
- Keep clients tiny.
- Share one embedding worker.
- Schedule bounded sequential work fast enough to serve several harnesses responsively.
- Make project generations immutable and mmap-first.
- Stream every indexing layer and constrain allocation by bytes/tokens, not counts alone.
- Deduplicate neural work globally by complete model/content identity.
- Keep Qwen3 FP16 as quality baseline, not an unquestioned production residency choice.
- Select quantized Qwen or a compact code-capable model only through LeIndex-specific evaluation.
- Evaluate whether reranking earns its second-model cost.
- Enforce aggregate resource, throughput, freshness, and quality gates together.
- Reject every apparent win that cheats by moving or hiding cost.

This architecture aims to remove bloat rather than ration useful behavior. Its success criterion is not merely lower memory; it is a smaller, faster, calmer system that returns equally good or better results under real multi-agent use.
