# WS1-2: Baseline Profiler + Safe Containment

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing `tools/memcheck` harness to satisfy spec §14 (full baseline protocol: env capture, GPU, heap profiles, contention, incremental workloads, query suite, 3× full index, descendant-tree counting), then apply WS2 containment defaults as **measured, env-driven** knobs — not runtime hacks.

**Architecture:** memcheck already samples RSS/PSS/anon/mapped/worker and drives 12 canonical phases. We *extend* it with the missing §14 capabilities and add env-configurable containment knobs to `leindex` itself, measured via memcheck before/after.

**Spec refs:** §8 (CPU/GPU/Thread/Allocator), §14 (Baseline Protocol), §3 (root-cause evidence).

**Tech Stack:** Rust, tempfile, serde, std::process (git/gpu-smi).

**Existing infra (DO NOT reinvent):**
- `tools/memcheck/src/{sampler,workload,report,diff,main}.rs` + `tests/{diff_logic,harness_integration}.rs`
- `src/cli/memory_cap.rs` (`MemoryCapGuard` with throttling)
- `src/bin/leindex.rs:14-16` (`memprof` feature → jemalloc; default = glibc)
- `src/embed/runtime_env.rs` (`default_ort_threads`, `LEINDEX_WORKER_ORT_THREADS`, batch/seq envs)
- `src/cli/leindex/indexing/mod.rs` (`IndexPipelineState`, phase checkpoints)
- Budget/baseline dirs: `docs/memory/{budgets,baselines}/`

---

## Task 1: Environment-capture module for memcheck

**Why:** Spec §14 item 2 requires recording hardware, kernel, allocator, provider, model digests, config, git revision, corpus hash. Currently `MemcheckReport` only has `fixture`, `phases`, `timestamp`.

**Files:**
- Create: `tools/memcheck/src/env_capture.rs`
- Modify: `tools/memcheck/src/main.rs` (add `mod env_capture;`)
- Modify: `tools/memcheck/src/report.rs` (`MemcheckReport` gains `environment: EnvironmentCapture`)

- [ ] **Step 1: Write failing test** (`tools/memcheck/src/env_capture.rs`)

```rust
//! Captures the §14 environment record: hardware, kernel, allocator env,
//! provider config, git revision, corpus hash, model digests.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvironmentCapture {
    pub kernel: String,            // uname -r
    pub hardware: HardwareInfo,
    pub allocator_env: HashMap<String, String>, // MALLOC_ARENA_MAX, MALLOC_CONF, etc.
    pub git_revision: String,      // HEAD OID
    pub corpus_tree_oid: String,   // git tree OID of fixture
    pub provider_config: HashMap<String, String>,
    pub model_digests: ModelDigests,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HardwareInfo {
    pub cpu_model: String,
    pub cpu_count: usize,
    pub mem_total_kib: u64,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelDigests {
    pub embed_model: String,
    pub embed_onnx_sha256: Option<String>,
    pub reranker_model: Option<String>,
}

impl EnvironmentCapture {
    pub fn capture(workspace: &Path, fixture: &Path) -> std::io::Result<Self> {
        todo!() // implemented in step 3
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs;

    #[test]
    fn test_capture_records_kernel_and_git() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git").args(["init"]).current_dir(tmp.path()).status().unwrap();
        fs::write(tmp.path().join("a.txt"), "x").unwrap();
        let cap = EnvironmentCapture::capture(tmp.path(), tmp.path()).unwrap();
        assert!(!cap.kernel.is_empty());
        assert!(!cap.git_revision.is_empty());
        assert!(cap.cpu_count > 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memcheck test_capture_records_kernel_and_git`
Expected: FAIL (todo!() panic)

- [ ] **Step 3: Implement `capture()`**

