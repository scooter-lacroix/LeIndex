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
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use leindex_embed::protocol::{
    self, BatchId, EmbedRequest, EmbedResponse, Frame, MsgType, RerankDocument, RerankRequest,
    RerankResponse, Response, WorkerError,
};

/// Read a response frame from the worker with timeout enforcement.
///
/// This is the core I/O routine used by the persistent reader thread.
/// It reads the 4-byte length prefix followed by the payload, enforcing
/// the max frame size guard to prevent excessive allocations.
fn read_frame_with_timeout<R: Read>(reader: &mut R) -> Result<Vec<u8>, ClientError> {
    // Read response length (4 bytes, little-endian)
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
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
        Err(e) => Err(ClientError::Ipc(format!(
            "failed to read frame payload: {}",
            e
        ))),
    }
}

/// Monotonic batch ID counter for correlating requests.
static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);

const EMBED_DAEMON_ENV: &str = "LEINDEX_EMBED_DAEMON";

fn embed_daemon_enabled() -> bool {
    std::env::var(EMBED_DAEMON_ENV)
        .ok()
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

/// Maximum response frame size in bytes.
///
/// This mirrors the worker-side guard (`max_frame_size * 2` = 32 MiB) to
/// prevent a compromised or buggy worker from causing excessive allocations.
/// A response larger than this is rejected with a clear protocol error.
const MAX_RESPONSE_FRAME_SIZE: u32 = 64 * 1024 * 1024; // 64 MiB

/// Timeout for IPC read/write operations.
///
/// If the worker does not respond within this window, the IPC operation
/// fails with a timeout error rather than blocking indefinitely.
/// Set to 600 seconds because the first MIGraphX compile of Qwen3 can take
/// just under five minutes before the compiled `.mxr` cache exists. Warm runs
/// are expected to complete in seconds.
const IPC_TIMEOUT_SECS: u64 = 600;

fn platform_binary_name(binary_name: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", binary_name)
    } else {
        binary_name.to_string()
    }
}

