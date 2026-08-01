# LeIndex 1.9.5 Release Notes

Release date: 2026-08-01

LeIndex 1.9.5 merges the embedding worker into the root crate, pins the ONNX
Runtime load-dynamic fix, makes every execution provider runtime-selectable,
and reworks the release pipeline for a single crate that ships two binaries.

## One crate, two binaries

- The `leindex-embed` worker source now lives in `src/embed/`; the root
  `leindex-embed` binary is a thin wrapper around
  `leindex::embed::worker_main::run`.
- The `crates/leindex-embed` subcrate is retired. `cargo install leindex
  --features onnx` installs both the `leindex` and `leindex-embed` binaries
  from the single root crate.
- All integration suites and worker tests run against the root crate; the
  release pipeline carries a retired-subcrate guard.

## ort 2.0.0-rc.13 and runtime-selectable providers

- `ort` is pinned to `2.0.0-rc.13` from crates.io, which contains the
  load-dynamic deadlock fix (upstream commit `17ed727`). The previous
  non-propagating `[patch.crates-io]` git patch is removed.
- The `onnx` feature compiles every execution-provider API (CUDA, MIGraphX,
  ROCm, CoreML) as marker/API features. No provider SDK is linked at build
  time; the worker discovers and registers providers at runtime via
  `load-dynamic`.

## Provider selection and setup

- `auto` resolves via `select_auto_from_availability` (CoreML -> MIGraphX ->
  CUDA -> CPU) before session attach, so `auto` is never passed to ORT.
- EP registration uses `.error_on_failure()` with a preserved GPU-to-CPU
  fallback on terminal provider failure.
- `rocm` is a deprecated alias that routes to MIGraphX and never registers
  `ort::ep::ROCm`.
- Setup gains Auto and CoreML options with host-aware install candidates,
  persists `auto`, and rejects stale PATH workers via a `--version` check.

## Runtime requirements (unchanged)

- ONNX Runtime is still resolved at runtime; the build does not require any
  provider SDK to be installed.
- Core TF-IDF retrieval and PDG relationships are published first and remain
  queryable if a provider is unavailable.
- Neural vectors are part of the default semantic path when the worker is
  healthy.

Cargo, worker, installer, npm, dashboard, pi, PyPI, lockfile, and runtime
version surfaces are aligned at `1.9.5`.

---

# LeIndex 1.9.0 Release Notes

Release date: 2026-07-21

LeIndex 1.9.0 makes the fast path the default: live Git/catalog reads no
longer hydrate a whole project, exact lookups bypass embedding work, and every
semantic result keeps TF-IDF and PDG context at its root. Neural vectors remain
part of the default semantic path. Model startup is observable and awaited by
state; terminal provider failure keeps the complete TF-IDF/PDG core retrieval
available.

## Reliability and latency

- MCP indexing is a registry-owned single-flight job. `leindex.index` returns a
  `job_id` and phase snapshot by default; polling or `wait=true` observes the
  same job, and a disconnected request cannot cancel persistence or publication.
- Request timeouts no longer interrupt correctness-critical tool calls. Health
  snapshots record generation, phase, Git head/tree, indexed/dirty counts,
  age, and the last failed phase in every applicable response's `_meta`.
- Git status uses one porcelain-v2 parse and returns native status before
  resident PDG enrichment. Git inventory honors ignore rules and excludes
  nested repositories/submodules from the root scan.
- Read-only catalog access is bounded and validates canonical paths and file
  hashes before returning exact symbols, file summaries, or grep fallback data.
- Mmap embeddings use an ID-to-row map and snapshot hydration avoids cloning
  the full vector corpus into heap memory.

## Core retrieval quality

- TF-IDF lexical retrieval and PDG relationships are mandatory core layers for
  applicable search, context, deep-analysis, status, and symbol responses.
  Responses report `tfidf_status` and `pdg_status` explicitly.
- Exact identifiers and text use deterministic lexical routing with no query
  neural request. Natural-language searches use the same TF-IDF/PDG nodes and
  add neural scores only after the worker reports ready.
- Rust extraction now preserves nested modules, impl methods (including
  associated functions), enum variants, byte ranges, and qualified names.
  Hybrid node chunks include bounded nearby documentation/review context.
- Request-scoped task context is ephemeral and never pollutes durable search or
  analysis caches. Optional traversal budgets return `partial` metadata rather
  than cancelling an owning operation.

## Distribution

Cargo, worker, installer, npm, dashboard, pi, PyPI, lockfile, and runtime
version surfaces are aligned at `1.9.0`.

---

Release date: 2026-07-10

LeIndex 1.8.4 repairs the local neural search path and removes repeated search
maintenance from unchanged projects. The release keeps LeIndex's original
node-level TF-IDF and PDG design, then adds correct Qwen3 semantic scoring
without turning semantic search into an isolated file/chunk index.

## What Changes In Daily Use

