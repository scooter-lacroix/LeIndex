use super::*;

pub(super) const EMBED_DAEMON_ENV: &str = "LEINDEX_EMBED_DAEMON";

pub(super) fn embed_daemon_enabled() -> bool {
    std::env::var(EMBED_DAEMON_ENV)
        .ok()
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(cfg!(unix))
}

/// Maximum response frame size in bytes.
///
/// Mirrors the worker-side incoming-frame guard (`max_frame_size * 2` = 32 MiB
/// with the default 16 MiB max_frame_size). A response larger than this is
/// rejected with a clear protocol error.
pub(super) const MAX_RESPONSE_FRAME_SIZE: u32 = 32 * 1024 * 1024; // 32 MiB

/// Read buffer capacity for BufReader wrapping the inference data path.
///
/// VAL-DAEMON-006: A 128KB buffer reduces the number of `read()` syscalls
/// for large embedding responses (e.g., 1024-dim x 32 batch x 4 bytes =
/// 128KB fits in a single read instead of many small reads).
pub(super) const READ_BUF_CAPACITY: usize = 128 * 1024;

/// Control-plane lock wait. The lock is held only while publishing a daemon
/// process/socket, never while loading ORT or compiling a model.
pub(super) const DAEMON_LOCK_WAIT_SECS: u64 = 5;

/// Socket bind is a transport startup guard, not an inference deadline. The
/// worker binds before model initialization, so a healthy process normally
/// reaches this point in milliseconds.
pub(super) const DAEMON_BIND_WAIT_SECS: u64 = 2;

/// Health probes are bounded only to avoid a dead control-plane socket
/// stalling a readiness observation. Model loading and inference have no
/// elapsed-time cancel.
#[cfg(unix)]
pub(super) const DAEMON_HEALTH_WAIT: Duration = Duration::from_millis(250);

/// Poll interval while a resident worker finishes model initialization.
pub(super) const DAEMON_READINESS_POLL: Duration = Duration::from_millis(25);

/// Maximum wall-clock duration to wait for the daemon to transition from
/// Initializing to Ready before treating the neural worker as unavailable.
///
/// This is a **readiness deadline**, not a cancellation of inference or model
/// loading. When the deadline fires, indexing proceeds with core TF-IDF/PDG
/// results. The daemon does NOT run the MIGraphX cold JIT compile before
/// reporting Ready (that compile is owned by ORT's native `.mxr` cache and
/// paid once at `leindex setup` warmup); Ready only waits on ORT dylib load +
/// model load + EP registration (~10 s observed). 120 s is therefore a generous
/// ceiling; a genuinely broken worker still fails fast (init reports `Failed`
/// immediately rather than stalling to this deadline).
pub(super) const DAEMON_READY_MAX_WAIT: Duration = Duration::from_secs(120);

/// Grace window for a stale daemon to exit after `SIGTERM` before receiving
/// `SIGKILL`.
///
/// VAL-DEADWORKER-003: This is a bounded process-cleanup guard for a daemon
/// that has already been confirmed dead by the read-error path. It is not a
/// request-path timeout and does not cancel inference.
pub(super) const STALE_DAEMON_KILL_GRACE: Duration = Duration::from_secs(1);

pub(super) fn platform_binary_name(binary_name: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", binary_name)
    } else {
        binary_name.to_string()
    }
}

/// Resolve the path to the worker binary.
///
/// First tries the running binary's directory and its Cargo `deps` parent, so
/// tests/invocations from `target/{debug,release}/deps` use the worker built
/// from this checkout instead of an older globally installed binary. Falls
/// back to PATH lookup for packaged installations.
pub(super) fn resolve_worker_binary() -> Result<PathBuf, std::io::Error> {
    let binary_name = platform_binary_name("leindex-embed");
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for candidate_dir in [Some(exe_dir), exe_dir.parent()].into_iter().flatten() {
                let sibling = candidate_dir.join(&binary_name);
                if sibling.is_file() {
                    return Ok(sibling);
                }
            }
        }
    }
    // Fall back to PATH lookup
    which::which(&binary_name).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("worker binary '{}' not found in PATH: {}", binary_name, e),
        )
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WorkerConfigEnv {
    pub(super) ort_dylib_path: Option<String>,
    pub(super) execution_provider: Option<String>,
    pub(super) model_name: Option<String>,
}

