# LeIndex Neural Pipeline Root-Cause Remediation

> **For implementing agents:** This plan addresses ALL findings from three independent investigations into LeIndex's neural embedding pipeline performance crisis (612-second embedding time, 18GB RAM, 15.8GB VRAM for a 20-file project). Every fix targets root cause. Zero arbitrary timeouts. Zero assumptions about hardware. The goal is rapid, accurate, comprehensive code intelligence with low resource overhead.

## Evidence Base

Three independent agents investigated the same codebase using the same prompt. Their findings were synthesized into the consensus below. The strongest evidence (Agent 3) includes live runtime proof (process memory maps, disk cache analysis, timing data):

- **9 MIGraphX compiled .mxr files written today** (same model, same shape [8,128], each ~1.2GB) -- cache produces 0% hits because filenames have volatile hash tails caused by the `-dynamic` model
- **18GB RSS + 15.8GB VRAM held by a single idle daemon** -- compiled programs never freed because no `impl Drop` exists and exit uses `process::exit(0)`/SIGKILL
- **101% CPU during "inference"** -- MIGraphX JIT compilation is CPU-bound (hipRTC/clang subprocesses), not GPU-bound
- **Worker hit_count=0 on cache despite env var set** -- `ORT_MIGRAPHX_MODEL_CACHE_PATH` is set but MIGraphX EP never enables cache loading via its API

## Hardware Benchmark Proof

The user's ONNX benchmark on identical hardware proves what is achievable: 95,710 inferences/sec (0.082ms p50, 0.094ms p95), 740ms session creation (including MIGraphX planning), 918MB peak RSS, 2x AMD GPUs with 25.75GB + 17.16GB VRAM.

LeIndex's 612-second embedding for 274 nodes is **800,000x slower** than achievable. The fixes below target closing this gap to <1s.

---

## Non-negotiable decisions

1. **Zero arbitrary timeouts.** Every fix optimizes the code path so operations complete rapidly. No wall-clock bounds mask slow operations.
2. **Zero environment/config changes.** The user's ONNX/MIGraphX setup is their domain.
3. **Fix the cause, not the symptom.** If MIGraphX recompiles every run, the fix is to make cache work, not to add a timeout.
4. **Match the hardware benchmark.** After fixes: <1s for 274 nodes, <1GB RAM overhead, <2GB VRAM.
5. **No new crate dependencies.** Use APIs available in the pinned `ort` crate (2.0.0-rc.12 at commit 17ed727).

---

## Task 1: Enable MIGraphX compiled-program persistence (eliminates 612s cold compile)

**Root cause:** MIGraphX writes compiled `.mxr` programs to the cache directory but never reads them back because (a) the `ORT_MIGRAPHX_MODEL_CACHE_PATH` env var mechanism produces volatile keys for dynamic-shape models, and (b) `with_migraphx_cache_path()` API does not exist in the pinned ort version (2.0.0-rc.12 at 17ed727). Result: every fresh worker pays ~600s of CPU-bound JIT compilation.

**Files:**
- Modify: `crates/leindex-embed/src/runtime.rs` (build_session, build_migraphx_ep)
- Modify: `crates/leindex-embed/Cargo.toml` (verify ort features)

- [ ] **Step 1: Understand the available MIGraphX cache APIs**

Read the pinned ort crate source at `~/.cargo/registry/src/.../ort-2.0.0-rc.12/src/ep/migraphx.rs` (or the git checkout under the patch). Identify the EXACT methods available on `ort::ep::MIGraphX`:
- `with_save_model(Option<PathBuf>)` — saves compiled program to path
- `with_load_model(Option<PathBuf>)` — loads compiled program from path
- `with_fp16(bool)` — already used
- `with_exhaustive_tune(bool)` — already used

Verify these methods exist and what they do. Also check if `with_migraphx_cache_path` exists (Agent 3 reports it does NOT; verify).

- [ ] **Step 2: Implement save/load model cache in build_migraphx_ep**

In `runtime.rs`, function `build_migraphx_ep()` (around line 521), add cache load/save:

