# LeIndex Installation Guide

Last updated: 2026-07-10 for LeIndex 1.8.4.

LeIndex is distributed through GitHub release bundles, crates.io, npm, and
PyPI. TF-IDF search works immediately after the binary is installed. Neural
search requires one explicit `leindex setup` run so LeIndex can select the
host provider and download the correct model.

Model files are never included in a release artifact.

## Release Bundle Installer

**Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.sh | bash
leindex setup
```

**macOS**

```bash
curl -fsSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install_macos.sh | bash
leindex setup
```

**Windows PowerShell**

```powershell
iwr https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.ps1 -UseBasicParsing | iex
leindex setup
```

The platform archive contains `leindex`, `leindex-embed`, and ONNX Runtime
libraries. The installer verifies the archive and installs those runtime
files. It does not copy or download model weights.

## Cargo

```bash
cargo install leindex
leindex setup
```

To install current Git source:

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
leindex setup
```

Cargo builds with runtime-dynamic ONNX loading; ONNX Runtime is not required on
the build host.

## npm MCP Package

```bash
npm install -g @leindex/mcp
npm run setup --prefix "$(npm root -g)/@leindex/mcp"
```

Use the standard MCP command after setup:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["-y", "@leindex/mcp"]
    }
  }
}
```

The npm package stores binaries under its package directory. Model assets are
installed by setup under `$LEINDEX_HOME/models`, so npm upgrades and cache
cleanup do not own or duplicate the model.

## PyPI

```bash
pip install leindex
leindex setup
```

The PyPI package bootstraps the Rust binary and exposes `leindex-setup`.
Neither the wheel nor source distribution contains model weights.

## Neural Provider Setup

Setup installs `huggingface_hub` if the `hf` CLI is unavailable, downloads
[Qwen3 Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) from
Hugging Face via Hugging Face CLI, installs the provider-specific ONNX Runtime
package, writes configuration, and runs an embedding smoke test.

```bash
leindex setup --neural --cpu
leindex setup --neural --gpu nvidia
leindex setup --neural --gpu amd
leindex setup --check
```

- CPU uses `onnxruntime`.
- NVIDIA uses `onnxruntime-gpu` and requires a working driver/CUDA stack.
- AMD uses `onnxruntime-migraphx` and requires a supported ROCm stack.

The same dynamic model graph is used by all providers. GPU setup fails if the
requested provider is not active instead of silently accepting CPU fallback.
The first MIGraphX setup can take several minutes while compiling and warming
the persistent cache.

See [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md) for runtime behavior,
environment overrides, and troubleshooting.

## Verification

```bash
leindex --version
leindex setup --check
leindex diagnostics
leindex index /path/to/project
leindex search "authentication flow" --project /path/to/project
leindex mcp --help
```

An unchanged indexed project should hydrate its persisted search snapshot.
Repeated search calls should not report refresh, deduplication, or index
maintenance unless source files or stored artifacts changed.

## Manual Build

```bash
git clone https://github.com/scooter-lacroix/LeIndex.git
cd LeIndex
cargo build --release --bins
./target/release/leindex setup
```

Outputs:

- `target/release/leindex`
- `target/release/leindex-embed`

## Optional Turso Configuration

Local storage is the default. Configure Turso only when remote storage is
wanted:

```bash
export LEINDEX_TURSO_URL="libsql://<db>.turso.io"
export LEINDEX_TURSO_AUTH_TOKEN="..."
export LEINDEX_HNSW_HOT_MB=256
```

Release automation is documented in
[docs/RELEASE_BINARY_WORKFLOW.md](docs/RELEASE_BINARY_WORKFLOW.md).
