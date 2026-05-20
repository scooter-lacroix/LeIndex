//! Workload driver — executes canonical phases against a fresh leindex process.
//!
//! Each phase launches a **fresh** `leindex` process (VAL-MEASURE-004) and
//! samples its RSS using `/proc` (VAL-MEASURE-005). The canonical phase order
//! is `idle_warm → index → idle_post → query → reindex → idle_final`
//! (VAL-MEASURE-001, VAL-MEASURE-002).
//!
//! Worker-aware extensions (VAL-CPHASE-036): the canonical workload includes
//! an idle pre-embed phase, a worker-active embed phase, and a phase that
//! exercises teardown/restart behavior.

use crate::report::PhaseReport;
use crate::sampler;
use anyhow::{Context, Result};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Canonical phase names in execution order (VAL-MEASURE-002, VAL-CPHASE-036).
///
/// The original 6 phases are preserved. Three worker-active phases are added
/// after the original phases to exercise the worker lifecycle:
/// - `embed_idle`: idle MCP process before any embed demand
/// - `embed_active`: MCP process with worker-active embedding (triggers worker spawn)
/// - `embed_teardown`: idle after worker teardown, verifying worker process cleanup
pub const CANONICAL_PHASES: &[&str] = &[
    "idle_warm",
    "index",
    "idle_post",
    "query",
    "reindex",
    "idle_final",
    "embed_idle",
    "embed_active",
    "embed_teardown",
];

/// The worker binary name used for child-process detection.
const WORKER_BINARY_NAME: &str = "leindex-embed";

/// Idle phase dwell time (seconds).
const IDLE_DWELL: Duration = Duration::from_secs(3);

/// Startup grace period before sampling begins.
const STARTUP_GRACE: Duration = Duration::from_millis(500);

/// Workload configuration.
pub struct WorkloadConfig {
    pub binary: PathBuf,
    pub fixture: PathBuf,
    pub sample_interval: Duration,
    pub verbose: bool,
    /// Path to the leindex-embed worker binary (for worker-active phases).
    /// If None, worker-active phases are skipped.
    pub worker_binary: Option<PathBuf>,
}