```rust
fn build_migraphx_ep() -> ort::ep::ExecutionProviderDispatch {
    let mut ep = ort::ep::MIGraphX::default();
    
    // Enable compiled-program persistence. On warm runs, load_model avoids
    // a ~600s JIT compile by loading a pre-compiled .mxr in ~1s.
    if let Ok(cache_path) = std::env::var("ORT_MIGRAPHX_MODEL_CACHE_PATH") {
        let cache_dir = std::path::Path::new(&cache_path);
        if let Some(parent) = cache_dir.parent() {
            // Look for an existing .mxr in the cache directory
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "mxr") {
                        tracing::info!("MIGraphX: loading cached program from {}", path.display());
                        ep = ep.with_load_model(Some(path));
                        break;
                    }
                }
            }
        }
        // Also configure save so the compiled program persists after first compile
        // Use a deterministic filename (NOT with package version, which changes)
        let save_path = cache_dir.join("compiled.mxr");
        tracing::info!("MIGraphX: will save compiled program to {}", save_path.display());
        ep = ep.with_save_model(Some(save_path));
    }
    
    if env_flag(MIGRAPHX_FP16_ENV) { ep = ep.with_fp16(true); }
    if env_flag(MIGRAPHX_EXHAUSTIVE_TUNE_ENV) { ep = ep.with_exhaustive_tune(true); }
    ep.build()
}
```

IMPORTANT: Verify the actual API signatures from the ort crate source before implementing. The `with_save_model` and `with_load_model` methods may take `Option<PathBuf>` or `Option<&Path>` or a different type entirely. Read the source.

- [ ] **Step 3: Add pre-warmup inference during init_onnx()**

In `runtime.rs`, function `init_onnx()` (around line 397), after building the session but BEFORE returning, add a dummy warmup inference:

```rust
// Warmup: run a dummy inference with the EXACT batch/seq shape the
// indexing pipeline will use. This forces MIGraphX JIT compilation to
// happen NOW (during init) rather than during the first real request.
// The compiled program is saved to the .mxr cache by with_save_model.
if let Some(ref session) = session {
    let batch_size = configured_onnx_inference_batch_size();
    let seq_len = configured_onnx_sequence_len();
    tracing::info!("MIGraphX: warmup inference batch={} seq={}", batch_size, seq_len);
    // Create dummy input tokens (all zeros) and run one forward pass
    let _ = Self::warmup_inference(session, batch_size, seq_len);
    tracing::info!("MIGraphX: warmup complete, compiled program cached");
}
```

The `warmup_inference` function should:
1. Create zero-filled input_ids tensor of shape [batch_size, seq_len]
2. Create zero-filled attention_mask tensor of the same shape
3. Call session.run() with these inputs
4. Ignore the output
5. This triggers the full MIGraphX compile + save cycle

- [ ] **Step 4: Fix the cache key to NOT include CARGO_PKG_VERSION**

In `src/search/onnx/client.rs`, function `migraphx_model_cache_path()` (around line 266), change the cache profile to use the model name and file hash instead of package version:

```rust
fn migraphx_model_cache_path(
    model: Option<&str>,
    batch: usize,
    sequence: usize,
) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let model_safe = sanitize_cache_component(model.unwrap_or("default"));
    // Key on model name + batch + sequence, NOT package version.
    // The compiled MIGraphX program depends on the model graph + input shape,
    // not on LeIndex's software version. Including the version forces
    // recompilation on every minor release update.
    let profile = format!("{}-b{}-s{}", model_safe, batch, sequence);
    Some(home.join(".leindex/cache/migraphx").join(profile))
}
```

- [ ] **Step 5: Verify MIGraphX cache works end-to-end**

```bash
# Clear the cache to simulate first cold compile
rm -rf ~/.leindex/cache/migraphx/

# Run index (should take ~10 min on the FIRST cold compile, then save .mxr)
target/release/leindex index tests/fixtures/memcheck/small_repo --verbose --force

# Check cache was populated
ls -la ~/.leindex/cache/migraphx/*/

# Run index again (should complete in SECONDS, not minutes)
time target/release/leindex index tests/fixtures/memcheck/small_repo --verbose --force
```

