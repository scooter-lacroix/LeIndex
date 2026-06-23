# Neural Search Setup Guide

This guide covers configuring LeIndex's neural (semantic) embeddings, choosing an
execution provider (CPU / GPU / AMD / NVIDIA), and troubleshooting common issues.

LeIndex ships with two search modes:

- **TF-IDF (lexical)**: keyword-based search. Works immediately after install
  with no additional setup. Good for exact-match and identifier lookups.
- **Neural (semantic)**: embedding-based search using the `qwen3-embed-0.6b`
  ONNX model. Finds code by meaning, not just keywords. Requires a one-time
  `leindex setup` to install ONNX Runtime and download model files.

If you skip `leindex setup`, LeIndex falls back to TF-IDF automatically and
prints a one-time notice pointing you to `leindex setup`. Search never hard-fails
because neural is unavailable.

---

## Quick Setup

### Interactive (recommended for first-time users)

```bash
leindex setup
```

The wizard asks:

1. Do you want neural embeddings / enhanced semantic search? **(Y/n)**
2. CPU or GPU-based neural embeddings? **(CPU / GPU)**
3. (GPU only) AMD (ROCm/MIGraphX) or NVIDIA (CUDA)? **(AMD / NVIDIA / N/A)**

It then installs the right ONNX Runtime pip package, downloads the model to
`~/.leindex/models/`, writes `~/.leindex/config/leindex.toml`, and runs a smoke
test.

### Non-interactive (CI / scripts)

```bash
# CPU neural search (universal baseline)
leindex setup --neural --cpu

# AMD GPU (ROCm / MIGraphX)
leindex setup --neural --gpu amd

# NVIDIA GPU (CUDA)
leindex setup --neural --gpu nvidia

# Disable neural, use TF-IDF only
leindex setup --no-neural

# Print current status without changing anything
leindex setup --check
```

---

## Execution Providers

### CPU (works everywhere)

Installs the plain `onnxruntime` pip package and selects the CPU execution
provider. This is the universal baseline and the recommended starting point.

```bash
leindex setup --neural --cpu
```

The pip package installed is `onnxruntime`. Inference runs on CPU; no GPU
drivers or toolkits are required.

### AMD GPU (ROCm / MIGraphX)

Installs `onnxruntime-migraphx` and registers the MIGraphX execution provider.
Requires a working ROCm installation with MIGraphX available on the system.

```bash
leindex setup --neural --gpu amd
```

The setup wizard discovers the MIGraphX-enabled `libonnxruntime` from the pip
wheel's `site-packages/onnxruntime/capi/` directory and records it in the config
file. If MIGraphX is not available at runtime (e.g. the provider `.so` is
missing), the worker falls back to CPU automatically with a logged warning.

### NVIDIA GPU (CUDA)

Installs `onnxruntime-gpu` and selects the CUDA execution provider. Requires a
CUDA-capable NVIDIA GPU and compatible CUDA toolkit drivers.

```bash
leindex setup --neural --gpu nvidia
```

The pip package installed is `onnxruntime-gpu`. The worker uses the CUDA
execution provider for inference.

---

## The ORT Discovery Chain

When the `leindex-embed` worker starts, it locates the ONNX Runtime shared
library (`libonnxruntime.so` / `libonnxruntime.dylib` / `onnxruntime.dll`) using
the following priority chain:

1. **`ORT_DYLIB_PATH` env var** (highest priority, explicit override)
2. **Config file**: `~/.leindex/config/leindex.toml` `[neural] ort_dylib_path`
3. **Bundled libs**: `~/.leindex/lib/` (GitHub Release / install.sh install)
4. **Sibling directory**: next to the `leindex-embed` binary (release bundle)
5. **pip site-packages**: `site-packages/onnxruntime/capi/` (`pip install`)
6. **System paths**: `/usr/local/lib`, `/usr/lib`, ld.so.conf (final fallback)

This means neural search works in any of these scenarios without manual path
configuration:

- GitHub Release bundle (bundled `lib/` is discovered via source 3 or 4)
- `pip install onnxruntime` (discovered via source 5)
- System-installed ORT in `/usr/local/lib` (discovered via source 6)
- Manual `ORT_DYLIB_PATH` override (source 1)

---

## Configuration File

Settings are persisted to `~/.leindex/config/leindex.toml` (or
`$LEINDEX_HOME/config/leindex.toml` if `LEINDEX_HOME` is set):

```toml
[neural]
enabled = true
execution_provider = "cpu"       # cpu | cuda | migraphx
ort_dylib_path = "/path/to/libonnxruntime.so"
model_dir = "~/.leindex/models"

[search]
search_mode = "hybrid"           # hybrid | text | neural
neural_weight = 0.3

[indexing]
batch_size = 500
max_files = 50000
```

Running `leindex setup` multiple times is safe (idempotent). It overwrites the
neural block and preserves user-tuned `[search]` and `[indexing]` sections
unless they conflict with new defaults.

---

## Model Files

The neural model is `qwen3-embed-0.6b.onnx` (approximately 569 MB), sourced
from the [onnx-community/Qwen3-Embedding-0.6B-ONNX](https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX)
HuggingFace repository. The following files are downloaded to
`~/.leindex/models/`:

