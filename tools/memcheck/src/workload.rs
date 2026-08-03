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
//!
//! Memory-pressure remediation extensions (T8 step-3): three further phases
//! guard the 2026-08 memory work:
//! - `mcp_idle_proliferation`: PROLIFERATION_COUNT concurrent idle MCP servers,
//!   reporting their COMBINED RSS (superlinear per-server growth guard).
//! - `worker_ort_threads`: the worker-active trigger under the capped
//!   `LEINDEX_WORKER_ORT_THREADS=1` setting (deterministic cross-host baseline).
//! - `stale_artifacts`: seeds dead-pid run-dir sidecars, runs
//!   `leindex cleanup --stale-daemons`, samples the sweep, and fails loudly if
//!   any seeded sidecar survives.

use crate::report::PhaseReport;
use crate::sampler;
use anyhow::{Context, Result};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Canonical phase names in execution order (VAL-MEASURE-002, VAL-CPHASE-036,
/// T8 step-3).
///
/// The original 6 phases are preserved. Three worker-active phases are added
/// after the original phases to exercise the worker lifecycle:
/// - `embed_idle`: idle MCP process before any embed demand
/// - `embed_active`: MCP process with worker-active embedding (triggers worker spawn)
/// - `embed_teardown`: idle after worker teardown, verifying worker process cleanup
///
/// Memory-pressure remediation (T8 step-3) adds three more:
/// - `mcp_idle_proliferation`: combined RSS of PROLIFERATION_COUNT concurrent
///   idle MCP servers (superlinear growth guard)
/// - `worker_ort_threads`: worker RSS under the capped ORT-thread setting
/// - `stale_artifacts`: `leindex cleanup --stale-daemons` RSS + seeded-sidecar
///   removal assertion
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
    "mcp_idle_proliferation",
    "worker_ort_threads",
    "stale_artifacts",
];

/// The worker binary name used for child-process detection.
const WORKER_BINARY_NAME: &str = "leindex-embed";

/// Concurrent idle MCP servers launched by the `mcp_idle_proliferation` phase.
const PROLIFERATION_COUNT: usize = 3;

/// Dwell time for the `mcp_idle_proliferation` phase.
const PROLIFERATION_DWELL: Duration = Duration::from_secs(4);

/// Dwell time for the `worker_ort_threads` phase.
const WORKER_ORT_THREADS_DWELL: Duration = Duration::from_secs(4);

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

/// Copy fixture source files into a disposable directory, excluding its index.
pub fn copy_fixture_source(source: &Path) -> Result<tempfile::TempDir> {
    let destination = tempfile::tempdir().context("failed to create isolated fixture directory")?;
    copy_fixture_contents(source, destination.path())?;
    Ok(destination)
}

fn copy_fixture_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("failed to read fixture directory {}", source.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in {}", source.display()))?;
        if entry.file_name() == ".leindex" {
            continue;
        }

        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", source_path.display()))?
            .is_dir()
        {
            std::fs::create_dir_all(&destination_path)
                .with_context(|| format!("failed to create {}", destination_path.display()))?;
            copy_fixture_contents(&source_path, &destination_path)?;
        } else {
            std::fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        }
    }

    Ok(())
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
        let (child, report) = run_idle_phase(config, "embed_teardown", IDLE_DWELL, true)?;
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
        // trivially since 0 < any threshold. Only the three `embed_*` phases
        // are inserted here: `worker_ort_threads` gets its placeholder at the
        // same canonical position as the real run (after mcp_idle_proliferation,
        // before stale_artifacts) so report order matches CANONICAL_PHASES on
        // both paths (VAL-MEASURE-002).
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
            reports.push(placeholder_report(phase_name));
        }
    }

    // ── Phase 10: mcp_idle_proliferation ────────────────────────────────
    // Spawn PROLIFERATION_COUNT concurrent idle MCP servers and measure their
    // combined RSS. The single-server reference is the `idle_warm` phase of the
    // same run; the harness test `test_mcp_idle_proliferation_linearity`
    // asserts combined ≤ 3.5 × single (memory-pressure T2/T8). Always runs.
    let report = run_idle_proliferation_phase(config)?;
    reports.push(report);

    // ── Phase 11: worker_ort_threads ────────────────────────────────────
    // Worker-gated: real run or placeholder at the SAME canonical position on
    // both paths (after mcp_idle_proliferation, before stale_artifacts), so
    // report order always matches CANONICAL_PHASES (VAL-MEASURE-002).
    if worker_available {
        let (child, report) = run_worker_ort_threads_phase(config)?;
        reports.push(report);
        kill_child(child);
    } else {
        reports.push(placeholder_report("worker_ort_threads"));
    }

    // ── Phase 12: stale_artifacts ───────────────────────────────────────
    // Seed dead-pid run-dir sidecars, run `leindex cleanup --stale-daemons`,
    // sample the sweep, assert every seeded sidecar was removed (T7/T8).
    // Always runs.
    let report = run_stale_artifacts_phase(config)?;
    reports.push(report);

    Ok(reports)
}