/// Run the full canonical workload and return per-phase reports.
///
/// Each phase launches a fresh `leindex` process, samples it for the
/// appropriate duration, then cleans up before the next phase.
///
/// Worker-active phases (VAL-CPHASE-036) are appended after the original
/// 6 phases. They exercise the worker spawn/embed/teardown lifecycle.
pub fn run_workload(config: &WorkloadConfig) -> Result<Vec<PhaseReport>> {
    let mut reports = Vec::with_capacity(CANONICAL_PHASES.len());

    // Clean any pre-existing index state so the run is deterministic.
    clean_index_state(&config.fixture);

    // ── Phase 1: idle_warm ──────────────────────────────────────────────
    // Launch a fresh leindex MCP process and let it sit idle.
    let (child, report) = run_idle_phase(config, "idle_warm", IDLE_DWELL, false)?;
    reports.push(report);
    kill_child(child);

    // ── Phase 2: index ──────────────────────────────────────────────────
    // Run `leindex index <fixture>` and sample the indexing process.
    let report = run_command_phase(config, "index", |bin, fixture| {
        let mut cmd = Command::new(bin);
        cmd.arg("index").arg(fixture);
        cmd
    })?;
    reports.push(report);

    // ── Phase 3: idle_post ──────────────────────────────────────────────
    // Launch a fresh MCP process against the now-indexed fixture.
    let (child, report) = run_idle_phase(config, "idle_post", IDLE_DWELL, false)?;
    reports.push(report);
    kill_child(child);

    // ── Phase 4: query ──────────────────────────────────────────────────
    // Run `leindex search <query> --project <fixture>` and sample.
    let report = run_command_phase(config, "query", |bin, fixture| {
        let mut cmd = Command::new(bin);
        cmd.arg("search")
            .arg("function")
            .arg("--project")
            .arg(fixture);
        cmd
    })?;
    reports.push(report);

    // ── Phase 5: reindex ────────────────────────────────────────────────
    // Run `leindex index <fixture> --force` and sample.
    let report = run_command_phase(config, "reindex", |bin, fixture| {
        let mut cmd = Command::new(bin);
        cmd.arg("index").arg(fixture).arg("--force");
        cmd
    })?;
    reports.push(report);

    // ── Phase 6: idle_final ─────────────────────────────────────────────
    let (child, report) = run_idle_phase(config, "idle_final", IDLE_DWELL, false)?;
    reports.push(report);
    kill_child(child);

    // ── Worker-active phases (VAL-CPHASE-036) ───────────────────────────
    // These phases exercise the worker lifecycle. They require the
    // leindex-embed binary to be available alongside the main binary.
    let worker_available = config
        .worker_binary
        .as_ref()
        .map(|p| p.exists())
        .unwrap_or(false);

    if worker_available {
        // ── Phase 7: embed_idle ─────────────────────────────────────────
        // Launch MCP process and let it sit idle (no worker spawned yet).
        let (child, report) = run_idle_phase(config, "embed_idle", IDLE_DWELL, false)?;
        reports.push(report);
        kill_child(child);

        // ── Phase 8: embed_active ───────────────────────────────────────
        // Launch MCP process, trigger a search that would use ONNX embeddings
        // (which spawns the worker), and sample both main + worker RSS.
        let (child, report) = run_embed_active_phase(config)?;
        reports.push(report);
        kill_child(child);

        // ── Phase 9: embed_teardown ─────────────────────────────────────
        // Launch MCP process after the worker has been torn down. This verifies
        // that the worker process is cleaned up and doesn't leak RSS.
        let (child, report) = run_idle_phase(config, "embed_teardown", IDLE_DWELL, false)?;
        reports.push(report);
        kill_child(child);
    } else {
        if config.verbose {
            eprintln!(
                "memcheck: skipping worker-active phases ({} not found)",
                config
                    .worker_binary
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "worker binary path not set".to_string())
            );
        }
        // Still add placeholder phases so the report has the right phase count.
        // Use u64::MAX sentinel values so the budget gate fails these phases
        // (they were not actually measured). A zero-valued report would pass
        // trivially since 0 < any threshold.
        eprintln!(
            "memcheck: WARNING: worker-active phases skipped ({} not found) — \
             placeholder reports will fail the budget gate",
            config
                .worker_binary
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "worker binary path not set".to_string())
        );
        for phase_name in &["embed_idle", "embed_active", "embed_teardown"] {
            reports.push(PhaseReport {
                phase: phase_name.to_string(),
                rss_min_kib: 0,
                rss_max_kib: u64::MAX,
                rss_p95_kib: 0,
                mapped_file_kib: 0,
                anon_kib: 0,
                sample_count: 0,
                duration_ms: 0,
                worker_rss_max_kib: 0,
                combined_rss_max_kib: u64::MAX,
            });
        }
    }

    Ok(reports)
}

// ─── Phase implementations ──────────────────────────────────────────────

/// Run an idle phase: launch a fresh leindex MCP process, sample for `dwell`.
///
/// When `track_worker` is true, the sampler also looks for child worker
/// processes (VAL-CPHASE-034).
fn run_idle_phase(
    config: &WorkloadConfig,
    phase_name: &str,
    dwell: Duration,
    track_worker: bool,
) -> Result<(Child, PhaseReport)> {
    if config.verbose {
        eprintln!("memcheck: phase '{}' starting (idle)", phase_name);
    }

    let child = launch_mcp_process(config)?;
    let pid = child.id();

    // Give the process time to initialise before sampling.
    std::thread::sleep(STARTUP_GRACE);

    let worker_name = if track_worker {
        Some(WORKER_BINARY_NAME)
    } else {
        None
    };
    let report =
        sample_pid_for_duration(pid, phase_name, dwell, config.sample_interval, worker_name)?;

    if config.verbose {
        eprintln!(
            "memcheck: phase '{}' complete — rss_max: {} KiB, worker_rss_max: {} KiB, samples: {}",
            phase_name, report.rss_max_kib, report.worker_rss_max_kib, report.sample_count
        );
    }

    Ok((child, report))
}

