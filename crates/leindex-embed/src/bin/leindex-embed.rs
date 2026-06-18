// leindex-embed — ONNX embedding worker process
//
// This is the entry point for the separate ONNX worker binary. The main
// leindex daemon spawns this process on first embed demand and communicates
// over local IPC (stdin/stdout pipes).
//
// VAL-CPHASE-001: The worker is a separate executable built alongside leindex.
// VAL-CPHASE-004: Worker transport uses local IPC only.
// VAL-CPHASE-005: Worker cold-starts on first embed demand.
// VAL-CPHASE-006: Worker remains reusable across successive batches.
// VAL-CPHASE-007: Worker idle timeout tears down the resident model process.
// VAL-CPHASE-008: Worker restart works after idle teardown.

use std::io;
use std::process;

use leindex_embed::runtime::{RuntimeConfig, WorkerRuntime};

fn main() {
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
}

#[cfg(test)]
mod tests {
    use leindex_embed::protocol::{self, BatchId, EmbedRequest, Frame, MsgType};
    use leindex_embed::runtime::{WorkerRuntime, DEFAULT_IDLE_TIMEOUT_SECS};
    use std::io::Cursor;
    use std::time::Duration;

    #[test]
    fn test_binary_embed_roundtrip_via_runtime() {
        // Build an embed request frame
        let request = EmbedRequest {
            texts: vec!["hello".to_string(), "world".to_string()],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let wire = frame.encode_wire().unwrap();

        // Verify frame encoding
        let decoded = Frame::from_wire_bytes(&wire[4..]).unwrap();
        assert_eq!(decoded.header.batch_id, BatchId::new(1));
        assert_eq!(decoded.header.msg_type, MsgType::EmbedRequest);
    }

    #[test]
    fn test_runtime_handles_embed_request() {
        let config = leindex_embed::runtime::RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let request = EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 8,
        };
        let frame = protocol::embed_request_frame(BatchId::new(42), request).unwrap();
        let response_frame = rt.dispatch(&frame);

        assert_eq!(response_frame.header.batch_id, BatchId::new(42));

        // Without a real ONNX session, dispatch returns an error frame
        #[cfg(feature = "onnx")]
        {
            assert_eq!(response_frame.header.msg_type, MsgType::Error);
        }

        // Without ONNX feature, dispatch returns a success response
        #[cfg(not(feature = "onnx"))]
        {
            assert_eq!(response_frame.header.msg_type, MsgType::EmbedResponse);
        }
    }

    #[test]
    fn test_run_loop_single_request() {
        let config = leindex_embed::runtime::RuntimeConfig {
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            ..leindex_embed::runtime::RuntimeConfig::default()
        };
        let mut rt = WorkerRuntime::new(config);

        let request = EmbedRequest {
            texts: vec!["hello".to_string()],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let wire = frame.encode_wire().unwrap();

        let reader = Cursor::new(wire);
        let writer = Cursor::new(Vec::<u8>::new());

        let result = rt.run_loop(reader, writer);
        assert!(result.is_ok());
    }
}
