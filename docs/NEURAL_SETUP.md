# Neural Search Setup

LeIndex combines TF-IDF, PDG, and neural signals over the same nodes through
local ONNX or remote embeddings:

- TF-IDF for exact vocabulary and identifier matches
- Qwen3 or remote embeddings for semantic similarity
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
leindex setup --embedding embeddinggemma2 --download-llama cpu --gpu-layers 0
leindex setup --embedding remote --embedding-endpoint http://127.0.0.1:9000/v1 \
  --embedding-model your-served-model
leindex setup --check
```

Setup writes `$LEINDEX_HOME/config/leindex.toml`, normally
`~/.leindex/config/leindex.toml`, then runs an embedding smoke test. A GPU
setup is successful only when the requested provider is active; silent CPU
fallback is reported as a failure.

`--embedding qwen3` selects local ONNX; non-interactive `--neural` also selects
Qwen3. `--embedding keep` retains the existing route. `--embedding embeddinggemma2`
(or the `--remote-gemma` alias) provisions the GGUF and a managed Linux service.
Supply `--llama-server /absolute/path/to/llama-server` (or its directory), or
`--download-llama cpu|vulkan|cuda`; setup never guesses PATH or another tool's runtime.
Official downloads use tag `b11574` and verify the archive against SHA256 metadata
from GitHub's HTTPS release API. Release assets are not claimed immutable.
CPU/Vulkan support x86_64 and ARM64 Linux; CUDA supports x86_64 Linux.

Managed setup binds loopback only. `--gpu-layers 0` selects llama.cpp CPU offload;
this is independent of ONNX `--cpu`/`--gpu`. Driver/SDK dependencies must be
available for the selected backend. Use an authenticated external HTTP(S) server
on other platforms or for nonlocal inference. Remote routes need neither ORT nor
a local Qwen model. `--check` sends a bounded inference request without starting
services; a stopped managed service is not ready, even when `auto_start = true`.
See [Remote EmbeddingGemma2 Preset](#remote-embeddinggemma2-preset) below.

The persisted provider is one of `auto`, `cpu`, `cuda`, `migraphx`, or
`coreml`. `auto` resolves by availability (CoreML -> MIGraphX -> CUDA -> CPU)
and is the local provider default; the interactive wizard selects a route first.
`rocm` is accepted as a
deprecated alias that routes to MIGraphX and never registers `ort::ep::ROCm`.

## Remote EmbeddingGemma2 Preset

Standard builds include `onnx` and `remote-embeddings`; no extra embedding
feature flags are needed. The following is a build command, not an installation
step:

```bash
cargo build --release
```

Fresh configuration defaults enable neural enrichment and select
`embeddinggemma-2`. Explicit `[neural] enabled = false`, `--no-neural` at setup,
or text search mode still disables enrichment. Existing TOML files,
including empty files and files with only part of `[indexing]` or
`[indexing.embeddings]`, retain legacy embedding defaults. `--embedding keep` preserves
the existing route; choosing another route changes it deliberately. To opt in manually, merge these keys into
`$LEINDEX_HOME/config/leindex.toml` (normally `~/.leindex/config/leindex.toml`):

```toml
[neural]
enabled = true

[indexing.embeddings]
enable_hybrid = true
remote_provider = "embeddinggemma-2"
```

Do not duplicate an existing TOML section. `[neural] enabled = true` and the
neural-search runtime feature flag gate enrichment; `search_mode = "text"`
disables it. Selecting the remote preset does not launch or install a server.

The preset fills unset keys with:

| Key | Preset value |
|---|---|
| `remote_endpoint` | `http://127.0.0.1:8080/v1` |
| `remote_model` | `embeddinggemma-2` |
| `remote_dimension` | `768` |
| `remote_doc_template` | `title: none \| text: {text}` |
| `remote_query_template` | `task: code retrieval \| query: {text}` |