| File | Purpose |
|------|---------|
| `qwen3-embed-0.6b.onnx` | ONNX model weights |
| `tokenizer.json` | HuggingFace tokenizer |
| `config.json` | Model metadata |
| `checksums.sha256` | SHA256 checksums for integrity verification |
| `LICENSE` | Model license (Apache 2.0) |

Setup verifies SHA256 checksums after download. If a checksum fails, the
corrupted file is deleted and re-downloaded automatically. If checksums already
match, the download is skipped (fast re-runs).

---

## Troubleshooting

### ORT not found anywhere

**Symptom**: The worker logs an error listing searched paths and neural search
falls back to TF-IDF.

**Cause**: ONNX Runtime is not installed in any of the discovery-chain locations.

**Fix**:

```bash
# Run the setup wizard to install ORT via pip
leindex setup --neural --cpu

# Or set ORT_DYLIB_PATH manually if you have ORT elsewhere
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so
```

### ONNX Runtime version mismatch

**Symptom**: The worker reports a version-mismatch error naming the expected and
detected ORT versions, instead of a cryptic segfault.

**Cause**: The discovered `libonnxruntime` has an API/ABI version incompatible
with the `ort` crate's expectations.

**Fix**:

```bash
# Reinstall a compatible ORT version
pip install --upgrade 'onnxruntime>=1.20,<2'

# Or for GPU
pip install --upgrade 'onnxruntime-gpu>=1.20,<2'

# Then re-run setup to update the recorded path
leindex setup --neural --cpu
```

### Corrupted or incomplete model files

**Symptom**: The embedding smoke test fails, or the worker reports a model
loading error.

**Cause**: One or more model files in `~/.leindex/models/` are missing,
truncated, or corrupted.

**Fix**:

```bash
# Remove the models directory and re-run setup to re-download
rm -rf ~/.leindex/models
leindex setup --neural --cpu
```

### GPU provider unavailable (falls back to CPU)

**Symptom**: After selecting AMD (MIGraphX) or NVIDIA (CUDA), the worker logs
that the requested provider is not available and falls back to CPU. Inference
still works, just slower.

**Cause**: The GPU drivers / toolkit (ROCm/MIGraphX or CUDA) are not installed
or not on the library path, so ORT cannot register the hardware provider.

**Fix**:

- **AMD**: Install ROCm and MIGraphX. Re-run
  `pip install --upgrade onnxruntime-migraphx`, then `leindex setup --neural --gpu amd`.
- **NVIDIA**: Install the CUDA toolkit and NVIDIA drivers. Re-run
  `pip install --upgrade onnxruntime-gpu`, then `leindex setup --neural --gpu nvidia`.
- If you do not have a GPU, switch to CPU:
  `leindex setup --neural --cpu`.

### pip not on PATH

**Symptom**: Setup aborts with an actionable error explaining pip is missing,
rather than crashing.

**Cause**: `pip` (or `pip3`) is not installed or not on `PATH`.

**Fix**: Install pip, then re-run setup.

- **Linux (Debian/Ubuntu)**: `sudo apt install python3-pip`
- **Linux (Fedora/RHEL)**: `sudo dnf install python3-pip`
- **Linux (Arch)**: `sudo pacman -S python-pip`
- **macOS**: `python3 -m ensurepip --upgrade` or use Homebrew `brew install python`
- Or set the `PIP_BIN` env var to the full path of your pip executable, then
  re-run `leindex setup`.

### Setup succeeds but search still uses TF-IDF

**Symptom**: `leindex setup --check` reports neural is configured, but search
results show no neural score component.

**Cause**: The index was built before neural was enabled, so it has no
embeddings stored.

**Fix**:

```bash
# Re-index the project so embeddings are generated
leindex index /path/to/project

# Then search
leindex search "authentication" --path /path/to/project
```

### Read-only home directory

**Symptom**: Setup exits with a permission error naming the offending path.

**Cause**: `~/.leindex/` cannot be created or written (permission denied, or a
read-only filesystem).

**Fix**:

```bash
# Point LeIndex at a writable directory
export LEINDEX_HOME=/var/lib/leindex   # or any writable path
leindex setup --neural --cpu
```

---

## Checking Status

```bash
# Print current setup status (read-only, no changes)
leindex setup --check
```

The status report includes:

- Neural: enabled / disabled
- Execution provider: cpu / cuda / migraphx
- ORT library path and version
- Model present / absent
- Last smoke-test result

You can also inspect the raw config:

```bash
cat ~/.leindex/config/leindex.toml
```

And check what ORT the worker actually loaded:

```bash
leindex diagnostics
```

Look for the `ort_path` and `ort_version` fields in the diagnostics output to
confirm which ONNX Runtime the worker resolved at runtime.

---

## See Also

- [Root README](../README.md) — install paths and feature overview
- [docs/MCP.md](MCP.md) — MCP server configuration
- [docs/R15_MODEL_DISTRIBUTION.md](R15_MODEL_DISTRIBUTION.md) — model bundling
  strategy
