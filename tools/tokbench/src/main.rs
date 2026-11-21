//! tokbench — deterministic offline benchmark: LeIndex vs Claude Code
//! native tools over a pinned 7-task corpus (brief §4.6, §7 measurement).
//!
//! `cargo xtask tokbench` orchestrates the build and invokes this binary;
//! the binary itself is also runnable directly:
//!
//! ```text
//! tokbench run  [--output report.json]   # markdown table to stdout + JSON
//! tokbench gate [--output report.json]   # same, exits non-zero unless every
//!                                        # task has ≥40% fewer tokens, ≤ round
//!                                        # trips and equal success
//! ```
//!
//! The gate is NOT wired into CI (SPEC: integration decides). It is the
//! release gate brief §7 describes.

mod corpus;
mod leindex_arm;
mod metrics;
mod native;
mod report;
mod tasks;
mod tokens;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// tokbench: token-efficiency benchmark harness.
#[derive(Parser, Debug)]
#[command(
    name = "tokbench",
    version,
    about = "Deterministic offline benchmark: LeIndex vs Claude Code native tools"
)]
struct Args {
    /// Path to write the JSON report to (accepted before or after the
    /// subcommand; honored by both `run` and `gate`).
    #[arg(long, global = true)]
    output: Option<PathBuf>,
    /// Defaults to `run` when omitted.
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the full 7-task corpus on both arms, print the markdown table,
    /// optionally write the JSON report.
    Run,
    /// Same run, plus gate verdicts; exits non-zero unless every task has
    /// ≥40% fewer tool-output tokens, ≤ round trips and equal success.
    Gate,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let output = args.output;
    let gate_mode = matches!(args.command, Some(Commands::Gate));

    let counter = tokens::Counter::load();
    eprintln!("tokbench: token counter: {}", counter.provenance());
    eprintln!(
        "tokbench: corpus hash {} ({} pinned files)",
        corpus::corpus_hash(),
        corpus::SOURCE_FILES.len()
    );

    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    let run = tasks::run_all(&rt, &counter).context("corpus run")?;

    print!("{}", report::markdown(&run, &counter));
    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&report::json_report(&run, &counter))
                .context("serialize report")?,
        )
        .with_context(|| format!("write {}", path.display()))?;
        eprintln!("tokbench: JSON report written to {}", path.display());
    }

    if gate_mode {
        for line in report::gate_lines(&run) {
            eprintln!("{line}");
        }
        let all_pass = run
            .tasks
            .iter()
            .all(|(n, l)| metrics::task_passes_gate(n, l));
        if !all_pass {
            anyhow::bail!("tokbench gate FAILED");
        }
    }
    Ok(())
}
