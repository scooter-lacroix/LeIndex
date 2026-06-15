//! Xtask runner for LeIndex development tasks.
//!
//! Provides `cargo xtask <subcommand>` entrypoints for local development
//! workflows including memory measurement via the memcheck harness.
//!
//! VAL-MEASURE-012: `cargo xtask memcheck` is the supported local entrypoint.
//! VAL-MEASURE-013: `cargo xtask memcheck --update-baseline` regenerates baselines.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

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
    ///
    /// Builds the release binary if needed, runs the memcheck workload,
    /// diffs against committed baselines and budget ceilings, and exits
    /// non-zero on regression (VAL-MEASURE-012).
    Memcheck {
        /// Update committed baselines instead of comparing (VAL-MEASURE-013).
        #[arg(long)]
        update_baseline: bool,
    },
}

/// Workspace root — xtask lives at `<root>/tools/xtask`.
fn workspace_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Some(Commands::Memcheck { update_baseline }) => run_memcheck(update_baseline),
        None => {
            Args::parse_from(["xtask", "--help"]);
            Ok(())
        }
    }
}

fn run_memcheck(update_baseline: bool) -> Result<()> {
    let root = workspace_root();
    let fixture = root.join("tests/fixtures/memcheck/small_repo");
    let leindex_bin = root.join("target/release/leindex");
    let leindex_embed_bin = root.join("target/release/leindex-embed");
    let memcheck_bin = root.join("target/release/memcheck");

    // Ensure the release binary exists
    if !leindex_bin.exists() {
        eprintln!("xtask: building release binary...");
        let status = Command::new("cargo")
            .args(["build", "--release", "-p", "leindex", "--features", "onnx"])
            .current_dir(&root)
            .status()
            .context("failed to run cargo build")?;
        if !status.success() {
            anyhow::bail!("cargo build --release failed");
        }
    }

    // Ensure the worker binary exists (for worker-active phases)
    if !leindex_embed_bin.exists() {
        eprintln!("xtask: building leindex-embed worker binary...");
        let status = Command::new("cargo")
            .args([
                "build",
                "--release",
                "-p",
                "leindex-embed",
                "--features",
                "onnx",
            ])
            .current_dir(&root)
            .status()
            .context("failed to build leindex-embed")?;
        if !status.success() {
            eprintln!(
                "xtask: warning — leindex-embed build failed, worker-active phases will be skipped"
            );
        }
    }

    // Ensure the memcheck binary exists
    if !memcheck_bin.exists() {
        eprintln!("xtask: building memcheck binary...");
        let status = Command::new("cargo")
            .args(["build", "--release", "-p", "memcheck"])
            .current_dir(&root)
            .status()
            .context("failed to build memcheck")?;
        if !status.success() {
            anyhow::bail!("cargo build -p memcheck failed");
        }
    }

    if !fixture.exists() {
        anyhow::bail!("fixture not found at {}", fixture.display());
    }

    // Build the memcheck command
    let mut cmd = Command::new(&memcheck_bin);
    cmd.arg(&fixture)
        .arg("--verbose")
        .arg("--output")
        .arg("target/memcheck-report.json");

    if update_baseline {
        cmd.arg("--update-baseline");
    }

    eprintln!("xtask: running memcheck against {}...", fixture.display());
    if update_baseline {
        eprintln!("  mode: update-baseline");
    } else {
        eprintln!("  mode: compare against baselines + budget");
    }

    // Run memcheck and propagate its exit code
    let status = cmd.status().context("failed to run memcheck")?;

    if !status.success() {
        anyhow::bail!("memcheck exited with {}", status);
    }

    Ok(())
}