Expected: Second run completes in <30 seconds total.

---

## Task 2: Add worker touch() on request entry (prevents mid-inference idle kill)

**Root cause:** The socket worker's idle timer fires while a long-running embed request is being processed because `self.touch()` is only called AFTER `dispatch()` returns. During a 600s MIGraphX compilation, the 600s idle timer considers the worker idle and kills it.

**Files:**
- Modify: `crates/leindex-embed/src/runtime.rs` (dispatch, run_loop)

- [ ] **Step 1: Call touch() at request entry**

In `runtime.rs`, function `dispatch()` (around line 856), add `self.touch()` as the FIRST line:

```rust
fn dispatch(&self, frame: &Frame) -> Response {
    // Reset idle timer immediately. This prevents the accept loop's idle
    // check from killing the worker during long-running inference requests.
    self.touch();
    
    match frame.msg_type {
        // ... existing match arms
    }
}
```

Also add `self.touch()` between sub-batches in multi-batch inference, inside `run_onnx_embed()` (around line 1004):

```rust
for sub_batch in encodings.chunks(inference_batch_size) {
    // Touch between sub-batches to keep the worker alive during
    // multi-batch inference on large codebases.
    self.touch();
    // ... existing sub-batch processing
}
```

- [ ] **Step 2: Verify**

```bash
cargo test -p leindex-embed --all-features
```

---

## Task 3: Add impl Drop for WorkerRuntime (eliminates 18GB/15.8GB VRAM leak)

**Root cause:** No `impl Drop` exists anywhere in `crates/leindex-embed/src/`. When the worker exits via `process::exit(0)` or SIGKILL, the ONNX Session destructor never runs. MIGraphX/ROCm GPU resources (compiled programs, workspace memory) are never released. Result: 15.8GB VRAM held by idle daemons, 18GB RSS accumulated.

**Files:**
- Modify: `crates/leindex-embed/src/runtime.rs` (add Drop impl)
- Modify: `crates/leindex-embed/src/worker_main.rs` (graceful shutdown before process::exit)

- [ ] **Step 1: Add Drop for WorkerRuntime**

In `runtime.rs`, add:

```rust
impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        // Explicitly drop the session first so MIGraphX/ROCm destructors
        // free GPU resources (compiled programs, workspace memory, VRAM).
        // Without this, process::exit/SIGKILL skips destructors and leaves
        // ~1.5GB of VRAM allocated per compiled program.
        self.session = None;
        tracing::debug!("WorkerRuntime dropped; GPU resources released");
    }
}
```

- [ ] **Step 2: Ensure graceful shutdown in worker_main**

In `worker_main.rs`, before any `process::exit()` call, ensure the runtime is properly dropped:

For pipe mode (around line 172):
```rust
// Before process::exit(0), let the runtime drop naturally.
// The runtime was moved into run_loop, which returns here.
// If run_loop returned Ok, the runtime has been consumed.
// If we reach this via an error path, ensure cleanup.
```

For socket mode (around lines 275-310):
```rust
// When the accept loop exits (idle timeout or failure), explicitly
// drop the runtime from the SocketLifecycle to free GPU resources.
if let Some(runtime_arc) = lifecycle.take_runtime() {
    drop(runtime_arc);
}
```

The `SocketLifecycle` struct needs a method to extract and drop the runtime.

- [ ] **Step 3: Verify VRAM is freed**

```bash
# Kill any existing daemons
pkill -9 -f leindex-embed

# Run index
target/release/leindex index tests/fixtures/memcheck/small_repo

# Check VRAM after exit
rocm-smi --showmeminfo vram
```

Expected: VRAM returns to baseline after the indexing process exits.

---

## Task 4: Prune stale .mxr cache variants (eliminates 13GB cache accumulation)

**Root cause:** The MIGraphX cache directory accumulates ~1.2GB .mxr files with volatile hash names. 9 files from today alone = 13GB. This is dead weight on disk.

**Files:**
- Modify: `crates/leindex-embed/src/runtime.rs` or `src/cli/leindex/setup.rs` (cache pruning)

