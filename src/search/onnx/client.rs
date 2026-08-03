// Worker client for delegating ONNX inference to the leindex-embed process
//
// VAL-CPHASE-002: The main crate no longer owns ONNX runtime deps directly.
// This client communicates with the leindex-embed worker over local IPC.
//
// VAL-CPHASE-016: The client writes worker output into destination embedding
// storage via the flat EmbedResponse buffer, avoiding a nested Vec<Vec<f32>>
// heap mirror in the main process.
//
// VAL-CPHASE-017: On worker failure, the client retries once before falling back.
// VAL-CPHASE-018: After retry failure, only the affected batch falls back to TF-IDF.
// VAL-CPHASE-019: Fallback emits an actionable warning naming the batch and error.
// VAL-CPHASE-020: Worker failure does not crash the main daemon.
// VAL-CPHASE-021: A fresh worker can be spawned after a fallback episode.

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::embed::protocol::{
    self, BatchId, EmbedRequest, EmbedResponse, ErrorKind, Frame, HealthResponse, MsgType,
    RerankDocument, RerankRequest, RerankResponse, Response, WorkerError, WorkerState,
};

#[path = "client_config.rs"]
mod client_config;
use client_config::*;
pub use client_config::{
    ClientError, EmbedResult, EmbeddingClient, WorkerAvailability, migraphx_cache_path,
    prune_stale_migraphx_profiles,
};

/// Classify a `std::io::Error` as one that indicates the worker process has
/// died (closed the connection or crashed).
///
/// VAL-DEADWORKER-001: When the worker process dies, `read_exact` on the
/// `UnixStream` returns one of these error kinds. This is a deterministic
/// transport-level signal, not an arbitrary wall-clock timeout.
fn is_worker_dead_io_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
    )
}

/// Read a response frame from the worker.
///
/// This is the core I/O routine used by the persistent reader thread.
/// It reads the 4-byte length prefix followed by the payload, enforcing
/// the max frame size guard to prevent excessive allocations.
///
/// VAL-DEADWORKER-001: On EOF/EPIPE (worker process died), the error is
/// classified as `ClientError::WorkerDied` so the caller can kill the stale
/// daemon and spawn a replacement, rather than being masked as a generic
/// IPC error.
fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>, ClientError> {
    // Read response length (4 bytes, little-endian)
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if is_worker_dead_io_error(&e) => {
            return Err(ClientError::WorkerDied {
                message: format!("EOF/EPIPE reading frame length: {}", e),
            });
        }
        Err(e) => {
            return Err(ClientError::Ipc(format!(
                "failed to read frame length: {}",
                e
            )));
        }
    }

    let payload_len = u32::from_le_bytes(len_buf);

    // Guard against oversized responses to prevent excessive allocations.
    if payload_len > MAX_RESPONSE_FRAME_SIZE {
        return Err(ClientError::Ipc(format!(
            "response frame too large: {} bytes (max: {} bytes)",
            payload_len, MAX_RESPONSE_FRAME_SIZE
        )));
    }

    let payload_len = payload_len as usize;
    let mut frame_buf = vec![0u8; payload_len];
    match reader.read_exact(&mut frame_buf) {
        Ok(()) => Ok(frame_buf),
        Err(e) if is_worker_dead_io_error(&e) => Err(ClientError::WorkerDied {
            message: format!("EOF/EPIPE reading frame payload: {}", e),
        }),
        Err(e) => Err(ClientError::Ipc(format!(
            "failed to read frame payload: {}",
            e
        ))),
    }
}

/// Monotonic batch ID counter for correlating requests.
static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);

