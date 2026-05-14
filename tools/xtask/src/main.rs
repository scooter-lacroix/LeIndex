//! Xtask runner for LeIndex development tasks.
//!
//! Provides `cargo xtask <subcommand>` entrypoints for local development
//! workflows including memory measurement via the memcheck harness.
//!
//! This is the Plan 0 bootstrap skeleton — subcommand implementations
//! will be filled in by subsequent features.

use anyhow::Result;
use clap::{Parser, Subcommand};

/// LeIndex development task runner.
#[derive(Parser, Debug)]
#[command(name = "xtask", version, about = "LeIndex development task runner")]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the memcheck harness against the canonical small fixture.
    Memcheck {
        /// Update committed baselines instead of comparing.
        #[arg(long)]
        update_baseline: bool,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Some(Commands::Memcheck { update_baseline }) => {
            eprintln!(
                "xtask memcheck: bootstrap skeleton — not yet implemented (update_baseline={})",
                update_baseline
            );
            // The full implementation will invoke the memcheck binary
            // and handle baseline comparison/update logic.
            Ok(())
        }
        None => {
            // No subcommand — print help.
            Args::parse_from(["xtask", "--help"]);
            Ok(())
        }
    }
}