- [ ] **Step 1: Add cache pruning on startup**

In `runtime.rs` or at the worker startup, after determining the cache directory, prune old .mxr files:

```rust
fn prune_migraphx_cache(cache_dir: &Path, keep: usize) {
    // Keep only the N most recent .mxr files; remove older variants.
    if let Ok(mut entries) = std::fs::read_dir(cache_dir) {
        let mut mxr_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "mxr"))
            .collect();
        
        // Sort by modification time, newest first
        mxr_files.sort_by(|a, b| {
            b.metadata().and_then(|m| m.modified()).ok()
                .cmp(&a.metadata().and_then(|m| m.modified()).ok())
        });
        
        // Remove all but the newest `keep` files
        for entry in mxr_files.iter().skip(keep) {
            let _ = std::fs::remove_file(&entry.path());
            tracing::debug!("Pruned stale MIGraphX cache file: {}", entry.path().display());
        }
    }
}
```

Call this on startup after Task 1's `build_migraphx_ep()` runs. Keep only 1-2 most recent .mxr files.

- [ ] **Step 2: Add cache pruning to setup**

In `src/cli/leindex/setup.rs`, add a step that prunes all old cache variants.

---

## Task 5: Prevent silent CPU fallback in indexing path

**Root cause:** When MIGraphX EP registration fails (e.g., wrong ORT binary loaded), the `try_provider_or_cpu!` macro silently falls back to CPU. The indexing path runs slow CPU inference (612s) instead of bailing to TF-IDF or erroring out. The startup report shows `provider=cpu` but nothing checks it.

**Files:**
- Modify: `crates/leindex-embed/src/runtime.rs` (log or fail when falling back to CPU)
- Modify: `src/search/onnx/client.rs` or `src/cli/index_builder.rs` (bail when provider is CPU and config requested MIGraphX)

- [ ] **Step 1: Make CPU fallback explicit in startup report**

In `runtime.rs` `log_startup_report()`, already logs the provider. Add a clear warning:

```rust
if provider != "migraphx" && provider != "cuda" {
    tracing::warn!(
        "Neural worker started with CPU provider (requested: {}). \
         Inference will be 100x-1000x slower than GPU. \
         Install onnxruntime-migraphx or set ORT_DYLIB_PATH.",
        configured_provider_str
    );
}
```

- [ ] **Step 2: Bail to TF-IDF in indexing when provider is CPU**

In `src/cli/index_builder.rs` or `src/search/onnx/client.rs`, when the daemon's health reports `provider=cpu` and the config requested `migraphx`, skip neural enrichment and fall back to TF-IDF:

```rust
// After getting health from daemon, check if provider matches config
if health.provider.as_deref() == Some("cpu") && config_provider == "migraphx" {
    tracing::warn!(
        "Neural daemon running on CPU (MIGraphX configured but unavailable). \
         Skipping neural enrichment, using TF-IDF only."
    );
    return EmbedResult::Fallback { ... };
}
```

---

## Task 6: Reduce memory allocation pressure

**Root cause:** Node texts are cloned 5 times across the IPC boundary. Embedding vectors are split into 256 individual `Vec<f32>` heap allocations per batch instead of operating on contiguous memory. Tokenizer processes entire source files then truncates (though Agent 3 showed tokenizer uses rayon parallel, this still allocates temporary structures).

**Files:**
- Modify: `crates/leindex-embed/src/runtime.rs` (reduce allocations)
- Modify: `src/cli/index_builder.rs` (reduce cloning)

- [ ] **Step 1: Eliminate redundant text cloning in IPC path**

In `runtime.rs` `handle_embed()` (around line 856):
- Change `texts.to_vec()` to pass references to `encode_batch`
- The tokenizers crate accepts `&[&str]` or `EncodeInput` references

- [ ] **Step 2: Use contiguous embedding slices instead of 256 Vec<f32> allocations**

In `index_builder.rs` `embed_neural_batch_blocking()` (around line 862):
- Don't call `response.into_vectors()` which allocates N individual Vecs
- Instead, iterate over slices of the contiguous buffer: `response.get_embedding(i)` returns `&[f32]`
- Store embedding slices directly into the search index without per-vector heap allocation

