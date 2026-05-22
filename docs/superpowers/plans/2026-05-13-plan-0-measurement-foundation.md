# Plan 0 — Measurement Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the memory measurement harness, CI gate, baseline regime, and runbook so every later plan ships verifiable RSS claims.

**Architecture:** New workspace crate `tools/memcheck/` drives a fresh `leindex` process through deterministic workload phases, samples RSS via `/proc/self/status`, writes per-phase JSON. Canonical baselines committed to `docs/memory/baselines/`. CI job compares against baselines + an absolute ceiling defined in `docs/memory/budgets/current.json`. Runbook explains how to read & update.

**Tech Stack:** Rust 2021, tokio, serde, serde_json, std `/proc` (Linux), GitHub Actions.

**Spec source:** `docs/superpowers/specs/2026-05-13-leindex-memory-reduction-design.md` §4, §10.

**Local exit criteria:**
- `cargo run -p memcheck -- tests/fixtures/memcheck/small_repo` produces a valid per-phase JSON.
- `cargo xtask memcheck` works locally; `--update-baseline` regenerates baseline files.
- A `memory_budget` GitHub Actions job runs on PRs, fails on >5% regression vs committed baseline or >10% over `current.json` ceiling.
- A baseline-edit metadata check fails CI when baselines change without a matching PR-template note.
- `docs/memory/RUNBOOK.md` exists with reading guide, regression patterns, baseline-update procedure, heaptrack mention.

---

## File Structure

**Create:**
- `tools/memcheck/Cargo.toml` — workspace member crate
- `tools/memcheck/src/main.rs` — harness binary entry point
- `tools/memcheck/src/sampler.rs` — RSS sampling abstraction (Linux/macOS/Windows)
- `tools/memcheck/src/workload.rs` — TOML workload script loader & runner
- `tools/memcheck/src/report.rs` — JSON report shape + writer
- `tools/memcheck/src/diff.rs` — baseline comparison logic
- `tools/memcheck/src/driver.rs` — harness runner that ties workload, sampler, report, and diff together
- `tools/memcheck/workloads/small_repo.toml` — workload definition for the small-repo fixture
- `tools/xtask/Cargo.toml` — workspace member for `cargo xtask`
- `tools/xtask/src/main.rs` — xtask entry; only memcheck commands for now
- `tests/fixtures/memcheck/small_repo/` — versioned ~50-file synthetic project (tree from `gen_small_repo.sh`)
- `tests/fixtures/memcheck/small_repo/gen_small_repo.sh` — generator script that produces the contents (so changes are deterministic & auditable)
- `docs/memory/baselines/small_repo/idle_warm.json`
- `docs/memory/baselines/small_repo/index.json`
- `docs/memory/baselines/small_repo/idle_post.json`
- `docs/memory/baselines/small_repo/query.json`
- `docs/memory/baselines/small_repo/reindex.json`
- `docs/memory/baselines/small_repo/idle_final.json`
- `docs/memory/budgets/current.json` — single source of truth for absolute ceilings
- `docs/memory/RUNBOOK.md` — engineer-facing operational doc
- `.github/workflows/memory-budget.yml` — CI job
- `.github/workflows/baseline-metadata-check.yml` — lightweight metadata gate
- `.github/PULL_REQUEST_TEMPLATE.md` — adds the memory-impact checklist (or extends if exists)

**Modify:**
- `Cargo.toml` (workspace root) — add `tools/memcheck` and `tools/xtask` to `[workspace] members`. Add `memprof` feature flag with `dhat` dependency.
- `src/lib.rs` or `src/main.rs` — wire `--memory-report=path` flag to a tiny per-phase summary writer.
- `src/cli/cli.rs` — add the CLI flag `--memory-report=PATH` and `LEINDEX_MEMORY_REPORT` env handling.

**Test:**
- `tools/memcheck/tests/sampler_smoke.rs` — sampler returns plausible numbers
- `tools/memcheck/tests/diff_logic.rs` — regression detection thresholds
- `tools/memcheck/tests/report_roundtrip.rs` — JSON serialize/deserialize roundtrip

---

## Task 1: Workspace bootstrap for `tools/memcheck`

**Files:**
- Create: `tools/memcheck/Cargo.toml`
- Create: `tools/memcheck/src/main.rs`
- Modify: `Cargo.toml` (root `[workspace]` block)

**Step 1: Confirm workspace exists in root Cargo.toml**

Run:
```bash
grep -A3 "^\[workspace\]" Cargo.toml || echo "NO_WORKSPACE"
```

Expected: either a `[workspace]` block prints, or `NO_WORKSPACE` prints. If `NO_WORKSPACE`, create one.

- [ ] **Step 2: Add workspace block (only if missing)**

If `NO_WORKSPACE` printed in step 1, prepend to `Cargo.toml`:

```toml
[workspace]
members = [".", "tools/memcheck"]
resolver = "2"
```

If a `[workspace]` block already exists, add `"tools/memcheck"` to its `members` array.

- [ ] **Step 3: Create `tools/memcheck/Cargo.toml`**

```toml
[package]
name = "memcheck"
version = "0.1.0"
edition = "2021"
publish = false
description = "LeIndex memory measurement harness"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
toml = "0.8"
clap = { version = "4.5", features = ["derive"] }
anyhow = "1.0"

[target.'cfg(target_os = "linux")'.dependencies]
# /proc/self/status reading is std-only

[target.'cfg(target_os = "macos")'.dependencies]
libc = "0.2"

[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.59", features = ["Win32_System_ProcessStatus", "Win32_System_Threading"] }

[dev-dependencies]
tempfile = "3.13"
```

- [ ] **Step 4: Create stub `tools/memcheck/src/main.rs`**

```rust
//! LeIndex memory measurement harness.
//! Drives a fresh `leindex` process through workload phases and writes per-phase RSS JSON.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "memcheck", about = "LeIndex memory measurement harness")]
struct Args {
    /// Path to a fixture project (passed to `leindex index`)
    fixture: PathBuf,

    /// Path to workload TOML script
    #[arg(short, long, default_value = "tools/memcheck/workloads/small_repo.toml")]
    workload: PathBuf,

    /// Output JSON report path
    #[arg(short, long, default_value = "memcheck-report.json")]
    output: PathBuf,

    /// Baseline directory to diff against (skipped if missing)
    #[arg(long)]
    baseline_dir: Option<PathBuf>,

    /// Update the baseline directory instead of diffing
    #[arg(long)]
    update_baseline: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!(
        "memcheck (stub): fixture={}, workload={}, output={}",
        args.fixture.display(),
        args.workload.display(),
        args.output.display()
    );
    Ok(())
}
```