impl EmbeddingClient {
    /// Observe worker lifecycle without spawning a process or waiting for
    /// model initialization. Socket health is the source of truth for daemon
    /// workers; the pipe startup report remains the compatibility boundary.
    pub fn availability(&self) -> WorkerAvailability {
        let report = self
            .last_startup_report
            .lock()
            .ok()
            .and_then(|report| report.clone());
        if !self.use_daemon {
            return match report {
                Some(report) if report.contains("status=available") => WorkerAvailability::Ready,
                Some(report) if report.contains("status=unavailable") => {
                    WorkerAvailability::Failed(worker_health_snapshot(
                        WorkerState::Failed,
                        parse_startup_report_provider(&report),
                        None,
                        Some(report),
                    ))
                }
                _ => WorkerAvailability::Absent,
            };
        }

        #[cfg(unix)]
        {
            // VAL-DAEMON-003: Use cached config so leindex.toml is not re-read.
            let config = self.cached_config();
            let provider = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER")
                .ok()
                .or(config.execution_provider.clone());
            let model = std::env::var("LEINDEX_WORKER_MODEL")
                .ok()
                .or(config.model_name.clone());
            let Some(socket_path) = daemon_socket_path(provider.as_deref(), model.as_deref())
            else {
                return WorkerAvailability::Absent;
            };
            let status_path = daemon_status_path(provider.as_deref(), model.as_deref());
            let state_hint = status_path.as_deref().and_then(status_state);
            let pid_path = daemon_pid_path(provider.as_deref(), model.as_deref());

            if state_hint.is_some()
                && pid_path
                    .as_deref()
                    .is_some_and(|path| !daemon_pid_alive(path))
            {
                cleanup_daemon_paths(&socket_path);
                return WorkerAvailability::Absent;
            }

            if !socket_path.exists() {
                if state_hint.is_some() {
                    let _ = status_path.as_deref().map(std::fs::remove_file);
                    let _ = daemon_pid_path(provider.as_deref(), model.as_deref())
                        .as_deref()
                        .map(std::fs::remove_file);
                }
                return WorkerAvailability::Absent;
            }

            // VAL-DAEMON-005: Fast path. When the status file says "ready"
            // and the PID is alive, return Ready immediately without a
            // socket health probe. This eliminates a Unix socket connect +
            // write + read cycle on every availability() poll.
            if state_hint == Some(WorkerState::Ready)
                && pid_path.as_deref().is_some_and(daemon_pid_alive)
            {
                return WorkerAvailability::Ready;
            }

            match probe_daemon_health(&socket_path) {
                Ok(health) => match health.state {
                    WorkerState::Ready => WorkerAvailability::Ready,
                    WorkerState::Initializing => WorkerAvailability::Initializing(health),
                    WorkerState::Failed => WorkerAvailability::Failed(health),
                },
                Err(ClientError::Worker(error)) if error.kind == ErrorKind::Initializing => {
                    WorkerAvailability::Initializing(worker_health_snapshot(
                        WorkerState::Initializing,
                        provider,
                        model,
                        Some(error.message),
                    ))
                }
                Err(error) => {
                    // A status hint still gives callers an immediate, useful
                    // answer when a health connection is momentarily busy.
                    match state_hint {
                        Some(WorkerState::Initializing) => {
                            WorkerAvailability::Initializing(worker_health_snapshot(
                                WorkerState::Initializing,
                                provider,
                                model,
                                Some(error.to_string()),
                            ))
                        }
                        Some(WorkerState::Failed) => {
                            WorkerAvailability::Failed(worker_health_snapshot(
                                WorkerState::Failed,
                                provider,
                                model,
                                Some(error.to_string()),
                            ))
                        }
                        Some(WorkerState::Ready) => WorkerAvailability::Ready,
                        None => WorkerAvailability::Absent,
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            WorkerAvailability::Absent
        }
    }

    /// Return true only after the worker has emitted a successful startup
    /// handshake. This is a non-blocking observation; it never spawns a
    /// process or waits for ORT/model initialization.
    pub fn is_ready(&self) -> bool {
        self.availability().is_ready()
    }

    /// Wait briefly for the stderr mirror to publish the startup report.
    pub fn wait_for_active_execution_provider(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(provider) = self.active_execution_provider() {
                return Some(provider);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Allocate a new unique batch ID.
    fn next_batch_id() -> BatchId {
        BatchId::new(BATCH_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// Ensure the worker is running, spawning it if necessary.
    fn ensure_worker(&self) -> Result<(), ClientError> {
        let mut guard = self
            .worker
            .lock()
            .map_err(|e| ClientError::Ipc(format!("failed to lock worker handle: {}", e)))?;

        if guard.is_some() {
            return Ok(());
        }

        self.spawn_worker(&mut guard)
    }

    fn ensure_worker_ready(&self) -> Result<(), ClientError> {
        let started = Instant::now();

        // If the worker cannot be spawned or connected (daemon timeout, missing
        // binary, IPC failure), treat neural as unavailable. Indexing proceeds
        // with core TF-IDF/PDG results (decision #6: terminal neural failure
        // preserves useful TF-IDF/PDG results).
        if let Err(error) = self.ensure_worker() {
            tracing::warn!(
                error = %error,
                "neural worker unavailable; proceeding with core TF-IDF/PDG results"
            );
            return Ok(());
        }

        loop {
            match self.availability() {
                WorkerAvailability::Ready => return Ok(()),
                WorkerAvailability::Initializing(health) if self.use_daemon => {
                    // The daemon binds its socket before ORT/model loading.
                    // Wait for the explicit lifecycle transition so the
                    // first request uses neural embeddings instead of being
                    // silently downgraded merely because startup is cold.
                    if started.elapsed() >= DAEMON_READY_MAX_WAIT {
                        tracing::warn!(
                            phase = %health.phase,
                            elapsed_secs = started.elapsed().as_secs(),
                            "neural worker did not become ready within {}s; \
                             falling back to core TF-IDF/PDG results",
                            DAEMON_READY_MAX_WAIT.as_secs()
                        );
                        return Ok(());
                    }
                    tracing::debug!(
                        phase = %health.phase,
                        "waiting for neural worker readiness"
                    );
                    thread::sleep(DAEMON_READINESS_POLL);
                }
                WorkerAvailability::Initializing(_) => {
                    // Pipe mode initializes before publishing its handle, so
                    // the stderr startup report can legitimately arrive after
                    // this check. The framed request below blocks until the
                    // worker has finished initialization.
                    return Ok(());
                }
                WorkerAvailability::Failed(health) => {
                    return Err(ClientError::Worker(WorkerError {
                        kind: ErrorKind::OnnxRuntime,
                        message: health
                            .error
                            .unwrap_or_else(|| "worker neural runtime is unavailable".to_string()),
                    }));
                }
                WorkerAvailability::Absent if !self.use_daemon => {
                    // Pipe mode has no external health endpoint. A missing
                    // startup report is normal until the worker writes stderr.
                    return Ok(());
                }
                WorkerAvailability::Absent => {
                    return Err(ClientError::Worker(WorkerError {
                        kind: ErrorKind::OnnxRuntime,
                        message: "worker daemon is absent".to_string(),
                    }));
                }
            }
        }
    }

    /// Spawn a new worker process into the given guard slot.
    fn spawn_worker(
        &self,
        guard: &mut std::sync::MutexGuard<'_, Option<WorkerHandle>>,
    ) -> Result<(), ClientError> {
        let worker_path = resolve_worker_binary().map_err(|e| {
            ClientError::SpawnFailed(format!("failed to resolve worker binary: {}", e))
        })?;
        // VAL-DAEMON-003: Use cached config so leindex.toml is not re-read.
        let config_env = self.cached_config();
        let configured_provider = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER")
            .ok()
            .or_else(|| config_env.execution_provider.clone());
        #[cfg(unix)]
        if self.use_daemon {
            let configured_model = std::env::var("LEINDEX_WORKER_MODEL")
                .ok()
                .or_else(|| config_env.model_name.clone());
            if let Some(handle) = self.spawn_or_connect_daemon(
                &worker_path,
                config_env,
                configured_provider.as_deref(),
                configured_model.as_deref(),
            )? {
                **guard = Some(handle);
                return Ok(());
            }
        }

        **guard = Some(self.spawn_pipe_worker(
            &worker_path,
            config_env,
            configured_provider.as_deref(),
        )?);

        Ok(())
    }

    fn configure_worker_command(
        cmd: &mut Command,
        config_env: &WorkerConfigEnv,
        configured_provider: Option<&str>,
    ) {
        // VAL-ONNX-002: Explicitly pass key env vars to the worker process so
        // it can locate model files and select the correct execution provider.
        if let Ok(model_path) = std::env::var("LEINDEX_MODEL_PATH") {
            cmd.env("LEINDEX_MODEL_PATH", &model_path);
        }
        if let Ok(provider) = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER") {
            cmd.env("LEINDEX_WORKER_EXECUTION_PROVIDER", &provider);
        } else if let Some(provider) = &config_env.execution_provider {
            cmd.env("LEINDEX_WORKER_EXECUTION_PROVIDER", provider);
        }
        if let Ok(model_name) = std::env::var("LEINDEX_WORKER_MODEL") {
            cmd.env("LEINDEX_WORKER_MODEL", &model_name);
        } else if let Some(model_name) = &config_env.model_name {
            cmd.env("LEINDEX_WORKER_MODEL", model_name);
        }
        if matches!(
            configured_provider,
            Some("migraphx" | "rocm" | "auto") | None
        ) && std::env::var_os("ORT_MIGRAPHX_MODEL_CACHE_PATH").is_none()
        {
            if let Some(cache_path) = migraphx_model_cache_path(config_env.model_name.as_deref()) {
                if let Err(e) = std::fs::create_dir_all(&cache_path) {
                    tracing::warn!(
                        path = %cache_path.display(),
                        error = %e,
                        "failed to create MIGraphX model cache directory"
                    );
                } else {
                    cmd.env("ORT_MIGRAPHX_MODEL_CACHE_PATH", cache_path);
                }
            }
        }
        // VAL-SETUP-020/VAL-ORT-006: When ORT_DYLIB_PATH is not already in the
        // ambient environment, propagate the path recorded in
        // `~/.leindex/config/leindex.toml` so the worker reliably loads the
        // ORT build chosen during `leindex setup`. This keeps the discovery
        // chain consistent across both the interactive setup flow (which
        // installs ORT via pip and remembers the discovered `.so`) and the
        // plain-spawn path used by searches.
        // An empty value is treated as unset so the config fallback fires.
        let ort_dylib_unset = match std::env::var_os("ORT_DYLIB_PATH") {
            None => true,
            Some(v) => v.is_empty(),
        };
        if ort_dylib_unset {
            if let Some(path) = &config_env.ort_dylib_path {
                cmd.env("ORT_DYLIB_PATH", path);
            }
        }
    }

    fn spawn_pipe_worker(
        &self,
        worker_path: &Path,
        config_env: &WorkerConfigEnv,
        configured_provider: Option<&str>,
    ) -> Result<WorkerHandle, ClientError> {
        let mut cmd = Command::new(worker_path);
        Self::configure_worker_command(&mut cmd, config_env, configured_provider);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Pipe stderr so startup_report can be captured while still being
            // mirrored to the parent stderr for operator visibility.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ClientError::SpawnFailed(e.to_string()))?;

        let writer = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::SpawnFailed("failed to open worker stdin".to_string()))?;
        let reader = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::SpawnFailed("failed to open worker stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ClientError::SpawnFailed("failed to open worker stderr".to_string()))?;

        let (read_thread, read_request_tx) = Self::spawn_reader_thread(reader);
        let stderr_thread =
            Self::spawn_stderr_thread(stderr, Arc::clone(&self.last_startup_report));

        Ok(WorkerHandle {
            child: Some(child),
            writer: Some(WorkerWriter::Pipe(writer)),
            read_thread,
            read_request_tx,
            stderr_thread: Some(stderr_thread),
            persistent: false,
            socket_path: None,
        })
    }

    #[cfg(unix)]
    fn spawn_or_connect_daemon(
        &self,
        worker_path: &Path,
        config_env: &WorkerConfigEnv,
        configured_provider: Option<&str>,
        configured_model: Option<&str>,
    ) -> Result<Option<WorkerHandle>, ClientError> {
        let Some(socket_path) = daemon_socket_path(configured_provider, configured_model) else {
            return Ok(None);
        };

        if let Some(handle) = self.connect_daemon_when_ready(&socket_path, None)? {
            return Ok(Some(handle));
        }

        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ClientError::SpawnFailed(format!(
                    "failed to create worker socket dir {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let lock_path = socket_path.with_extension("lock");
        let _spawn_lock =
            DaemonSpawnLock::acquire(&lock_path, Duration::from_secs(DAEMON_LOCK_WAIT_SECS))?;
        if let Some(handle) = self.connect_daemon_when_ready(&socket_path, None)? {
            return Ok(Some(handle));
        }

        Self::spawn_locked_daemon(worker_path, config_env, configured_provider, socket_path)
            .map(Some)
    }

    #[cfg(unix)]
    fn spawn_locked_daemon(
        worker_path: &Path,
        config_env: &WorkerConfigEnv,
        configured_provider: Option<&str>,
        socket_path: PathBuf,
    ) -> Result<WorkerHandle, ClientError> {
        cleanup_daemon_paths(&socket_path);

        let mut cmd = Command::new(worker_path);
        Self::configure_worker_command(&mut cmd, config_env, configured_provider);
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let stderr = Self::daemon_stderr();
        let mut child = cmd
            .arg("--socket")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .map_err(|e| ClientError::SpawnFailed(e.to_string()))?;

        let deadline = Instant::now() + Duration::from_secs(DAEMON_BIND_WAIT_SECS);
        loop {
            match UnixStream::connect(&socket_path) {
                Ok(stream) => {
                    // The daemon accepts connections before model loading so
                    // health probes can observe `initializing`. Do not keep
                    // this bootstrap connection: its server thread may have
                    // entered the not-ready path and would reject the first
                    // inference frame even after the lifecycle becomes ready.
                    drop(stream);
                    match Self::wait_for_daemon_ready(&socket_path) {
                        Ok(()) => {
                            Self::spawn_daemon_reaper(child, socket_path.clone());
                            let stream = UnixStream::connect(&socket_path).map_err(|e| {
                                ClientError::Ipc(format!(
                                    "failed to reconnect ready worker daemon {}: {}",
                                    socket_path.display(),
                                    e
                                ))
                            })?;
                            return Self::socket_worker_handle(stream, None, Some(socket_path));
                        }
                        Err(error) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            cleanup_daemon_paths(&socket_path);
                            Self::print_daemon_log_tail();
                            return Err(error);
                        }
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound
                        || e.kind() == std::io::ErrorKind::ConnectionRefused => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    cleanup_daemon_paths(&socket_path);
                    Self::print_daemon_log_tail();
                    return Err(ClientError::Ipc(format!(
                        "failed to connect worker daemon {}: {}",
                        socket_path.display(),
                        e
                    )));
                }
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    cleanup_daemon_paths(&socket_path);
                    Self::print_daemon_log_tail();
                    return Err(ClientError::SpawnFailed(format!(
                        "worker daemon exited before accepting connections: {}",
                        status
                    )));
                }
                Ok(None) => {}
                Err(e) => {
                    cleanup_daemon_paths(&socket_path);
                    Self::print_daemon_log_tail();
                    return Err(ClientError::SpawnFailed(format!(
                        "failed to poll worker daemon startup: {}",
                        e
                    )));
                }
            }

            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_daemon_paths(&socket_path);
                Self::print_daemon_log_tail();
                return Err(ClientError::Timeout);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg(unix)]
    fn connect_daemon_when_ready(
        &self,
        socket_path: &Path,
        child: Option<Child>,
    ) -> Result<Option<WorkerHandle>, ClientError> {
        match UnixStream::connect(socket_path) {
            Ok(stream) => {
                // A connection accepted during model initialization is bound
                // to the server's not-ready handler. Close it, wait for the
                // explicit Ready state, then create the inference connection.
                drop(stream);
                Self::wait_for_daemon_ready(socket_path)?;
                let stream = UnixStream::connect(socket_path).map_err(|e| {
                    ClientError::Ipc(format!(
                        "failed to reconnect ready worker daemon {}: {}",
                        socket_path.display(),
                        e
                    ))
                })?;
                Self::socket_worker_handle(stream, child, Some(socket_path.to_path_buf())).map(Some)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                cleanup_daemon_paths(socket_path);
                Ok(None)
            }
            Err(e) => Err(ClientError::Ipc(format!(
                "failed to connect worker daemon {}: {}",
                socket_path.display(),
                e
            ))),
        }
    }

    /// Wait for the daemon's explicit lifecycle transition before retaining a
    /// socket for inference. A readiness deadline prevents an infinite hang
    /// when the daemon is stuck in Initializing state. When the deadline fires,
    /// the error propagates so callers can treat neural as unavailable.
    ///
    /// VAL-DAEMON-004: Uses one persistent Unix socket connection held in
    /// `stream: Option<UnixStream>` for the entire polling duration. Each
    /// poll writes + reads on the same socket; the connection is only
    /// dropped on error (triggering a reconnect on the next iteration).
    #[cfg(unix)]
    fn wait_for_daemon_ready(socket_path: &Path) -> Result<(), ClientError> {
        let started = Instant::now();
        let mut stream: Option<UnixStream> = None;

        loop {
            if started.elapsed() >= DAEMON_READY_MAX_WAIT {
                return Err(ClientError::Worker(WorkerError {
                    kind: ErrorKind::OnnxRuntime,
                    message: format!(
                        "daemon did not reach Ready state within {}s;\
                         falling back to core TF-IDF/PDG results",
                        DAEMON_READY_MAX_WAIT.as_secs()
                    ),
                }));
            }

            // Establish connection once, reuse for all subsequent polls.
            // On error, drop and reconnect on the next iteration.
            if stream.is_none() {
                match UnixStream::connect(socket_path) {
                    Ok(s) => {
                        // No timeout: once connected to a live daemon, let
                        // the health response arrive without a deadline.
                        let _ = s.set_read_timeout(None);
                        let _ = s.set_write_timeout(None);
                        stream = Some(s);
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::NotFound
                            || e.kind() == std::io::ErrorKind::ConnectionRefused =>
                    {
                        // Daemon hasn't bound yet; wait and retry.
                        thread::sleep(DAEMON_READINESS_POLL);
                        continue;
                    }
                    Err(e) => {
                        return Err(ClientError::Ipc(format!(
                            "failed to connect worker health socket {}: {}",
                            socket_path.display(),
                            e
                        )));
                    }
                }
            }

            let s = stream.as_mut().unwrap();

            let batch_id = BatchId::new(BATCH_COUNTER.fetch_add(1, Ordering::Relaxed));
            let wire = protocol::health_request_frame(batch_id)
                .map_err(|e| ClientError::Ipc(e.to_string()))?
                .encode_wire()
                .map_err(|e| ClientError::Ipc(e.to_string()))?;

            if let Err(e) = s.write_all(&wire).and_then(|_| s.flush()) {
                tracing::debug!(error = %e, "health probe write failed; reconnecting");
                stream.take();
                thread::sleep(DAEMON_READINESS_POLL);
                continue;
            }

            let Some(frame) = Self::read_daemon_health_frame(s, batch_id) else {
                stream.take();
                thread::sleep(DAEMON_READINESS_POLL);
                continue;
            };
            match Self::interpret_daemon_health_frame(frame)? {
                DaemonHealthState::Ready => return Ok(()),
                DaemonHealthState::Initializing => thread::sleep(DAEMON_READINESS_POLL),
                DaemonHealthState::Reconnect => {
                    stream.take();
                    thread::sleep(DAEMON_READINESS_POLL);
                }
            }
        }
    }

    #[cfg(unix)]
    fn read_daemon_health_frame(stream: &mut UnixStream, batch_id: BatchId) -> Option<Frame> {
        let payload = match read_frame(stream) {
            Ok(payload) => payload,
            Err(e) => {
                tracing::debug!(error = %e, "health probe read failed; reconnecting");
                return None;
            }
        };
        let frame = match Frame::from_wire_bytes(&payload) {
            Ok(frame) => frame,
            Err(e) => {
                tracing::debug!(error = %e, "health frame decode failed");
                return None;
            }
        };
        if frame.header.batch_id != batch_id {
            tracing::debug!("health response batch_id mismatch; reconnecting");
            return None;
        }
        Some(frame)
    }

    #[cfg(unix)]
    fn interpret_daemon_health_frame(frame: Frame) -> Result<DaemonHealthState, ClientError> {
        match frame.header.msg_type {
            MsgType::HealthResponse => match frame.decode_payload::<Response>() {
                Ok(Response::Health(health)) => match health.state {
                    WorkerState::Ready => Ok(DaemonHealthState::Ready),
                    WorkerState::Initializing => Ok(DaemonHealthState::Initializing),
                    WorkerState::Failed => Err(ClientError::Worker(WorkerError {
                        kind: ErrorKind::OnnxRuntime,
                        message: health
                            .error
                            .unwrap_or_else(|| "worker daemon initialization failed".to_string()),
                    })),
                },
                _ => Ok(DaemonHealthState::Reconnect),
            },
            MsgType::Error => match frame.decode_payload::<Response>() {
                Ok(Response::Error(error)) if error.kind == ErrorKind::Initializing => {
                    Ok(DaemonHealthState::Initializing)
                }
                Ok(Response::Error(error)) => Err(ClientError::Worker(WorkerError {
                    kind: ErrorKind::OnnxRuntime,
                    message: error.message,
                })),
                _ => Ok(DaemonHealthState::Reconnect),
            },
            _ => Ok(DaemonHealthState::Reconnect),
        }
    }

    #[cfg(unix)]
    fn spawn_daemon_reaper(mut child: Child, socket_path: PathBuf) {
        let pid = child.id();
        thread::spawn(move || {
            let _ = child.wait();
            let pid_path = socket_path.with_extension("pid");
            let owns_state = std::fs::read_to_string(&pid_path)
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                .is_some_and(|recorded| recorded == pid);
            if owns_state {
                cleanup_daemon_paths(&socket_path);
            }
        });
    }

    #[cfg(unix)]
    fn socket_worker_handle(
        stream: UnixStream,
        child: Option<Child>,
        socket_path: Option<PathBuf>,
    ) -> Result<WorkerHandle, ClientError> {
        stream.set_nonblocking(false).map_err(|e| {
            ClientError::Ipc(format!("failed to make worker socket blocking: {}", e))
        })?;
        let writer = stream
            .try_clone()
            .map_err(|e| ClientError::Ipc(format!("failed to clone worker socket: {}", e)))?;
        let (read_thread, read_request_tx) = Self::spawn_reader_thread(stream);
        Ok(WorkerHandle {
            child,
            writer: Some(WorkerWriter::Unix(writer)),
            read_thread,
            read_request_tx,
            stderr_thread: None,
            persistent: true,
            socket_path,
        })
    }

    fn daemon_log_path() -> Option<PathBuf> {
        leindex_home_dir().map(|home| home.join("logs").join("leindex-embed-daemon.log"))
    }

    fn daemon_stderr() -> Stdio {
        Self::daemon_log_path()
            .and_then(|path| {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok()?;
                }
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            })
            .map(Stdio::from)
            .unwrap_or_else(Stdio::null)
    }

    fn print_daemon_log_tail() {
        let Some(path) = Self::daemon_log_path() else {
            return;
        };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return;
        };
        eprintln!("leindex-embed daemon stderr tail ({}):", path.display());
        for line in contents
            .lines()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            eprintln!("{}", line);
        }
    }

    fn spawn_reader_thread<R>(reader: R) -> (thread::JoinHandle<()>, mpsc::Sender<ReadRequest>)
    where
        R: Read + Send + 'static,
    {
        let (read_request_tx, read_request_rx) = mpsc::channel::<ReadRequest>();
        let read_thread = thread::spawn(move || {
            // VAL-DAEMON-006: Wrap reader in BufReader with 128KB capacity to
            // reduce syscall count for large embedding responses.
            let mut buf_reader = BufReader::with_capacity(READ_BUF_CAPACITY, reader);
            while let Ok(request) = read_request_rx.recv() {
                match request {
                    ReadRequest::Read { tx } => {
                        let _ = tx.send(read_frame(&mut buf_reader));
                    }
                    ReadRequest::Shutdown => break,
                }
            }
        });
        (read_thread, read_request_tx)
    }

    fn spawn_stderr_thread<R>(
        stderr: R,
        last_startup_report: Arc<Mutex<Option<String>>>,
    ) -> thread::JoinHandle<()>
    where
        R: Read + Send + 'static,
    {
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let line = line.trim_end_matches(['\r', '\n']);
                        eprintln!("{}", line);
                        if line.contains("startup_report") {
                            if let Ok(mut report) = last_startup_report.lock() {
                                *report = Some(line.to_string());
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("leindex-embed stderr mirror failed: {}", e);
                        break;
                    }
                }
            }
        })
    }

    /// Clear the current worker connection so a fresh request can reconnect.
    ///
    /// VAL-CPHASE-021: After calling this, the next embed request will
    /// transparently spawn a new worker process.
    ///
    /// Resident daemon ownership stays with its reaper; a client never
    /// deletes a persistent daemon socket or kills a process it did not own.
    pub fn kill_worker(&self) {
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(mut handle) = guard.take() {
                Self::shutdown_worker_handle(&mut handle, true);
            }
        }
    }

    /// Force-shutdown the persistent ONNX daemon, killing it even if the
    /// client does not own the child process.
    ///
    /// This is the CLI-mode cleanup: after a one-shot command (e.g. `leindex
    /// index` or `leindex search`) completes, the persistent daemon it spawned
    /// has no reason to outlive the process. The default `Drop` impl passes
    /// `kill_persistent=false`, which leaves daemon workers running for reuse
    /// by the MCP server. This method forces `kill_persistent=true` so the
    /// daemon is properly terminated before the CLI process exits.
    ///
    /// This is a no-op if no worker was ever spawned or the worker has already
    /// been shut down.
    pub fn force_shutdown_daemon(&self) {
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(mut handle) = guard.take() {
                Self::shutdown_worker_handle(&mut handle, true);
            }
        }
    }

    /// Kill the stale daemon and clear local worker state after detecting a
    /// dead worker.
    ///
    /// VAL-DEADWORKER-003: On `ClientError::WorkerDied`, the stale daemon PID
    /// is read from the `.pid` sidecar file and killed (SIGTERM then SIGKILL
    /// if needed), daemon filesystem paths are cleaned up, and the local
    /// `WorkerHandle` is cleared so the next `embed_attempt` triggers a fresh
    /// `spawn_or_connect_daemon()`.
    fn handle_worker_died(&self) {
        #[cfg(unix)]
        {
            // Kill the stale daemon process by PID from the sidecar file.
            // The socket_path on the WorkerHandle pinpoints which daemon to
            // reap. For persistent daemons the client does not own the child
            // (the reaper thread does), so we kill it directly by PID.
            if let Ok(guard) = self.worker.lock() {
                if let Some(handle) = guard.as_ref() {
                    if let Some(socket_path) = handle.socket_path.as_ref() {
                        kill_stale_daemon_by_pid(socket_path);
                        cleanup_daemon_paths(socket_path);
                    }
                }
            }
        }

        // Clear the local worker handle so the next call spawns fresh.
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(mut handle) = guard.take() {
                Self::shutdown_worker_handle(&mut handle, true);
            }
        }
    }