/// Read worker-relevant values from the user-level
/// `~/.leindex/config/leindex.toml` (honoring `$LEINDEX_HOME`).
///
/// VAL-SETUP-020/VAL-ORT-006: when the worker is spawned from the daemon we
/// surface the dylib path chosen during `leindex setup` so the worker's ORT
/// discovery chain picks the same build. The lookup is intentionally minimal
/// (text scan, mirroring `leindex-embed::ort_discovery::read_config_ort_path`)
/// because the file is tiny and pulling a full TOML parser into the search
/// crate is not worth it.
///
pub(super) fn read_worker_config_env_from_config() -> WorkerConfigEnv {
    let Some(home) = leindex_home_dir() else {
        return WorkerConfigEnv::default();
    };
    let cfg = home.join("config").join("leindex.toml");
    let Ok(contents) = std::fs::read_to_string(&cfg) else {
        return WorkerConfigEnv::default();
    };
    let mut parsed = WorkerConfigEnv::default();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = parse_config_assignment(line, "ort_dylib_path") {
            parsed.ort_dylib_path = Some(value);
        } else if let Some(value) = parse_config_assignment(line, "execution_provider") {
            if !value.eq_ignore_ascii_case("auto") {
                parsed.execution_provider = Some(value);
            }
        } else if let Some(value) = parse_config_assignment(line, "model_name") {
            parsed.model_name = Some(value);
        }
    }
    parsed
}

#[cfg(test)]
pub(super) fn read_ort_dylib_path_from_config() -> Option<String> {
    read_worker_config_env_from_config().ort_dylib_path
}

#[cfg(test)]
pub(super) fn read_execution_provider_from_config() -> Option<String> {
    read_worker_config_env_from_config().execution_provider
}

#[cfg(test)]
pub(super) fn read_worker_model_name_from_config() -> Option<String> {
    read_worker_config_env_from_config().model_name
}

pub(super) fn migraphx_model_cache_path(model_name: Option<&str>) -> Option<std::path::PathBuf> {
    let model = sanitize_cache_component(model_name.unwrap_or("qwen3-embed-0.6b-dynamic"));
    let batch = leindex_embed::runtime::configured_onnx_inference_batch_size(
        model_name.unwrap_or("qwen3-embed-0.6b-dynamic"),
        "migraphx",
    );
    let sequence = leindex_embed::runtime::configured_onnx_sequence_len();
    // Key on batch + sequence only. A compiled MIGraphX program depends on the
    // model graph + input shape, never on LeIndex's software version (the model
    // name is already a parent dir segment). Including the package version here
    // forced a fresh ~600s JIT recompile on every minor release bump and grew a
    // new ~1.2GB cache profile each time. The worker prunes stale `.mxr` files
    // within this dir on startup; `leindex setup` prunes stale sibling profiles.
    let profile = format!("b{}-s{}", batch, sequence);
    leindex_home_dir().map(|home| {
        home.join("cache")
            .join("migraphx")
            .join(model)
            .join(profile)
    })
}

/// Public accessor for the MIGraphX model cache path.
///
/// VAL-DAEMON-007: Used by `leindex setup --warmup` to check whether the
/// MIGraphX cache is cold (absent) and should trigger auto-warmup.
pub fn migraphx_cache_path(model_name: &str) -> std::path::PathBuf {
    migraphx_model_cache_path(Some(model_name))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/leindex-migraphx-cache-unresolved"))
}

/// Remove stale MIGraphX cache profile directories for `model_name`, keeping
/// only the profile matching the current batch + sequence length.
///
/// Each profile dir holds a ~1.2 GB compiled program. Old profiles — left
/// behind by a prior package version, batch size, or sequence length —
/// accumulate without bound. The worker prunes `.mxr` files within the active
/// profile on startup; this prunes the sibling profile dirs themselves, so the
/// cache tree never grows past one live profile. Returns the number removed.
pub fn prune_stale_migraphx_profiles(model_name: &str) -> usize {
    // Resolve the real cache path directly. Never fall back to the
    // `/tmp/leindex-migraphx-cache-unresolved` sentinel used by
    // migraphx_cache_path — pruning against that sentinel's parent (/tmp)
    // would delete arbitrary sibling directories.
    let current = match migraphx_model_cache_path(Some(model_name)) {
        Some(path) => path,
        None => return 0,
    };
    let Some(parent) = current.parent() else {
        return 0;
    };
    let mut removed = 0;
    let Ok(entries) = std::fs::read_dir(parent) else {
        return 0;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path == current || !path.is_dir() {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                tracing::debug!("pruned stale MIGraphX cache profile: {}", path.display());
                removed += 1;
            }
            Err(error) => tracing::warn!(
                "failed to prune stale MIGraphX cache profile {}: {}",
                path.display(),
                error
            ),
        }
    }
    removed
}

