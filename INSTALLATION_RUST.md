# LeIndex Rust Installation Guide

This guide covers crates.io and source builds for LeIndex 2.0. For release
bundles, npm, and PyPI, see [INSTALLATION.md](INSTALLATION.md).

## Requirements

- current stable Rust toolchain
- a C/C++ build toolchain for native dependencies
- Python 3 with pip when neural search is enabled
- sufficient disk space for Rust build output and the separately downloaded
  Qwen3 model

Supported release targets are Linux x86_64/aarch64, macOS x86_64/arm64, and
Windows x86_64.

Ubuntu/Debian build prerequisites:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev python3 python3-pip
```

macOS requires Xcode Command Line Tools:

```bash
xcode-select --install
```

Windows source builds require Rust's MSVC toolchain and Microsoft C++ Build
Tools.

## crates.io

```bash
cargo install leindex
leindex --version
leindex setup
```

Cargo installs `leindex` and `leindex-embed` under `$CARGO_HOME/bin`,
normally `~/.cargo/bin`. The build uses ONNX Runtime dynamic loading, so ORT
does not need to be installed while Cargo compiles LeIndex.

## Build From Source

```bash
git clone https://github.com/scooter-lacroix/LeIndex.git
cd LeIndex
cargo build --release --bins
./target/release/leindex --version
./target/release/leindex setup
```

Outputs:

- `target/release/leindex`
- `target/release/leindex-embed`

Use `cargo install --path . --locked` to install the checked-out source into
Cargo's binary directory. Add `--features system-one` to include the
experimental System-One gate.

## Neural Setup

The Rust crate does not package model files. Setup selects the embedding
route — local Qwen3 ONNX, a managed EmbeddingGemma2 GGUF service on Linux, or
an external OpenAI-compatible server — then writes
`~/.leindex/config/leindex.toml`.

```bash
leindex setup --embedding qwen3 --cpu
leindex setup --embedding qwen3 --gpu nvidia
leindex setup --embedding qwen3 --gpu amd
# Linux managed service: explicit compatible runtime, or verified official download
leindex setup --embedding embeddinggemma2 --download-llama cpu --gpu-layers 0
# Alternative: --llama-server /absolute/path/to/llama-server
leindex setup --embedding remote --embedding-endpoint http://127.0.0.1:9000/v1 \
  --embedding-model your-served-model
leindex setup --check
```

The Qwen3 ONNX provider packages are `onnxruntime`, `onnxruntime-gpu`, and
`onnxruntime-migraphx`, respectively. CUDA or ROCm/MIGraphX system
dependencies remain the responsibility of the host. Setup validates the active
provider and rejects silent GPU-to-CPU fallback.

Managed Gemma services are Linux-only and bind loopback only. Official
llama.cpp downloads pin tag `b11574` and verify the archive against SHA256
metadata from GitHub's HTTPS release API before extraction. Remote routes do
not require ONNX Runtime. `--check` makes a bounded readiness request without
starting services; a stopped managed service is reported as not ready.
Manage the llama-server processes with `leindex services
list|start|stop`; with `auto_start = true` in `[services.<name>]`, any CLI,
daemon, or MCP call needing the service starts it on demand. One llama-server
serves one model, so an embeddings service (port 8080 in the embeddinggemma-2
preset) and a System One decision-model service (port 8181 for `laya`) use
different ports. On multi-GPU hosts pin the device explicitly, e.g.
`extra_args = ["-ngl", "99", "--device", "Vulkan1"]`, because auto device
selection can pick an integrated GPU. Changing the embedding model or its
prompt templates changes the vector space — re-index afterwards.

### System One (experimental)

The optional System-One decision gate is **experimental** and ships only in
feature builds:

```bash
cargo install leindex --features system-one
leindex setup --system-one   # interactive wizard; writes [system_one] + [services.laya]
```

Then arm the runtime flag in every process that serves MCP or runs the CLI:
`export LEINDEX_FEATURE_SYSTEM_ONE=1` (or an `env` block in your MCP client).
Configuring `[system_one]` alone never enables the gate. `leindex setup
--check` on a feature build reports the gate's build, flag, and endpoint
status, and its probe sends one real decision request (bounded by
`system_one.timeout_ms`). `--check` never starts the managed service: start
it beforehand or the probe fails, even with `auto_start = true`. The gate
fails open: any provider problem leaves calls and results untouched.
`laya`/`jev` providers run as managed services and need no API key; the
remote `openai` provider reads a key from the environment variable *name*
given in `api_key_env`.

The model is stored under `$LEINDEX_HOME/models`, normally
`~/.leindex/models`; it is not written into the Cargo registry or build
directory.

See [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md) for provider tuning and
runtime details.

## Verification

```bash
leindex --version
leindex setup --check
leindex config validate
leindex services list
leindex diagnostics
leindex index /path/to/project
leindex search "request authentication" --project /path/to/project
```

TF-IDF search remains usable before neural setup and whenever the neural worker
is unavailable. After indexing, unchanged CLI and MCP calls hydrate the
persisted snapshot rather than repeating index maintenance.

## PATH

If Cargo's binary directory is not already available:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Add that line to the shell profile used for LeIndex and any MCP client launched
from the shell.

## Troubleshooting

**Cargo cannot compile native dependencies**

Update Rust and install the platform build tools listed above. Avoid using
`cargo clean` unless stale build output is the suspected cause because it
forces a full rebuild.

**`leindex-embed` is missing**

Build or install all binaries with `cargo build --release --bins` or
`cargo install leindex --force`.

**Release link step dies under memory pressure**

`--release` builds use link-time optimization, which is memory-hungry. On a
busy machine the linker can be killed; retry the install without competing
builds before suspecting the toolchain.

**ONNX Runtime is not found**

Run `leindex setup`. For a manual runtime, set `ORT_DYLIB_PATH` to the
absolute shared-library path, then run `leindex setup --check`.

**Hugging Face CLI cannot be installed**

Install it directly with
`python3 -m pip install --upgrade huggingface_hub`, or set `HF_BIN` to an
existing `hf` executable.

## Removal

```bash
cargo uninstall leindex
```

Removing `${LEINDEX_HOME:-$HOME/.leindex}` also removes downloaded models,
provider caches, configuration, and global LeIndex data. Project-local `.leindex/` directories
must be removed separately when their persisted indexes are no longer wanted.
