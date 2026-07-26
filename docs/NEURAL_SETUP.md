# Neural Search Setup

LeIndex 1.9.0 combines TF-IDF, PDG, and neural signals over the same nodes when
the ONNX feature is enabled:

- TF-IDF for exact vocabulary and identifier matches
- Qwen3 embeddings for semantic similarity
- graph structure for callers, callees, dependencies, and symbol importance

Neural scoring is the active semantic companion to the existing node index. It
is not a separate file or chunk sidecar: semantic results combine neural and
TF-IDF signals while retaining the file, symbol, and dependency context used by
LeIndex's navigation and analysis tools.

## Quick Setup

TF-IDF works without neural setup. Run one of these commands to enable the
hybrid path:

```bash
leindex setup
leindex setup --neural --cpu
leindex setup --neural --gpu nvidia
leindex setup --neural --gpu amd
leindex setup --check
```

Setup writes `$LEINDEX_HOME/config/leindex.toml`, normally
`~/.leindex/config/leindex.toml`, then runs an embedding smoke test. A GPU
setup is successful only when the requested provider is active; silent CPU
fallback is reported as a failure.

## Model Provisioning

Models are never included in GitHub Release archives, crates.io, npm, or PyPI
artifacts. `leindex setup` owns model provisioning.

The 1.9.0 profile downloads
[Qwen3 Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) from
Hugging Face via Hugging Face CLI:

- `model.onnx`, installed as `qwen3-embed-0.6b-dynamic.onnx`
- `tokenizer.json`
- `config.json`

The selected export is a single-file ONNX graph with dynamic batch dimensions
and the three inputs required by Qwen3: `input_ids`, `attention_mask`, and
`position_ids`. It does not depend on external weight shards or a
framework-specific runtime.

Setup searches `HF_BIN`, `hf`, and `huggingface-cli`. If none is
available, it installs `huggingface_hub` using the same Python/pip discovery
path used for ONNX Runtime. Downloads are staged before installation. Setup
then writes a SHA256 manifest for the model, tokenizer, and config; later runs
verify all three files before reuse.

The same validated graph runs on CPU, CUDA, and MIGraphX. Provider selection
changes ONNX Runtime and session configuration, not the model weights.

## Provider Paths

### CPU

```bash
leindex setup --neural --cpu
```

Setup installs or validates `onnxruntime`. CPU is the compatibility path for
machines without a supported GPU. TF-IDF remains available if neural inference
is unavailable.

### NVIDIA CUDA

```bash
leindex setup --neural --gpu nvidia
```

Setup installs or validates `onnxruntime-gpu`, starts the worker, and confirms
that CUDA is the active provider. The NVIDIA driver and CUDA dependencies
required by the ONNX Runtime wheel must already be usable on the host.

### AMD ROCm/MIGraphX

```bash
leindex setup --neural --gpu amd
```

Setup installs or validates `onnxruntime-migraphx` and requires MIGraphX to
be active. ROCm must already support the installed GPU.

MIGraphX compilation is shape-specific and can take several minutes on the
first setup. LeIndex uses a stable 128-token shape, ORT graph optimization
level 3, setup warmup, and a persistent cache under
`$LEINDEX_HOME/cache/migraphx`. Normal CLI and MCP requests reuse the compiled
program through the resident worker.

Optional MIGraphX tuning:

```bash
export LEINDEX_MIGRAPHX_FP16=1
export LEINDEX_MIGRAPHX_EXHAUSTIVE_TUNE=1
leindex setup --neural --gpu amd
```

These options trade setup time and numerical behavior for provider-specific
performance. They are opt-in so the default path remains reproducible.

## Runtime Behavior

On Unix, LeIndex connects to a local socket whose identity includes the model,
provider, batch size, and sequence length. The resident `leindex-embed` process retains the ONNX session,
GPU allocations, and compiled cache across short-lived CLI commands and MCP
calls. It exits after ten minutes without a client. Non-Unix and setup smoke
paths use direct worker IPC with a one-minute idle limit.

Set `LEINDEX_EMBED_DAEMON=0` to force direct child-pipe IPC for isolated test
harnesses or process supervisors. Normal CLI and MCP use should leave resident
reuse enabled.