/// Run the embed_active phase: launch MCP process, trigger a search that
/// activates the ONNX worker, and sample both main + worker RSS.
///
/// VAL-CPHASE-036: The canonical workload includes a worker-active embed phase.
/// VAL-CPHASE-034: The memcheck harness detects the worker process once
/// embedding begins and records it separately from the main daemon.
///
/// The search is triggered by sending a JSON-RPC `tools/call` message directly
/// to the MCP process's stdin, rather than launching a separate CLI command.
/// This ensures the MCP process (which we are sampling) actually receives the
/// search request and spawns the embedding worker as a child process.
fn run_embed_active_phase(config: &WorkloadConfig) -> Result<(Child, PhaseReport)> {
    if config.verbose {
        eprintln!("memcheck: phase 'embed_active' starting (worker-active)");
    }

    let mut child = launch_mcp_process(config)?;
    let pid = child.id();

    // Take stdin/stdout pipes for MCP JSON-RPC communication.
    let mut stdin_pipe = child
        .stdin
        .take()
        .context("failed to take MCP stdin pipe")?;
    let stdout_pipe = child
        .stdout
        .take()
        .context("failed to take MCP stdout pipe")?;
    let mut stdout_reader = std::io::BufReader::new(stdout_pipe);

    // Give the process time to initialise
    std::thread::sleep(STARTUP_GRACE);

    // MCP handshake: send initialize request, read response.
    let init_request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
    stdin_pipe
        .write_all(format!("{}\n", init_request).as_bytes())
        .context("failed to write initialize request to MCP stdin")?;
    stdin_pipe
        .flush()
        .context("failed to flush MCP stdin")?;

    // Read the initialize response (line-delimited JSON).
    let mut init_response = String::new();
    stdout_reader
        .read_line(&mut init_response)
        .context("failed to read initialize response from MCP stdout")?;

    if config.verbose {
        eprintln!(
            "memcheck: MCP initialize response: {}",
            init_response.trim()
        );
    }

    // Send initialized notification (no response expected).
    let initialized_notification =
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    stdin_pipe
        .write_all(format!("{}\n", initialized_notification).as_bytes())
        .context("failed to write initialized notification to MCP stdin")?;
    stdin_pipe
        .flush()
        .context("failed to flush MCP stdin")?;

    // Trigger a search via tools/call in a background thread.
    // This sends the request to the MCP process we are sampling, which will
    // activate the embedding path and spawn the worker as a child.
    let fixture_path = config.fixture.display().to_string();
    let search_handle = std::thread::spawn(move || {
        let search_request = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"search","arguments":{{"query":"function","project_path":"{}"}}}}}}"#,
            fixture_path.replace('\\', "\\\\").replace('"', "\\\"")
        );
        if let Err(e) = stdin_pipe.write_all(format!("{}\n", search_request).as_bytes()) {
            eprintln!("memcheck: failed to write search request to MCP stdin: {}", e);
            return;
        }
        if let Err(e) = stdin_pipe.flush() {
            eprintln!("memcheck: failed to flush MCP stdin after search: {}", e);
            return;
        }
        // Read the search response so the MCP process can complete.
        let mut search_response = String::new();
        if let Err(e) = stdout_reader.read_line(&mut search_response) {
            eprintln!(
                "memcheck: failed to read search response from MCP stdout: {}",
                e
            );
        }
    });

    // Sample the MCP process (and its worker child) for the dwell period.
    let dwell = Duration::from_secs(5);
    let report = sample_pid_for_duration(
        pid,
        "embed_active",
        dwell,
        config.sample_interval,
        Some(WORKER_BINARY_NAME),
    )?;

    // Wait for the search to finish (ignore result)
    let _ = search_handle.join();

    if config.verbose {
        eprintln!(
            "memcheck: phase 'embed_active' complete — main_rss_max: {} KiB, worker_rss_max: {} KiB, combined_rss_max: {} KiB, samples: {}",
            report.rss_max_kib, report.worker_rss_max_kib, report.combined_rss_max_kib, report.sample_count
        );
    }

    Ok((child, report))
}