/// Resolve the path to the worker binary.
///
/// First tries to find `leindex-embed` in the same directory as the running
/// binary (sibling path), so the worker is found even when the main binary
/// is invoked via absolute path. Falls back to PATH lookup if the sibling
/// doesn't exist.
fn resolve_worker_binary() -> Result<PathBuf, std::io::Error> {
    let binary_name = platform_binary_name("leindex-embed");
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let sibling = exe_dir.join(&binary_name);
            if sibling.exists() {
                return Ok(sibling);
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
struct WorkerConfigEnv {
    ort_dylib_path: Option<String>,
    execution_provider: Option<String>,
    model_name: Option<String>,
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
fn read_worker_config_env_from_config() -> WorkerConfigEnv {
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
fn read_ort_dylib_path_from_config() -> Option<String> {
    read_worker_config_env_from_config().ort_dylib_path
}

#[cfg(test)]
fn read_execution_provider_from_config() -> Option<String> {
    read_worker_config_env_from_config().execution_provider
}

#[cfg(test)]
fn read_worker_model_name_from_config() -> Option<String> {
    read_worker_config_env_from_config().model_name
}

fn migraphx_model_cache_path(model_name: Option<&str>) -> Option<std::path::PathBuf> {
    let model = sanitize_cache_component(model_name.unwrap_or("qwen3-embed-0.6b-dynamic"));
    let batch = leindex_embed::runtime::configured_onnx_inference_batch_size(
        model_name.unwrap_or("qwen3-embed-0.6b-dynamic"),
        "migraphx",
    );
    let sequence = leindex_embed::runtime::configured_onnx_sequence_len();
    let profile = format!(
        "v{}-b{}-s{}",
        sanitize_cache_component(env!("CARGO_PKG_VERSION")),
        batch,
        sequence
    );
    leindex_home_dir().map(|home| {
        home.join("cache")
            .join("migraphx")
            .join(model)
            .join(profile)
    })
}

fn sanitize_cache_component(value: &str) -> String {
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
fn daemon_socket_path(provider: Option<&str>, model_name: Option<&str>) -> Option<PathBuf> {
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

fn parse_config_assignment(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let value_part = rest.strip_prefix('=')?.trim();
    let trimmed = value_part.trim_matches(|c| c == '"' || c == '\'').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_startup_report_provider(line: &str) -> Option<String> {
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
fn leindex_home_dir() -> Option<std::path::PathBuf> {
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

    /// IPC operation timed out.
    #[error(
        "IPC timeout: worker did not respond within {} seconds",
        IPC_TIMEOUT_SECS
    )]
    Timeout,
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
    worker: Arc<Mutex<Option<WorkerHandle>>>,
    /// Last startup_report line observed on worker stderr.
    last_startup_report: Arc<Mutex<Option<String>>>,
    /// Whether Unix socket daemon reuse is allowed.
    use_daemon: bool,
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
        }
    }
}

/// Handle to a running worker process with its stdin/stdout pipes.
struct WorkerHandle {
    /// The child process.
    child: Option<Child>,
    /// Local transport for sending frames to the worker.
    writer: Option<WorkerWriter>,
    /// Persistent reader thread that reads IPC responses from the worker's stdout.
    /// Uses a oneshot channel to receive the response data with timeout enforcement.
    read_thread: thread::JoinHandle<()>,
    /// Channel sender to signal the read thread to perform a read and return the result.
    read_request_tx: std::sync::mpsc::Sender<ReadRequest>,
    /// Thread that mirrors worker stderr and captures startup reports.
    stderr_thread: Option<thread::JoinHandle<()>>,
    /// True when this handle is connected to a daemon intended to outlive this client.
    persistent: bool,
    /// Unix socket path for resident daemons, used to remove stale sockets on failure.
    socket_path: Option<PathBuf>,
}

enum WorkerWriter {
    Pipe(std::process::ChildStdin),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl WorkerWriter {
    fn shutdown(&self) {
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
enum ReadRequest {
    /// Request a read. Response sent via the channel.
    Read {
        tx: mpsc::Sender<Result<Vec<u8>, ClientError>>,
    },
    /// Signal the read thread to shut down.
    Shutdown,
}

#[cfg(unix)]
struct DaemonSpawnLock {
    file: std::fs::File,
}

#[cfg(unix)]
impl DaemonSpawnLock {
    fn acquire(path: &Path, timeout: Duration) -> Result<Self, ClientError> {
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
        }
    }

    /// Create a direct child-pipe client.
    pub fn new_pipe() -> Self {
        Self {
            worker: Arc::new(Mutex::new(None)),
            last_startup_report: Arc::new(Mutex::new(None)),
            use_daemon: false,
        }
    }

    /// Return the execution provider reported by the worker startup report.
    pub fn active_execution_provider(&self) -> Option<String> {
        self.last_startup_report
            .lock()
            .ok()
            .and_then(|line| line.as_deref().and_then(parse_startup_report_provider))
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

    /// Spawn a new worker process into the given guard slot.
    fn spawn_worker(
        &self,
        guard: &mut std::sync::MutexGuard<'_, Option<WorkerHandle>>,
    ) -> Result<(), ClientError> {
        let worker_path = resolve_worker_binary().map_err(|e| {
            ClientError::SpawnFailed(format!("failed to resolve worker binary: {}", e))
        })?;
        let config_env = read_worker_config_env_from_config();
        let configured_provider = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER")
            .ok()
            .or_else(|| config_env.execution_provider.clone());
        let configured_model = std::env::var("LEINDEX_WORKER_MODEL")
            .ok()
            .or_else(|| config_env.model_name.clone());

        #[cfg(unix)]
        if self.use_daemon {
            if let Some(handle) = self.spawn_or_connect_daemon(
                &worker_path,
                &config_env,
                configured_provider.as_deref(),
                configured_model.as_deref(),
            )? {
                **guard = Some(handle);
                return Ok(());
            }
        }

        **guard = Some(self.spawn_pipe_worker(
            &worker_path,
            &config_env,
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
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
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

        if let Some(handle) = self.connect_daemon(&socket_path, None)? {
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
            DaemonSpawnLock::acquire(&lock_path, Duration::from_secs(IPC_TIMEOUT_SECS))?;
        if let Some(handle) = self.connect_daemon(&socket_path, None)? {
            return Ok(Some(handle));
        }
        let _ = std::fs::remove_file(&socket_path);

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

        let deadline = Instant::now() + Duration::from_secs(IPC_TIMEOUT_SECS);
        loop {
            match UnixStream::connect(&socket_path) {
                Ok(stream) => {
                    return Self::socket_worker_handle(stream, Some(child), Some(socket_path))
                        .map(Some);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound
                        || e.kind() == std::io::ErrorKind::ConnectionRefused => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&socket_path);
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
                    let _ = std::fs::remove_file(&socket_path);
                    Self::print_daemon_log_tail();
                    return Err(ClientError::SpawnFailed(format!(
                        "worker daemon exited before accepting connections: {}",
                        status
                    )));
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = std::fs::remove_file(&socket_path);
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
                let _ = std::fs::remove_file(&socket_path);
                Self::print_daemon_log_tail();
                return Err(ClientError::Timeout);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg(unix)]
    fn connect_daemon(
        &self,
        socket_path: &Path,
        child: Option<Child>,
    ) -> Result<Option<WorkerHandle>, ClientError> {
        match UnixStream::connect(socket_path) {
            Ok(stream) => {
                Self::socket_worker_handle(stream, child, Some(socket_path.to_path_buf())).map(Some)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                let _ = std::fs::remove_file(socket_path);
                Ok(None)
            }
            Err(e) => Err(ClientError::Ipc(format!(
                "failed to connect worker daemon {}: {}",
                socket_path.display(),
                e
            ))),
        }
    }

    #[cfg(unix)]
    fn socket_worker_handle(
        stream: UnixStream,
        child: Option<Child>,
        socket_path: Option<PathBuf>,
    ) -> Result<WorkerHandle, ClientError> {
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

    fn spawn_reader_thread<R>(mut reader: R) -> (thread::JoinHandle<()>, mpsc::Sender<ReadRequest>)
    where
        R: Read + Send + 'static,
    {
        let (read_request_tx, read_request_rx) = mpsc::channel::<ReadRequest>();
        let read_thread = thread::spawn(move || {
            while let Ok(request) = read_request_rx.recv() {
                match request {
                    ReadRequest::Read { tx } => {
                        let _ = tx.send(read_frame_with_timeout(&mut reader));
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

    /// Kill the current worker and clear the handle so a fresh worker
    /// can be spawned on the next request.
    ///
    /// VAL-CPHASE-021: After calling this, the next embed request will
    /// transparently spawn a new worker process.
    ///
    /// On Unix, sends SIGTERM first and waits up to 2 seconds before
    /// falling back to SIGKILL. On other platforms, drops stdin (EOF)
    /// then waits with a timeout before killing.
    pub fn kill_worker(&self) {
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(mut handle) = guard.take() {
                Self::shutdown_worker_handle(&mut handle, true);
            }
        }
    }

    fn shutdown_worker_handle(handle: &mut WorkerHandle, kill_persistent: bool) {
        let _ = handle.read_request_tx.send(ReadRequest::Shutdown);
        if let Some(writer) = handle.writer.as_ref() {
            writer.shutdown();
        }
        drop(handle.writer.take());

        let should_kill_child = !handle.persistent || kill_persistent;
        if should_kill_child {
            if handle.persistent {
                if let Some(socket_path) = handle.socket_path.take() {
                    let _ = std::fs::remove_file(socket_path);
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

        let (replacement_tx, _replacement_rx) = mpsc::channel::<ReadRequest>();
        let old_tx = std::mem::replace(&mut handle.read_request_tx, replacement_tx);
        drop(old_tx);

        let replacement_thread = thread::spawn(|| {});
        let read_thread = std::mem::replace(&mut handle.read_thread, replacement_thread);
        let _ = read_thread.join();

        if let Some(stderr_thread) = handle.stderr_thread.take() {
            let _ = stderr_thread.join();
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
        self.ensure_worker()?;

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

    /// Send an embed request to the worker and return the response.
    ///
    /// This is the simple API that returns an error on failure rather than
    /// falling back. For retry-once fallback semantics, use `embed_with_fallback`.
    pub fn embed(
        &self,
        texts: &[String],
        expected_dim: usize,
    ) -> Result<EmbedResponse, ClientError> {
        self.ensure_worker()?;

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
        self.ensure_worker()?;

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
    /// Uses a persistent reader thread with a oneshot channel to enforce
    /// timeout on blocking pipe I/O. The persistent thread is spawned once
    /// when the worker starts and reused for all requests, avoiding the
    /// overhead of spawning a new thread per request.
    ///
    /// On timeout, the worker is left in an undefined state but no stdout
    /// is consumed (the thread may still be blocked on read — the process
    /// will be killed via kill_worker if needed).
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
            self.kill_worker();
            return Err(ClientError::Ipc(format!(
                "failed to write to worker: {}",
                e
            )));
        }
        if let Err(e) = writer.flush() {
            drop(guard);
            self.kill_worker();
            return Err(ClientError::Ipc(format!(
                "failed to flush worker transport: {}",
                e
            )));
        }

        // Use the persistent reader thread to read the response with timeout.
        let (tx, rx) = mpsc::channel();
        handle
            .read_request_tx
            .send(ReadRequest::Read { tx })
            .map_err(|_e| ClientError::Ipc("reader thread channel closed".to_string()))?;

        // Wait for the read with timeout.
        match rx.recv_timeout(Duration::from_secs(IPC_TIMEOUT_SECS)) {
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
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if handle.persistent {
                    if let Some(mut handle) = guard.take() {
                        Self::shutdown_worker_handle(&mut handle, false);
                    }
                    return Err(ClientError::Timeout);
                }
                drop(guard);
                self.kill_worker();
                Err(ClientError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                drop(guard);
                self.kill_worker();
                Err(ClientError::Ipc(
                    "reader thread disconnected unexpectedly".to_string(),
                ))
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
    use leindex_embed::protocol::ErrorKind;

    #[test]
    fn test_client_creation() {
        let _client = EmbeddingClient::new();
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
        std::env::set_var("LEINDEX_HOME", tmp.path());
        std::env::remove_var("LEINDEX_ONNX_INFERENCE_BATCH_SIZE");
        std::env::remove_var("LEINDEX_ONNX_SEQUENCE_LEN");

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

        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    fn test_read_execution_provider_from_config_skips_auto() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());

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

        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    fn test_read_worker_model_name_from_config_returns_value() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());

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

        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    fn test_migraphx_model_cache_path_uses_leindex_home() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());
        std::env::remove_var("LEINDEX_ONNX_INFERENCE_BATCH_SIZE");
        std::env::remove_var("LEINDEX_ONNX_SEQUENCE_LEN");
        let expected = tmp
            .path()
            .join("cache")
            .join("migraphx")
            .join("qwen3-embed-0_6b-dynamic")
            .join("v1_8_4-b8-s128");

        assert_eq!(
            migraphx_model_cache_path(Some("qwen3-embed-0.6b-dynamic")).as_deref(),
            Some(expected.as_path())
        );

        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    #[cfg(unix)]
    fn test_daemon_socket_path_includes_inference_shape() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());
        std::env::remove_var("LEINDEX_ONNX_INFERENCE_BATCH_SIZE");
        std::env::remove_var("LEINDEX_ONNX_SEQUENCE_LEN");

        let socket =
            daemon_socket_path(Some("migraphx"), Some("qwen3-embed-0.6b-dynamic")).unwrap();
        let filename = socket.file_name().and_then(|name| name.to_str()).unwrap();
        assert!(filename.starts_with("leindex-embed-"));
        assert!(filename.ends_with(".sock"));
        assert!(socket.to_string_lossy().len() <= 100);

        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    fn test_read_ort_dylib_path_from_config_returns_none_when_absent() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());

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

        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    fn test_read_ort_dylib_path_from_config_handles_single_quotes() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());

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

        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    fn test_leindex_home_dir_prefers_env_override() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("LEINDEX_HOME", "/custom/leindex/home");
        assert_eq!(
            leindex_home_dir(),
            Some(std::path::PathBuf::from("/custom/leindex/home"))
        );
        std::env::remove_var("LEINDEX_HOME");
    }

    #[test]
    fn test_leindex_home_dir_falls_back_to_home() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("LEINDEX_HOME");
        std::env::set_var("HOME", "/home/testuser");
        let home = leindex_home_dir();
        assert_eq!(
            home,
            Some(std::path::PathBuf::from("/home/testuser/.leindex"))
        );
        std::env::remove_var("HOME");
    }

    #[test]
    fn test_leindex_home_dir_relative_env_ignored() {
        let _g = TEST_ENV_LOCK.lock().unwrap();
        std::env::set_var("LEINDEX_HOME", "relative/path");
        std::env::set_var("HOME", "/home/fallback");
        // Should fall back to HOME-based path, not use relative.
        let home = leindex_home_dir();
        assert_eq!(
            home,
            Some(std::path::PathBuf::from("/home/fallback/.leindex"))
        );
        std::env::remove_var("LEINDEX_HOME");
        std::env::remove_var("HOME");
    }
}