CPU and CUDA indexing use dynamic batches of up to 32 texts by default, with
partial final batches sent at their real size. MIGraphX compiles one input
shape, so it uses a stable batch of 8 and sequence length of 128; incomplete
batches and single queries are padded, then padded outputs are discarded.
Qwen3 output uses last-unpadded-token pooling followed by L2 normalization.

Hybrid indexing and query embedding actively start the configured worker. A
cold worker is awaited through its explicit initializing → ready/failed state,
so healthy `auto`/MIGraphX builds contribute neural vectors to the same result.
If the provider reaches a terminal failure, LeIndex returns the complete
TF-IDF/PDG core result and reports the neural state; no elapsed-time request
timeout cancels model loading or inference.

## Persisted Search State

Project search state lives under `<project>/.leindex/`:

| Artifact | Purpose |
|---|---|
| `embeddings.bin` | memory-mapped TF-IDF vectors |
| `neural_embeddings.bin` | memory-mapped Qwen3 vectors |
| `search_snapshot.bin` | PDG fingerprint and search metadata |
| `leindex.db` | persisted project graph and metadata |

An unchanged project hydrates these artifacts directly. LeIndex does not run
refresh, deduplication, or index maintenance before every search. Changed
files flow through incremental indexing, and a snapshot rebuild occurs only
when the PDG fingerprint or persisted artifacts require it. A project created
before snapshots performs one compatibility rebuild, then writes the snapshot
for later calls.

## Configuration

Example `~/.leindex/config/leindex.toml`:

```toml
[neural]
enabled = true
execution_provider = "migraphx"
model_dir = "/home/user/.leindex/models"
model_name = "qwen3-embed-0.6b-dynamic"
ort_dylib_path = "/path/to/libonnxruntime.so"
ort_version = "1.25.0"

[search]
search_mode = "hybrid"
neural_weight = 0.3

[indexing]
batch_size = 500
max_files = 50000
```

Useful overrides:

| Variable | Meaning | Default |
|---|---|---|
| `LEINDEX_HOME` | config, model, cache, and worker root | `~/.leindex` |
| `HF_BIN` | explicit Hugging Face CLI executable | auto-detected |
| `PIP_BIN` | explicit pip executable | auto-detected |
| `ORT_DYLIB_PATH` | explicit ONNX Runtime library | discovery chain |
| `LEINDEX_ONNX_INFERENCE_BATCH_SIZE` | maximum inference batch | 32 dynamic, 1 legacy |
| `LEINDEX_ONNX_SEQUENCE_LEN` | fixed token shape | 128 |
| `LEINDEX_MIGRAPHX_FP16` | enable MIGraphX FP16 | off |
| `LEINDEX_MIGRAPHX_EXHAUSTIVE_TUNE` | exhaustive MIGraphX tuning | off |

## Diagnostics

```bash
leindex setup --check
leindex diagnostics
```

Check the worker log under `$LEINDEX_HOME/logs` when provider startup fails.
The report distinguishes configured and active providers and includes the ORT
path selected by the runtime discovery chain:

1. `ORT_DYLIB_PATH`
2. configured `ort_dylib_path`
3. `$LEINDEX_HOME/lib`
4. libraries beside the worker bundle
5. Python `site-packages/onnxruntime/capi`
6. system loader locations and bare-name loader fallback

## Troubleshooting

**Hugging Face CLI missing**

Run `python3 -m pip install --upgrade huggingface_hub`, or set `HF_BIN` to
an `hf` or `huggingface-cli` executable. Setup normally performs this
installation automatically.

**Wrong or incomplete model**

Re-run `leindex setup`. Missing manifests, checksum mismatches, fixed-batch
exports, and incomplete external-data graphs are not accepted by the current
profile.

**GPU requested but CPU active**

For NVIDIA, verify the driver/CUDA dependencies and
`onnxruntime-gpu`. For AMD, verify ROCm, the MIGraphX provider library, and
`onnxruntime-migraphx`. `leindex setup --check` reports the resolved ORT
library; the setup smoke test reports provider fallback as an error.

**First AMD request is compiling**

Allow `leindex setup --neural --gpu amd` to finish warmup. Do not delete
`$LEINDEX_HOME/cache/migraphx` between runs. Retrieval remains available
through TF-IDF while the neural path initializes.

**Search returned lexical results only**

This is expected when neural setup is disabled, the worker is unavailable, or
the worker has not reached its ready state. Run `leindex setup --check` and
inspect diagnostics; the TF-IDF/PDG core remains the complete result path.
