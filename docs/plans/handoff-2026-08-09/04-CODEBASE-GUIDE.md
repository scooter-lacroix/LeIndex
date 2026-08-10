# Codebase Guide — Architecture and Key Files

**As of:** 2026-08-09

---

## What LeIndex is

LeIndex is a local code indexing and semantic search tool. It parses source files into a Program Dependence Graph (PDG), computes TF-IDF and neural embeddings, stores them in SQLite + content-addressed files, and serves hybrid (lexical + semantic) search queries via CLI or MCP/HTTP.

## High-level data flow

```
Source files
  → parse (tree-sitter) → AST → PDG (nodes + edges)
  → TF-IDF vectorization (lexical)
  → Neural embedding (ONNX worker process via IPC)
  → Storage (SQLite + CAS generation files)
  → Search (hybrid scorer: TF-IDF + neural + optional reranker)
```

## Process architecture

LeIndex runs as two cooperating processes:

1. **Main daemon** (`leindex`): CLI commands, indexing pipeline, search engine, MCP/HTTP server.
2. **Embed worker** (`leindex-embed`): Separate process that loads the ONNX model and runs inference. Communicates via length-prefixed bincode frames over stdin/stdout (pipe mode) or Unix socket (daemon mode).

The main daemon spawns the worker on demand via `EmbeddingClient` in `src/search/onnx/client.rs`.

---

## Key source files

### Embedding worker (the process that loads ONNX)

| File | Role |
|------|------|
| `src/embed/runtime.rs` | `WorkerRuntime` — holds the ONNX session, tokenizer, cache, cancellation registry. Contains `handle_embed`, `run_onnx_embed`, `run_onnx_embed_text_batch_loop`, `run_onnx_embed_sub_batch[_inner]`, `build_session`, `probe_migraphx_compile_timeout`. ~2700 lines. |
| `src/embed/worker_main.rs` | Worker process entry point. Pipe mode (`run_loop` over stdin/stdout) and socket mode (`run_socket_accept_loop`). Spawns client handler threads (up to 16). PR_SET_PDEATHSIG guard. |
| `src/embed/protocol.rs` | Wire protocol: `Frame`, `EmbedRequest`, `EmbedResponse`, `BatchId`, `MsgType`, `WorkerError`, `ErrorKind`. `EmbedResponse::try_new()` validates output length. |
| `src/embed/runtime_env.rs` | Environment-driven config: `configured_onnx_inference_batch_size`, `configured_onnx_sequence_len`, `build_position_ids`, memory probes. |
| `src/embed/ort_discovery.rs` | ORT dynamic library discovery chain: env → config → `~/.leindex/lib` → sibling → pip → system → bare-loader. Includes `resolve_config_ort_path` (stale path sibling search) and `pick_best_ort_lib` (version-aware selection). |
| `src/embed/provider.rs` | Execution provider selection: CPU/CUDA/MIGraphX/ROCm availability checks and fallback logic. |
| `src/embed/model_path.rs` | Model file resolution: env → bundled → `~/.leindex/models`. |
| `src/embed/cache/store.rs` | Global embedding cache: content-addressed vector rows keyed by `CacheKey` (model digest + tokenizer digest + content hash + pooling + normalization + dim). Probe/put/GC. |
| `src/embed/batch.rs` | Legacy batch splitting (`split_request`, `stitch_responses`). |
| `src/embed/batching.rs` | `BatchBudget` — token/byte/count ceilings for batch shaping. |

### Embedding client (the daemon side that talks to the worker)

| File | Role |
|------|------|
| `src/search/onnx/client.rs` | `EmbeddingClient` — spawns/reuses worker process, sends Embed/Rerank/Health/CacheProbe frames, reads responses. `embed_with_fallback` is the main entry. Frame sharding (16 MiB budget) in `embed_attempt`. `configure_worker_command` sets up the worker process env. |
| `src/search/onnx/client_config.rs` | `WorkerConfigEnv`, `ClientError`, frame-size constants, config-reading helpers. |

### Indexing pipeline

| File | Role |
|------|------|
| `src/cli/index_builder/mod.rs` | Index building orchestration: parse → PDG → TF-IDF → neural embedding → storage. `embed_pending_neural_batch` with 64 KiB text cap and borrowed `&str` dedup. |
| `src/cli/index_builder/hybrid.rs` | `HybridEmbedder` — wraps the embedding client, generic over `AsRef<str>`. |

### Storage