/// Placeholder report for a worker-gated phase that could not run because the
/// worker binary is missing. `u64::MAX` sentinels make the budget gate fail
/// these phases loudly (they were not actually measured); a zero-valued report
/// would pass trivially since 0 < any threshold.
fn placeholder_report(phase_name: &str) -> PhaseReport {
    PhaseReport {
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
    }
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

/// Run a worker-active phase: launch an MCP process, trigger a search that
/// activates the ONNX worker, and sample both main + worker RSS.
///
/// VAL-CPHASE-036: The canonical workload includes worker-active phases.
/// VAL-CPHASE-034: The memcheck harness detects the worker process once
/// embedding begins and records it separately from the main daemon.
///
/// The search is triggered by sending a JSON-RPC `tools/call` message directly
/// to the MCP process's stdin, rather than launching a separate CLI command.
/// This ensures the MCP process (which we are sampling) actually receives the
/// search request and spawns the embedding worker as a child process.
///
/// `extra_env` is applied to the launched MCP process (and inherited by the
/// worker it spawns), so phases can pin settings such as
/// `LEINDEX_WORKER_ORT_THREADS` deterministically.
fn run_worker_active_phase(
    config: &WorkloadConfig,
    phase_name: &str,
    dwell: Duration,
    extra_env: &[(&str, &str)],
) -> Result<(Child, PhaseReport)> {
    if config.verbose {
        eprintln!("memcheck: phase '{}' starting (worker-active)", phase_name);
    }

    let mut child = launch_mcp_process_with_env(config, extra_env)?;
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
    let init_request =
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
    stdin_pipe
        .write_all(format!("{}\n", init_request).as_bytes())
        .context("failed to write initialize request to MCP stdin")?;
    stdin_pipe.flush().context("failed to flush MCP stdin")?;

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
    let initialized_notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    stdin_pipe
        .write_all(format!("{}\n", initialized_notification).as_bytes())
        .context("failed to write initialized notification to MCP stdin")?;
    stdin_pipe.flush().context("failed to flush MCP stdin")?;

    // Send an explicit semantic request directly so stdin stays open during
    // sampling. This is a real hybrid request: TF-IDF/PDG remain core while
    // the default auto provider starts and evaluates neural embeddings.
    //
    // Previously this used a background thread that moved stdin_pipe and
    // stdout_reader. When the thread finished (after reading the response),
    // it dropped stdin_pipe, closing the child's stdin. The MCP server then
    // saw EOF and exited before the sampling loop could collect enough
    // samples, resulting in mapped_file_kib=0 and anon_kib=0.
    //
    // Now we send the request, sample while stdin remains open, then read
    // the response afterwards.
    let fixture_path = config.fixture.display().to_string();
    let search_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"leindex.search","arguments":{{"query":"how does this project route requests","search_mode":"semantic","project_path":"{}"}}}}}}"#,
        fixture_path.replace('\\', "\\\\").replace('"', "\\\"")
    );
    if let Err(e) = stdin_pipe.write_all(format!("{}\n", search_request).as_bytes()) {
        eprintln!(
            "memcheck: failed to write search request to MCP stdin: {}",
            e
        );
    }
    if let Err(e) = stdin_pipe.flush() {
        eprintln!("memcheck: failed to flush MCP stdin after search: {}", e);
    }

    // Sample the MCP process (and its worker child) for the dwell period.
    // stdin_pipe is still in scope so the child process stays alive.
    let report = sample_pid_for_duration(
        pid,
        phase_name,
        dwell,
        config.sample_interval,
        Some(WORKER_BINARY_NAME),
    )?;

    // Read the search response now that sampling is complete. The background
    // reader reports EOF/transport errors through the channel; no elapsed-time
    // cancellation hides a slow model initialization or inference failure.
    let (tx, rx) = std::sync::mpsc::channel();
    let search_handle = std::thread::spawn(move || {
        let mut search_response = String::new();
        if let Err(e) = stdout_reader.read_line(&mut search_response) {
            eprintln!(
                "memcheck: failed to read search response from MCP stdout: {}",
                e
            );
        }
        let _ = tx.send(search_response);
    });

    let response = rx
        .recv()
        .context("MCP search response reader disconnected")?;
    if config.verbose {
        eprintln!("memcheck: MCP search response: {}", response.trim());
    }
    search_handle
        .join()
        .map_err(|_| anyhow::anyhow!("MCP search response reader panicked"))?;
    drop(stdin_pipe);

    if config.verbose {
        eprintln!(
            "memcheck: phase '{}' complete — main_rss_max: {} KiB, worker_rss_max: {} KiB, combined_rss_max: {} KiB, samples: {}",
            phase_name,
            report.rss_max_kib,
            report.worker_rss_max_kib,
            report.combined_rss_max_kib,
            report.sample_count
        );
    }

    Ok((child, report))
}

