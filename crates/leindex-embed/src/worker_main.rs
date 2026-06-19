// Worker entry point shared by the leindex-embed subcrate binary and the
// root crate's cargo-install-friendly wrapper binary.
//
// VAL-CARGO-005: `cargo install leindex --features onnx` must install BOTH
// the `leindex` and `leindex-embed` binaries. Because `cargo install <pkg>`
// only installs `[[bin]]` targets declared in `<pkg>`'s own `Cargo.toml`,
// the root leindex crate mirrors the worker binary via a thin wrapper at
// `src/bin/leindex-embed.rs`. Both binaries call this function so the
// worker logic lives in a single place and is feature-unified through the
// library crate.
//
// VAL-CPHASE-001: The worker is a separate executable built alongside leindex.
// VAL-CPHASE-004: Worker transport uses local IPC only.
// VAL-CPHASE-005: Worker cold-starts on first embed demand.
// VAL-CPHASE-006: Worker remains reusable across successive batches.
// VAL-CPHASE-007: Worker idle timeout tears down the resident model process.
// VAL-CPHASE-008: Worker restart works after idle teardown.

use std::io;
use std::process;

use crate::runtime::{RuntimeConfig, WorkerRuntime};

/// Run the leindex-embed worker.
///
/// Installs the process-leak guard (PR_SET_PDEATHSIG on Linux), initialises
/// logging, builds the runtime config from the environment, and runs the IPC
/// loop over stdin/stdout. Returns the process exit code.
///
/// This function is the single source of truth for the worker entry point.
/// It is called by:
///   - `crates/leindex-embed/src/bin/leindex-embed.rs` (subcrate binary)
///   - `src/bin/leindex-embed.rs` (root crate cargo-install wrapper)
///
/// VAL-CARGO-005/VAL-RELEASE-002: `leindex-embed --version` prints the
/// release version (matching `leindex --version`) and exits 0 so install
/// verification scripts can confirm both binaries are present and correct.
pub fn run() -> ! {
    // Handle --version / -V before any heavy initialization.
    //
    // VAL-CARGO-005: evidence requires `leindex-embed --version` to print
    // the release version. VAL-RELEASE-002 requires the same from the
    // release bundle worker binary. This must run before logging init so
    // the version string is the only stdout output (no tracing noise).
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() == 2 && (argv[1] == "--version" || argv[1] == "-V") {
        // Use the subcrate version (same as Cargo.toml version, kept in
        // parity with the root crate by AGENTS.md version-parity rule).
        println!("leindex-embed {}", env!("CARGO_PKG_VERSION"));
        process::exit(0);
    }
    // ── Process-leak guard: PR_SET_PDEATHSIG (Linux) ─────────────────────
    //
    // Request SIGKILL from the kernel the moment our parent process dies.
    // Without this, when a test runner (or the user) SIGKILLs the `leindex`
    // parent process, the parent's `Drop` impl never runs and this worker
    // keeps running — holding ~1.5 GB of ROCm/MIGraphX runtime — until its
    // idle timeout fires. During multi-project test sweeps this orphaned
    // worker accumulation was measured at ~47 GB of RAM+swap across 28
    // orphaned workers.
    //
    // PR_SET_PDEATHSIG is the most robust fix for orphaned workers because
    // it is enforced by the kernel independently of the parent's exit path
    // (graceful Drop, SIGTERM, SIGKILL, segfault, OOM kill, etc.).
    //
    // This MUST be installed BEFORE any allocations or heavy initialization
    // so the parent-death signal association is in place even if startup
    // later blocks or crashes. On non-Linux platforms this is a no-op
    // (PR_SET_PDEATHSIG is Linux-specific; macOS/Windows have no direct
    // equivalent and are unaffected).
    //
    // Belt-and-suspenders note: the main crate also inherits this worker
    // into its own process group, so group-wide signal delivery reaches
    // the worker as well. PR_SET_PDEATHSIG is the primary defense.
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0)` is a simple
        // scalar kernel syscall with no pointer arguments. The second
        // argument is the signal number (SIGKILL). The remaining arguments
        // are unused for this option and passed as 0.
        unsafe {
            let rc = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
            if rc != 0 {
                // prctl failures are extremely rare (kernel would have to
                // be out of memory for the syscall stub). We log to stderr
                // but proceed anyway — the idle timeout is the fallback.
                eprintln!(
                    "leindex-embed: warning: prctl(PR_SET_PDEATHSIG) failed (rc={}, errno may follow); \
                     worker will rely on idle timeout for cleanup",
                    rc
                );
            }
        }

        // Defensive check: if our parent already died between fork and this
        // prctl call, exit immediately rather than running with a dead parent.
        // The `getppid()` returns 1 (init/systemd) when reparented.
        let ppid = unsafe { libc::getppid() };
        if ppid == 1 {
            // Already orphaned: parent died during our startup. Exit at once
            // to avoid running with init as a faux-parent.
            eprintln!(
                "leindex-embed: parent process already exited during startup (ppid=1); \
                 exiting to avoid orphaned worker"
            );
            process::exit(0);
        }
    }

    // Initialize minimal logging
    // IMPORTANT: tracing output MUST go to stderr, not stdout, because stdout
    // is used for IPC frame communication with the parent leindex process.
    // Writing tracing logs to stdout would corrupt the IPC protocol.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    tracing::info!("leindex-embed worker starting");

    // Build runtime config from environment
    let config = RuntimeConfig::from_env();

    // Create the worker runtime
    let mut runtime = WorkerRuntime::new(config);

    // Run the main IPC loop over stdin/stdout
    // VAL-CPHASE-004: Local IPC only (stdin/stdout pipes)
    // Note: we pass io::stdin() directly (not .lock()) because the run_loop
    // spawns a helper thread that needs the reader to be Send.
    if let Err(e) = runtime.run(io::stdin(), io::stdout()) {
        tracing::error!("worker loop failed: {}", e);
        process::exit(1);
    }

    tracing::info!("leindex-embed worker exiting cleanly");
    process::exit(0);
}
