//! Memcheck harness for LeIndex memory measurement.
//!
//! This binary drives a deterministic workload against a fresh `leindex`
//! process, samples RSS at regular intervals, and writes a JSON report.
//!
//! Canonical phases: idle_warm → index → idle_post → query → reindex → idle_final

mod diff;
mod env_capture;
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

    /// Path to the baselines directory (default: `<workspace>`/docs/memory/baselines).
    #[arg(long)]
    baselines_dir: Option<PathBuf>,

    /// Path to the budget file (default: `<workspace>`/docs/memory/budgets/current.json).
    #[arg(long)]
    budget_path: Option<PathBuf>,

    /// Print verbose output.
    #[arg(short, long)]
    verbose: bool,

    /// glibc malloc arena cap applied to all child processes (spec §8.2).
    /// Set to 0 to leave MALLOC_ARENA_MAX unset (useful for comparison runs).
    /// Default: 2 (containment default for default/glibc builds).
    #[arg(long, default_value = "2")]
    malloc_arena_max: u32,
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

    // Canonicalize: phases launch children with a different working directory
    // (temp dir, fixture dir), and a relative `--binary` path would be
    // resolved against the CHILD's cwd, where it does not exist.
    let binary = binary
        .canonicalize()
        .with_context(|| format!("failed to resolve binary path {}", binary.display()))?;

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

    // ── Containment default: MALLOC_ARENA_MAX ────────────────────────
    //
    // Spec §8.2: default (non-memprof) builds use glibc malloc, which
    // allocates arenas per-thread. Capping at 2 (the containment default)
    // prevents arena blowup on multi-core hosts. We set it in the harness
    // process environment so every spawned child inherits it, and so the
    // env_capture module records it in allocator_env.
    //
    // VAL-BASE-009: when memcheck runs the child with MALLOC_ARENA_MAX=2,
    // the baseline JSON's environment.allocator_env includes the setting.
    // VAL-CONT-001: leindex launches with MALLOC_ARENA_MAX without error.
    //
    // Use --malloc-arena-max 0 to skip setting the env (for comparison
    // against the uncapped baseline).
    if args.malloc_arena_max > 0 {
        let val = args.malloc_arena_max.to_string();
        // SAFETY: single-threaded CLI startup before any child is spawned
        // or any env_capture read occurs.
        unsafe {
            std::env::set_var("MALLOC_ARENA_MAX", &val);
        }
        if args.verbose {
            eprintln!("  MALLOC_ARENA_MAX={}", val);
        }
    }

    let worker_capable = std::process::Command::new(&binary)
        .args([sampler::WORKER_CMDLINE_TOKEN, "--version"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .starts_with("leindex-embed ")
        });

    let isolated_fixture = workload::copy_fixture_source(&fixture)?;
    let config = workload::WorkloadConfig {
        binary,
        fixture: isolated_fixture.path().to_path_buf(),
        sample_interval: std::time::Duration::from_millis(args.sample_interval_ms),
        verbose: args.verbose,
        worker_capable,
        heap_profile_dir: None,
    };

    let phases = workload::run_workload(&config)?;

    let environment = env_capture::EnvironmentCapture::capture(&workspace_root, &fixture)
        .with_context(|| "failed to capture environment metadata")?;

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
        environment,
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