After an index is current, CLI and MCP searches load a persisted search
snapshot instead of refreshing, deduplicating, and rebuilding search state
before every result. Changed files still use incremental indexing. Maintenance
runs when the PDG fingerprint or stored artifacts say it is needed.

Neural inference now runs in a resident local worker on Unix. The loaded model,
GPU allocations, and provider compile cache survive individual CLI processes,
so repeated commands do not pay model startup on every call. The resident
socket worker exits after ten idle minutes; direct pipe workers exit after one.

A hybrid query waits at most 250 ms for its neural vector by default. When the
neural path is cold or unhealthy, TF-IDF and structural results return without
waiting for a multi-second embedding operation. This is a graceful scoring
fallback, not a loss of MCP connectivity or graph functionality.

## Correct Qwen3 Integration

The former fixed-batch export could only process one row and the runtime used
mean pooling, which is not the Qwen3 embedding contract. 1.8.4 uses the
validated single-file dynamic export for
[Qwen3 Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B), downloaded
from Hugging Face via Hugging Face CLI.

The worker now:

- sends `input_ids`, `attention_mask`, and `position_ids`
- uses dynamic batches of up to 32 rows on CPU and CUDA
- uses one stable 8-row batch on MIGraphX, padding only incomplete work and discarding padded outputs
- uses a stable 128-token input shape by default
- selects the final unpadded token and L2-normalizes the vector
- preserves row order through IPC

These changes address both throughput and embedding accuracy. The same graph
works with CPU, NVIDIA CUDA, and AMD MIGraphX.

## GPU And CPU Providers

`leindex setup` installs or validates the provider-specific ONNX Runtime:

| Host | Setup command | Runtime |
|---|---|---|
| CPU-only | `leindex setup --neural --cpu` | `onnxruntime` |
| NVIDIA | `leindex setup --neural --gpu nvidia` | `onnxruntime-gpu` / CUDA |
| AMD ROCm | `leindex setup --neural --gpu amd` | `onnxruntime-migraphx` |

Setup records both the requested and active provider. A GPU smoke test fails
when ORT silently executes on CPU.

MIGraphX uses ORT graph optimization level 3, a stable 8-by-128 input profile,
setup warmup, and a model/version/shape-specific compiled-program cache under
`$LEINDEX_HOME/cache/migraphx`.
The first compile can still take several minutes; it is performed during setup
and reused by the resident worker.

## Model Distribution

No model weights are packaged in GitHub, crates.io, npm, or PyPI artifacts.
Platform bundles contain the main binary, `leindex-embed`, and ONNX Runtime
libraries.

During `leindex setup`, LeIndex finds or installs Hugging Face CLI, downloads
the model, tokenizer, and config into a staging directory, installs them under
`$LEINDEX_HOME/models`, and generates SHA256 checksums. Later setup runs
verify all three files before reuse. Fixed-batch files and incomplete
external-data exports are not selected by the 1.8.4 setup profile.

## Search Storage

The hybrid index remains per PDG node:

- `.leindex/embeddings.bin`: TF-IDF mmap rows
- `.leindex/neural_embeddings.bin`: Qwen3 mmap rows
- `.leindex/search_snapshot.bin`: PDG fingerprint and hydration metadata
- `.leindex/leindex.db`: graph and project metadata

This means semantic search contributes to the same symbol, context, impact,
and traversal operations as TF-IDF and structural scoring. There is no
separate semantic file/chunk sidecar to reconcile with graph results.

## Reliability Fixes

- duplicate node appends deduplicate instead of panicking
- deduplication tracks retained identifiers correctly
- memory-budget CI avoids unrelated model downloads
- worker startup output cannot corrupt MCP stdio
- provider startup and MIGraphX compile messages are retained in worker logs
- unchanged search calls avoid redundant refresh and index maintenance
- model presence checks now require a complete, checksummed profile
- the resident worker accepts simultaneous MCP and CLI connections without one long-lived client blocking the listener
- concurrent processes cannot race while creating or replacing the daemon socket
- transient listener failures no longer terminate the resident worker, and disconnected clients no longer leave blocked reader threads behind
- MIGraphX shape policy follows the active provider selected from `auto`
- setup waits for the worker startup report before checking GPU activation
- corrupt mmap node IDs produce explicit row-level errors during snapshot hydration
- PDG fingerprinting uses bounded fixed-size record digests instead of per-node and per-edge formatted strings

## Upgrade

1. Upgrade LeIndex through the installation surface already in use.
2. Run the provider-specific `leindex setup` command.
3. Let AMD setup finish its first MIGraphX compile and warmup.
4. Run `leindex setup --check` and `leindex diagnostics`.
5. Re-index a project once if it predates `search_snapshot.bin`; later
   unchanged calls will hydrate the snapshot.

Existing TF-IDF indexes remain useful throughout setup and migration. The
first load of an older project may create the new search snapshot, but normal
subsequent searches should not perform maintenance without source changes.