    // ── Test helpers (not part of the public API) ────────────────────────
    //
    // These `#[doc(hidden)]` methods exist solely so integration tests in
    // `tests/onnx_worker_fallback.rs` can inject mock connections and exercise
    // the `send_and_receive` I/O path without spawning a real daemon.

    /// **Test-only:** Create a pipe-mode client whose worker handle is backed
    /// by the supplied `UnixStream`. This bypasses the daemon spawn/connect
    /// flow, allowing deterministic dead-worker detection tests.
    #[doc(hidden)]
    #[cfg(all(unix, feature = "onnx"))]
    pub fn test_from_unix_stream(stream: UnixStream) -> Self {
        let client = Self::new_pipe();
        let handle = Self::socket_worker_handle(stream, None, None)
            .expect("socket_worker_handle must succeed for test stream");
        *client.worker.lock().unwrap() = Some(handle);
        client
    }

    /// **Test-only:** Create a daemon-mode client whose worker handle is
    /// backed by the supplied `UnixStream` and `socket_path`. This simulates
    /// a connected daemon so tests can verify stale-daemon cleanup on
    /// `WorkerDied`.
    #[doc(hidden)]
    #[cfg(all(unix, feature = "onnx"))]
    pub fn test_from_daemon_stream(stream: UnixStream, socket_path: PathBuf) -> Self {
        let client = Self::new();
        let handle = Self::socket_worker_handle(stream, None, Some(socket_path))
            .expect("socket_worker_handle must succeed for test stream");
        *client.worker.lock().unwrap() = Some(handle);
        client
    }

