# LeIndex Installation Guide

Last updated: 2026-10-15 for LeIndex 2.0.

LeIndex is distributed through GitHub release bundles, crates.io, npm, and
PyPI. Core TF-IDF retrieval and PDG relationships are published first after
the binary is installed. Run `leindex setup` once to provision a neural
embedding route (local Qwen3 ONNX by default, a managed Linux GGUF service,
or an external embeddings server); healthy builds then use all three
signals for indexing and semantic retrieval.

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

On Linux, `leindex setup --embedding embeddinggemma2` can additionally
download a separate official llama.cpp runtime (`--download-llama
cpu|vulkan|cuda`): the download pins tag `b11574` and verifies the archive
against SHA256 metadata from GitHub's HTTPS release API before extraction.
Release assets are not claimed immutable. Managed services bind loopback
only; see [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md).

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
[Qwen3 Embedding (LeIndex code fine-tune, int4)](https://huggingface.co/ScooterLacroix/qwen3-embed-0.6b-int4-code) from
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

## Managed GGUF Services and System One

`leindex setup --embedding embeddinggemma2` and `leindex setup --system-one`
create **managed services** — llama-server processes that LeIndex starts,
health-checks, and stops (Linux only):

```bash
leindex services list   # name, port, model, auto_start, running state
leindex services start <name>
leindex services stop <name>
```

Services bind loopback only. Check `auto_start` in each `[services.<name>]`
section: with it true, any CLI, daemon, or MCP call needing the service starts
it on demand. Edit ports, model paths, or `extra_args` through
`leindex config set` or the config file. One llama-server serves one model,
so the embeddings service and a System One decision-model service must use
different ports (the embeddinggemma-2 preset targets 8080; the System One
wizard defaults `laya` to 8181).

**System One (experimental)** validates tool calls and search results with a
small typed-decision model before/after they reach your agent. It requires a
source build (`cargo install leindex --features system-one` — release bundles
and default crates.io builds do not include it), a `[system_one]` config
section (`leindex setup --system-one` writes one), and
`LEINDEX_FEATURE_SYSTEM_ONE=1` exported in every process that serves MCP or
runs the CLI — configuring `[system_one]` alone never enables the gate. It
fails open: any provider problem leaves calls and results untouched. The
`laya` provider runs as a managed local service and needs no API key; the
remote `jev`/`typesafe`/`openai` providers take `base_url`, `model`, and
`api_key_env` (the environment variable *name* — never the key itself);
`mode = "off"` is the default and disables the gate.

Known sharp edges, learned the hard way:

- **`config set` is all-or-nothing.** The whole document is validated after
  every edit, so while any key references a service or preset that does not
  exist, *every* `config set` fails — not just edits touching that key. The
  error names the exact `leindex config unset <key>` that removes the
  dangling reference.
- **Pin GPU devices explicitly.** On multi-GPU hosts, llama-server auto
  device selection can land on an integrated GPU. Pin it, e.g.
  `extra_args = ["-ngl", "99", "--device", "Vulkan1"]`.
- **`--check` probes but never spawns** the embedding service; a stopped
  managed service reads as not ready even with `auto_start = true`. The
  System One probe also never starts the managed service: start it beforehand
  or the probe fails, even with `auto_start = true`. It sends one real decision
  request (bounded by `system_one.timeout_ms`).
- **Changing the embedding model changes the vector space.** Re-index after
  switching models or prompt templates; never mix old and new vectors.
- **Release-mode LTO links are memory-hungry.** On busy machines a
  `cargo install --release` link step can be killed for memory; retry it
  without competing builds before suspecting the toolchain.

## Verification

```bash
leindex --version
leindex setup --check     # embeddings probe + System One gate status
leindex config validate
leindex services list     # Linux managed llama-server services
leindex diagnostics
leindex index /path/to/project
leindex search "authentication flow" --project /path/to/project
leindex mcp --help
```

`setup --check` sends one bounded embedding request (it never starts
services) and, on `system-one` builds, reports the gate's build, flag, and
endpoint status. An unchanged indexed project should hydrate its persisted
search snapshot. Repeated search calls should not report refresh,
deduplication, or index maintenance unless source files or stored artifacts
changed.

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