| File | Role |
|------|------|
| `src/storage/schema.rs` | SQLite schema: tables, indexes, migrations. v3→v4 migration dedupes `intel_nodes`. `SCHEMA_VERSION = 4`. |
| `src/storage/pdg_store.rs` | `save_pdg` / `load_pdg` — incremental upsert with `ON CONFLICT(project_id, node_id)`, unchanged-row skip via `node_content_hash`, stale-node deletion. |
| `src/storage/generation/` | Content-addressed generation storage: CAS blobs, mmap readers, generation leases, manifest. |

### Configuration

| File | Role |
|------|------|
| `src/config.rs` | `LeIndexConfig` — TOML config at `~/.leindex/config/leindex.toml`. `[neural]` section: `ort_dylib_path`, `execution_provider`, `model_name`, etc. |
| `src/feature_flags.rs` | Runtime feature flags (`LEINDEX_FEATURE_*` env vars). Controls neural search, streaming pipeline, global embed cache, etc. |

### Binaries

| File | Role |
|------|------|
| `src/bin/leindex.rs` | Main CLI binary. |
| `src/bin/leindex-embed.rs` | Worker binary — thin wrapper around `leindex::embed::worker_main::run()`. |
| `src/bin/leindexd.rs` | Daemon binary (MCP server over Unix socket). |

---

## Critical data structures

### `WorkerRuntime` (`src/embed/runtime.rs`)

```
WorkerRuntime {
    config: RuntimeConfig,
    last_activity: Arc<Mutex<Instant>>,
    shutdown_flag: Arc<AtomicBool>,
    active_embed_cancels: Arc<Mutex<HashMap<BatchId, Arc<AtomicBool>>>>,  // R2: batch-scoped
    cache: Option<Arc<Mutex<GlobalEmbeddingCache>>>,
    session: Option<Arc<Mutex<Session>>>,           // ONNX embedder
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,
    provider_runtime_status: ProviderRuntimeStatus,
    rerank_session: Arc<Mutex<Option<Arc<Mutex<Session>>>>>,  // lazy reranker
    rerank_tokenizer: Arc<Mutex<Option<Arc<tokenizers::Tokenizer>>>>,
    rerank_init_lock: Arc<Mutex<()>>,
}
```

**Important:** `WorkerRuntime` is `#[derive(Clone)]`. In socket mode, each client-handler thread gets a clone. All `Arc` fields are shared. This is why the cancellation flag had to become batch-scoped — a single global flag would be reset by any new embed request across any handler thread.

### Embed request flow (direct path, no cache)

```
dispatch() → register_embed_cancel(batch_id) → handle_embed(frame, &cancel_token)
  → run_onnx_embed(session, tokenizer, texts, dim, &cancel_token)
    → run_onnx_embed_text_batch_loop(texts, batch_size, fixed, dim, &cancel_token, tokenize_fn, infer_fn)
      for each text chunk:
        → check cancel_token
        → tokenize(sub_texts)
        → validate encoding count
        → if fixed_batch && partial: pad encodings, run, trim
        → else: run sub_batch directly
        → validate output length (exact match)
      → validate aggregate output length
    → EmbedResponse::try_new(all_pooled, count, dim)
```

### Embed request flow (cache path)

```
dispatch() → register_embed_cancel(batch_id) → handle_embed(frame, &cancel_token)
  → handle_embed_with_cache(texts, cache_keys, dim, &cancel_token)
    → allocate flat Vec<f32> output + Vec<bool> filled
    → probe cache (hold lock, then release)
    → copy hits into output rows, validate dimensions
    → collect miss indices
    → borrow miss texts as Vec<&str>
    → embed_texts(miss_texts, dim, &cancel_token)
    → validate miss output length
    → copy miss results into output rows
    → write misses back to cache (re-acquire lock)
    → EmbedResponse::try_new(vectors, n, dim)
```

---

## ORT loading model

LeIndex uses the `ort` crate (v2.0.0-rc.13) with `load-dynamic` feature. This means:

- The ORT shared library (`libonnxruntime.so`) is **not linked at compile time**.
- It must be explicitly loaded at runtime via `ort::init_from(path)` before any `Session::builder()` call.
- The discovery chain in `src/embed/ort_discovery.rs` locates the library.
- `ORT_DYLIB_PATH` env var is the highest-priority override.
- The config `ort_dylib_path` is a hint that the worker resolves (including sibling-version fallback for stale paths).
- The daemon launcher (`configure_worker_command`) no longer promotes config paths to `ORT_DYLIB_PATH` — it lets the worker's discovery chain handle resolution.

This is different from Python's `onnxruntime` package, which bundles and auto-loads its `.so` via the import system.
