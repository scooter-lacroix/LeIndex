//! Memcheck harness for LeIndex memory measurement.
//!
//! This binary drives a deterministic workload against a fresh `leindex`
//! process, samples RSS at regular intervals, and writes a JSON report.
//!
//! Canonical phases: idle_warm → index → idle_post → query → reindex → idle_final

mod report;
mod sampler;
mod workload;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

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

    let binary = match args.binary {
        Some(p) => p,
        None => {
            // Auto-detect: look for target/release/leindex relative to workspace
            let workspace_root = find_workspace_root(&fixture)?;
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

    if args.verbose {
        eprintln!("memcheck: starting harness");
        eprintln!("  binary:  {}", binary.display());
        eprintln!("  fixture: {}", fixture.display());
        eprintln!("  interval: {}ms", args.sample_interval_ms);
        if let Some(ref output) = args.output {
            eprintln!("  output:  {}", output.display());
        }
    }

    let config = workload::WorkloadConfig {
        binary,
        fixture,
        sample_interval: std::time::Duration::from_millis(args.sample_interval_ms),
        verbose: args.verbose,
    };

    let phases = workload::run_workload(&config)?;

    let full_report = report::MemcheckReport {
        fixture: config.fixture.display().to_string(),
        phases,
        timestamp: chrono_now(),
    };

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
            println!("{}", json);
        }
    }

    Ok(())
}

/// Find the workspace root by walking up from the fixture path looking for Cargo.toml with [workspace].
fn find_workspace_root(start: &PathBuf) -> Result<PathBuf> {
    let mut dir = start.as_path();
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
                if contents.contains("[workspace]") {
                    return Ok(dir.to_path_buf());
                }
            }
        }
        dir = dir.parent().ok_or_else(|| {
            anyhow::anyhow!("could not find workspace root from {}", start.display())
        })?;
    }
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
