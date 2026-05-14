//! Memcheck harness for LeIndex memory measurement.
//!
//! This binary drives a deterministic workload against a fresh `leindex`
//! process, samples RSS at regular intervals, and writes a JSON report.
//!
//! This is the Plan 0 bootstrap skeleton — the harness, sampler, workload,
//! report, and diff logic will be filled in by subsequent features.

use anyhow::Result;
use clap::Parser;

/// Memcheck harness for LeIndex memory measurement.
#[derive(Parser, Debug)]
#[command(
    name = "memcheck",
    version,
    about = "LeIndex memory measurement harness"
)]
struct Args {
    /// Path to the fixture directory to measure.
    fixture: Option<String>,

    /// Path to write the JSON report.
    #[arg(long)]
    output: Option<String>,

    /// Update committed baselines instead of comparing.
    #[arg(long)]
    update_baseline: bool,

    /// Print verbose output.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.verbose {
        eprintln!("memcheck: bootstrap skeleton");
        if let Some(ref fixture) = args.fixture {
            eprintln!("  fixture: {}", fixture);
        }
        if let Some(ref output) = args.output {
            eprintln!("  output: {}", output);
        }
        if args.update_baseline {
            eprintln!("  mode: update-baseline");
        }
    }

    // Plan 0 bootstrap: skeleton only.
    // The full harness implementation (sampler, workload driver, report writer,
    // diff logic) will be added by the next feature in the plan0-foundation milestone.
    eprintln!("memcheck: harness skeleton — not yet implemented");

    Ok(())
}