pub(super) fn sanitize_cache_component(value: &str) -> String {
    let value: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if value.is_empty() {
        "unknown".to_string()
    } else {
        value
    }
}

#[cfg(unix)]
pub(super) fn daemon_socket_path(
    provider: Option<&str>,
    model_name: Option<&str>,
) -> Option<PathBuf> {
    let home = leindex_home_dir()?;
    let provider_name = provider.unwrap_or("auto");
    let model_name = model_name.unwrap_or("qwen3-embed-0.6b");
    let batch =
        leindex_embed::runtime::configured_onnx_inference_batch_size(model_name, provider_name);
    let sequence = leindex_embed::runtime::configured_onnx_sequence_len();
    let descriptor = format!(
        "{}:{provider_name}:{model_name}:b{batch}:s{sequence}",
        env!("CARGO_PKG_VERSION")
    );
    let digest = blake3::hash(descriptor.as_bytes()).to_hex();
    let socket_path = home
        .join("run")
        .join(format!("leindex-embed-{}.sock", &digest[..16]));

    use std::os::unix::ffi::OsStrExt;
    (socket_path.as_os_str().as_bytes().len() <= 100).then_some(socket_path)
}

#[cfg(unix)]
pub(super) fn daemon_status_path(
    provider: Option<&str>,
    model_name: Option<&str>,
) -> Option<PathBuf> {
    daemon_socket_path(provider, model_name).map(|path| path.with_extension("status"))
}

#[cfg(unix)]
pub(super) fn daemon_pid_path(provider: Option<&str>, model_name: Option<&str>) -> Option<PathBuf> {
    daemon_socket_path(provider, model_name).map(|path| path.with_extension("pid"))
}

#[cfg(unix)]
pub(super) fn cleanup_daemon_paths(socket_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
    let _ = std::fs::remove_file(socket_path.with_extension("status"));
    let _ = std::fs::remove_file(socket_path.with_extension("pid"));
    let _ = std::fs::remove_file(socket_path.with_extension("start"));
}

#[cfg(target_os = "linux")]
fn daemon_pid_is_owned(pid: libc::pid_t, socket_path: &Path) -> bool {
    let expected_start = std::fs::read_to_string(socket_path.with_extension("start"))
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let Some(expected_start) = expected_start else {
        return false;
    };
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
    let actual_start = stat
        .as_deref()
        .and_then(|stat| stat.rsplit_once(") "))
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|value| value.parse::<u64>().ok());
    if actual_start != Some(expected_start) {
        return false;
    }
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
    let Some(cmdline) = cmdline else {
        return false;
    };
    let command = String::from_utf8_lossy(&cmdline);
    if !command.split('\0').any(|arg| arg.contains("leindex-embed")) {
        return false;
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok();
    let Some(uid_line) = status
        .as_deref()
        .and_then(|status| status.lines().find(|line| line.starts_with("Uid:")))
    else {
        return false;
    };
    let Some(uid) = uid_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return false;
    };
    uid == unsafe { libc::geteuid() }
}

#[cfg(not(target_os = "linux"))]
/// Fail closed on Unix platforms without a process-identity API wired here;
/// stale cleanup must never signal an unverified PID.
fn daemon_pid_is_owned(_pid: libc::pid_t, _socket_path: &Path) -> bool {
    false
}