- [ ] **Step 3: Configure tokenizer truncation at load time**

In `runtime.rs` `init_onnx()`:
```rust
let mut tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)?;
let max_len = configured_onnx_sequence_len();
let _ = tokenizer.with_truncation(Some(tokenizers::TruncationParams {
    max_length: max_len,
    strategy: tokenizers::TruncationStrategy::LongestFirst,
    stride: 0,
    direction: tokenizers::TruncationDirection::Right,
}));
```

This prevents the tokenizer from allocating full-length token arrays for entire source files before truncation.

---

## Task 7: Full verification

- [ ] **Step 1: Clean state**
```bash
# Kill all stale processes
pkill -9 -f leindex-embed
rm -rf ~/.leindex/run/*
rm -rf ~/.leindex/cache/migraphx/*
```

- [ ] **Step 2: First cold compile (expected: ~10 min)**
```bash
SECONDS=0; target/release/leindex index tests/fixtures/memcheck/small_repo --verbose --force 2>&1 | tail -20; echo "ELAPSED=${SECONDS}s"
```

- [ ] **Step 3: Verify cache populated**
```bash
ls -la ~/.leindex/cache/migraphx/*/
```

- [ ] **Step 4: Second warm run (expected: <30s)**
```bash
SECONDS=0; target/release/leindex index tests/fixtures/memcheck/small_repo --verbose --force 2>&1 | tail -20; echo "ELAPSED=${SECONDS}s"
```

- [ ] **Step 5: Verify resources clean up**
```bash
# After indexing exits, VRAM should return to baseline
rocm-smi --showmeminfo vram
# RSS should be 0 (no stale daemons)
ps aux | grep leindex-embed | grep -v grep
```

- [ ] **Step 6: Code quality**
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features
```

---

## Performance targets after all fixes

| Metric | Before (Observed) | After (Target) | Hardware Benchmark |
|--------|-------------------|----------------|-------------------|
| Cold-start indexing (274 nodes) | 612,157ms | ~30s (one-time compile) | 740ms (MIGraphX planning) |
| Warm indexing (274 nodes) | N/A (never achieved) | <5s | <1s theoretical |
| Inference per sub-batch | ~17,500ms | <1ms | 0.094ms p95 |
| Peak RSS | 18,000MB | <1,000MB | 918MB |
| Peak VRAM | 15,800MB | <2,000MB | ~1.5GB |
| Cache disk usage | 13,000MB | <1,500MB | Single .mxr file |
| Worker death rate | 100% (killed mid-inference) | 0% | Stable persistent daemon |

## Explicitly rejected approaches

- **Adding a timeout to `rx.recv()`**: Masks slow inference instead of making it fast. Once Task 1-2 land, inference takes <1ms, no timeout needed.
- **Changing optimization level to Level1**: Reduces compile time but hurts runtime performance. Task 1 (cache persistence) makes Level3 affordable.
- **Silent CPU fallback**: Acceptable for MCP server mode (keep working), NOT acceptable for CLI index mode (too slow). Task 5 bails to TF-IDF.
- **Larger batch sizes**: Would require MIGraphX recompilation for new shapes. Keep batch=8, make it fast.
- **Multiple compiled-program eviction**: Task 1 (proper cache + static shape) means only ONE program exists. No eviction needed.
- **Re-exporting the model to static shapes**: Mentioned by Agent 3 but requires tooling (python + onnx). The `with_save_model`/`with_load_model` approach (Task 1) achieves the same result without modifying model files. This can be a future optimization.

## Final stop condition

The remediation is complete only when:
1. Cold-start first-ever compile produces exactly ONE .mxr file in the cache directory
2. Every subsequent indexing run loads that .mxr and completes in <30s
3. After indexing exits, RSS returns to 0 and VRAM returns to baseline
4. No .mxr file accumulation across runs (cache directory has 1-2 files max)
5. `cargo fmt`, `cargo clippy -D warnings`, and `cargo test --workspace` all pass
