//! Workload driver — executes canonical phases against a fresh leindex process.
//!
//! Each phase launches a **fresh** `leindex` process (VAL-MEASURE-004) and
//! samples its RSS using `/proc` (VAL-MEASURE-005). The canonical phase order
//! is `idle_warm → index → idle_post → query → reindex → idle_final`
//! (VAL-MEASURE-001, VAL-MEASURE-002).

use crate::report::PhaseReport;
use crate::sampler;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Canonical phase names in execution order (VAL-MEASURE-002).
pub const CANONICAL_PHASES: &[&str] = &[
    "idle_warm",
    "index",
    "idle_post",
    "query",
    "reindex",
    "idle_final",
];

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
}

/// Run the full canonical workload and return per-phase reports.
///
/// Each phase launches a fresh `leindex` process, samples it for the
/// appropriate duration, then cleans up before the next phase.
pub fn run_workload(config: &WorkloadConfig) -> Result<Vec<PhaseReport>> {
    let mut reports = Vec::with_capacity(CANONICAL_PHASES.len());

    // Clean any pre-existing index state so the run is deterministic.
    clean_index_state(&config.fixture);

    // ── Phase 1: idle_warm ──────────────────────────────────────────────
    // Launch a fresh leindex MCP process and let it sit idle.
    let (child, report) = run_idle_phase(config, "idle_warm", IDLE_DWELL)?;
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
    let (child, report) = run_idle_phase(config, "idle_post", IDLE_DWELL)?;
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
    let (child, report) = run_idle_phase(config, "idle_final", IDLE_DWELL)?;
    reports.push(report);
    kill_child(child);

    Ok(reports)
}

// ─── Phase implementations ──────────────────────────────────────────────

/// Run an idle phase: launch a fresh leindex MCP process, sample for `dwell`.
fn run_idle_phase(
    config: &WorkloadConfig,
    phase_name: &str,
    dwell: Duration,
) -> Result<(Child, PhaseReport)> {
    if config.verbose {
        eprintln!("memcheck: phase '{}' starting (idle)", phase_name);
    }

    let child = launch_mcp_process(config)?;
    let pid = child.id();

    // Give the process time to initialise before sampling.
    std::thread::sleep(STARTUP_GRACE);

    let report = sample_pid_for_duration(pid, phase_name, dwell, config.sample_interval)?;

    if config.verbose {
        eprintln!(
            "memcheck: phase '{}' complete — rss_max: {} KiB, samples: {}",
            phase_name, report.rss_max_kib, report.sample_count
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
            if let Ok(s) = sampler::sample(pid) {
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
fn sample_pid_for_duration(
    pid: u32,
    phase_name: &str,
    dwell: Duration,
    sample_interval: Duration,
) -> Result<PhaseReport> {
    let start = Instant::now();
    let mut samples = Vec::new();

    while start.elapsed() < dwell {
        if let Ok(s) = sampler::sample(pid) {
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
fn build_phase_report(
    phase_name: &str,
    samples: &mut Vec<sampler::MemorySample>,
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

    PhaseReport {
        phase: phase_name.to_string(),
        rss_min_kib: rss_min,
        rss_max_kib: rss_max,
        rss_p95_kib: rss_p95,
        mapped_file_kib: mapped_file,
        anon_kib: anon,
        sample_count: samples.len(),
        duration_ms: duration.as_millis() as u64,
    }
}

/// Clean any existing leindex index state from the fixture directory.
fn clean_index_state(fixture: &PathBuf) {
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
            ]
        );
    }

    #[test]
    fn test_canonical_phases_count() {
        assert_eq!(CANONICAL_PHASES.len(), 6);
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
    }

    #[test]
    fn test_build_phase_report_with_samples() {
        let mut samples = vec![
            sampler::MemorySample {
                rss_kib: 100,
                mapped_file_kib: 10,
                anon_kib: 90,
                pss_kib: 100,
            },
            sampler::MemorySample {
                rss_kib: 200,
                mapped_file_kib: 20,
                anon_kib: 180,
                pss_kib: 200,
            },
            sampler::MemorySample {
                rss_kib: 150,
                mapped_file_kib: 15,
                anon_kib: 135,
                pss_kib: 150,
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
            },
            sampler::MemorySample {
                rss_kib: 120,
                mapped_file_kib: 80,
                anon_kib: 40,
                pss_kib: 120,
            },
            sampler::MemorySample {
                rss_kib: 110,
                mapped_file_kib: 30,
                anon_kib: 80,
                pss_kib: 110,
            },
        ];
        let report = build_phase_report("test", &mut samples, Duration::from_secs(1));
        assert_eq!(report.mapped_file_kib, 80); // peak
        assert_eq!(report.anon_kib, 80); // peak
    }
}