/// Kill a stale daemon process by reading its PID from the `.pid` sidecar
/// file next to the socket.
///
/// VAL-DEADWORKER-003: When dead-worker detection fires, the stale daemon PID
/// (recorded during spawn) is read from the status/PID sidecar and sent
/// `SIGTERM`. If the process does not exit within a short grace window, it
/// receives `SIGKILL`. This is a bounded cleanup of a known-dead process,
/// not a request-path timeout.
#[cfg(unix)]
pub(super) fn kill_stale_daemon_by_pid(socket_path: &Path) {
    let pid_path = socket_path.with_extension("pid");
    let Some(pid) = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
    else {
        return;
    };
    if pid <= 0 || !daemon_pid_is_owned(pid, socket_path) {
        return;
    }

    // Validate immediately before signaling. The sidecar/start-time checks
    // narrow the PID-reuse window; fail closed if ownership changed.
    if !daemon_pid_is_owned(pid, socket_path) {
        return;
    }
    let _ = unsafe { libc::kill(pid, libc::SIGTERM) };

    // Bounded grace window for graceful exit, then SIGKILL.
    let deadline = Instant::now() + STALE_DAEMON_KILL_GRACE;
    loop {
        // Re-check ownership before escalation so PID reuse cannot turn the
        // cleanup path into a signal against an unrelated process.
        if !daemon_pid_is_owned(pid, socket_path) {
            break;
        }
        // Check liveness: kill(pid, 0) returns 0 if the process exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if !alive {
            break;
        }
        if Instant::now() >= deadline {
            if daemon_pid_is_owned(pid, socket_path) {
                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn worker_health_snapshot(
    state: WorkerState,
    provider: Option<String>,
    model: Option<String>,
    error: Option<String>,
) -> HealthResponse {
    let phase = match state {
        WorkerState::Initializing => "initializing",
        WorkerState::Ready => "ready",
        WorkerState::Failed => "failed",
    };
    HealthResponse {
        state,
        phase: phase.to_string(),
        started_unix_ms: 0,
        provider,
        model: model.unwrap_or_else(|| "qwen3-embed-0.6b".to_string()),
        error,
    }
}

#[cfg(unix)]
pub(super) fn status_state(path: &Path) -> Option<WorkerState> {
    let status = std::fs::read_to_string(path).ok()?;
    match status.trim() {
        "initializing" => Some(WorkerState::Initializing),
        "ready" => Some(WorkerState::Ready),
        "failed" => Some(WorkerState::Failed),
        _ => None,
    }
}

#[cfg(unix)]
pub(super) fn daemon_pid_alive(path: &Path) -> bool {
    let Some(pid) = std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
    else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
pub(super) fn probe_daemon_health(socket_path: &Path) -> Result<HealthResponse, ClientError> {
    probe_daemon_health_with_timeout(socket_path, Some(DAEMON_HEALTH_WAIT))
}

#[cfg(unix)]
pub(super) fn probe_daemon_health_with_timeout(
    socket_path: &Path,
    read_timeout: Option<Duration>,
) -> Result<HealthResponse, ClientError> {
    let mut stream = UnixStream::connect(socket_path).map_err(|error| {
        ClientError::Ipc(format!(
            "failed to connect worker health socket {}: {}",
            socket_path.display(),
            error
        ))
    })?;
    if let Some(timeout) = read_timeout {
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|_| stream.set_write_timeout(Some(timeout)))
            .map_err(|error| {
                ClientError::Ipc(format!("failed to configure health socket: {}", error))
            })?;
    }

    let batch_id = BatchId::new(BATCH_COUNTER.fetch_add(1, Ordering::Relaxed));
    let wire = protocol::health_request_frame(batch_id)
        .map_err(|error| ClientError::Ipc(error.to_string()))?
        .encode_wire()
        .map_err(|error| ClientError::Ipc(error.to_string()))?;
    stream
        .write_all(&wire)
        .and_then(|_| stream.flush())
        .map_err(|error| {
            ClientError::Ipc(format!("failed to send worker health request: {}", error))
        })?;

    let payload = read_frame(&mut stream)?;
    let frame =
        Frame::from_wire_bytes(&payload).map_err(|error| ClientError::Ipc(error.to_string()))?;
    if frame.header.batch_id != batch_id {
        return Err(ClientError::Protocol(format!(
            "health response batch_id mismatch: expected {}, got {}",
            batch_id, frame.header.batch_id
        )));
    }
    match frame.header.msg_type {
        MsgType::HealthResponse => match frame
            .decode_payload::<Response>()
            .map_err(|error| ClientError::Ipc(error.to_string()))?
        {
            Response::Health(health) => Ok(health),
            _ => Err(ClientError::Protocol(
                "expected Health response payload".to_string(),
            )),
        },
        MsgType::Error => match frame
            .decode_payload::<Response>()
            .map_err(|error| ClientError::Ipc(error.to_string()))?
        {
            Response::Error(error) => Err(ClientError::Worker(error)),
            _ => Err(ClientError::Protocol(
                "expected Error response payload".to_string(),
            )),
        },
        other => Err(ClientError::Protocol(format!(
            "unexpected health response type: {:?}",
            other
        ))),
    }
}

pub(super) fn parse_config_assignment(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let value_part = rest.strip_prefix('=')?.trim();
    let trimmed = value_part.trim_matches(|c| c == '"' || c == '\'').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn parse_startup_report_provider(line: &str) -> Option<String> {
    let (_, report) = line.split_once("startup_report")?;
    report
        .split_whitespace()
        .find_map(|part| part.strip_prefix("provider="))
        .filter(|provider| !provider.is_empty())
        .map(|provider| provider.trim_matches(|c| c == ',' || c == ';').to_string())
}

/// Resolve the LeIndex home directory (`~/.leindex` or `$LEINDEX_HOME`).
///
/// Uses environment variables directly (rather than the `dirs` crate) so we
/// don't couple the `onnx` feature to the `cli` feature's optional `dirs`
/// dependency. `$LEINDEX_HOME` wins over `$HOME/.leindex` to stay consistent
/// with the rest of the codebase (see `cli/neural_config.rs` and
/// `crates/leindex-embed`).
pub(super) fn leindex_home_dir() -> Option<std::path::PathBuf> {
    if let Ok(custom) = std::env::var("LEINDEX_HOME") {
        let p = std::path::PathBuf::from(&custom);
        if p.is_absolute() {
            return Some(p);
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".leindex"))
}

/// Errors that can occur when communicating with the embedding worker.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Failed to spawn the worker process.
    #[error("failed to spawn worker: {0}")]
    SpawnFailed(String),

    /// IPC communication error.
    #[error("IPC error: {0}")]
    Ipc(String),

    /// Worker reported an error.
    #[error("worker error: {0}")]
    Worker(WorkerError),

    /// Protocol-level error (unexpected message type, etc.).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// A control-plane operation exceeded its bounded transport guard.
    ///
    /// This is never used to cancel model loading or inference. The request
    /// path waits for a connected peer or receives an explicit worker error.
    #[error("worker control-plane operation timed out")]
    Timeout,

    /// The worker process died during an inference request.
    ///
    /// VAL-DEADWORKER-001/002: The reader thread detected EOF or EPIPE on the
    /// Unix stream, confirming the worker process is no longer alive. This is
    /// detected through the natural read-error path, not an arbitrary timeout.
    /// On this error, the stale daemon is killed and local state cleared so
    /// the next request triggers a fresh `spawn_or_connect_daemon()`.
    #[error("worker process died: {message}")]
    WorkerDied {
        /// Diagnostic context (e.g. "EOF reading frame", "reader thread disconnected").
        message: String,
    },
}

/// Non-blocking observation of the local neural worker.
#[derive(Debug, Clone)]
pub enum WorkerAvailability {
    /// The worker has completed model initialization and accepts inference.
    Ready,
    /// The daemon is reachable but still loading runtime/model state.
    Initializing(HealthResponse),
    /// Initialization completed unsuccessfully; the health payload explains why.
    Failed(HealthResponse),
    /// No worker process/socket is currently available.
    Absent,
}

impl WorkerAvailability {
    /// Return whether neural inference can be attempted now.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub(super) fn is_unavailable(&self) -> bool {
        matches!(self, Self::Initializing(_) | Self::Failed(_) | Self::Absent)
    }
}

/// Result of an embed request with fallback semantics.
///
/// VAL-CPHASE-016: On success, contains the flat row-major EmbedResponse
/// from the worker, which can be written directly into destination storage
/// without creating a nested Vec<Vec<f32>> heap mirror.
///
/// VAL-CPHASE-018: On fallback, contains a TF-IDF-degraded embedding for
/// the affected batch only.
#[derive(Debug)]
pub enum EmbedResult {
    /// Worker returned a successful embedding response.
    Success(EmbedResponse),
    /// Worker failed after retry; fell back to TF-IDF for this batch.
    /// The caller should use the TF-IDF embedding as a degraded substitute.
    Fallback {
        /// The batch ID that triggered the fallback.
        batch_id: BatchId,
        /// The error that caused the fallback (from the retry attempt).
        error: ClientError,
    },
}

impl EmbedResult {
    /// Returns true if this result represents a successful worker response.
    pub fn is_success(&self) -> bool {
        matches!(self, EmbedResult::Success(_))
    }

    /// Returns true if this result represents a TF-IDF fallback.
    pub fn is_fallback(&self) -> bool {
        matches!(self, EmbedResult::Fallback { .. })
    }

    /// Extract the successful response, if any.
    pub fn into_success(self) -> Option<EmbedResponse> {
        match self {
            EmbedResult::Success(resp) => Some(resp),
            EmbedResult::Fallback { .. } => None,
        }
    }
}

/// Client for the leindex-embed worker process.
///
/// Manages the worker lifecycle and provides methods for sending embed
/// and rerank requests over local IPC with retry-once fallback semantics.
///
/// VAL-CPHASE-020: Worker failure does not crash the main daemon — errors
/// are returned as `EmbedResult::Fallback` rather than panicking.
///
/// VAL-CPHASE-021: After a fallback episode, the worker handle is cleared
/// so the next request spawns a fresh worker.
pub struct EmbeddingClient {
    /// Worker process handle, if currently running.
    /// Shared via Arc so that Clone shares the same worker handle.
    pub(super) worker: Arc<Mutex<Option<WorkerHandle>>>,
    /// Last startup_report line observed on worker stderr.
    pub(super) last_startup_report: Arc<Mutex<Option<String>>>,
    /// Whether Unix socket daemon reuse is allowed.
    pub(super) use_daemon: bool,
    /// Cached worker config env, read from leindex.toml at most once per client.
    /// VAL-DAEMON-003: Avoids re-reading leindex.toml on every availability() poll.
    pub(super) cached_config: OnceLock<WorkerConfigEnv>,
}

/// Manual Debug impl — Child doesn't implement Debug.
impl fmt::Debug for EmbeddingClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbeddingClient")
            .field("worker", &self.worker.lock().map(|g| g.is_some()))
            .field(
                "active_execution_provider",
                &self.active_execution_provider(),
            )
            .finish()
    }
}