/// Run the canonical `embed_active` phase (5s dwell, default env).
fn run_embed_active_phase(config: &WorkloadConfig) -> Result<(Child, PhaseReport)> {
    run_worker_active_phase(config, "embed_active", Duration::from_secs(5), &[])
}

/// Run the `worker_ort_threads` phase: the worker-active trigger under the
/// T5/D3 capped ORT-thread setting. Pinning `LEINDEX_WORKER_ORT_THREADS=1`
/// explicitly makes the baseline deterministic across hosts (the default is
/// host-parallelism-derived). The {0,4,1} empirical record and the
/// cap ≤ no-cap assertion live in the opt-in harness test
/// `test_worker_ort_threads_cap_leq_nocap` (LEINDEX_MEMCHECK_EXPENSIVE=1).
fn run_worker_ort_threads_phase(config: &WorkloadConfig) -> Result<(Child, PhaseReport)> {
    run_worker_active_phase(
        config,
        "worker_ort_threads",
        WORKER_ORT_THREADS_DWELL,
        &[("LEINDEX_WORKER_ORT_THREADS", "1")],
    )
}

/// Run the `mcp_idle_proliferation` phase: launch PROLIFERATION_COUNT
/// concurrent idle MCP servers and report their COMBINED RSS. Each server gets
/// its own throwaway `LEINDEX_HOME` so concurrent starts never contend on the
/// advisory run-dir lock (memory-pressure T2/T8).
fn run_idle_proliferation_phase(config: &WorkloadConfig) -> Result<PhaseReport> {
    if config.verbose {
        eprintln!(
            "memcheck: phase 'mcp_idle_proliferation' starting ({} concurrent idle MCP servers)",
            PROLIFERATION_COUNT
        );
    }

    let root = tempfile::tempdir().context("mcp_idle_proliferation: create temp home root")?;
    let root_path = root.path().to_path_buf();
    let mut homes = Vec::with_capacity(PROLIFERATION_COUNT);
    for i in 0..PROLIFERATION_COUNT {
        let home = root_path.join(format!("home-{i}"));
        std::fs::create_dir_all(&home)
            .with_context(|| format!("mcp_idle_proliferation: create {}", home.display()))?;
        homes.push(home);
    }

    let mut children = Vec::with_capacity(PROLIFERATION_COUNT);
    for home in &homes {
        let home_str = home
            .to_str()
            .context("mcp_idle_proliferation: non-UTF8 temp home path")?;
        let env = [("LEINDEX_HOME", home_str)];
        match launch_mcp_process_with_env(config, &env) {
            Ok(child) => children.push(child),
            Err(e) => {
                // Kilo WARNING: partial-failure process leak. If a launch fails
                // after earlier children were already spawned, the `?` would
                // return early and drop the temp `root` while live servers
                // still run inside it (and never kill them). Kill everything
                // launched so far before propagating.
                for child in children {
                    kill_child(child);
                }
                return Err(e);
            }
        }
    }
    std::thread::sleep(STARTUP_GRACE);

    let pids: Vec<u32> = children.iter().map(|c| c.id()).collect();
    let report = match sample_pids_for_duration(
        &pids,
        "mcp_idle_proliferation",
        PROLIFERATION_DWELL,
        config.sample_interval,
    ) {
        Ok(report) => report,
        Err(e) => {
            // Same leak class as the launch loop: never drop the temp root
            // while sampled servers still run.
            for child in children {
                kill_child(child);
            }
            return Err(e);
        }
    };

    for child in children {
        kill_child(child);
    }

    if config.verbose {
        eprintln!(
            "memcheck: phase 'mcp_idle_proliferation' complete — combined rss_max: {} KiB ({} servers), samples: {}",
            report.rss_max_kib, PROLIFERATION_COUNT, report.sample_count
        );
    }

    Ok(report)
}