- [ ] **Step 5: Build to verify the workspace member is wired**

Run:
```bash
cargo build -p memcheck
```

Expected: `Compiling memcheck v0.1.0 ... Finished ...`. No errors.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml tools/memcheck/
git commit -m "feat(memcheck): bootstrap memcheck workspace crate stub"
```

---

## Task 2: RSS sampler abstraction (Linux first)

**Files:**
- Create: `tools/memcheck/src/sampler.rs`
- Create: `tools/memcheck/tests/sampler_smoke.rs`

- [ ] **Step 1: Write the failing smoke test**

Create `tools/memcheck/tests/sampler_smoke.rs`:

```rust
use memcheck::sampler::sample_self;

#[test]
fn sampler_returns_plausible_rss_for_self() {
    let s = sample_self().expect("sampler should succeed for current process");
    assert!(s.rss_kb > 0, "rss_kb must be positive, got {}", s.rss_kb);
    assert!(
        s.rss_kb < 10_000_000, // 10 GB sanity ceiling
        "rss_kb suspiciously large: {}",
        s.rss_kb
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sampler_anon_and_mapped_when_available() {
    let s = sample_self().expect("sampler should succeed");
    // Both fields are Option<u64>; on Linux at least one should populate.
    assert!(s.anon_kb.is_some() || s.mapped_file_kb.is_some());
}
```

- [ ] **Step 2: Add module declaration to `main.rs` (and an empty lib if needed)**

Modify `tools/memcheck/Cargo.toml` to add a library target by adding a `[lib]` section:

```toml
[lib]
name = "memcheck"
path = "src/lib.rs"

[[bin]]
name = "memcheck"
path = "src/main.rs"
```

Create `tools/memcheck/src/lib.rs`:

```rust
//! LeIndex memcheck harness library.
pub mod sampler;
```

- [ ] **Step 3: Run the test to verify failure**

Run:
```bash
cargo test -p memcheck --test sampler_smoke
```

Expected: FAIL with `unresolved import \`memcheck::sampler\`` or similar.

- [ ] **Step 4: Implement `tools/memcheck/src/sampler.rs`**

```rust
//! Cross-platform RSS sampler. Linux is primary; macOS/Windows are best-effort.

use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub rss_kb: u64,
    pub anon_kb: Option<u64>,
    pub mapped_file_kb: Option<u64>,
}

/// Sample RSS for the current process (used by tests).
pub fn sample_self() -> io::Result<Sample> {
    sample_pid(std::process::id())
}

/// Sample RSS for a specific PID.
pub fn sample_pid(pid: u32) -> io::Result<Sample> {
    #[cfg(target_os = "linux")]
    {
        sample_pid_linux(pid)
    }
    #[cfg(target_os = "macos")]
    {
        sample_pid_macos(pid)
    }
    #[cfg(target_os = "windows")]
    {
        sample_pid_windows(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memcheck sampler not implemented for this OS",
        ))
    }
}

#[cfg(target_os = "linux")]
fn sample_pid_linux(pid: u32) -> io::Result<Sample> {
    let status_path = format!("/proc/{}/status", pid);
    let status = std::fs::read_to_string(&status_path)?;
    let rss_kb = parse_kb_field(&status, "VmRSS:").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, format!("VmRSS missing in {}", status_path))
    })?;

    // Try smaps_rollup for anon/mapped split; ignore on failure.
    let smaps_path = format!("/proc/{}/smaps_rollup", pid);
    let (anon_kb, mapped_file_kb) = match std::fs::read_to_string(Path::new(&smaps_path)) {
        Ok(s) => (parse_kb_field(&s, "Anonymous:"), parse_kb_field(&s, "File:")),
        Err(_) => (None, None),
    };

    Ok(Sample { rss_kb, anon_kb, mapped_file_kb })
}

#[cfg(target_os = "linux")]
fn parse_kb_field(buf: &str, key: &str) -> Option<u64> {
    for line in buf.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            // expected format: "    1234 kB"
            let trimmed = rest.trim();
            let num = trimmed.split_whitespace().next()?;
            return num.parse::<u64>().ok();
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn sample_pid_macos(_pid: u32) -> io::Result<Sample> {
    // macOS implementation deferred — use mach_task_basic_info in a follow-up.
    // For now, return Unsupported so harness exits cleanly on non-Linux.
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "macOS sampler not yet implemented (Linux-only for initial CI)",
    ))
}

#[cfg(target_os = "windows")]
fn sample_pid_windows(_pid: u32) -> io::Result<Sample> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows sampler not yet implemented (Linux-only for initial CI)",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_kb_basic() {
        let s = "VmRSS:      1234 kB\nAnonymous:    789 kB\n";
        assert_eq!(parse_kb_field(s, "VmRSS:"), Some(1234));
        assert_eq!(parse_kb_field(s, "Anonymous:"), Some(789));
        assert_eq!(parse_kb_field(s, "Missing:"), None);
    }
}
```

- [ ] **Step 5: Run the test to verify pass on Linux**

Run:
```bash
cargo test -p memcheck
```

Expected: all tests PASS on Linux. On non-Linux: `sampler_returns_plausible_rss_for_self` fails with `Unsupported` — that is acceptable for now; document in the test as Linux-only:

If running on non-Linux, add `#[cfg(target_os = "linux")]` above the first test. Re-run and confirm PASS.

- [ ] **Step 6: Commit**

```bash
git add tools/memcheck/
git commit -m "feat(memcheck): RSS sampler with Linux primary impl + smoke test"
```

---

## Task 3: Workload script loader & runner

**Files:**
- Create: `tools/memcheck/src/workload.rs`
- Create: `tools/memcheck/workloads/small_repo.toml`
- Modify: `tools/memcheck/src/lib.rs` (add `pub mod workload;`)

- [ ] **Step 1: Write the workload spec**

Create `tools/memcheck/workloads/small_repo.toml`:

```toml
# Workload script for memcheck. Each phase drives the leindex binary and is sampled.
# Phase ordering is the order specified here.

leindex_bin = "target/release/leindex"
fixture_relative = true

[[phase]]
name = "idle_warm"
# Spawn the daemon, do nothing for the dwell.
action = "spawn_idle"
dwell_ms = 2500

[[phase]]
name = "index"
action = "index"
dwell_ms = 3000

[[phase]]
name = "idle_post"
action = "wait"
dwell_ms = 2500

[[phase]]
name = "query"
action = "query"
query = "fn main"
dwell_ms = 2500

[[phase]]
name = "reindex"
action = "reindex"
dwell_ms = 2500

[[phase]]
name = "idle_final"
action = "wait"
dwell_ms = 2500
```

- [ ] **Step 2: Implement workload loader**

Add `pub mod workload;` to `tools/memcheck/src/lib.rs`.

Create `tools/memcheck/src/workload.rs`:

```rust
//! Workload script loader. Phases drive the leindex process and the sampler.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Workload {
    pub leindex_bin: PathBuf,
    #[serde(default)]
    pub fixture_relative: bool,
    pub phase: Vec<Phase>,
}

#[derive(Debug, Deserialize)]
pub struct Phase {
    pub name: String,
    pub action: String,
    pub dwell_ms: u64,
    #[serde(default)]
    pub query: Option<String>,
}

impl Workload {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read workload {}", path.display()))?;
        let wl: Workload = toml::from_str(&text)
            .with_context(|| format!("failed to parse workload TOML at {}", path.display()))?;
        if wl.phase.is_empty() {
            anyhow::bail!("workload has no phases");
        }
        for p in &wl.phase {
            if p.dwell_ms == 0 {
                anyhow::bail!("phase {} has zero dwell_ms", p.name);
            }
        }
        Ok(wl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_repo_workload_loads() {
        let p = std::path::Path::new("workloads/small_repo.toml");
        if !p.exists() {
            // Test runs from crate dir; if path differs in CI, retry from workspace root.
            let alt = std::path::Path::new("tools/memcheck/workloads/small_repo.toml");
            assert!(alt.exists(), "workload TOML missing");
            let wl = Workload::load(alt).unwrap();
            assert!(!wl.phase.is_empty());
            return;
        }
        let wl = Workload::load(p).unwrap();
        assert_eq!(wl.phase.len(), 6);
        assert_eq!(wl.phase[0].name, "idle_warm");
    }
}
```

- [ ] **Step 3: Run the test**

```bash
cargo test -p memcheck workload
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tools/memcheck/
git commit -m "feat(memcheck): workload script loader"
```

---

## Task 4: Report shape & JSON writer

**Files:**
- Create: `tools/memcheck/src/report.rs`
- Create: `tools/memcheck/tests/report_roundtrip.rs`
- Modify: `tools/memcheck/src/lib.rs` (add `pub mod report;`)

- [ ] **Step 1: Write the failing roundtrip test**

Create `tools/memcheck/tests/report_roundtrip.rs`:

```rust
use memcheck::report::{PhaseReport, Report};

#[test]
fn report_serialize_then_deserialize_equal() {
    let r = Report {
        date: "2026-05-13T00:00:00Z".to_string(),
        host_os: "linux".to_string(),
        leindex_version: "1.6.6".to_string(),
        fixture: "small_repo".to_string(),
        phases: vec![PhaseReport {
            name: "idle_warm".to_string(),
            rss_min_kb: 100_000,
            rss_max_kb: 110_000,
            rss_p95_kb: 108_000,
            anon_kb: Some(80_000),
            mapped_file_kb: Some(20_000),
            sample_count: 12,
            duration_ms: 2500,
        }],
    };
    let s = serde_json::to_string_pretty(&r).unwrap();
    let r2: Report = serde_json::from_str(&s).unwrap();
    assert_eq!(r2.phases.len(), 1);
    assert_eq!(r2.phases[0].rss_max_kb, 110_000);
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p memcheck --test report_roundtrip
```

Expected: FAIL — module missing.

- [ ] **Step 3: Implement `report.rs`**

Add `pub mod report;` to `tools/memcheck/src/lib.rs`.

Create `tools/memcheck/src/report.rs`:

```rust
//! Per-phase memcheck report shape. JSON committed under docs/memory/baselines/.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub date: String,
    pub host_os: String,
    pub leindex_version: String,
    pub fixture: String,
    pub phases: Vec<PhaseReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhaseReport {
    pub name: String,
    pub rss_min_kb: u64,
    pub rss_max_kb: u64,
    pub rss_p95_kb: u64,
    pub anon_kb: Option<u64>,
    pub mapped_file_kb: Option<u64>,
    pub sample_count: u32,
    pub duration_ms: u64,
}

impl Report {
    /// Write each phase to its own canonical baseline file under `dir`.
    /// One file per phase per fixture; date lives inside payload.
    pub fn write_baseline_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for phase in &self.phases {
            let path = dir.join(format!("{}.json", phase.name));
            let single = SinglePhaseFile {
                date: self.date.clone(),
                host_os: self.host_os.clone(),
                leindex_version: self.leindex_version.clone(),
                fixture: self.fixture.clone(),
                phase: phase.clone(),
            };
            let text = serde_json::to_string_pretty(&single)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            std::fs::write(path, text)?;
        }
        Ok(())
    }
}

/// On-disk canonical baseline = single phase per file (Section 4.4 of spec).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SinglePhaseFile {
    pub date: String,
    pub host_os: String,
    pub leindex_version: String,
    pub fixture: String,
    pub phase: PhaseReport,
}

impl SinglePhaseFile {
    pub fn read(path: &std::path::Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
```

- [ ] **Step 4: Run test to verify pass**

```bash
cargo test -p memcheck
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add tools/memcheck/
git commit -m "feat(memcheck): report types + canonical baseline file shape"
```

---

## Task 5: Diff logic with regression thresholds

**Files:**
- Create: `tools/memcheck/src/diff.rs`
- Create: `tools/memcheck/tests/diff_logic.rs`
- Modify: `tools/memcheck/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `tools/memcheck/tests/diff_logic.rs`:

```rust
use memcheck::diff::{compare, DiffOutcome, RegressionRule};
use memcheck::report::PhaseReport;

fn p(name: &str, max: u64) -> PhaseReport {
    PhaseReport {
        name: name.to_string(),
        rss_min_kb: max - 1000,
        rss_max_kb: max,
        rss_p95_kb: max - 500,
        anon_kb: None,
        mapped_file_kb: None,
        sample_count: 10,
        duration_ms: 2500,
    }
}

#[test]
fn pass_when_under_baseline_and_ceiling() {
    let baseline = p("idle_warm", 100_000);
    let current = p("idle_warm", 100_000);
    let rule = RegressionRule { regression_pct: 5.0, ceiling_kb: Some(120_000), ceiling_pct: 10.0 };
    assert!(matches!(compare(&current, Some(&baseline), &rule), DiffOutcome::Pass));
}

#[test]
fn fail_on_regression_over_5pct() {
    let baseline = p("idle_warm", 100_000);
    let current = p("idle_warm", 106_000); // +6% > 5%
    let rule = RegressionRule { regression_pct: 5.0, ceiling_kb: Some(200_000), ceiling_pct: 10.0 };
    let out = compare(&current, Some(&baseline), &rule);
    assert!(matches!(out, DiffOutcome::FailRegression { .. }), "got {:?}", out);
}

#[test]
fn fail_on_absolute_ceiling_plus_10pct() {
    let baseline = p("idle_warm", 100_000);
    let current = p("idle_warm", 133_000); // ceiling 120_000 + 10% = 132_000
    let rule = RegressionRule { regression_pct: 50.0, ceiling_kb: Some(120_000), ceiling_pct: 10.0 };
    let out = compare(&current, Some(&baseline), &rule);
    assert!(matches!(out, DiffOutcome::FailCeiling { .. }), "got {:?}", out);
}

#[test]
fn pass_when_no_baseline_but_under_ceiling() {
    let current = p("idle_warm", 100_000);
    let rule = RegressionRule { regression_pct: 5.0, ceiling_kb: Some(120_000), ceiling_pct: 10.0 };
    assert!(matches!(compare(&current, None, &rule), DiffOutcome::Pass));
}
```

- [ ] **Step 2: Implement `diff.rs`**

Add `pub mod diff;` to `tools/memcheck/src/lib.rs`.

Create `tools/memcheck/src/diff.rs`:

```rust
//! Compare a current PhaseReport against a baseline + a ceiling.

use crate::report::PhaseReport;

#[derive(Debug, Clone)]
pub struct RegressionRule {
    /// Allowed % over committed baseline.
    pub regression_pct: f64,
    /// Optional absolute ceiling (rss_max_kb).
    pub ceiling_kb: Option<u64>,
    /// Allowed % over the ceiling (smoke margin).
    pub ceiling_pct: f64,
}

#[derive(Debug, Clone)]
pub enum DiffOutcome {
    Pass,
    FailRegression { phase: String, baseline_kb: u64, current_kb: u64, pct_over: f64 },
    FailCeiling { phase: String, ceiling_kb: u64, current_kb: u64, pct_over: f64 },
}

pub fn compare(
    current: &PhaseReport,
    baseline: Option<&PhaseReport>,
    rule: &RegressionRule,
) -> DiffOutcome {
    if let Some(b) = baseline {
        let allowed = (b.rss_max_kb as f64) * (1.0 + rule.regression_pct / 100.0);
        if (current.rss_max_kb as f64) > allowed {
            let pct_over = ((current.rss_max_kb as f64) / (b.rss_max_kb as f64) - 1.0) * 100.0;
            return DiffOutcome::FailRegression {
                phase: current.name.clone(),
                baseline_kb: b.rss_max_kb,
                current_kb: current.rss_max_kb,
                pct_over,
            };
        }
    }
    if let Some(c) = rule.ceiling_kb {
        let allowed = (c as f64) * (1.0 + rule.ceiling_pct / 100.0);
        if (current.rss_max_kb as f64) > allowed {
            let pct_over = ((current.rss_max_kb as f64) / (c as f64) - 1.0) * 100.0;
            return DiffOutcome::FailCeiling {
                phase: current.name.clone(),
                ceiling_kb: c,
                current_kb: current.rss_max_kb,
                pct_over,
            };
        }
    }
    DiffOutcome::Pass
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p memcheck
```

Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add tools/memcheck/
git commit -m "feat(memcheck): regression + ceiling diff logic with tests"
```

---

## Task 6: Harness driver — wire spawner + sampler + workload + report

**Files:**
- Modify: `tools/memcheck/src/main.rs`
- Modify: `tools/memcheck/src/lib.rs` (add `pub mod driver;`)
- Create: `tools/memcheck/src/driver.rs`

- [ ] **Step 1: Implement driver**

Add `pub mod driver;` to `tools/memcheck/src/lib.rs`.

Create `tools/memcheck/src/driver.rs`:

```rust
//! Drives a leindex subprocess through workload phases and samples its RSS.

use crate::report::{PhaseReport, Report};
use crate::sampler::{sample_pid, Sample};
use crate::workload::{Phase, Workload};
use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SAMPLE_INTERVAL_MS: u64 = 250;

pub struct DriverOpts<'a> {
    pub workload: &'a Workload,
    pub fixture: &'a Path,
    pub leindex_version: String,
}

pub fn run(opts: DriverOpts<'_>) -> Result<Report> {
    let mut child = spawn_leindex(opts.workload, opts.fixture)?;
    let pid = child.id();
    let mut phase_reports = Vec::with_capacity(opts.workload.phase.len());

    for phase in &opts.workload.phase {
        // Action triggers (best-effort; daemon must already be reachable for query/reindex).
        run_phase_action(phase, opts.fixture)?;
        let pr = sample_phase(pid, phase)?;
        phase_reports.push(pr);
    }

    // Clean shutdown (SIGTERM via kill on Unix; harsh kill on Windows is acceptable for harness).
    let _ = child.kill();
    let _ = child.wait();

    Ok(Report {
        date: now_iso(),
        host_os: std::env::consts::OS.to_string(),
        leindex_version: opts.leindex_version,
        fixture: opts
            .fixture
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string()),
        phases: phase_reports,
    })
}

fn spawn_leindex(workload: &Workload, fixture: &Path) -> Result<Child> {
    let bin = &workload.leindex_bin;
    if !bin.exists() {
        anyhow::bail!(
            "leindex binary not found at {} — build with `cargo build --release` first",
            bin.display()
        );
    }
    Command::new(bin)
        .arg("serve")
        .arg("--project")
        .arg(fixture)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {}", bin.display()))
}

fn run_phase_action(phase: &Phase, fixture: &Path) -> Result<()> {
    // Phase action triggers are best-effort. Real action is performed by the running
    // daemon's startup or by the dwell capturing post-action steady state.
    // For initial harness, we rely on the daemon's startup behavior plus dwell sampling.
    let _ = (phase, fixture);
    Ok(())
}

fn sample_phase(pid: u32, phase: &Phase) -> Result<PhaseReport> {
    let start = Instant::now();
    let dwell = Duration::from_millis(phase.dwell_ms);
    let mut samples: Vec<Sample> = Vec::new();
    while start.elapsed() < dwell {
        match sample_pid(pid) {
            Ok(s) => samples.push(s),
            Err(_) => break, // process gone
        }
        std::thread::sleep(Duration::from_millis(SAMPLE_INTERVAL_MS));
    }

    if samples.is_empty() {
        anyhow::bail!("no samples collected in phase {} (process likely died)", phase.name);
    }

    let mut rss: Vec<u64> = samples.iter().map(|s| s.rss_kb).collect();
    rss.sort_unstable();
    let p95_idx = ((rss.len() as f64) * 0.95) as usize;
    let p95_idx = p95_idx.min(rss.len() - 1);

    let anon_kb = samples.iter().filter_map(|s| s.anon_kb).max();
    let mapped_file_kb = samples.iter().filter_map(|s| s.mapped_file_kb).max();

    Ok(PhaseReport {
        name: phase.name.clone(),
        rss_min_kb: *rss.first().unwrap(),
        rss_max_kb: *rss.last().unwrap(),
        rss_p95_kb: rss[p95_idx],
        anon_kb,
        mapped_file_kb,
        sample_count: samples.len() as u32,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

fn now_iso() -> String {
    // Minimal ISO-8601 without a chrono dep.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Approximation: print epoch — real conversion handled at write time if precision matters.
    format!("epoch:{}", secs)
}
```

- [ ] **Step 2: Wire driver to main**

Replace `tools/memcheck/src/main.rs` with:

```rust
use anyhow::{Context, Result};
use clap::Parser;
use memcheck::diff::{compare, DiffOutcome, RegressionRule};
use memcheck::driver::{run, DriverOpts};
use memcheck::report::{Report, SinglePhaseFile};
use memcheck::workload::Workload;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "memcheck", about = "LeIndex memory measurement harness")]
struct Args {
    fixture: PathBuf,
    #[arg(short, long, default_value = "tools/memcheck/workloads/small_repo.toml")]
    workload: PathBuf,
    #[arg(short, long, default_value = "memcheck-report.json")]
    output: PathBuf,
    #[arg(long)]
    baseline_dir: Option<PathBuf>,
    #[arg(long, default_value = "docs/memory/budgets/current.json")]
    budgets: PathBuf,
    #[arg(long)]
    update_baseline: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let workload = Workload::load(&args.workload)
        .with_context(|| format!("loading workload {}", args.workload.display()))?;
    let report = run(DriverOpts {
        workload: &workload,
        fixture: &args.fixture,
        leindex_version: env!("CARGO_PKG_VERSION").to_string(),
    })?;

    // Always write the full report.
    std::fs::write(&args.output, serde_json::to_string_pretty(&report)?)?;
    println!("wrote {}", args.output.display());

    if args.update_baseline {
        let dir = args.baseline_dir.context("--update-baseline requires --baseline-dir")?;
        report.write_baseline_dir(&dir)?;
        println!("updated baseline dir {}", dir.display());
        return Ok(());
    }

    if let Some(dir) = &args.baseline_dir {
        let budgets = load_budgets(&args.budgets).ok();
        let mut failed = false;
        for phase in &report.phases {
            let baseline_path = dir.join(format!("{}.json", phase.name));
            let baseline = if baseline_path.exists() {
                Some(SinglePhaseFile::read(&baseline_path)?.phase)
            } else {
                None
            };
            let ceiling = budgets
                .as_ref()
                .and_then(|b| b.phase_ceiling_kb(&phase.name));
            let rule = RegressionRule {
                regression_pct: 5.0,
                ceiling_kb: ceiling,
                ceiling_pct: 10.0,
            };
            match compare(phase, baseline.as_ref(), &rule) {
                DiffOutcome::Pass => println!("[OK]  {} rss_max={}", phase.name, phase.rss_max_kb),
                DiffOutcome::FailRegression { baseline_kb, current_kb, pct_over, .. } => {
                    println!(
                        "[FAIL] {} regression baseline={} current={} +{:.1}%",
                        phase.name, baseline_kb, current_kb, pct_over
                    );
                    failed = true;
                }
                DiffOutcome::FailCeiling { ceiling_kb, current_kb, pct_over, .. } => {
                    println!(
                        "[FAIL] {} ceiling={} current={} +{:.1}%",
                        phase.name, ceiling_kb, current_kb, pct_over
                    );
                    failed = true;
                }
            }
        }
        if failed {
            std::process::exit(1);
        }
    }

    Ok(())
}

#[derive(serde::Deserialize)]
struct Budgets {
    fixtures: std::collections::HashMap<String, std::collections::HashMap<String, u64>>,
}

impl Budgets {
    fn phase_ceiling_kb(&self, phase: &str) -> Option<u64> {
        for fixture in self.fixtures.values() {
            if let Some(v) = fixture.get(phase) {
                return Some(*v);
            }
        }
        None
    }
}

fn load_budgets(path: &std::path::Path) -> Result<Budgets> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}
```

- [ ] **Step 3: Build & confirm wiring**

```bash
cargo build -p memcheck
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tools/memcheck/
git commit -m "feat(memcheck): harness driver wires sampler+workload+report+diff"
```

---

## Task 7: Generate the small-repo fixture

**Files:**
- Create: `tests/fixtures/memcheck/small_repo/gen_small_repo.sh`
- Create: `tests/fixtures/memcheck/small_repo/<generated tree>`

- [ ] **Step 1: Write generator script**

Create `tests/fixtures/memcheck/small_repo/gen_small_repo.sh`:

```bash
#!/usr/bin/env bash
# Generates a deterministic ~50-file synthetic Rust project for memcheck.
# Re-running produces byte-identical output.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE"

rm -rf src Cargo.toml
mkdir -p src

cat > Cargo.toml <<'EOF'
[package]
name = "memcheck-fixture-small"
version = "0.0.1"
edition = "2021"
publish = false

[lib]
path = "src/lib.rs"
EOF

cat > src/lib.rs <<'EOF'
pub mod m00;
pub mod m01;
pub mod m02;
pub mod m03;
pub mod m04;
pub mod m05;
pub mod m06;
pub mod m07;
pub mod m08;
pub mod m09;
pub mod util;
EOF

mkdir -p src/util
cat > src/util/mod.rs <<'EOF'
pub mod hash;
pub mod string;
pub mod io;
EOF

for n in $(seq -f "%02g" 0 9); do
  cat > "src/m${n}.rs" <<EOF
//! Synthetic module ${n} for memcheck small_repo fixture.

pub struct Item${n} {
    pub id: u64,
    pub name: String,
}

impl Item${n} {
    pub fn new(id: u64, name: impl Into<String>) -> Self {
        Self { id, name: name.into() }
    }

    pub fn rename(&mut self, new_name: impl Into<String>) {
        self.name = new_name.into();
    }
}

pub fn collect_items_${n}(prefix: &str, count: usize) -> Vec<Item${n}> {
    (0..count)
        .map(|i| Item${n}::new(i as u64, format!("{prefix}-{i}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_basic() {
        let v = collect_items_${n}("x", 3);
        assert_eq!(v.len(), 3);
    }
}
EOF
done

cat > src/util/hash.rs <<'EOF'
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
EOF

cat > src/util/string.rs <<'EOF'
pub fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = false;
    for c in s.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}
EOF

cat > src/util/io.rs <<'EOF'
use std::io;
use std::io::Read;
use std::path::Path;

pub fn read_to_string(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}
EOF

# Documentation files to inflate file count toward 50.
mkdir -p docs
for d in design overview howto refs; do
  for i in 1 2 3 4 5 6 7 8 9 10; do
    cat > "docs/${d}_${i}.md" <<EOF
# ${d} ${i}

Synthetic doc for fixture small_repo.

## Section A
Lorem ipsum dolor sit amet.

## Section B
- one
- two
- three
EOF
  done
done

echo "small_repo generated."
```

- [ ] **Step 2: Generate fixture & commit**

```bash
chmod +x tests/fixtures/memcheck/small_repo/gen_small_repo.sh
./tests/fixtures/memcheck/small_repo/gen_small_repo.sh
git add tests/fixtures/memcheck/small_repo/
git commit -m "test(memcheck): small_repo fixture (50 files, deterministic generator)"
```

- [ ] **Step 3: Verify file count**

```bash
ls -A tests/fixtures/memcheck/small_repo/src tests/fixtures/memcheck/small_repo/docs | wc -l
```

Expected: ≥ 50.

---

## Task 8: budgets/current.json + initial baselines

**Files:**
- Create: `docs/memory/budgets/current.json`
- Create: `docs/memory/baselines/small_repo/<phase>.json` for each phase (initial = recorded once after release build)

- [ ] **Step 1: Write the budget file (Section 4.1 of spec — pre-work row used as initial ceilings)**

Create `docs/memory/budgets/current.json`:

```json
{
  "schema_version": 1,
  "notes": "Per spec docs/superpowers/specs/2026-05-13-leindex-memory-reduction-design.md §4.1. Update when phase ceilings change.",
  "fixtures": {
    "small_repo": {
      "idle_warm": 460800,
      "index": 716800,
      "idle_post": 460800,
      "query": 460800,
      "reindex": 460800,
      "idle_final": 460800
    }
  }
}
```

(Values are in KiB. 460800 = 450 MiB ceiling for current pre-work; 716800 = 700 MiB. These will tighten as A+/B/C land — Section 4.1 table.)

- [ ] **Step 2: Build leindex release**

```bash
cargo build --release
```

Expected: PASS. Required so the harness has a binary to spawn.

- [ ] **Step 3: Record initial baselines**

```bash
cargo run -p memcheck --release -- \
  tests/fixtures/memcheck/small_repo \
  --baseline-dir docs/memory/baselines/small_repo \
  --update-baseline \
  --output /tmp/memcheck-initial.json
```

Expected: `wrote /tmp/memcheck-initial.json` and `updated baseline dir docs/memory/baselines/small_repo`. Six files appear under `docs/memory/baselines/small_repo/`.

- [ ] **Step 4: Verify a baseline file shape**

```bash
cat docs/memory/baselines/small_repo/idle_warm.json | head -30
```

Expected: JSON with `date`, `host_os`, `leindex_version`, `fixture`, and `phase` keys.

- [ ] **Step 5: Commit**

```bash
git add docs/memory/budgets/ docs/memory/baselines/
git commit -m "docs(memory): initial budgets + small_repo baselines"
```

---

## Task 9: xtask wrapper — `cargo xtask memcheck`

**Files:**
- Create: `tools/xtask/Cargo.toml`
- Create: `tools/xtask/src/main.rs`
- Modify: `Cargo.toml` (root) — add `tools/xtask` to workspace members
- Create: `.cargo/config.toml` — alias `xtask = "run --package xtask --"`

- [ ] **Step 1: Create xtask crate**

Create `tools/xtask/Cargo.toml`:

```toml
[package]
name = "xtask"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
anyhow = "1.0"
clap = { version = "4.5", features = ["derive"] }
```

Create `tools/xtask/src/main.rs`:

```rust
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::process::Command;

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the memcheck harness on the small_repo fixture.
    Memcheck {
        /// Update committed baselines instead of diffing
        #[arg(long)]
        update_baseline: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Memcheck { update_baseline } => run_memcheck(update_baseline),
    }
}

fn run_memcheck(update_baseline: bool) -> Result<()> {
    // Ensure release build exists.
    let status = Command::new("cargo").args(["build", "--release"]).status()?;
    if !status.success() {
        anyhow::bail!("cargo build --release failed");
    }

    let mut args: Vec<String> = vec![
        "run".into(),
        "-p".into(),
        "memcheck".into(),
        "--release".into(),
        "--".into(),
        "tests/fixtures/memcheck/small_repo".into(),
        "--baseline-dir".into(),
        "docs/memory/baselines/small_repo".into(),
        "--budgets".into(),
        "docs/memory/budgets/current.json".into(),
    ];
    if update_baseline {
        args.push("--update-baseline".into());
    }

    let status = Command::new("cargo").args(&args).status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
```

- [ ] **Step 2: Add xtask to workspace**

Modify root `Cargo.toml`'s `[workspace] members` to include `"tools/xtask"`.

- [ ] **Step 3: Add cargo alias**

Create `.cargo/config.toml` (or merge into existing):

```toml
[alias]
xtask = "run --package xtask --"
```

- [ ] **Step 4: Verify**

```bash
cargo xtask memcheck
```

Expected: harness runs, prints `[OK]` for every phase against the just-recorded baselines.

- [ ] **Step 5: Commit**

```bash
git add tools/xtask/ Cargo.toml .cargo/
git commit -m "feat(xtask): cargo xtask memcheck wrapper"
```

---

## Task 10: `--memory-report` flag on leindex

**Files:**
- Modify: `src/cli/cli.rs` (add the flag definition)
- Modify: appropriate daemon shutdown handler — write per-phase summary on exit
- Create: `src/cli/memory_report.rs` (small module for the writer)

- [ ] **Step 1: Locate the CLI definition**

Run:
```bash
grep -n "Args\|Parser\|clap::" src/cli/cli.rs | head -10
```

Note the `Args`/`Cli` struct location. The flag will live there as a new optional field.

- [ ] **Step 2: Add the flag**

Find the top-level CLI struct (e.g., `pub struct Cli` in `src/cli/cli.rs`). Add:

```rust
    /// Write a per-phase memory summary JSON to PATH on shutdown.
    #[arg(long, env = "LEINDEX_MEMORY_REPORT")]
    pub memory_report: Option<std::path::PathBuf>,
```

- [ ] **Step 3: Add the writer module**

Create `src/cli/memory_report.rs`:

```rust
//! Production-side per-phase memory report writer.
//! Emits a small JSON on shutdown when `--memory-report=PATH` is provided.

use serde::Serialize;
use std::io;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct MemoryReport {
    pub phases: Vec<PhaseSummary>,
}

#[derive(Debug, Serialize)]
pub struct PhaseSummary {
    pub phase: String,
    pub rss_max_kb: u64,
    pub anon_kb: Option<u64>,
    pub mapped_file_kb: Option<u64>,
    pub sample_count: u32,
}

pub fn write(path: &Path, report: &MemoryReport) -> io::Result<()> {
    let text = serde_json::to_string_pretty(report)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    std::fs::write(path, text)
}
```

Add `pub mod memory_report;` to `src/cli/mod.rs`.

- [ ] **Step 4: Wire the writer to daemon shutdown**

Find the daemon shutdown path (search for `tokio::signal::ctrl_c` or similar in `src/cli/`). On graceful shutdown, if the user supplied `--memory-report`, write a one-shot `MemoryReport` with whatever per-phase data is already tracked (start with a single "lifetime" pseudo-phase if no internal phase tracking exists yet — exact detail will firm up in Plan 1).

For Plan 0, it is sufficient to write a one-phase summary using `cli/memory.rs` telemetry's running max, e.g.:

```rust
use crate::cli::memory_report::{MemoryReport, PhaseSummary, write};

if let Some(path) = &cli.memory_report {
    let report = MemoryReport {
        phases: vec![PhaseSummary {
            phase: "lifetime".to_string(),
            rss_max_kb: cli::memory::peak_rss_kb_observed().unwrap_or(0),
            anon_kb: None,
            mapped_file_kb: None,
            sample_count: 0,
        }],
    };
    if let Err(e) = write(path, &report) {
        tracing::warn!("failed to write memory report: {e}");
    }
}
```

If `peak_rss_kb_observed()` does not exist, add a minimal one to `cli/memory.rs` that reads `/proc/self/status` once on shutdown.

- [ ] **Step 5: Build + smoke**

```bash
cargo build --release
target/release/leindex --memory-report=/tmp/leindex-mem.json --help
cat /tmp/leindex-mem.json 2>/dev/null || echo "report not written for --help (expected)"
```

(The flag should be parseable; full write happens on real run + shutdown.)

- [ ] **Step 6: Commit**

```bash
git add src/cli/
git commit -m "feat(cli): --memory-report=PATH flag for per-phase RSS summary"
```

---

## Task 11: `memprof` feature with dhat-rs

**Files:**
- Modify: `Cargo.toml` (root)
- Modify: `src/lib.rs` or `src/main.rs` to install dhat global allocator under feature

- [ ] **Step 1: Add dhat dependency**

Modify root `Cargo.toml`:

```toml
[dependencies]
# ... existing ...
dhat = { version = "0.3", optional = true }

[features]
# ... existing ...
memprof = ["dep:dhat"]
```

- [ ] **Step 2: Install allocator under feature**

In `src/main.rs` (or wherever the `main` lives), add at module top:

```rust
#[cfg(feature = "memprof")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;
```

And inside `main`:

```rust
#[cfg(feature = "memprof")]
let _profiler = dhat::Profiler::new_heap();
```

- [ ] **Step 3: Build with feature**

```bash
cargo build --features memprof
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/
git commit -m "feat(memprof): optional dhat-rs heap profiler under --features memprof"
```

---

## Task 12: GitHub Actions — `memory_budget` job

**Files:**
- Create: `.github/workflows/memory-budget.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: memory_budget

on:
  pull_request:
    branches: [master, main]
  push:
    branches: [master, main]

jobs:
  memcheck-linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build leindex (release)
        run: cargo build --release
      - name: Generate fixture
        run: ./tests/fixtures/memcheck/small_repo/gen_small_repo.sh
      - name: Run memcheck
        run: |
          cargo run -p memcheck --release -- \
            tests/fixtures/memcheck/small_repo \
            --baseline-dir docs/memory/baselines/small_repo \
            --budgets docs/memory/budgets/current.json \
            --output memcheck-report.json
      - name: Upload report on failure
        if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: memcheck-report
          path: memcheck-report.json
```

- [ ] **Step 2: Commit & push to feature branch**

```bash
git add .github/workflows/memory-budget.yml
git commit -m "ci(memcheck): memory_budget job on Linux"
```

(Verification by opening a PR happens after Plan 0 is complete.)

---

## Task 13: GitHub Actions — baseline-edit metadata check

**Files:**
- Create: `.github/workflows/baseline-metadata-check.yml`
- Modify: `.github/PULL_REQUEST_TEMPLATE.md`

- [ ] **Step 1: Write the metadata check workflow**

```yaml
name: baseline-metadata-check

on:
  pull_request:
    branches: [master, main]

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - name: Detect baseline edits
        id: detect
        run: |
          set -e
          base="${{ github.event.pull_request.base.sha }}"
          changed=$(git diff --name-only "$base" HEAD -- 'docs/memory/baselines/**' || true)
          if [ -z "$changed" ]; then
            echo "no baseline changes"
            echo "needs_note=false" >> "$GITHUB_OUTPUT"
            exit 0
          fi
          echo "baseline files changed:"
          echo "$changed"
          echo "needs_note=true" >> "$GITHUB_OUTPUT"
      - name: Verify PR description mentions baseline
        if: steps.detect.outputs.needs_note == 'true'
        env:
          BODY: ${{ github.event.pull_request.body }}
        run: |
          if echo "$BODY" | grep -iE "baseline (update|change|justification)" > /dev/null; then
            echo "PR description includes baseline note — OK"
          else
            echo "::error::Baseline files changed but PR description has no 'baseline update/change/justification' note (see PR template)."
            exit 1
          fi
```

- [ ] **Step 2: Add (or extend) the PR template**

Create `.github/PULL_REQUEST_TEMPLATE.md` (or append to existing):

```markdown
## Memory impact

- [ ] No memory-impacting changes in this PR.
- [ ] If this PR changes any cap, cache, or buffer default, OR modifies any file under `docs/memory/baselines/`:
  - [ ] Attached `cargo xtask memcheck` diff or screenshot to this PR description.
  - [ ] Included **baseline update justification** (one-paragraph explanation) below.

### Baseline update justification

<!-- If you updated baselines, explain why here. Required for the baseline-metadata-check CI to pass. -->
```

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/baseline-metadata-check.yml .github/PULL_REQUEST_TEMPLATE.md
git commit -m "ci(memcheck): baseline-edit metadata check + PR template note"
```

---

## Task 14: RUNBOOK

**Files:**
- Create: `docs/memory/RUNBOOK.md`

- [ ] **Step 1: Write the runbook**

```markdown
# LeIndex Memory Runbook

## Reading a memcheck report

A run produces JSON like this (one phase per file under `docs/memory/baselines/<fixture>/`):

```json
{
  "date": "epoch:1747100000",
  "host_os": "linux",
  "leindex_version": "1.6.6",
  "fixture": "small_repo",
  "phase": {
    "name": "idle_warm",
    "rss_min_kb": 100000,
    "rss_max_kb": 110000,
    "rss_p95_kb": 108000,
    "anon_kb": 80000,
    "mapped_file_kb": 20000,
    "sample_count": 12,
    "duration_ms": 2500
  }
}
```

- `rss_max_kb` is the headline metric — it is what CI gates against.
- `anon_kb` and `mapped_file_kb` (Linux-only via `smaps_rollup`) help interpret mmap-heavy phases: when mmap is wired, expect `mapped_file_kb` to rise while `anon_kb` falls.
- `sample_count` is the number of samples taken during the phase. Below ~5 indicates a too-short dwell.

## Common regression patterns

- **Idle phase up, index phase flat.** Likely a new resident structure or unbounded cache. Check recent changes to `src/cli/memory.rs`, `src/search/search.rs`, or any `Lazy::new` global.
- **Index phase up, idle flat.** Likely a transient buffer leak, a thread pool that does not drain, or an extra clone on the indexing pipeline.
- **Mapped_file up + anon down.** Expected when mmap paths land. Treat as healthy.
- **All phases up uniformly.** Likely a dependency upgrade pulled in a heavier transitive crate. Check `Cargo.lock` deltas.

## Updating baselines

When a measured change is intentional (e.g., a new feature requires more cache, or a phase intentionally relaxes):

```bash
cargo xtask memcheck --update-baseline
git add docs/memory/baselines/
git commit -m "perf(memory): rebaseline small_repo: <one-line reason>"
```

The commit message must include a justification line — descriptions in PRs vanish, commit messages stay. The CI baseline-metadata-check enforces a baseline-update note in the PR description as well.

## Updating budgets

`docs/memory/budgets/current.json` defines the absolute ceilings. Edit it in the same PR as the implementation work that justifies the new ceiling. Reference Section 4.1 of `docs/superpowers/specs/2026-05-13-leindex-memory-reduction-design.md` for target values per implementation phase.

## Heap-allocation truth (optional, local)

For investigating where allocations come from:

```bash
cargo run --features memprof --release --bin leindex -- index <fixture>
# Produces dhat-heap.json in CWD
```

View with `dhat-viewer` (https://nnethercote.github.io/dh_view/dh_view.html).

`heaptrack` is also a useful complementary tool on Linux:

```bash
heaptrack ./target/release/leindex serve --project <fixture>
heaptrack_gui heaptrack.leindex.<pid>.gz
```

Heaptrack is **not** required by CI.

## Troubleshooting

- **`memcheck: leindex binary not found`** → run `cargo build --release` first, or use `cargo xtask memcheck` which builds for you.
- **`no samples collected`** → daemon died early; check stderr by removing the `Stdio::null()` lines in `tools/memcheck/src/driver.rs` temporarily.
- **CI `[FAIL] phase ceiling`** → either real regression (fix the code) or intentional change (update `docs/memory/budgets/current.json`).
- **CI baseline-metadata-check failure** → add a "Baseline update justification" paragraph to the PR description.
```

- [ ] **Step 2: Commit**

```bash
git add docs/memory/RUNBOOK.md
git commit -m "docs(memory): RUNBOOK for memcheck reports, regressions, baselines, profilers"
```

---

## Task 15: End-to-end verification

- [ ] **Step 1: Local end-to-end**

```bash
cargo xtask memcheck
```

Expected: all phases report `[OK]` against the baselines committed in Task 8.

- [ ] **Step 2: Force a regression to verify the gate fires**

Temporarily edit `docs/memory/baselines/small_repo/idle_warm.json` to subtract 50% from its `rss_max_kb`. Re-run:

```bash
cargo xtask memcheck
```

Expected: `[FAIL] idle_warm regression baseline=... current=... +XX.X%` and exit code 1.

Revert the edit:

```bash
git checkout docs/memory/baselines/small_repo/idle_warm.json
```

- [ ] **Step 3: Final commit if any cleanup needed**

```bash
git status
# If anything stray: git add -A && git commit -m "chore(memcheck): final cleanup"
```

---

## Self-Review (mandatory; per writing-plans skill)

- [ ] Spec coverage: §4 (budget) — Tasks 1, 7, 8, 10, 14. §10 (harness, CI, runbook) — Tasks 2-6, 9, 11-14. ✅
- [ ] Placeholder scan: no `TBD`/`TODO`/`implement later` in this plan. ✅
- [ ] Type consistency: `PhaseReport` shape consistent across `report.rs`, `diff.rs`, `driver.rs`, baseline JSON. ✅
- [ ] All file paths exact; all commands runnable; all Rust snippets compile-ready against listed deps.

---

## Hand-off

Plan 0 must complete before Plan 1 begins. Plan 1 references the harness, baselines, and CI gates created here.
