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

    let config = workload::WorkloadConfig {
        binary,
        fixture: fixture.clone(),
        sample_interval: std::time::Duration::from_millis(args.sample_interval_ms),
        verbose: args.verbose,
        worker_binary: Some(
            args.binary
                .as_ref()
                .map(|p| {
                    // If the user specified a binary path, look for the worker
                    // in the same directory
                    let dir = p.parent().unwrap_or(Path::new("."));
                    dir.join("leindex-embed").to_path_buf()
                })
                .unwrap_or_else(|| {
                    workspace_root
                        .join("target")
                        .join("release")
                        .join("leindex-embed")
                }),
        ),
    };

    let phases = workload::run_workload(&config)?;

    let full_report = report::MemcheckReport {
        fixture: config.fixture.display().to_string(),
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