/// Manual Clone impl — shares the worker handle via Arc.
impl Clone for EmbeddingClient {
    fn clone(&self) -> Self {
        Self {
            worker: Arc::clone(&self.worker),
            last_startup_report: Arc::clone(&self.last_startup_report),
            use_daemon: self.use_daemon,
            cached_config: OnceLock::new(),
        }
    }
}

/// Handle to a running worker process with its stdin/stdout pipes.
pub(super) struct WorkerHandle {
    /// The child process.
    pub(super) child: Option<Child>,
    /// Local transport for sending frames to the worker.
    pub(super) writer: Option<WorkerWriter>,
    /// Persistent reader thread that reads IPC responses from the worker's stdout.
    /// Uses a oneshot channel to receive the response data with timeout enforcement.
    pub(super) read_thread: thread::JoinHandle<()>,
    /// Channel sender to signal the read thread to perform a read and return the result.
    pub(super) read_request_tx: std::sync::mpsc::Sender<ReadRequest>,
    /// Thread that mirrors worker stderr and captures startup reports.
    pub(super) stderr_thread: Option<thread::JoinHandle<()>>,
    /// True when this handle is connected to a daemon intended to outlive this client.
    pub(super) persistent: bool,
    /// Unix socket path for resident daemons, used to remove stale sockets on failure.
    pub(super) socket_path: Option<PathBuf>,
}