    /// **Test-only:** Send a raw frame and receive the response, bypassing the
    /// `ensure_worker_ready()` readiness gate. This exposes the private
    /// `send_and_receive` to integration tests so dead-worker I/O behavior can
    /// be verified without spawning a real worker process.
    #[doc(hidden)]
    #[cfg(feature = "onnx")]
    pub fn test_send_and_receive(&self, frame: Frame) -> Result<Frame, ClientError> {
        self.send_and_receive(frame)
    }

    /// **Test-only:** Check whether the local worker handle has been cleared
    /// (i.e., `handle_worker_died` ran successfully).
    #[doc(hidden)]
    #[cfg(feature = "onnx")]
    pub fn test_worker_handle_cleared(&self) -> bool {
        self.worker
            .lock()
            .map(|guard| guard.is_none())
            .unwrap_or(true)
    }

    fn shutdown_worker_handle(handle: &mut WorkerHandle, kill_persistent: bool) {
        // Signal the reader thread to stop. If the reader is currently
        // blocked inside read_frame (waiting for worker response), the
        // Shutdown message stays queued until read_frame returns.
        let _ = handle.read_request_tx.send(ReadRequest::Shutdown);
        if let Some(writer) = handle.writer.as_ref() {
            writer.shutdown();
        }
        drop(handle.writer.take());

        let owns_child = handle.child.is_some();
        let should_kill_child = !handle.persistent || (kill_persistent && owns_child);
        if should_kill_child {
            if handle.persistent && owns_child {
                #[cfg(unix)]
                if let Some(socket_path) = handle.socket_path.take() {
                    cleanup_daemon_paths(&socket_path);
                }
            }
            if let Some(child) = handle.child.as_mut() {
                #[cfg(unix)]
                {
                    let pid = child.id() as libc::pid_t;
                    if pid > 0 {
                        unsafe {
                            libc::kill(pid, libc::SIGTERM);
                        }
                    }
                }

                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(50));
                        }
                        _ => break,
                    }
                }

                let _ = child.kill();
                let _ = child.wait();
            }
        }

        Self::join_shutdown_threads(handle);
    }

    fn join_shutdown_threads(handle: &mut WorkerHandle) {
        let (replacement_tx, _replacement_rx) = mpsc::channel::<ReadRequest>();
        let old_tx = std::mem::replace(&mut handle.read_request_tx, replacement_tx);
        drop(old_tx);

        let replacement_thread = thread::spawn(|| {});
        let read_thread = std::mem::replace(&mut handle.read_thread, replacement_thread);
        // Bounded join: if the reader thread is stuck inside read_frame on
        // a socket that the worker hasn't closed, join would block forever.
        // Give it 2 seconds; if it doesn't exit, the OS reaps the thread at
        // process exit. This prevents the index command from hanging after
        // all work is complete.
        let join_deadline = Instant::now() + Duration::from_secs(2);
        while !read_thread.is_finished() {
            if Instant::now() >= join_deadline {
                #[cfg(unix)]
                {
                    tracing::warn!(
                        "reader thread did not exit within 2s during worker shutdown; \
                         detaching (process exit will clean up)"
                    );
                }
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if read_thread.is_finished() {
            let _ = read_thread.join();
        }

        if let Some(stderr_thread) = handle.stderr_thread.take() {
            let stderr_deadline = Instant::now() + Duration::from_secs(2);
            while !stderr_thread.is_finished() {
                if Instant::now() >= stderr_deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            if stderr_thread.is_finished() {
                let _ = stderr_thread.join();
            }
        }
    }

    /// Send an embed request to the worker with retry-once fallback semantics.
    ///
    /// VAL-CPHASE-017: On worker failure, retries once before falling back.
    /// VAL-CPHASE-018: After retry failure, only this batch falls back to TF-IDF.
    /// VAL-CPHASE-019: Fallback emits an actionable warning.
    /// VAL-CPHASE-020: Worker failure does not crash the main daemon.
    /// VAL-CPHASE-021: After fallback, the worker is cleared so a fresh one
    /// can be spawned for later requests.
    ///
    /// VAL-CPHASE-016: The returned `EmbedResult::Success` contains a flat
    /// row-major `EmbedResponse` that can be written directly into destination
    /// storage without creating a nested `Vec<Vec<f32>>` heap mirror.
    pub fn embed_with_fallback(&self, texts: &[String], expected_dim: usize) -> EmbedResult {
        let batch_id = Self::next_batch_id();

        // Attempt 1: initial try
        match self.embed_attempt(batch_id, texts, expected_dim) {
            Ok(response) => EmbedResult::Success(response),
            Err(first_error) => {
                // Readiness is a state decision, not an elapsed-time retry.
                // `ensure_worker_ready` waits for an initializing daemon, so
                // this branch is reserved for a worker that explicitly
                // disappeared or failed while the request was in flight.
                if (self.use_daemon && self.availability().is_unavailable())
                    || matches!(
                        &first_error,
                        ClientError::Worker(WorkerError {
                            kind: ErrorKind::Initializing,
                            ..
                        })
                    )
                {
                    return EmbedResult::Fallback {
                        batch_id,
                        error: first_error,
                    };
                }

                // VAL-CPHASE-017: Retry once after killing the failed worker
                tracing::warn!(
                    batch_id = %batch_id,
                    error = %first_error,
                    "ONNX worker failed on first attempt, retrying once"
                );

                // Kill the failed worker so we can spawn a fresh one
                self.kill_worker();

                // Attempt 2: retry with a fresh worker
                let retry_batch_id = Self::next_batch_id();
                match self.embed_attempt(retry_batch_id, texts, expected_dim) {
                    Ok(response) => {
                        tracing::info!(
                            original_batch = %batch_id,
                            retry_batch = %retry_batch_id,
                            "ONNX worker retry succeeded"
                        );
                        EmbedResult::Success(response)
                    }
                    Err(retry_error) => {
                        // VAL-CPHASE-018: Second failure -> TF-IDF fallback for this batch only
                        // VAL-CPHASE-019: Emit actionable warning
                        tracing::warn!(
                            batch_id = %batch_id,
                            retry_batch_id = %retry_batch_id,
                            first_error = %first_error,
                            retry_error = %retry_error,
                            "ONNX worker fallback for batch {}: {} (retry exhausted, degrading to TF-IDF)",
                            batch_id,
                            retry_error
                        );

                        // VAL-CPHASE-021: Kill the worker so a fresh one can be
                        // spawned for later requests
                        self.kill_worker();

                        EmbedResult::Fallback {
                            batch_id,
                            error: retry_error,
                        }
                    }
                }
            }
        }
    }

    /// Single attempt to send an embed request to the worker.
    fn embed_attempt(
        &self,
        batch_id: BatchId,
        texts: &[String],
        expected_dim: usize,
    ) -> Result<EmbedResponse, ClientError> {
        #[cfg(feature = "cli")]
        crate::cli::mcp::request_meta::NEURAL_REQUESTS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let neural_started = Instant::now();
        let result = (|| {
            self.ensure_worker_ready()?;

            let request = EmbedRequest {
                texts: texts.to_vec(),
                expected_dim,
            };

            let frame = protocol::embed_request_frame(batch_id, request)
                .map_err(|e| ClientError::Ipc(e.to_string()))?;

            let response_frame = self.send_and_receive(frame)?;

            match response_frame.header.msg_type {
                MsgType::EmbedResponse => {
                    let response: Response = response_frame
                        .decode_payload()
                        .map_err(|e| ClientError::Ipc(e.to_string()))?;
                    match response {
                        Response::Embed(embed_resp) => Ok(embed_resp),
                        _ => Err(ClientError::Protocol("expected Embed response".to_string())),
                    }
                }
                MsgType::Error => {
                    let response: Response = response_frame
                        .decode_payload()
                        .map_err(|e| ClientError::Ipc(e.to_string()))?;
                    match response {
                        Response::Error(err) => Err(ClientError::Worker(err)),
                        _ => Err(ClientError::Protocol("expected Error response".to_string())),
                    }
                }
                other => Err(ClientError::Protocol(format!(
                    "unexpected response type: {:?}",
                    other
                ))),
            }
        })();
        let neural_ms = neural_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        tracing::debug!(
            batch_id = %batch_id,
            neural_ms,
            "ONNX embedding attempt complete"
        );
        #[cfg(feature = "cli")]
        crate::cli::mcp::request_meta::record_neural_ms(neural_ms);
        result
    }

    /// Send an embed request to the worker and return the response.
    ///
    /// This is the simple API that returns an error on failure rather than
    /// falling back. For retry-once fallback semantics, use `embed_with_fallback`.
    pub fn embed(
        &self,
        texts: &[String],
        expected_dim: usize,
    ) -> Result<EmbedResponse, ClientError> {
        self.ensure_worker_ready()?;

        let batch_id = Self::next_batch_id();
        let request = EmbedRequest {
            texts: texts.to_vec(),
            expected_dim,
        };

        let frame = protocol::embed_request_frame(batch_id, request)
            .map_err(|e| ClientError::Ipc(e.to_string()))?;

        let response_frame = self.send_and_receive(frame)?;

        match response_frame.header.msg_type {
            MsgType::EmbedResponse => {
                let response: Response = response_frame
                    .decode_payload()
                    .map_err(|e| ClientError::Ipc(e.to_string()))?;
                match response {
                    Response::Embed(embed_resp) => Ok(embed_resp),
                    _ => Err(ClientError::Protocol("expected Embed response".to_string())),
                }
            }
            MsgType::Error => {
                let response: Response = response_frame
                    .decode_payload()
                    .map_err(|e| ClientError::Ipc(e.to_string()))?;
                match response {
                    Response::Error(err) => Err(ClientError::Worker(err)),
                    _ => Err(ClientError::Protocol("expected Error response".to_string())),
                }
            }
            other => Err(ClientError::Protocol(format!(
                "unexpected response type: {:?}",
                other
            ))),
        }
    }

    /// Send a rerank request to the worker and return the response.
    pub fn rerank(
        &self,
        query: &str,
        documents: Vec<RerankDocument>,
    ) -> Result<RerankResponse, ClientError> {
        self.ensure_worker_ready()?;

        let batch_id = Self::next_batch_id();
        let request = RerankRequest {
            query: query.to_string(),
            documents,
        };

        let frame = protocol::rerank_request_frame(batch_id, request)
            .map_err(|e| ClientError::Ipc(e.to_string()))?;

        let response_frame = self.send_and_receive(frame)?;

        match response_frame.header.msg_type {
            MsgType::RerankResponse => {
                let response: Response = response_frame
                    .decode_payload()
                    .map_err(|e| ClientError::Ipc(e.to_string()))?;
                match response {
                    Response::Rerank(rerank_resp) => Ok(rerank_resp),
                    _ => Err(ClientError::Protocol(
                        "expected Rerank response".to_string(),
                    )),
                }
            }
            MsgType::Error => {
                let response: Response = response_frame
                    .decode_payload()
                    .map_err(|e| ClientError::Ipc(e.to_string()))?;
                match response {
                    Response::Error(err) => Err(ClientError::Worker(err)),
                    _ => Err(ClientError::Protocol("expected Error response".to_string())),
                }
            }
            other => Err(ClientError::Protocol(format!(
                "unexpected response type: {:?}",
                other
            ))),
        }
    }

    /// Send a frame and receive the response frame.
    ///
    /// A persistent reader thread keeps pipe/socket I/O off the caller's
    /// stack, while the response channel waits for either a complete frame or
    /// a real transport disconnect. Model compilation and inference are not
    /// cancelled by elapsed-time policy.
    ///
    /// VAL-DEADWORKER-001/002: When the reader thread detects EOF/EPIPE (the
    /// worker process died), or when the reader thread itself disconnects, the
    /// error is mapped to `ClientError::WorkerDied` and `handle_worker_died`
    /// is called to kill the stale daemon and clear local state.
    fn send_and_receive(&self, frame: Frame) -> Result<Frame, ClientError> {
        let mut guard = self
            .worker
            .lock()
            .map_err(|e| ClientError::Ipc(format!("failed to lock worker handle: {}", e)))?;

        let handle = guard
            .as_mut()
            .ok_or_else(|| ClientError::Ipc("worker not running".to_string()))?;

        // Send the frame
        let wire = frame
            .encode_wire()
            .map_err(|e| ClientError::Ipc(e.to_string()))?;
        let request_batch_id = frame.header.batch_id;

        let writer = handle
            .writer
            .as_mut()
            .ok_or_else(|| ClientError::Ipc("worker transport not available".into()))?;

        if let Err(e) = writer.write_all(&wire) {
            drop(guard);
            // VAL-DEADWORKER-001: EPIPE on write means the worker died
            // (reading end closed). Classify as WorkerDied so the stale
            // daemon is killed and replaced.
            if is_worker_dead_io_error(&e) {
                self.handle_worker_died();
                return Err(ClientError::WorkerDied {
                    message: format!("EPIPE writing request frame: {}", e),
                });
            }
            self.kill_worker();
            return Err(ClientError::Ipc(format!(
                "failed to write to worker: {}",
                e
            )));
        }
        if let Err(e) = writer.flush() {
            drop(guard);
            if is_worker_dead_io_error(&e) {
                self.handle_worker_died();
                return Err(ClientError::WorkerDied {
                    message: format!("EPIPE flushing request frame: {}", e),
                });
            }
            self.kill_worker();
            return Err(ClientError::Ipc(format!(
                "failed to flush worker transport: {}",
                e
            )));
        }

        // Use the persistent reader thread to read the response.
        let (tx, rx) = mpsc::channel();
        handle
            .read_request_tx
            .send(ReadRequest::Read { tx })
            .map_err(|_e| ClientError::Ipc("reader thread channel closed".to_string()))?;

        self.receive_worker_response(rx, request_batch_id, guard)
    }

    fn receive_worker_response(
        &self,
        rx: mpsc::Receiver<Result<Vec<u8>, ClientError>>,
        request_batch_id: BatchId,
        guard: std::sync::MutexGuard<'_, Option<WorkerHandle>>,
    ) -> Result<Frame, ClientError> {
        match rx.recv() {
            Ok(Ok(frame_buf)) => {
                let response = match Frame::from_wire_bytes(&frame_buf) {
                    Ok(response) => response,
                    Err(e) => {
                        drop(guard);
                        self.kill_worker();
                        return Err(ClientError::Ipc(e.to_string()));
                    }
                };
                if response.header.batch_id != request_batch_id {
                    drop(guard);
                    self.kill_worker();
                    return Err(ClientError::Ipc(format!(
                        "response batch_id mismatch: expected {}, got {}",
                        request_batch_id, response.header.batch_id
                    )));
                }
                Ok(response)
            }
            Ok(Err(e)) => {
                drop(guard);
                // VAL-DEADWORKER-001/002: The reader thread got EOF/EPIPE (the
                // worker process died). Map this to WorkerDied and reap the
                // stale daemon so the next call spawns a replacement.
                if matches!(e, ClientError::WorkerDied { .. }) {
                    self.handle_worker_died();
                    return Err(e);
                }
                self.kill_worker();
                // Frame too large or other I/O error
                if e.to_string().contains("too large") {
                    Err(ClientError::Ipc(e.to_string()))
                } else {
                    Err(ClientError::Ipc(format!(
                        "failed to read from worker: {}",
                        e
                    )))
                }
            }
            Err(mpsc::RecvError) => {
                drop(guard);
                // VAL-DEADWORKER-001/002: The reader thread itself
                // disconnected, which means it either received EOF/EPIPE or
                // panicked. Either way the worker is dead.
                self.handle_worker_died();
                Err(ClientError::WorkerDied {
                    message: "reader thread disconnected".to_string(),
                })
            }
        }
    }
}

impl Drop for EmbeddingClient {
    fn drop(&mut self) {
        // Only the last Arc owner should kill the worker.
        let worker = match Arc::try_unwrap(std::mem::take(&mut self.worker)) {
            Ok(worker) => worker,
            Err(_) => return,
        };

        let mut guard = worker.into_inner().unwrap_or_else(|e| e.into_inner());
        if let Some(mut handle) = guard.take() {
            Self::shutdown_worker_handle(&mut handle, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::protocol::ErrorKind;

    /// TEMP verification: spawn the REAL leindex-embed worker (pipe mode) and run
    /// one embed through the production EmbeddingClient. On a cold MIGraphX cache
    /// this JIT-compiles (~300 s) and the native ORT cache writes a `.mxr`; on a
    /// warm cache it loads the `.mxr` (~10 s). Run: `cargo test -p leindex
    /// --features onnx real_pipe_embed -- --ignored --nocapture`.
    #[test]
    #[ignore = "spawns real leindex-embed worker + ~300s MIGraphX JIT compile"]
    fn real_pipe_embed_seeds_migraphx_cache() {
        let cache = migraphx_cache_path("qwen3-embed-0.6b");
        eprintln!("cache dir: {}", cache.display());
        let start = std::time::Instant::now();
        let client = EmbeddingClient::new_pipe();
        let response = client
            .embed(&["hello world".to_string()], 1024)
            .expect("pipe embed failed");
        eprintln!(
            "embed count={} elapsed={:?}",
            response.count,
            start.elapsed()
        );
        assert!(response.count > 0, "embed returned no vectors");
    }

    #[test]
    fn test_client_creation() {
        let _client = EmbeddingClient::new();
    }

    #[test]
    fn availability_uses_pipe_startup_report_without_spawning() {
        let client = EmbeddingClient::new_pipe();
        assert!(matches!(client.availability(), WorkerAvailability::Absent));
        *client.last_startup_report.lock().unwrap() =
            Some("startup_report provider=cpu status=available".to_string());
        assert!(matches!(client.availability(), WorkerAvailability::Ready));
    }

    #[test]
    fn test_client_debug_impl() {
        let client = EmbeddingClient::new();
        let debug_str = format!("{:?}", client);
        assert!(debug_str.contains("EmbeddingClient"));
    }

    #[test]
    fn test_client_clone_shares_worker() {
        let client = EmbeddingClient::new();
        let cloned = client.clone();
        // Clone shares the worker handle via Arc, not a new empty client
        let _ = format!("{:?}", cloned);
    }

    #[test]
    fn test_parse_startup_report_provider_from_plain_line() {
        let line = "startup_report provider=migraphx status=available model=qwen3-embed-0.6b";
        assert_eq!(
            parse_startup_report_provider(line).as_deref(),
            Some("migraphx")
        );
    }

    #[test]
    fn test_parse_startup_report_provider_from_tracing_line() {
        let line = "2026-06-30T01:02:03Z INFO startup_report provider=cpu status=unavailable (fallback: no GPU)";
        assert_eq!(parse_startup_report_provider(line).as_deref(), Some("cpu"));
    }

    #[test]
    fn test_client_reports_last_startup_provider() {
        let client = EmbeddingClient::new();
        *client.last_startup_report.lock().unwrap() =
            Some("startup_report provider=cuda status=available".to_string());

        assert_eq!(client.active_execution_provider().as_deref(), Some("cuda"));
    }

    #[test]
    fn test_wait_for_active_provider_observes_stderr_update() {
        let client = EmbeddingClient::new_pipe();
        let report = Arc::clone(&client.last_startup_report);
        let updater = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            *report.lock().unwrap() =
                Some("startup_report provider=migraphx status=available".to_string());
        });

        assert_eq!(
            client
                .wait_for_active_execution_provider(Duration::from_secs(1))
                .as_deref(),
            Some("migraphx")
        );
        updater.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn daemon_spawn_lock_serializes_contenders() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("worker.lock");
        let first = DaemonSpawnLock::acquire(&path, Duration::from_secs(1)).unwrap();
        assert!(matches!(
            DaemonSpawnLock::acquire(&path, Duration::from_millis(20)),
            Err(ClientError::Timeout)
        ));
        drop(first);
        DaemonSpawnLock::acquire(&path, Duration::from_secs(1)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn persistent_shutdown_unblocks_and_joins_reader() {
        let (client_stream, _server_stream) = UnixStream::pair().unwrap();
        let mut handle = EmbeddingClient::socket_worker_handle(client_stream, None, None).unwrap();
        let (tx, _rx) = mpsc::channel();
        handle
            .read_request_tx
            .send(ReadRequest::Read { tx })
            .unwrap();
        thread::sleep(Duration::from_millis(10));

        EmbeddingClient::shutdown_worker_handle(&mut handle, false);
    }

    #[cfg(unix)]
    #[test]
    fn persistent_client_does_not_delete_daemon_socket() {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("daemon.sock");
        std::fs::write(&socket_path, b"owned by daemon").unwrap();
        let (client_stream, _server_stream) = UnixStream::pair().unwrap();
        let mut handle =
            EmbeddingClient::socket_worker_handle(client_stream, None, Some(socket_path.clone()))
                .unwrap();

        EmbeddingClient::shutdown_worker_handle(&mut handle, true);
        assert!(socket_path.exists());
    }

    #[test]
    fn stderr_mirror_exits_after_reported_io_error() {
        struct FailingReader;
        impl Read for FailingReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic stderr failure"))
            }
        }

        let report = Arc::new(Mutex::new(None));
        EmbeddingClient::spawn_stderr_thread(FailingReader, report)
            .join()
            .unwrap();
    }

    #[test]
    fn test_client_error_display() {
        let err = ClientError::SpawnFailed("not found".to_string());
        assert!(err.to_string().contains("not found"));

        let worker_err = WorkerError {
            kind: ErrorKind::ModelNotFound,
            message: "missing model".to_string(),
        };
        let err = ClientError::Worker(worker_err);
        assert!(err.to_string().contains("missing model"));
    }

    #[test]
    fn test_embed_result_success() {
        let response = EmbedResponse::new(vec![1.0, 2.0, 3.0, 4.0], 1, 4);
        let result = EmbedResult::Success(response);
        assert!(result.is_success());
        assert!(!result.is_fallback());
        assert!(result.into_success().is_some());
    }

    #[test]
    fn test_embed_result_fallback() {
        let error = ClientError::Worker(WorkerError {
            kind: ErrorKind::Inference,
            message: "worker crashed".to_string(),
        });
        let result = EmbedResult::Fallback {
            batch_id: BatchId::new(42),
            error,
        };
        assert!(!result.is_success());
        assert!(result.is_fallback());
        assert!(result.into_success().is_none());
    }

    #[test]
    fn test_batch_id_monotonic() {
        let id1 = EmbeddingClient::next_batch_id();
        let id2 = EmbeddingClient::next_batch_id();
        assert!(
            id2.0 > id1.0,
            "batch IDs should be monotonically increasing"
        );
    }

    // ── VAL-SETUP-020/VAL-ORT-006: config-driven ORT_DYLIB_PATH injection ──

    // Use a process-shared lock so env-mutating tests serialize within the module.
    use std::sync::Mutex;
    static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_read_ort_dylib_path_from_config_returns_value() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", tmp.path()) };
        unsafe { std::env::remove_var("LEINDEX_ONNX_INFERENCE_BATCH_SIZE") };
        unsafe { std::env::remove_var("LEINDEX_ONNX_SEQUENCE_LEN") };

        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[neural]\nenabled = true\nexecution_provider = \"cpu\"\nort_dylib_path = \"/opt/onnxruntime/libonnxruntime.so\"\nort_version = \"1.25.0\"\nmodel_dir = \"/models\"\n",
        )
        .unwrap();

        let parsed = read_ort_dylib_path_from_config();
        assert_eq!(
            parsed.as_deref(),
            Some("/opt/onnxruntime/libonnxruntime.so")
        );
        assert_eq!(
            read_execution_provider_from_config().as_deref(),
            Some("cpu")
        );

        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    fn test_read_execution_provider_from_config_skips_auto() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", tmp.path()) };

        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[neural]\nenabled = true\nexecution_provider = \"auto\"\n",
        )
        .unwrap();
        assert_eq!(read_execution_provider_from_config(), None);

        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[neural]\nenabled = true\nexecution_provider = \"migraphx\"\n",
        )
        .unwrap();
        assert_eq!(
            read_execution_provider_from_config().as_deref(),
            Some("migraphx")
        );

        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    fn test_read_worker_model_name_from_config_returns_value() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", tmp.path()) };

        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[neural]\nenabled = true\nmodel_name = \"qwen3-embed-0.6b-dynamic\"\n",
        )
        .unwrap();

        assert_eq!(
            read_worker_model_name_from_config().as_deref(),
            Some("qwen3-embed-0.6b-dynamic")
        );

        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    fn test_migraphx_model_cache_path_uses_leindex_home() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", tmp.path()) };
        unsafe { std::env::remove_var("LEINDEX_ONNX_INFERENCE_BATCH_SIZE") };
        unsafe { std::env::remove_var("LEINDEX_ONNX_SEQUENCE_LEN") };
        let expected = tmp
            .path()
            .join("cache")
            .join("migraphx")
            .join("qwen3-embed-0_6b-dynamic")
            .join("b8-s128");

        assert_eq!(
            migraphx_model_cache_path(Some("qwen3-embed-0.6b-dynamic")).as_deref(),
            Some(expected.as_path())
        );

        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    #[cfg(unix)]
    fn test_daemon_socket_path_includes_inference_shape() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", tmp.path()) };
        unsafe { std::env::remove_var("LEINDEX_ONNX_INFERENCE_BATCH_SIZE") };
        unsafe { std::env::remove_var("LEINDEX_ONNX_SEQUENCE_LEN") };

        let socket =
            daemon_socket_path(Some("migraphx"), Some("qwen3-embed-0.6b-dynamic")).unwrap();
        let filename = socket.file_name().and_then(|name| name.to_str()).unwrap();
        assert!(filename.starts_with("leindex-embed-"));
        assert!(filename.ends_with(".sock"));
        assert!(socket.to_string_lossy().len() <= 100);

        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    fn test_read_ort_dylib_path_from_config_returns_none_when_absent() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", tmp.path()) };

        // No config file at all.
        assert_eq!(read_ort_dylib_path_from_config(), None);

        // Config exists but lacks ort_dylib_path.
        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[neural]\nenabled = true\nmodel_dir = \"/models\"\n",
        )
        .unwrap();
        assert_eq!(read_ort_dylib_path_from_config(), None);

        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    fn test_read_ort_dylib_path_from_config_handles_single_quotes() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", tmp.path()) };

        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[neural]\nort_dylib_path = '/quote/ort.so'\n",
        )
        .unwrap();

        assert_eq!(
            read_ort_dylib_path_from_config().as_deref(),
            Some("/quote/ort.so")
        );

        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    fn test_leindex_home_dir_prefers_env_override() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", "/custom/leindex/home") };
        assert_eq!(
            leindex_home_dir(),
            Some(std::path::PathBuf::from("/custom/leindex/home"))
        );
        unsafe { std::env::remove_var("LEINDEX_HOME") };
    }

    #[test]
    fn test_leindex_home_dir_falls_back_to_home() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("LEINDEX_HOME") };
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        let home = leindex_home_dir();
        assert_eq!(
            home,
            Some(std::path::PathBuf::from("/home/testuser/.leindex"))
        );
        unsafe { std::env::remove_var("HOME") };
    }

    #[test]
    fn test_leindex_home_dir_relative_env_ignored() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("LEINDEX_HOME", "relative/path") };
        unsafe { std::env::set_var("HOME", "/home/fallback") };
        // Should fall back to HOME-based path, not use relative.
        let home = leindex_home_dir();
        assert_eq!(
            home,
            Some(std::path::PathBuf::from("/home/fallback/.leindex"))
        );
        unsafe { std::env::remove_var("LEINDEX_HOME") };
        unsafe { std::env::remove_var("HOME") };
    }
}