Read `/proc/cpuinfo` (model name, count), `/proc/meminfo` (MemTotal), `uname -r` via libc, allocator env vars (`MALLOC_ARENA_MAX`, `MALLOC_CONF`, `LD_PRELOAD`), `git rev-parse HEAD` and `git rev-parse <fixture>:<path>` for tree OID, provider env (`LEINDEX_ONNX_INFERENCE_BATCH_SIZE`, `LEINDEX_WORKER_ORT_THREADS`, etc.). Use `std::process::Command` for git.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memcheck test_capture_records_kernel_and_git`
Expected: PASS

- [ ] **Step 5: Wire into MemcheckReport**

In `report.rs`, add `pub environment: EnvironmentCapture` to `MemcheckReport` (with `#[serde(default)]` for backward compat with existing baseline JSON). In `main.rs`, call `EnvironmentCapture::capture()` before workload and include in report.

- [ ] **Step 6: Run full memcheck test suite + clippy**

Run: `cargo test -p memcheck && cargo clippy -p memcheck -- -D warnings`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add tools/memcheck/src/env_capture.rs tools/memcheck/src/main.rs tools/memcheck/src/report.rs
git commit -m "feat(memcheck): add §14 environment capture (hw/kernel/git/corpus/model digests)"
```

---

## Task 2: GPU state sampling

**Why:** Spec §14 item 8 + §2.1 require GPU VRAM/utilization reporting. Currently sampler.rs only reads `/proc`. Spec §7 budget ledger must count GPU memory.

**Files:**
- Modify: `tools/memcheck/src/sampler.rs` (add `GpuSample`, extend `MemorySample`)
- Modify: `tools/memcheck/src/report.rs` (`PhaseReport` gains `gpu_vram_mib: Option<u64>`)

- [ ] **Step 1: Write failing test**

```rust
// in sampler.rs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GpuSample {
    pub vram_used_mib: Option<u64>,
    pub gpu_utilization_pct: Option<u8>,
    pub provider: Option<String>, // "rocm" | "cuda" | "migraphx"
}

pub fn sample_gpu() -> GpuSample { todo!() }

