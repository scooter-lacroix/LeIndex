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

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::protocol::{self, ErrorKind, Frame, HealthResponse, MsgType, WorkerError, WorkerState};
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

    let socket_path = match parse_socket_arg(&argv) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            process::exit(2);
        }
    };
    // ── Process-leak guard: PR_SET_PDEATHSIG (Linux) ─────────────────
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
    // CRITICAL: This guard applies to ALL worker modes — both pipe and
    // socket/daemon workers. Socket workers call `setsid()`, detaching
    // them from the parent's process group, so group-wide signal delivery
    // does NOT reach them. Without PR_SET_PDEATHSIG, socket daemons survive
    // indefinitely when the parent exits, becoming orphan zombies until
    // their idle timeout fires (up to 10 minutes).
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0)` is a simple
        // scalar kernel syscall with no pointer arguments. The second
        // argument is the signal number (SIGKILL). The remaining arguments
        // are unused for this option and passed as 0.
        unsafe {
            let rc = libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
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
    let mut config = RuntimeConfig::from_env();
    if socket_path.is_some() {
        config.idle_timeout = Duration::from_secs(600);
    }

    if let Some(path) = socket_path {
        // Bind the readiness endpoint before loading ORT/model state. A
        // spawning client can now distinguish "process exists but is
        // initializing" from "process failed before it had an FD" and the
        // daemon lock is never held while waiting for a missing socket.
        if let Err(e) = run_socket_worker(config, path) {
            tracing::error!("socket worker failed: {}", e);
            process::exit(1);
        }
    } else {
        // Pipe workers have no external readiness endpoint, so initialize
        // immediately before entering their framed IPC loop.
        //
        // The runtime is scoped so it is dropped — releasing the ONNX/MIGraphX
        // session and GPU resources via `WorkerRuntime::Drop` — BEFORE the clean
        // `process::exit(0)` below, which otherwise skips all destructors.
        let loop_error = {
            let runtime = WorkerRuntime::new(config);
            // Run the main IPC loop over stdin/stdout
            // VAL-CPHASE-004: Local IPC only (stdin/stdout pipes)
            // Note: we pass io::stdin() directly (not .lock()) because the run_loop
            // spawns a helper thread that needs the reader to be Send.
            runtime.run(io::stdin(), io::stdout()).err()
        };
        // `runtime` dropped here → GPU resources released.
        if let Some(e) = loop_error {
            tracing::error!("worker loop failed: {}", e);
            process::exit(1);
        }
    }

    tracing::info!("leindex-embed worker exiting cleanly");
    process::exit(0);
}

fn parse_socket_arg(argv: &[String]) -> Result<Option<PathBuf>, &'static str> {
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        if arg == "--socket" {
            return iter
                .next()
                .map(PathBuf::from)
                .map(Some)
                .ok_or("--socket requires a path");
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn run_socket_worker(config: RuntimeConfig, socket_path: PathBuf) -> anyhow::Result<()> {
    use std::os::unix::net::UnixListener;

    let status_path = socket_path.with_extension("status");
    let pid_path = socket_path.with_extension("pid");
    let initial_health = HealthResponse {
        state: WorkerState::Initializing,
        phase: "initializing".to_string(),
        started_unix_ms: unix_now_ms(),
        provider: Some(config.execution_provider.clone()),
        model: config.model_name.clone(),
        error: None,
    };
    write_worker_pid(&pid_path, process::id())?;
    write_worker_status(&status_path, "initializing")?;
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            let _ = write_worker_status(&status_path, "failed");
            let _ = std::fs::remove_file(&status_path);
            let _ = std::fs::remove_file(&pid_path);
            return Err(error.into());
        }
    };
    listener.set_nonblocking(true)?;
    // Socket is visible now. Model/ORT initialization runs in exactly one
    // thread so health requests and fast fallback remain responsive.
    let lifecycle = Arc::new(SocketLifecycle::new(initial_health));
    let init_lifecycle = Arc::clone(&lifecycle);
    let init_status_path = status_path.clone();
    std::thread::spawn(move || {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| WorkerRuntime::new(config)));
        match result {
            Ok(runtime) if runtime.is_neural_ready() => {
                runtime.log_startup_report();
                let health = runtime.health_response(WorkerState::Ready, None);
                init_lifecycle.set_ready(runtime, health);
                let _ = write_worker_status(&init_status_path, "ready");
            }
            Ok(runtime) => {
                runtime.log_startup_report();
                let error = "neural runtime unavailable after initialization".to_string();
                let health = runtime.health_response(WorkerState::Failed, Some(error));
                init_lifecycle.set_failed(health);
                let _ = write_worker_status(&init_status_path, "failed");
            }
            Err(_) => {
                let health = init_lifecycle
                    .failed_health("worker runtime initialization panicked".to_string());
                init_lifecycle.set_failed(health);
                let _ = write_worker_status(&init_status_path, "failed");
            }
        }
    });
    tracing::info!(
        "leindex-embed socket worker listening at {}",
        socket_path.display()
    );

    loop {
        if lifecycle.is_failed() {
            tracing::error!("socket worker initialization failed; shutting down");
            lifecycle.shutdown();
            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&status_path);
            let _ = std::fs::remove_file(&pid_path);
            return Ok(());
        }
        if let Some(runtime) = lifecycle.runtime() {
            if runtime.is_idle_expired() {
                tracing::info!("socket worker idle timeout expired");
                lifecycle.shutdown();
                let _ = std::fs::remove_file(&socket_path);
                let _ = std::fs::remove_file(&status_path);
                let _ = std::fs::remove_file(&pid_path);
                return Ok(());
            }
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                // `UnixListener` is nonblocking so the accept loop can
                // observe idle shutdown, but accepted sockets must be
                // blocking. Otherwise the first health/frame read can race
                // the peer write and return EAGAIN, which looks like a dead
                // worker to the client.
                if let Err(error) = stream.set_nonblocking(false) {
                    tracing::warn!(error = %error, "failed to make worker client socket blocking");
                    continue;
                }
                let connection_lifecycle = Arc::clone(&lifecycle);
                std::thread::spawn(move || handle_socket_client(connection_lifecycle, stream));
            }
            Err(e) => {
                let Some(delay) = accept_retry_delay(&e) else {
                    let _ = write_worker_status(&status_path, "failed");
                    let _ = std::fs::remove_file(&status_path);
                    let _ = std::fs::remove_file(&pid_path);
                    return Err(e.into());
                };
                if e.kind() != io::ErrorKind::WouldBlock {
                    tracing::warn!(error = %e, "transient socket accept error; retrying");
                }
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
            }
        }
    }
}

#[cfg(unix)]
enum SocketLifecycleState {
    Initializing(HealthResponse),
    Ready {
        runtime: WorkerRuntime,
        health: HealthResponse,
    },
    Failed(HealthResponse),
}

#[cfg(unix)]
struct SocketLifecycle {
    state: Mutex<SocketLifecycleState>,
}

#[cfg(unix)]
impl SocketLifecycle {
    fn new(health: HealthResponse) -> Self {
        Self {
            state: Mutex::new(SocketLifecycleState::Initializing(health)),
        }
    }

    fn runtime(&self) -> Option<WorkerRuntime> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*state {
            SocketLifecycleState::Ready { runtime, .. } => Some(runtime.clone()),
            SocketLifecycleState::Initializing(_) | SocketLifecycleState::Failed(_) => None,
        }
    }

    fn health(&self) -> HealthResponse {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*state {
            SocketLifecycleState::Initializing(health)
            | SocketLifecycleState::Failed(health)
            | SocketLifecycleState::Ready { health, .. } => health.clone(),
        }
    }

    fn is_failed(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        matches!(&*state, SocketLifecycleState::Failed(_))
    }

    fn failed_health(&self, error: String) -> HealthResponse {
        let mut health = self.health();
        health.state = WorkerState::Failed;
        health.phase = "failed".to_string();
        health.error = Some(error);
        health
    }

    fn set_ready(&self, runtime: WorkerRuntime, health: HealthResponse) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = SocketLifecycleState::Ready { runtime, health };
    }

    fn set_failed(&self, health: HealthResponse) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *state = SocketLifecycleState::Failed(health);
    }

    /// Drain the runtime so its ONNX session (and any MIGraphX/ROCm GPU
    /// resources) are released deterministically on shutdown, instead of
    /// waiting for `process::exit`/SIGKILL (which skip `Drop`). The Ready
    /// runtime is replaced with a Failed health so any lingering probe observes
    /// a terminal state. No-op when the worker never reached Ready.
    fn shutdown(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Derive the Failed health from the held state WITHOUT calling
        // self.failed_health()/self.health(): both re-lock this non-reentrant
        // Mutex, and we already hold it here -> deadlock. The worker would hang
        // in shutdown, never releasing its ONNX/GPU session or removing its
        // socket/PID files (multi-GB process stuck). Read the health straight
        // off the locked state instead.
        let mut next_health = match &*state {
            SocketLifecycleState::Initializing(health)
            | SocketLifecycleState::Failed(health)
            | SocketLifecycleState::Ready { health, .. } => health.clone(),
        };
        next_health.state = WorkerState::Failed;
        next_health.phase = "failed".to_string();
        next_health.error = Some("worker shutting down".to_string());
        let previous = std::mem::replace(&mut *state, SocketLifecycleState::Failed(next_health));
        // Dropping the previous Ready{runtime,..} drops the canonical runtime
        // clone; WorkerRuntime::Drop frees the session when this is the last Arc.
        drop(previous);
    }
}

#[cfg(unix)]
fn write_worker_status(path: &std::path::Path, status: &str) -> io::Result<()> {
    let next = path.with_extension("status.next");
    std::fs::write(&next, format!("{}\n", status))?;
    std::fs::rename(next, path)
}

#[cfg(unix)]
fn write_worker_pid(path: &std::path::Path, pid: u32) -> io::Result<()> {
    let next = path.with_extension("pid.next");
    std::fs::write(&next, format!("{}\n", pid))?;
    std::fs::rename(next, path)
}

#[cfg(unix)]
fn handle_socket_client(lifecycle: Arc<SocketLifecycle>, stream: std::os::unix::net::UnixStream) {
    let Some(runtime) = lifecycle.runtime() else {
        return handle_not_ready_client(lifecycle, stream);
    };
    let reader = match stream.try_clone() {
        Ok(reader) => reader,
        Err(e) => {
            tracing::warn!(error = %e, "failed to clone embedding client socket");
            return;
        }
    };

    if let Err(e) = runtime.run_loop(reader, stream) {
        if e.downcast_ref::<io::Error>().is_some_and(|io_err| {
            matches!(
                io_err.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::UnexpectedEof
            )
        }) {
            tracing::debug!("socket client disconnected before response was written");
        } else {
            tracing::warn!(error = %e, "embedding socket client failed");
        }
    }
}

#[cfg(unix)]
fn handle_not_ready_client(
    lifecycle: Arc<SocketLifecycle>,
    mut stream: std::os::unix::net::UnixStream,
) {
    let mut len_buf = [0u8; 4];
    if stream.read_exact(&mut len_buf).is_err() {
        return;
    }
    let payload_len = u32::from_le_bytes(len_buf) as usize;
    let max_frame = RuntimeConfig::default().max_frame_size.saturating_mul(2);
    if payload_len > max_frame {
        return;
    }
    let mut payload = vec![0u8; payload_len];
    if stream.read_exact(&mut payload).is_err() {
        return;
    }
    let Ok(frame) = Frame::from_wire_bytes(&payload) else {
        return;
    };
    let response = if frame.header.msg_type == MsgType::HealthRequest {
        protocol::health_response_frame(frame.header.batch_id, lifecycle.health())
    } else {
        let health = lifecycle.health();
        let kind = if health.state == WorkerState::Initializing {
            ErrorKind::Initializing
        } else {
            ErrorKind::OnnxRuntime
        };
        protocol::error_frame(
            frame.header.batch_id,
            WorkerError {
                kind,
                message: health
                    .error
                    .unwrap_or_else(|| format!("worker is {}", health.phase)),
            },
        )
    };
    let Ok(response) = response.and_then(|response| response.encode_wire()) else {
        return;
    };
    let _ = stream.write_all(&response);
    let _ = stream.flush();
}

#[cfg(unix)]
fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn accept_retry_delay(error: &io::Error) -> Option<Duration> {
    match error.kind() {
        io::ErrorKind::WouldBlock => Some(Duration::from_millis(100)),
        io::ErrorKind::Interrupted => Some(Duration::ZERO),
        io::ErrorKind::ConnectionAborted => Some(Duration::from_millis(10)),
        _ if matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) => {
            Some(Duration::from_millis(250))
        }
        _ => None,
    }
}

#[cfg(not(unix))]
fn run_socket_worker(_config: RuntimeConfig, _socket_path: PathBuf) -> anyhow::Result<()> {
    anyhow::bail!("socket worker mode is only supported on Unix")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn socket_argument_requires_a_path() {
        assert_eq!(parse_socket_arg(&["worker".into()]), Ok(None));
        assert_eq!(
            parse_socket_arg(&["worker".into(), "--socket".into()]),
            Err("--socket requires a path")
        );
        assert_eq!(
            parse_socket_arg(&["worker".into(), "--socket".into(), "run.sock".into()]),
            Ok(Some(PathBuf::from("run.sock")))
        );
    }

    #[test]
    fn accept_retry_delay_covers_transient_errors() {
        assert_eq!(
            accept_retry_delay(&io::Error::from(io::ErrorKind::WouldBlock)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            accept_retry_delay(&io::Error::from(io::ErrorKind::Interrupted)),
            Some(Duration::ZERO)
        );
        assert_eq!(
            accept_retry_delay(&io::Error::from_raw_os_error(libc::EMFILE)),
            Some(Duration::from_millis(250))
        );
        assert!(accept_retry_delay(&io::Error::from(io::ErrorKind::InvalidInput)).is_none());
    }

    #[test]
    fn initializing_socket_answers_health_and_rejects_inference() {
        use std::os::unix::net::UnixStream;

        let lifecycle = Arc::new(SocketLifecycle::new(HealthResponse {
            state: WorkerState::Initializing,
            phase: "initializing".to_string(),
            started_unix_ms: 1,
            provider: Some("cpu".to_string()),
            model: "test-model".to_string(),
            error: None,
        }));

        let (mut client, server) = UnixStream::pair().unwrap();
        let server_lifecycle = Arc::clone(&lifecycle);
        let worker = std::thread::spawn(move || handle_not_ready_client(server_lifecycle, server));

        let health_batch = protocol::BatchId::new(1);
        client
            .write_all(
                &protocol::health_request_frame(health_batch)
                    .unwrap()
                    .encode_wire()
                    .unwrap(),
            )
            .unwrap();
        let mut len = [0u8; 4];
        client.read_exact(&mut len).unwrap();
        let mut payload = vec![0; u32::from_le_bytes(len) as usize];
        client.read_exact(&mut payload).unwrap();
        let response = Frame::from_wire_bytes(&payload).unwrap();
        let decoded: protocol::Response = response.decode_payload().unwrap();
        match decoded {
            protocol::Response::Health(health) => {
                assert_eq!(health.state, WorkerState::Initializing)
            }
            _ => panic!("expected health response"),
        }
        worker.join().unwrap();

        let (mut client, server) = UnixStream::pair().unwrap();
        let server_lifecycle = Arc::clone(&lifecycle);
        let worker = std::thread::spawn(move || handle_not_ready_client(server_lifecycle, server));
        let embed = protocol::embed_request_frame(
            protocol::BatchId::new(2),
            protocol::EmbedRequest {
                texts: vec!["test".to_string()],
                expected_dim: 4,
            },
        )
        .unwrap();
        client.write_all(&embed.encode_wire().unwrap()).unwrap();
        client.read_exact(&mut len).unwrap();
        let mut payload = vec![0; u32::from_le_bytes(len) as usize];
        client.read_exact(&mut payload).unwrap();
        let response = Frame::from_wire_bytes(&payload).unwrap();
        let decoded: protocol::Response = response.decode_payload().unwrap();
        match decoded {
            protocol::Response::Error(error) => assert_eq!(error.kind, ErrorKind::Initializing),
            _ => panic!("expected initializing error"),
        }
        worker.join().unwrap();
    }
}