/// Run a one-shot command phase (index / query / reindex).
///
/// Spawns the command, samples its PID in a background thread until it
/// exits, then returns the phase report.
fn run_command_phase(
    config: &WorkloadConfig,
    phase_name: &str,
    build_cmd: impl Fn(&PathBuf, &PathBuf) -> Command,
) -> Result<PhaseReport> {
    if config.verbose {
        eprintln!("memcheck: phase '{}' starting (command)", phase_name);
    }

    let mut cmd = build_cmd(&config.binary, &config.fixture);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    if config.verbose {
        eprintln!("  command: {:?}", cmd);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to launch {} command", phase_name))?;
    let pid = child.id();

    let start = Instant::now();
    let _sample_interval = config.sample_interval;

    // Sampler thread — collects samples until the command exits.
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();

    let sampler_handle = std::thread::spawn(move || {
        let mut samples = Vec::new();
        // Use a tighter inner loop for command phases: sample as fast as
        // possible (every 10ms) so we capture peak RSS even for short-lived
        // commands.  The outer `done` flag is checked between samples.
        let fast_interval = Duration::from_millis(10);
        while !done_clone.load(Ordering::Relaxed) {
            if let Ok(s) = sampler::sample(pid, None) {
                samples.push(s);
            }
            std::thread::sleep(fast_interval);
        }
        samples
    });

    // Wait for the command to finish.
    let status = child
        .wait()
        .with_context(|| format!("{} command did not complete", phase_name))?;
    done.store(true, Ordering::Relaxed);

    let mut samples = sampler_handle
        .join()
        .map_err(|_| anyhow::anyhow!("sampler thread panicked"))?;

    let duration = start.elapsed();

    if !status.success() && config.verbose {
        eprintln!("  warning: {} command exited with {:?}", phase_name, status);
    }

    let report = build_phase_report(phase_name, &mut samples, duration);

    if config.verbose {
        eprintln!(
            "memcheck: phase '{}' complete — rss_max: {} KiB, samples: {}",
            phase_name, report.rss_max_kib, report.sample_count
        );
    }

    Ok(report)
}

// ─── Helpers ────────────────────────────────────────────────────────────

/// Launch a leindex process that stays alive (MCP stdio mode).
fn launch_mcp_process(config: &WorkloadConfig) -> Result<Child> {
    let mut cmd = Command::new(&config.binary);
    cmd.arg("--project")
        .arg(&config.fixture)
        .arg("mcp")
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if config.verbose {
        eprintln!("  launching MCP: {:?}", cmd);
    }

    cmd.spawn()
        .with_context(|| format!("failed to launch {}", config.binary.display()))
}

/// Kill a child process gracefully (SIGKILL then reap).
fn kill_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Sample a PID for a fixed duration, collecting full memory samples.
///
/// When `worker_name` is `Some`, also samples any child worker process
/// matching that name (VAL-CPHASE-034).
fn sample_pid_for_duration(
    pid: u32,
    phase_name: &str,
    dwell: Duration,
    sample_interval: Duration,
    worker_name: Option<&str>,
) -> Result<PhaseReport> {
    let start = Instant::now();
    let mut samples = Vec::new();

    while start.elapsed() < dwell {
        if let Ok(s) = sampler::sample(pid, worker_name) {
            samples.push(s);
        }
        std::thread::sleep(sample_interval);
    }

    let duration = start.elapsed();
    Ok(build_phase_report(phase_name, &mut samples, duration))
}

/// Build a [`PhaseReport`] from collected samples.
///
/// RSS min/max/p95 are computed from the sample set. Mapped-file and
/// anonymous memory use the **peak** values across all samples so that
/// mmap-heavy phases are captured correctly (VAL-MEASURE-003, VAL-MEASURE-006).
///
/// Worker-aware extensions (VAL-CPHASE-035): worker_rss_max_kib is the peak
/// worker RSS across all samples. combined_rss_max_kib is the peak of
/// (main_rss + worker_rss) across all samples.
fn build_phase_report(
    phase_name: &str,
    samples: &mut [sampler::MemorySample],
    duration: Duration,
) -> PhaseReport {
    if samples.is_empty() {
        return PhaseReport {
            phase: phase_name.to_string(),
            rss_min_kib: 0,
            rss_max_kib: 0,
            rss_p95_kib: 0,
            mapped_file_kib: 0,
            anon_kib: 0,
            sample_count: 0,
            duration_ms: duration.as_millis() as u64,
            worker_rss_max_kib: 0,
            combined_rss_max_kib: 0,
        };
    }

    let rss_values: Vec<u64> = samples.iter().map(|s| s.rss_kib).collect();
    let rss_min = *rss_values.iter().min().unwrap_or(&0);
    let rss_max = *rss_values.iter().max().unwrap_or(&0);

    // p95 calculation
    let mut sorted = rss_values;
    sorted.sort_unstable();
    let p95_idx = ((sorted.len() as f64) * 0.95).ceil() as usize;
    let rss_p95 = sorted
        .get(p95_idx.saturating_sub(1))
        .copied()
        .unwrap_or(rss_max);

    // Use peak mapped-file and anonymous across all samples.
    let mapped_file = samples.iter().map(|s| s.mapped_file_kib).max().unwrap_or(0);
    let anon = samples.iter().map(|s| s.anon_kib).max().unwrap_or(0);

    // Worker-aware metrics (VAL-CPHASE-035)
    let worker_rss_max = samples.iter().map(|s| s.worker_rss_kib).max().unwrap_or(0);
    let combined_rss_max = samples
        .iter()
        .map(|s| s.rss_kib + s.worker_rss_kib)
        .max()
        .unwrap_or(rss_max);

    PhaseReport {
        phase: phase_name.to_string(),
        rss_min_kib: rss_min,
        rss_max_kib: rss_max,
        rss_p95_kib: rss_p95,
        mapped_file_kib: mapped_file,
        anon_kib: anon,
        sample_count: samples.len(),
        duration_ms: duration.as_millis() as u64,
        worker_rss_max_kib: worker_rss_max,
        combined_rss_max_kib: combined_rss_max,
    }
}

/// Clean any existing leindex index state from the fixture directory.
fn clean_index_state(fixture: &Path) {
    let leindex_dir = fixture.join(".leindex");
    if leindex_dir.exists() {
        let _ = std::fs::remove_dir_all(&leindex_dir);
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_phases_order() {
        assert_eq!(
            CANONICAL_PHASES,
            &[
                "idle_warm",
                "index",
                "idle_post",
                "query",
                "reindex",
                "idle_final",
                "embed_idle",
                "embed_active",
                "embed_teardown",
            ]
        );
    }

    #[test]
    fn test_canonical_phases_count() {
        assert_eq!(CANONICAL_PHASES.len(), 9);
    }

    #[test]
    fn test_build_phase_report_empty() {
        let mut samples = Vec::new();
        let report = build_phase_report("test", &mut samples, Duration::from_secs(1));
        assert_eq!(report.phase, "test");
        assert_eq!(report.sample_count, 0);
        assert_eq!(report.rss_min_kib, 0);
        assert_eq!(report.rss_max_kib, 0);
        assert_eq!(report.rss_p95_kib, 0);
        assert_eq!(report.mapped_file_kib, 0);
        assert_eq!(report.anon_kib, 0);
        assert_eq!(report.worker_rss_max_kib, 0);
        assert_eq!(report.combined_rss_max_kib, 0);
    }

    #[test]
    fn test_build_phase_report_with_samples() {
        let mut samples = vec![
            sampler::MemorySample {
                rss_kib: 100,
                mapped_file_kib: 10,
                anon_kib: 90,
                pss_kib: 100,
                worker_rss_kib: 0,
            },
            sampler::MemorySample {
                rss_kib: 200,
                mapped_file_kib: 20,
                anon_kib: 180,
                pss_kib: 200,
                worker_rss_kib: 50,
            },
            sampler::MemorySample {
                rss_kib: 150,
                mapped_file_kib: 15,
                anon_kib: 135,
                pss_kib: 150,
                worker_rss_kib: 30,
            },
        ];
        let report = build_phase_report("test", &mut samples, Duration::from_millis(500));
        assert_eq!(report.phase, "test");
        assert_eq!(report.sample_count, 3);
        assert_eq!(report.rss_min_kib, 100);
        assert_eq!(report.rss_max_kib, 200);
        assert!(report.rss_p95_kib >= 100);
        assert!(report.rss_p95_kib <= 200);
        assert_eq!(report.duration_ms, 500);
        // Peak mapped_file and anon
        assert_eq!(report.mapped_file_kib, 20);
        assert_eq!(report.anon_kib, 180);
        // Worker-aware metrics
        assert_eq!(report.worker_rss_max_kib, 50);
        assert_eq!(report.combined_rss_max_kib, 250); // 200 + 50
    }

    #[test]
    fn test_build_phase_report_p95() {
        // With 20 samples, p95 should be the 19th value when sorted.
        let mut samples: Vec<sampler::MemorySample> = (1..=20)
            .map(|i| sampler::MemorySample {
                rss_kib: i * 10,
                mapped_file_kib: 0,
                anon_kib: 0,
                pss_kib: i * 10,
                worker_rss_kib: 0,
            })
            .collect();
        let report = build_phase_report("test", &mut samples, Duration::from_secs(1));
        // p95 of 20 samples: index = ceil(20*0.95) = 19, so 19th value = 190
        assert_eq!(report.rss_p95_kib, 190);
    }

    #[test]
    fn test_build_phase_report_peak_mapped_anon() {
        let mut samples = vec![
            sampler::MemorySample {
                rss_kib: 100,
                mapped_file_kib: 50,
                anon_kib: 50,
                pss_kib: 100,
                worker_rss_kib: 0,
            },
            sampler::MemorySample {
                rss_kib: 120,
                mapped_file_kib: 80,
                anon_kib: 40,
                pss_kib: 120,
                worker_rss_kib: 0,
            },
            sampler::MemorySample {
                rss_kib: 110,
                mapped_file_kib: 30,
                anon_kib: 80,
                pss_kib: 110,
                worker_rss_kib: 0,
            },
        ];
        let report = build_phase_report("test", &mut samples, Duration::from_secs(1));
        assert_eq!(report.mapped_file_kib, 80); // peak
        assert_eq!(report.anon_kib, 80); // peak
    }

    #[test]
    fn test_build_phase_report_worker_aware() {
        // Test that worker RSS is tracked and combined correctly
        let mut samples = vec![
            sampler::MemorySample {
                rss_kib: 50000,
                mapped_file_kib: 0,
                anon_kib: 0,
                pss_kib: 0,
                worker_rss_kib: 0,
            },
            sampler::MemorySample {
                rss_kib: 60000,
                mapped_file_kib: 0,
                anon_kib: 0,
                pss_kib: 0,
                worker_rss_kib: 80000,
            },
            sampler::MemorySample {
                rss_kib: 55000,
                mapped_file_kib: 0,
                anon_kib: 0,
                pss_kib: 0,
                worker_rss_kib: 90000,
            },
        ];
        let report = build_phase_report("embed_active", &mut samples, Duration::from_secs(1));
        assert_eq!(report.rss_max_kib, 60000);
        assert_eq!(report.worker_rss_max_kib, 90000);
        assert_eq!(report.combined_rss_max_kib, 145000); // 55000 + 90000
    }
}