#[cfg(test)]
#[test]
fn test_gpu_sample_returns_some_on_amdgpu_or_none_elsewhere() {
    let s = sample_gpu();
    // On a box with ROCm: vram_used_mib is Some. On headless CI: all None.
    // Either is valid; we just assert it doesn't panic.
    let _ = s.vram_used_mib;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p memcheck test_gpu_sample`
Expected: FAIL

- [ ] **Step 3: Implement `sample_gpu()`**

Try `rocm-smi --showmeminfo vram --json` (AMD/ROCm/MIGraphX), then `nvidia-smi --query-gpu=memory.used,utilization.gpu --format=csv,noheader,nounits` (CUDA). Parse first device only. Return `GpuSample::default()` (all None) when neither tool exists (headless CI).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p memcheck test_gpu_sample`
Expected: PASS (None on CI, Some on GPU box)

- [ ] **Step 5: Extend MemorySample + PhaseReport**

Add `pub gpu: GpuSample` to `MemorySample`. In `workload.rs`, aggregate peak `gpu.vram_used_mib` per phase into `PhaseReport.gpu_vram_mib`.

- [ ] **Step 6: Commit**

```bash
git add tools/memcheck/src/sampler.rs tools/memcheck/src/report.rs tools/memcheck/src/workload.rs
git commit -m "feat(memcheck): add GPU VRAM/utilization sampling (rocm-smi/nvidia-smi)"
```

---

## Task 3: Heap-profile snapshots at phase boundaries

**Why:** Spec §14 item 9 + §3 require heap/allocation profiles at phase boundaries to confirm the §3 hypotheses (exact retained-byte split). The `memprof` feature already enables jemalloc.

**Files:**
- Modify: `tools/memcheck/src/sampler.rs` (add `capture_heap_profile()`)
- Modify: `tools/memcheck/src/workload.rs` (call at each phase boundary)
- Modify: `src/bin/leindex.rs` (document `MALLOC_CONF` for `memprof` builds — doc comment only)

- [ ] **Step 1: Write failing test**

```rust
// in sampler.rs
/// Captures a jemalloc heap profile via mallctl when memprof is active.
/// On default (glibc) builds, captures /proc/<pid>/smaps as a fallback.
pub fn capture_heap_profile(pid: u32, phase: &str, out_dir: &Path) -> std::io::Result<PathBuf> {
    todo!()
}

#[cfg(test)]
#[test]
fn test_heap_profile_writes_file() {
    let tmp = tempfile::tempdir().unwrap();
    let p = capture_heap_profile(std::process::id(), "test", tmp.path()).unwrap();
    assert!(p.exists());
}
```

- [ ] **Step 2: Run test to verify it fails → Step 3: Implement**

Initial approach: capture `/proc/<pid>/smaps` (anonymous RSS by mapping) as a phase-boundary snapshot — works on all Linux without requiring the memprof build. Document that `cargo build --features memprof` + `MALLOC_CONF=prof:true` enables jemalloc epoch-based dumping for deeper analysis (add doc comment to `src/bin/leindex.rs`).

- [ ] **Step 4: Run test to verify it passes → Step 5: Wire into workload**

In `workload.rs`, at each `CANONICAL_PHASES` boundary (before and after), call `capture_heap_profile(child_pid, phase, &profile_dir)`.

- [ ] **Step 6: Commit**

```bash
git add tools/memcheck/src/sampler.rs tools/memcheck/src/workload.rs src/bin/leindex.rs
git commit -m "feat(memcheck): capture heap/smaps snapshots at phase boundaries"
```

---

## Task 4: Descendant process-tree counting

**Why:** Spec §14: "The harness must count descendant processes and survive worker restarts." Sampler currently discovers one named child worker; doesn't count the full tree or detect leaked grandchildren.

**Files:**
- Modify: `tools/memcheck/src/sampler.rs` (add `count_descendants(pid) -> DescendantTree`)
- Modify: `tools/memcheck/src/report.rs` (`PhaseReport` gains `descendants: DescendantTree`)

- [ ] **Step 1: Write failing test**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DescendantTree {
    pub total: usize,
    pub by_name: HashMap<String, usize>,
    pub combined_rss_kib: u64,
}

pub fn count_descendants(root_pid: u32) -> std::io::Result<DescendantTree> { todo!() }

#[cfg(test)]
#[test]
fn test_count_descendants_includes_spawned_child() {
    let child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
    let tree = count_descendants(std::process::id()).unwrap();
    assert!(tree.total >= 1);
    child.kill().ok();
}
```

- [ ] **Step 2 → 3: Implement** by walking `/proc/*/stat` PPID fields from `root_pid` down (BFS). Sum RSS.

- [ ] **Step 4 → 5: Wire** — add `descendants: DescendantTree` to `PhaseReport`, sampled at phase peak.

- [ ] **Step 6: Commit**

```bash
git add tools/memcheck/src/sampler.rs tools/memcheck/src/report.rs
git commit -m "feat(memcheck): count descendant process tree per phase"
```

---

## Task 5: Three-client/two-project contention phase

**Why:** Spec §13 scenario 1 + §14 item 6 require a 3-client/2-project *active* contention workload. `mcp_idle_proliferation` only measures *idle* concurrent servers.

**Files:**
- Modify: `tools/memcheck/src/workload.rs` (add `run_contention_phase()`)
- Modify: `CANONICAL_PHASES` to append `"contention_3c_2p"`

- [ ] **Step 1: Write failing test** — spawn 3 `leindex mcp --stdio` children pointing at 2 fixture copies, drive interleaved `tools run leindex_search` calls concurrently for a fixed dwell, assert all 3 respond and combined RSS is captured.
- [ ] **Step 2-4: Implement, verify fail/pass.**
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(memcheck): add 3-client/2-project active contention phase"
```

---

## Task 6: Incremental + query-suite phases

**Why:** Spec §14 items 4-5 require incremental (no-op/one-file/burst/delete) and fixed query suite (cold/warm). Current phases only do `index`/`reindex`/`query` (single).

**Files:**
- Modify: `tools/memcheck/src/workload.rs`

- [ ] **Step 1:** Add phases: `incremental_noop`, `incremental_one_file`, `incremental_burst`, `incremental_delete`, `query_suite_cold`, `query_suite_warm`, `full_index_run2`, `full_index_run3`. Each drives the child `leindex` via `tools run` and samples RSS.
- [ ] **Step 2-5:** TDD each phase: failing test → implement → pass → commit per phase.
- [ ] **Step 6: Commit** (one commit per phase, or grouped if small).

---

## Task 7 (WS2): Env-configurable Tokio worker count

**Why:** Spec §8.1: "Daemon Tokio workers: start at 2; benchmark 2–4." Currently `#[tokio::main]` uses `available_parallelism`. This is a *real code change to leindex*, measured by memcheck.

**Files:**
- Modify: `src/bin/leindex.rs`

- [ ] **Step 1: Write failing test** (extract helper for testability)

```rust
// src/bin/leindex.rs
const TOKIO_WORKERS_ENV: &str = "LEINDEX_TOKIO_WORKERS";

fn configured_worker_count() -> usize {
    std::env::var(TOKIO_WORKERS_ENV)
        .ok().and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(2) // §8.1: start at 2
}

fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(configured_worker_count())
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime")
}

// replace #[tokio::main] with:
fn main() -> anyhow::Result<()> {
    let rt = build_runtime();
    rt.block_on(leindex::cli::cli::main())
}

#[cfg(test)]
mod test {
    #[test]
    fn test_default_workers_is_two() {
        std::env::remove_var("LEINDEX_TOKIO_WORKERS");
        assert_eq!(super::configured_worker_count(), 2);
    }
}
```

- [ ] **Step 2-4:** TDD. Verify fail → implement → pass.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat: env-configurable Tokio worker count (LEINDEX_TOKIO_WORKERS, default 2)"
```

---

## Task 8 (WS2): Document MALLOC_ARENA_MAX for default builds

**Why:** Spec §8.2. Default (non-`memprof`) builds use glibc malloc. `MALLOC_ARENA_MAX=2` is a launch-time env, not a runtime knob. This is a *documentation + installer* change, not Rust code.

**Files:**
- Modify: `.env.example`
- Modify: installer script(s) referenced in AGENTS.md (locate first)

- [ ] **Step 1:** Verify the claim via memcheck: add a memcheck run mode that launches the child with `MALLOC_ARENA_MAX=2` env and compares RSS against unset. Record both in baseline JSON.
- [ ] **Step 2:** Add to `.env.example`:
```
# glibc malloc arena cap (default builds only; memprof build uses jemalloc).
# Reduces per-thread arena blowup. Must be set BEFORE leindex starts.
MALLOC_ARENA_MAX=2
```
- [ ] **Step 3: Commit**

```bash
git commit -m "docs: document MALLOC_ARENA_MAX=2 for default glibc builds"
```

---

## Task 9 (WS2): Benchmark Tokio 2 vs 4 + ORT thread sweep

**Why:** Spec §8.1 says "benchmark 2–4", not assume. `default_ort_threads()` is already `3/4 * parallelism`; confirm or adjust.

**Files:** No code change — measurement only.

- [ ] **Step 1:** Run memcheck with `LEINDEX_TOKIO_WORKERS=2` vs `=4`, record RSS + p95 latency in baseline JSON.
- [ ] **Step 2:** Run memcheck `worker_ort_threads` phase sweep (1, 2, 4), record.
- [ ] **Step 3:** Write findings to `docs/baselines/2026-08-04-ws2-thread-sweep.md`. If 4 workers materially helps latency within budget, note for the daemon workstream.
- [ ] **Step 4: Commit**

```bash
git add docs/baselines/2026-08-04-ws2-thread-sweep.md
git commit -m "docs(ws2): record Tokio/ORT thread-sweep measurements"
```

---

## Task 10: Full validation + pristine v1.9.x pre-baseline (the "before" anchor)

**Critical:** This task captures the IMMUTABLE pre-v2.0.0 baseline *before any architectural change lands*. It is the comparison anchor for the shipped `BENCHMARKS.md` and all marketing claims. Run it FIRST in SP1, before any code change from Tasks 1-9 ships to default builds (Tasks 1-9 are additive memcheck extensions + flag-gated containment; capture the anchor against the unmodified release binary).

- [ ] **Step 1: Validation suite**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: PASS (zero warnings per AGENTS.md zero-tolerance policy)

- [ ] **Step 2: Build the unmodified v1.9.x release binary** (the "before" subject)

```bash
cargo build --release --bin leindex
```

- [ ] **Step 3: Capture the pristine pre-baseline** against this repo + ≥1 large-project fixture

For each corpus, measure and record to `docs/baselines/2026-08-04-pre-v190-anchor.json`:
- `.leindex/` total bytes + breakdown (`du` of db/generations/jobs/*.bin)
- Generation count + per-generation size + dedup ratio (sha256-identical detection)
- Steady-state RSS (daemon/MCP idle warm) via memcheck `idle_warm`
- Index-peak RSS via memcheck `index` phase (main + worker + combined)
- p50/p95/p99 of a fixed query suite, cold + warm
- Full-index wall time (3 runs)

This file is the **immutable before-anchor** — do not overwrite it in later workstreams.

- [ ] **Step 4: Run memcheck 3× on representative corpus**; verify no monotonic RSS growth (spec §13 scenario 13).

- [ ] **Step 5: Confirm §3 hypotheses or reject them**

Using heap profiles (Task 3) + descendant counts (Task 4) + environment capture (Task 1), fill the §3 hypothesis table (retained-byte split among enriched content, tokens, parse sigs, PDG/checkpoint, vector staging, fragment rows, fragmentation). Write to `docs/baselines/2026-08-04-ws1-hypothesis-confirmation.md`.

- [ ] **Step 6: Commit**

```bash
git add docs/baselines/
git commit -m "docs(ws1): pristine v1.9.x pre-baseline anchor + §3 hypothesis confirmation"
```

---

## Handoff Summary (fill after execution)

```text
Workstream: WS1-2
Revision: 1.0
Invariant status: anti-cheat rules preserved; no behavior disabled/skipped/shrunk
Files changed: tools/memcheck/src/{env_capture,sampler,workload,report,main}.rs, src/bin/leindex.rs, .env.example
Tests run/results: [fill]
Benchmark artifacts: docs/baselines/2026-08-04-ws1-baseline.json, ws2-thread-sweep.md, ws1-hypothesis-confirmation.md
Before/after resource table: [fill from memcheck diff]
Before/after quality table: N/A (no retrieval behavior changed)
Unverified assumptions: [fill — e.g., GPU sampling path on CI]
Known risks: contention phase may be flaky on 1-core CI
Rollback: revert commits; memcheck extensions are additive
Next workstream prerequisites: WS3 (daemon) needs confirmed §3 hypotheses + working env capture
```

---

## Spec-coverage check (§14)

| §14 requirement | Task |
|---|---|
| Hardware/kernel/allocator/provider/model/git/corpus capture | Task 1 |
| GPU sampling | Task 2 |
| Heap profiles at phase boundaries | Task 3 |
| Descendant tree counting | Task 4 |
| 3-client/2-project contention | Task 5 |
| Incremental workloads + query suite cold/warm | Task 6 |
| Full index 3× runs | Task 6 (`full_index_run2/3`) |
| Machine-readable JSON | Existing memcheck + Tasks 1-4 extend it |
| Tokio worker benchmark | Tasks 7, 9 |
| MALLOC_ARENA_MAX measurement | Tasks 8, 9 |
| Confirms/rejects §3 hypotheses | Task 10 |
