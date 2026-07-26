# LeIndex Rust Installation Guide

This guide covers crates.io and source builds for LeIndex 1.9.0. For release
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
Cargo's binary directory.

## Neural Setup

The Rust crate does not package model files. Setup selects the execution
provider, installs the matching ONNX Runtime package, installs
`huggingface_hub` if needed, and downloads
[Qwen3 Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) from
Hugging Face via Hugging Face CLI, then writes
`~/.leindex/config/leindex.toml`.

```bash
leindex setup --neural --cpu
leindex setup --neural --gpu nvidia
leindex setup --neural --gpu amd
leindex setup --check
```

The provider packages are `onnxruntime`, `onnxruntime-gpu`, and
`onnxruntime-migraphx`, respectively. CUDA or ROCm/MIGraphX system
dependencies remain the responsibility of the host. Setup validates the active
provider and rejects silent GPU-to-CPU fallback.

The model is stored under `$LEINDEX_HOME/models`, normally
`~/.leindex/models`; it is not written into the Cargo registry or build
directory.

See [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md) for provider tuning and
runtime details.

## Verification

```bash
leindex --version
leindex setup --check
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