pub(super) enum WorkerWriter {
    Pipe(std::process::ChildStdin),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl WorkerWriter {
    pub(super) fn shutdown(&self) {
        #[cfg(unix)]
        if let WorkerWriter::Unix(stream) = self {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Write for WorkerWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            WorkerWriter::Pipe(stdin) => stdin.write(buf),
            #[cfg(unix)]
            WorkerWriter::Unix(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            WorkerWriter::Pipe(stdin) => stdin.flush(),
            #[cfg(unix)]
            WorkerWriter::Unix(stream) => stream.flush(),
        }
    }
}

/// Request sent to the persistent reader thread.
pub(super) enum ReadRequest {
    /// Request a read. Response sent via the channel.
    Read {
        tx: mpsc::Sender<Result<Vec<u8>, ClientError>>,
    },
    /// Signal the read thread to shut down.
    Shutdown,
}

#[cfg(unix)]
pub(super) enum DaemonHealthState {
    Ready,
    Initializing,
    Reconnect,
}

#[cfg(unix)]
pub(super) struct DaemonSpawnLock {
    file: std::fs::File,
}

#[cfg(unix)]
impl DaemonSpawnLock {
    pub(super) fn acquire(path: &Path, timeout: Duration) -> Result<Self, ClientError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| {
                ClientError::SpawnFailed(format!(
                    "failed to open worker daemon lock {}: {}",
                    path.display(),
                    e
                ))
            })?;
        let deadline = Instant::now() + timeout;
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                return Ok(Self { file });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(ClientError::SpawnFailed(format!(
                    "failed to lock worker daemon startup {}: {}",
                    path.display(),
                    error
                )));
            }
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(unix)]
impl Drop for DaemonSpawnLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl Default for EmbeddingClient {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingClient {
    /// Create a new embedding client.
    ///
    /// The worker is not spawned until the first request is made (cold start).
    pub fn new() -> Self {
        Self {
            worker: Arc::new(Mutex::new(None)),
            last_startup_report: Arc::new(Mutex::new(None)),
            use_daemon: embed_daemon_enabled(),
            cached_config: OnceLock::new(),
        }
    }