/// Sample the SUM of RSS across multiple PIDs on every tick, producing a
/// single aggregate [`PhaseReport`] for the group.
fn sample_pids_for_duration(
    pids: &[u32],
    phase_name: &str,
    dwell: Duration,
    sample_interval: Duration,
) -> Result<PhaseReport> {
    let start = Instant::now();
    let mut samples = Vec::new();

    while start.elapsed() < dwell {
        let mut combined = 0u64;
        let mut mapped = 0u64;
        let mut anon = 0u64;
        let mut all_ok = true;
        for &pid in pids {
            match sampler::sample(pid, None) {
                Ok(s) => {
                    combined += s.rss_kib;
                    mapped += s.mapped_file_kib;
                    anon += s.anon_kib;
                }
                Err(_) => {
                    all_ok = false;
                    break;
                }
            }
        }
        if all_ok {
            samples.push(sampler::MemorySample {
                rss_kib: combined,
                mapped_file_kib: mapped,
                anon_kib: anon,
                pss_kib: 0,
                worker_rss_kib: 0,
            });
        }
        std::thread::sleep(sample_interval);
    }

    let duration = start.elapsed();
    Ok(build_phase_report(phase_name, &mut samples, duration))
}

/// Run the `stale_artifacts` phase: seed dead-pid run-dir sidecars, run
/// `leindex cleanup --stale-daemons`, sample the sweep command's RSS, then
/// assert every seeded sidecar was removed. A surviving sidecar fails the
/// phase loudly (memory-pressure T7/T8).
///
/// The seeded home is either a pre-provisioned one pointed at by
/// `LEINDEX_MEMCHECK_STALE_HOME` (used by the harness integration test) or a
/// throwaway temp dir created here.
fn run_stale_artifacts_phase(config: &WorkloadConfig) -> Result<PhaseReport> {
    if config.verbose {
        eprintln!("memcheck: phase 'stale_artifacts' starting (seed dead sidecars + cleanup)");
    }

    let (home_path, created) = match std::env::var_os("LEINDEX_MEMCHECK_STALE_HOME") {
        Some(h) => (PathBuf::from(h), false),
        None => {
            let home = tempfile::tempdir().context("stale_artifacts: create temp LEINDEX_HOME")?;
            (home.keep(), true)
        }
    };
    let run_dir = home_path.join("run");
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("stale_artifacts: create {}", run_dir.display()))?;

    // Sidecar naming matches the T7 sweeper (cleanup.rs `sweep_run_dir`):
    // `leindex-embed-<stem>.<ext>` where ext ∈ {lock, pid, sock, status, ...}.
    // A pid file naming a provably-dead pid (max i32 pid, never allocated on
    // Linux) makes every sidecar in the stem stale regardless of age.
    let stem = "leindex-embed-memcheck";
    const DEAD_PID: &str = "2147483647";
    let seeded = ["pid", "lock", "sock"];
    for ext in seeded {
        let contents = if ext == "pid" { DEAD_PID } else { "" };
        std::fs::write(
            run_dir.join(format!("{stem}.{ext}")),
            format!("{contents}\n"),
        )
        .with_context(|| format!("stale_artifacts: seed {stem}.{ext}"))?;
    }

    let home_for_cmd = home_path.clone();
    let report = run_command_phase(config, "stale_artifacts", move |bin, _fixture| {
        let mut cmd = Command::new(bin);
        cmd.arg("cleanup").arg("--stale-daemons");
        cmd.env("LEINDEX_HOME", &home_for_cmd);
        cmd
    })?;

    for ext in seeded {
        let path = run_dir.join(format!("{stem}.{ext}"));
        if path.exists() {
            anyhow::bail!(
                "stale_artifacts: sidecar {} survived `cleanup --stale-daemons` — T7 sweeper regression",
                path.display()
            );
        }
    }

    if created {
        let _ = std::fs::remove_dir_all(&home_path);
    }

    if config.verbose {
        eprintln!(
            "memcheck: phase 'stale_artifacts' complete — cleanup rss_max: {} KiB, all {} seeded sidecars removed",
            report.rss_max_kib,
            seeded.len()
        );
    }

    Ok(report)
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
    // Drain stdout/stderr to prevent pipe buffer deadlock. The OS pipe
    // buffer is typically 64KB; without draining, the child process
    // blocks on write once the buffer is full, causing an indefinite hang.
    cmd.current_dir(&config.fixture)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if config.verbose {
        eprintln!("  command: {:?}", cmd);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to launch {} command", phase_name))?;
    let pid = child.id();

    // Spawn drain threads for stdout and stderr to prevent pipe deadlock.
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout_handle = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    let stderr_handle = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            match line {
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

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

    // Join drain threads so they don't outlive the phase.
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    // Clean up any leindex-embed daemon the command spawned. Command phases
    // invoke one-shot CLI subcommands (index, search, reindex) that may start
    // a persistent ONNX worker daemon. Without this cleanup, the daemon
    // survives the command's exit and leaks as an orphan process.
    let worker_pids = find_worker_pids(pid, WORKER_BINARY_NAME);
    if !worker_pids.is_empty() && config.verbose {
        eprintln!(
            "memcheck: phase '{}' cleaning up {} orphaned worker process(es)",
            phase_name,
            worker_pids.len()
        );
    }
    cleanup_command_phase_workers(&worker_pids);

    let duration = start.elapsed();

    if !status.success() {
        anyhow::bail!(
            "{} command exited with {:?} — aborting memcheck",
            phase_name,
            status
        );
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
    launch_mcp_process_with_env(config, &[])
}

/// Launch a leindex MCP process with extra environment overrides applied on
/// top of the standard harness environment (inherited by any worker it spawns).
fn launch_mcp_process_with_env(
    config: &WorkloadConfig,
    extra_env: &[(&str, &str)],
) -> Result<Child> {
    let mut cmd = Command::new(&config.binary);
    cmd.arg("mcp")
        .arg("--stdio")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LEINDEX_EMBED_DAEMON", "0");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    if config.verbose {
        eprintln!("  launching MCP: {:?}", cmd);
    }

    cmd.spawn()
        .with_context(|| format!("failed to launch {}", config.binary.display()))
}

/// Kill a child process gracefully (SIGKILL then reap).
///
/// In addition to killing the primary child, this also reaps any orphaned
/// `leindex-embed` worker processes that the child spawned. Without this
/// explicit cleanup, killing the main leindex process with SIGKILL would
/// orphan the worker (each ~1.5 GB with ROCm libs loaded), and prior to the
/// PR_SET_PDEATHSIG fix the worker would linger until its 5-minute idle
/// timeout. This belt-and-suspenders cleanup ensures the memcheck harness
/// never accumulates stale workers between phases even if PR_SET_PDEATHSIG
/// is somehow unavailable (e.g., non-Linux platforms or older kernels).
fn kill_child(mut child: Child) {
    let pid = child.id();
    let worker_pids = find_worker_pids(pid, WORKER_BINARY_NAME);

    let _ = child.kill();
    let _ = child.wait();

    // Only reap workers proven to be direct children before the measured
    // process exited. PPID-1 workers may belong to another LeIndex session.
    reap_worker_processes(&worker_pids);
}

/// Find and kill any lingering `leindex-embed` worker processes.
///
/// Kills only worker PIDs captured as direct children of the measured process.
/// This complements the worker's PR_SET_PDEATHSIG guard without touching
/// resident workers owned by another CLI or MCP session.
///
/// On non-Linux platforms this is a no-op (the memcheck harness is Linux-only
/// anyway, but the conditional keeps the code portable).
fn reap_worker_processes(worker_pids: &[u32]) {
    for &worker_pid in worker_pids {
        // SAFETY: `kill(pid, SIGKILL)` is a scalar syscall with no pointer
        // arguments. We ignore the return value because the worker may have
        // already exited between our scan and the kill.
        unsafe {
            let _ = libc::kill(worker_pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// Kill orphaned worker processes spawned by a command phase, using
/// SIGTERM followed by SIGKILL after a 3-second grace period.
///
/// This mirrors the `shutdown_worker_handle` SIGTERM→wait→SIGKILL escalation
/// used in the main crate's EmbeddingClient. SIGTERM gives the worker a
/// chance to flush its ONNX runtime state gracefully; SIGKILL ensures
/// termination even if the worker is wedged.
fn cleanup_command_phase_workers(worker_pids: &[u32]) {
    if worker_pids.is_empty() {
        return;
    }

    // Phase 1: SIGTERM all workers.
    for &worker_pid in worker_pids {
        // SAFETY: kill(pid, SIGTERM) is a scalar syscall.
        unsafe {
            let _ = libc::kill(worker_pid as libc::pid_t, libc::SIGTERM);
        }
    }

    // Phase 2: Wait up to 3 seconds for graceful exit.
    let deadline = Instant::now() + Duration::from_secs(3);
    let checked: Vec<u32> = worker_pids.to_vec();
    let mut exited = Vec::new();
    while Instant::now() < deadline && exited.len() < checked.len() {
        for &worker_pid in &checked {
            if exited.contains(&worker_pid) {
                continue;
            }
            // Check if the process has exited by sending signal 0 (no-op).
            // SAFETY: kill(pid, 0) checks existence without sending a signal.
            let alive = unsafe {
                libc::kill(worker_pid as libc::pid_t, 0) == 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            };
            if !alive {
                exited.push(worker_pid);
            }
        }
        if exited.len() < checked.len() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // Phase 3: SIGKILL any survivors.
    for &worker_pid in &checked {
        if !exited.contains(&worker_pid) {
            // SAFETY: kill(pid, SIGKILL) is a scalar syscall.
            unsafe {
                let _ = libc::kill(worker_pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// Scan `/proc` for worker processes matching `worker_name` whose PPID is
/// `parent_pid`.
fn find_worker_pids(parent_pid: u32, worker_name: &str) -> Vec<u32> {
    let mut found = Vec::new();

    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return found,
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };

        let candidate_pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if candidate_pid == parent_pid || candidate_pid == std::process::id() {
            continue;
        }

        // Read the process name and ppid.
        let comm = match read_proc_comm(candidate_pid) {
            Some(c) => c,
            None => continue,
        };

        // comm is truncated to 15 chars on Linux, so use starts_with.
        if comm != worker_name && !comm.starts_with(worker_name) {
            continue;
        }

        // Only workers directly owned by the measured process are eligible.
        let ppid = read_proc_ppid(candidate_pid);
        if ppid == parent_pid {
            found.push(candidate_pid);
        }
    }

    found
}

/// Read the process name from `/proc/<pid>/comm`.
fn read_proc_comm(pid: u32) -> Option<String> {
    let path = format!("/proc/{}/comm", pid);
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read the parent PID from `/proc/<pid>/stat`.
fn read_proc_ppid(pid: u32) -> u32 {
    let path = format!("/proc/{}/stat", pid);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };

    // Format: pid (comm) state ppid ...
    // The comm field may contain spaces and parens, so find the last ')'
    // and parse from there.
    let Some(close_paren) = content.rfind(')') else {
        return 0;
    };

    let rest = &content[close_paren + 1..];
    let mut fields = rest.split_whitespace();

    fields.next(); // state
    fields
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
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
                "mcp_idle_proliferation",
                "worker_ort_threads",
                "stale_artifacts",
            ]
        );
    }

    #[test]
    fn test_canonical_phases_count() {
        assert_eq!(CANONICAL_PHASES.len(), 12);
    }

    #[test]
    fn test_copy_fixture_source_excludes_root_leindex() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("src")).unwrap();
        std::fs::create_dir_all(source.path().join(".leindex")).unwrap();
        std::fs::write(source.path().join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        std::fs::write(source.path().join(".leindex/index.db"), "source index").unwrap();

        let isolated = copy_fixture_source(source.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(isolated.path().join("src/lib.rs")).unwrap(),
            "pub fn fixture() {}\n"
        );
        assert!(!isolated.path().join(".leindex").exists());
        assert_eq!(
            std::fs::read_to_string(source.path().join(".leindex/index.db")).unwrap(),
            "source index"
        );
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

    // ── Worker-reaping cleanup tests ───────────────────────────────────
    //
    // These tests exercise the helper functions used by `kill_child` to
    // scan /proc for orphaned `leindex-embed` workers after a phase ends.

    #[test]
    fn test_read_proc_comm_for_self() {
        let pid = std::process::id();
        let comm = read_proc_comm(pid);
        assert!(comm.is_some(), "should be able to read comm for self");
        let comm = comm.unwrap();
        assert!(!comm.is_empty());
        // The memcheck test binary's comm should not match the worker name.
        assert_ne!(comm, WORKER_BINARY_NAME);
    }

    #[test]
    fn test_read_proc_ppid_for_self() {
        let pid = std::process::id();
        let ppid = read_proc_ppid(pid);
        // ppid should be a positive value (the test runner or shell).
        assert!(ppid > 0, "ppid should be positive, got {}", ppid);
    }

    #[test]
    fn test_read_proc_ppid_missing_pid() {
        // A PID that almost certainly does not exist.
        let ppid = read_proc_ppid(u32::MAX);
        assert_eq!(ppid, 0, "missing pid should return 0 ppid");
    }

    #[test]
    fn test_read_proc_comm_missing_pid() {
        let comm = read_proc_comm(u32::MAX);
        assert!(comm.is_none(), "missing pid should return None");
    }

    #[test]
    fn test_find_worker_pids_finds_no_workers_for_self() {
        // The memcheck test process should not have any leindex-embed children.
        let pid = std::process::id();
        let workers = find_worker_pids(pid, WORKER_BINARY_NAME);
        assert!(
            workers.is_empty(),
            "expected no worker children for test process, found {:?}",
            workers
        );
    }

    #[test]
    fn test_reap_worker_processes_no_crash_with_no_workers() {
        // Ensures the reaper is a no-op when no workers are running.
        reap_worker_processes(&[]);
    }
}
