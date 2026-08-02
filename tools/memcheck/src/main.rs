//! Memcheck harness for LeIndex memory measurement.
//!
//! This binary drives a deterministic workload against a fresh `leindex`
//! process, samples RSS at regular intervals, and writes a JSON report.
//!
//! Canonical phases: idle_warm → index → idle_post → query → reindex → idle_final

mod diff;
mod report;
mod sampler;
mod workload;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};

/// Memcheck harness for LeIndex memory measurement.
#[derive(Parser, Debug)]
#[command(
    name = "memcheck",
    version,
    about = "LeIndex memory measurement harness"
)]
struct Args {
    /// Path to the fixture directory to measure.
    fixture: PathBuf,

    /// Path to write the JSON report.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Path to the leindex binary (default: auto-detect from target/release).
    #[arg(long)]
    binary: Option<PathBuf>,

    /// Sampling interval in milliseconds (default: 250).
    #[arg(long, default_value = "250")]
    sample_interval_ms: u64,

    /// Update committed baselines instead of comparing.
    #[arg(long)]
    update_baseline: bool,

    /// Path to the baselines directory (default: <workspace>/docs/memory/baselines).
    #[arg(long)]
    baselines_dir: Option<PathBuf>,

    /// Path to the budget file (default: <workspace>/docs/memory/budgets/current.json).
    #[arg(long)]
    budget_path: Option<PathBuf>,

    /// Print verbose output.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let fixture = args
        .fixture
        .canonicalize()
        .with_context(|| format!("fixture path does not exist: {}", args.fixture.display()))?;

    let workspace_root = diff::find_workspace_root(&fixture)?;

    let binary = match args.binary {
        Some(ref p) => p.clone(),
        None => {
            // Auto-detect: look for target/release/leindex relative to workspace
            workspace_root
                .join("target")
                .join("release")
                .join("leindex")
        }
    };

    if !binary.exists() {
        anyhow::bail!(
            "leindex binary not found at {}. Build with: cargo build --release --bin leindex",
            binary.display()
        );
    }

    let baselines_dir = args
        .baselines_dir
        .unwrap_or_else(|| workspace_root.join("docs/memory/baselines"));
    let budget_path = args
        .budget_path
        .unwrap_or_else(|| workspace_root.join("docs/memory/budgets/current.json"));

    if args.verbose {
        eprintln!("memcheck: starting harness");
        eprintln!("  binary:  {}", binary.display());
        eprintln!("  fixture: {}", fixture.display());
        eprintln!("  interval: {}ms", args.sample_interval_ms);
        eprintln!("  baselines: {}", baselines_dir.display());
        eprintln!("  budget: {}", budget_path.display());
        if args.update_baseline {
            eprintln!("  mode: update-baseline");
        }
        if let Some(ref output) = args.output {
            eprintln!("  output:  {}", output.display());
        }
    }

    let isolated_fixture = workload::copy_fixture_source(&fixture)?;
    let config = workload::WorkloadConfig {
        binary,
        fixture: isolated_fixture.path().to_path_buf(),
        sample_interval: std::time::Duration::from_millis(args.sample_interval_ms),
        verbose: args.verbose,
        worker_binary: resolve_worker_binary(args.binary.as_deref(), &workspace_root),
    };

    let phases = workload::run_workload(&config)?;

    let full_report = report::MemcheckReport {
        // Report the CANONICAL fixture path (the user-supplied small_repo,
        // already canonicalized above), not the disposable isolated copy the
        // workload actually measured against. The copy exists only to keep
        // index writes out of the source fixture (VAL-MEASURE-004); recording
        // the temp path would make reports non-reproducible and fail
        // `test_report_json_is_valid_and_parseable`, which asserts the
        // fixture field references the canonical fixture name.
        fixture: fixture.display().to_string(),
        phases,
        timestamp: chrono_now(),
    };

    // Write report to file or stdout
    let json = serde_json::to_string_pretty(&full_report).context("failed to serialize report")?;
    match args.output {
        Some(ref path) => {
            std::fs::write(path, &json)
                .with_context(|| format!("failed to write report to {}", path.display()))?;
            if args.verbose {
                eprintln!("memcheck: report written to {}", path.display());
            }
        }
        None => {
            // Don't print JSON to stdout when doing diff — it would mix with diff output
            if !args.update_baseline {
                // Still write to a temp location for diff
            }
        }
    }

    // Extract fixture name for baseline operations
    let fixture_name = fixture
        .file_name()
        .map(|n| n.to_str().unwrap_or("unknown"))
        .unwrap_or("unknown");

    if args.update_baseline {
        // VAL-MEASURE-008 / VAL-MEASURE-013: overwrite canonical baseline files
        diff::write_all_baselines(&baselines_dir, fixture_name, &full_report.phases)?;
        eprintln!(
            "memcheck: updated {} baseline files in {}/{}",
            full_report.phases.len(),
            baselines_dir.display(),
            fixture_name
        );
        return Ok(());
    }

    // Diff against baselines and budget
    let budget = diff::load_budget(&budget_path)?;
    let diff_result = diff::diff_report(&full_report, &baselines_dir, &budget);

    // Print diff summary
    let diff_output = diff::format_diff(&diff_result);
    eprintln!("{}", diff_output);

    if !diff_result.all_passed {
        anyhow::bail!("memcheck: regression detected — one or more phases exceeded thresholds");
    }

    Ok(())
}

/// Resolve the `leindex-embed` worker binary path for the worker-active phases.
///
/// Probe order (first existing candidate wins):
/// 1. `LEINDEX_WORKER_BINARY` env override (explicit, e.g. a custom install dir)
/// 2. Alongside the user-specified main binary (`--binary`): `leindex-embed`
///    (and `leindex-embed.exe` on Windows)
/// 3. `target/release/leindex-embed` (canonical release layout)
/// 4. `target/debug/leindex-embed` (debug layout — `cargo test --workspace`
///    builds the worker bin in debug, so CI/harness runs without a release
///    worker still get real worker-active phases instead of u64::MAX sentinels)
///
/// Falls back to the canonical release path when nothing exists so the
/// workload's "worker not found" warning + loud-failure budget gate still
/// behave as designed (a genuinely-missing worker must fail loudly, not pass
/// trivially).
fn resolve_worker_binary(main_binary: Option<&Path>, workspace_root: &Path) -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("LEINDEX_WORKER_BINARY") {
        let candidate = PathBuf::from(env_path);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    if let Some(main) = main_binary {
        let dir = main.parent().unwrap_or(Path::new("."));
        for name in ["leindex-embed", "leindex-embed.exe"] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    let release = workspace_root
        .join("target")
        .join("release")
        .join("leindex-embed");
    if release.exists() {
        return Some(release);
    }
    let debug = workspace_root
        .join("target")
        .join("debug")
        .join("leindex-embed");
    if debug.exists() {
        return Some(debug);
    }
    // No worker found anywhere: keep the canonical release path so the
    // workload's existence check drives the loud-failure path.
    Some(release)
}

/// Get a simple timestamp string.
fn chrono_now() -> String {
    // Use a simple approach without chrono dependency
    let output = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok();
    output
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string())
        .trim()
        .to_string()
}