    /// Create a direct child-pipe client.
    pub fn new_pipe() -> Self {
        Self {
            worker: Arc::new(Mutex::new(None)),
            last_startup_report: Arc::new(Mutex::new(None)),
            use_daemon: false,
            cached_config: OnceLock::new(),
        }
    }

    /// Return the execution provider reported by the worker startup report.
    pub fn active_execution_provider(&self) -> Option<String> {
        self.last_startup_report
            .lock()
            .ok()
            .and_then(|line| line.as_deref().and_then(parse_startup_report_provider))
    }

    /// The model name the worker is configured to load (env or leindex.toml).
    pub fn configured_model_name(&self) -> Option<String> {
        std::env::var("LEINDEX_WORKER_MODEL")
            .ok()
            .or_else(|| self.cached_config().model_name.clone())
    }

    /// Return a human-readable reason when a GPU execution provider was
    /// *requested* but the worker *fell back to CPU*.
    ///
    /// This is the signal the indexing pipeline uses to bail neural enrichment
    /// to TF-IDF instead of silently running 100-1000x slower CPU inference the
    /// user did not ask for. Returns `None` (proceed with neural) when:
    ///   - the configured provider is `cpu`, `auto`, or unset — the user either
    ///     asked for CPU or accepted whatever is available, so the path stays
    ///     fully operational; or
    ///   - the requested GPU provider is actually active; or
    ///   - the worker could not yet report a provider (e.g. still compiling on a
    ///     cold GPU start) — do not punish a legitimate cold compile.
    pub fn cpu_fallback_reason(&self) -> Option<String> {
        let requested = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER")
            .ok()
            .or_else(|| self.cached_config().execution_provider.clone());
        let requested_gpu = matches!(
            requested.as_deref(),
            Some("migraphx") | Some("cuda") | Some("rocm")
        );
        if !requested_gpu {
            return None;
        }
        // Bring the worker up so its actual provider is observable. This spawns
        // the daemon the enrichment pass would spawn anyway, so it is not net
        // extra work; on a fast CPU fallback the worker reports Ready quickly.
        let _ = self.ensure_worker_ready();
        match self.active_execution_provider().as_deref() {
            Some("cpu") => Some(format!(
                "neural worker fell back to CPU although `{}` was requested; \
                 skipping neural enrichment (TF-IDF only). Point ORT_DYLIB_PATH at a \
                 migraphx-enabled libonnxruntime, or set execution_provider = \"cpu\" \
                 in ~/.leindex/config/leindex.toml to use CPU embeddings deliberately.",
                requested.unwrap_or_default()
            )),
            _ => None,
        }
    }

    /// Return cached worker config env, reading from leindex.toml at most once.
    ///
    /// VAL-DAEMON-003: The config file is read on the first call and stored in
    /// a `OnceLock`, so subsequent `availability()` polling does not re-read
    /// the file from disk.
    pub(super) fn cached_config(&self) -> &WorkerConfigEnv {
        self.cached_config
            .get_or_init(read_worker_config_env_from_config)
    }
}
