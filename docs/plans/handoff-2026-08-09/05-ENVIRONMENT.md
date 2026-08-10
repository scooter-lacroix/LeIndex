# Environment — Host-Specific State

**As of:** 2026-08-09 23:40 EDT

---

## Hardware

- **GPU:** AMD Radeon RX 7900 XTX (24 GB VRAM)
- **ROCm:** 7.2.4 (`/opt/rocm/.info/version` = `7.2.4`)
- **OS:** CachyOS Linux
- **Shell:** fish 4.8.1

## ONNX Runtime

- **Version:** 1.27.1 (installed via pip in mlstack)
- **Location:** `/home/scooter/.mlstack/global/lib/python3.12/site-packages/onnxruntime/capi/libonnxruntime.so.1.27.1`
- **MIGraphX provider:** `libonnxruntime_providers_migraphx.so` (present in same directory)
- **Python check:** `python3 -c "import onnxruntime; print(onnxruntime.__version__)"` → `1.27.1`

## Config file (`~/.leindex/config/leindex.toml`)

```toml
[neural]
enabled = true
execution_provider = "migraphx"
ort_dylib_path = "/home/scooter/.mlstack/global/lib/python3.12/site-packages/onnxruntime/capi/libonnxruntime.so.1.27.1"
ort_version = "1.27.1"
model_dir = "/home/scooter/.leindex/models"
model_name = "qwen3-embed-0.6b-dynamic-uint8.opt"

[search]
search_mode = "hybrid"
neural_weight = 0.6
rerank_enabled = true
rerank_top_n = 80
```

**Note:** The config `ort_dylib_path` now points at `1.27.1` (the file exists). An earlier version of this config pointed at `1.25.0`, which had been replaced by the pip upgrade. The daemon launcher was promoting this stale path into `ORT_DYLIB_PATH`. That promotion has been removed (uncommitted fix in `client.rs`).

## MIGraphX compile hang (CRITICAL known issue)

On this host with ROCm 7.2.4 and MIGraphX 2.15.0:

1. `Session::builder()...commit_from_file(model)` succeeds in ~6 seconds. Session BUILD is fine.
2. The **first `session.run(...)`** triggers ORT's lazy `MIGraphXExecutionProvider::Compile` → `migraphx::program::compile` → `module::repeat_while_changes`, which **spins forever** (pass loop never converges).
3. This hangs **every model** at both Level1 and Level3 optimization.
4. The **CPU provider works perfectly**: session build ~3s, inference ~0.07s for batch=1/seq=16.
5. Previously-working `.mxr` cache files (dated 26 Jul) were compiled under an earlier ROCm; all new compiles hang.

**Impact:** The MIGraphX compile probe (`probe_migraphx_compile_timeout`) fires on every cold worker start, times out after 20s, and falls back to CPU. However, the timed-out probe thread (R1) continues spinning and retains GPU resources.

## LeIndex home

- `LEINDEX_HOME` defaults to `~/.leindex`
- Models: `~/.leindex/models/`
- Config: `~/.leindex/config/leindex.toml`
- MIGraphX cache: `~/.leindex/cache/migraphx/`
- Embed cache: `~/.leindex/embed-cache/` (when `LEINDEX_FEATURE_GLOBAL_EMBED_CACHE` is enabled)

## Build commands

```bash
# Standard check (no ONNX)
TIRITH=0 cargo check

# ONNX feature check
TIRITH=0 cargo check --features onnx

# Full validation gate
TIRITH=0 cargo fmt --all --check
TIRITH=0 cargo clippy --workspace --all-targets --features onnx -- -D warnings
TIRITH=0 cargo test --workspace --exclude memcheck

# ONNX runtime tests only
TIRITH=0 cargo test --lib --features onnx embed::runtime::tests --no-fail-fast
TIRITH=0 cargo test --lib --features onnx worker_cache --no-fail-fast

# Release build with neural support
TIRITH=0 cargo build --release --features onnx

# Warmup (will fall back to CPU due to MIGraphX hang)
timeout 60 leindex setup --neural --gpu amd --warmup
```

## Shell quirks

- **TIRITH:** Always prefix commands with `TIRITH=0`. Without it, commands may be intercepted by a TIRITH system that blocks execution.
- **cat = bat:** `cat` is aliased to `bat`, which invokes a pager. Use `git --no-pager diff` or pipe through `head`/`tail` to avoid getting stuck.
- **rg vs grep:** `rg` (ripgrep) is available. Do not use `grep -R` (unsupported flag).
- **fd vs find:** `fd` is available but has different flags than `find`.