The task prefixes follow the
[EmbeddingGemma2 model card](https://huggingface.co/google/embeddinggemma-2)
CodeRetrieval instructions. LeIndex applies document templates to embedding
batches and query templates to single-query calls. Serve raw inputs without
adding a second task prefix. Explicit endpoint, model, dimension, and template
keys override preset values. Changing model or templates changes the vector
space: force a reindex rather than mixing old and new vectors.

### llama-server

Use a llama.cpp build that supports EmbeddingGemma2 and a compatible local GGUF
(for example, the Q8 file from
[unsloth/embeddinggemma-2-GGUF](https://huggingface.co/unsloth/embeddinggemma-2-GGUF)).
After downloading the model and accepting its license as needed:

```bash
llama-server -m /path/to/embeddinggemma-2.gguf \
  --alias embeddinggemma-2 --embedding --pooling mean \
  --host 127.0.0.1 --port 8080 -ngl 99
```

GPU offload depends on the llama.cpp build and available memory; use `-ngl 0`
for CPU-only execution. Model loading errors must be resolved before indexing.

### Managed services (`leindex services`)

On Linux, instead of keeping a terminal open, declare the server as a managed
service and let LeIndex own the process:

```toml
# ~/.leindex/config/leindex.toml
[services.gemma]
binary = "/absolute/path/to/llama-server"
model = "/path/to/embeddinggemma-2.gguf"
alias = "embeddinggemma-2"
host = "127.0.0.1"
port = 8080
extra_args = ["--embedding", "--pooling", "mean", "-ngl", "99"]
auto_start = false

[indexing.embeddings]
enable_hybrid = true
remote_provider = "embeddinggemma-2"
remote_service = "gemma"   # endpoint + model derive from the service
```

`leindex services list` shows state, `start`/`stop`/`restart` manage the
process (logs land in `~/.leindex/logs/services/gemma.log`). With
`auto_start = true`, LeIndex spawns it on demand before the first remote
embedding call. One llama-server serves one model: run a second section (for
example `[services.laya]` on port 8181) for the System-One gate's decision
model — see the `[system_one]` block in `leindex.toml.example`.

### vLLM

Use a vLLM version whose pooling runner supports the model's architecture.
The Hugging Face model is not the GGUF file used by llama.cpp:

```bash
vllm serve google/embeddinggemma-2 \
  --runner pooling --served-model-name embeddinggemma-2 \
  --host 127.0.0.1 --port 8080
```

Runtime/model support is version-dependent. These are launch recipes, not
benchmark results or a claim that both runtimes have passed the bakeoff. If
vLLM rejects the architecture or configuration, retain the actual error and
use a supported runtime; do not substitute another model under the preset
alias. Run only one server on port 8080. To use another port, set
`remote_endpoint` explicitly.

### Readiness and diagnostics

Before indexing, make a real embeddings request:

```bash
curl --fail-with-body http://127.0.0.1:8080/v1/embeddings \
  -H 'Content-Type: application/json' \
  -d '{"model":"embeddinggemma-2","input":["task: code retrieval | query: find cache invalidation"]}'
```

Verify that the response contains a 768-element vector, then run
`leindex index /path/to/project --force`. A healthy `/health` response alone
does not verify embeddings or task support.

CLI and MCP diagnostics expose `embedding_preset` separately from the active
`embedding_model` status. `measured_embedding_dimension` is the vector length
observed in a successful response, **not** the configured `remote_dimension`.
It is `null` until the resident embedder observes a response; a lightweight CLI
process without that embedder also reports it as unknown. Missing live MCP
diagnostics report an unknown preset and a null measured dimension.

### Fallback behavior and local Qwen opt-in

The construction ladder is remote → local Qwen ONNX → TF-IDF/PDG; construction
does not probe the remote server. Node enrichment freezes its document provider
for each neural generation:

- If remote fails before any successful remote rows, the whole generation
  selects local Qwen (1024 dimensions) when compiled; local worker failures
  still leave the lexical/graph core usable.
- Once any remote rows succeed, a later HTTP, count, or dimension failure skips
  the remaining documents through TF-IDF/PDG. It never switches that generation
  to Qwen or retries remote within it: one mmap contains one vector space.
- A new neural generation retries remote with a fresh selection. Checkpoints
  record the selected provider identity, including remote task templates.

Query failures retain TF-IDF/PDG without a Qwen retry. Incremental fragment
sync pins the selected provider identity before embedding, since it may reuse
existing rows; remote failure there skips remaining fragments without Qwen.
Unknown or changed provider identities force re-embedding and exclude old mmap
rows, including same-dimension task-template changes. Real local-worker and
model-server readiness remain runtime requirements; focused regression tests
are not evidence of those capabilities.

To select local Qwen deliberately, run `leindex setup --embedding qwen3 --cpu`
(or use `--gpu amd|nvidia`). Setup clears remote endpoint/service selection.
Use `--embedding keep` instead when you do not want to change the existing route.

The remote master switch does not disable the separately configured local
ONNX path. To disable neural enrichment altogether, use `[neural] enabled =
false` or text search mode.

### Custom OpenAI-compatible endpoint

Other models remain explicit opt-ins. Supply their own endpoint/model and
model-card task templates; do not inherit EmbeddingGemma2 prefixes:

```toml
[indexing.embeddings]
enable_hybrid = true
remote_provider = "custom"
remote_endpoint = "http://127.0.0.1:8081/v1"
remote_model = "your-served-model"
# remote_dimension = 1024  # only if verified for this model
# remote_doc_template = "document instruction: {text}"
# remote_query_template = "query instruction: {text}"
```

With `custom`, omitted templates preserve raw text. An omitted dimension is
measured from the first response; an explicit dimension is validated against
responses. Prefer `remote_api_key_env = "EMBEDDINGS_API_KEY"` to name an environment
variable holding the bearer token; do not commit the token. Use TLS for non-loopback servers.
CLI equivalents are `--embedding-api-key-env EMBEDDINGS_API_KEY`,
`--embedding-dimension 1024`, `--embedding-doc-template 'document instruction: {text}'`
and `--embedding-query-template 'query instruction: {text}'`. Omitted fields on
same-route reruns are retained; explicit empty template or key-variable flags clear them.
Code and query text are sent to the configured endpoint. The preset stays on
loopback; overriding it to a hosted service is an explicit data-sharing choice.

## Configuration and System One

```bash
leindex config path
leindex config show
leindex config get search.search_mode
leindex config set search.search_mode '"hybrid"'
leindex config unset indexing.embeddings.remote_endpoint
leindex config validate
```

`config set` takes a dotted key and TOML value; quote string values as shown.
`show`/`get` redact secrets. Validate configuration after manual changes.

`leindex setup --system-one` opens the terminal wizard for the separate decision
route (managed Laya or external provider). The gate requires a build with
`--features system-one` and `LEINDEX_FEATURE_SYSTEM_ONE=1` in the MCP/server
process. Configuration alone does not enable that runtime gate. Name credentials
through `api_key_env`, never place tokens in examples or shared config.

## Model Provisioning

Models are never included in GitHub Release archives, crates.io, npm, or PyPI
artifacts. `leindex setup` owns model provisioning.

The 1.9.5 profile downloads
[Qwen3 Embedding (LeIndex code fine-tune, int4)](https://huggingface.co/ScooterLacroix/qwen3-embed-0.6b-int4-code) from
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
provider, batch size, and sequence length. The resident embed-worker process (a `leindex --internal-embed-worker` re-exec; named `leindex-embed` in process listings) retains the ONNX session,
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
neural_weight = 0.4

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
