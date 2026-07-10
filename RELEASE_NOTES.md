# LeIndex 1.8.4 Release Notes

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
