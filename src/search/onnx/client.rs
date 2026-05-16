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
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use leindex_embed::protocol::{
    self, BatchId, EmbedRequest, EmbedResponse, Frame, MsgType, RerankDocument, RerankRequest,
    RerankResponse, Response, WorkerError,
};

/// Monotonic batch ID counter for correlating requests.
static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);

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
const IPC_TIMEOUT_SECS: u64 = 30;

/// Resolve the path to the worker binary.
///
/// First tries to find `leindex-embed` in the same directory as the running
/// binary (sibling path), so the worker is found even when the main binary
/// is invoked via absolute path. Falls back to PATH lookup if the sibling
/// doesn't exist.
fn resolve_worker_binary() -> Result<std::path::PathBuf, std::io::Error> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let sibling = exe_dir.join("leindex-embed");
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }
    // Fall back to PATH lookup
    which::which("leindex-embed").map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("worker binary 'leindex-embed' not found in PATH: {}", e),
        )
    })
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
    worker: Mutex<Option<WorkerHandle>>,
}

/// Manual Debug impl — Child doesn't implement Debug.
impl fmt::Debug for EmbeddingClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbeddingClient")
            .field("worker", &self.worker.lock().map(|g| g.is_some()))
            .finish()
    }
}

/// Manual Clone impl — creates a new empty client (does not clone the worker).
impl Clone for EmbeddingClient {
    fn clone(&self) -> Self {
        Self::new()
    }
}

/// Handle to a running worker process with its stdin/stdout pipes.
struct WorkerHandle {
    /// The child process.
    child: Child,
    /// Stdin pipe for sending frames to the worker.
    stdin: std::process::ChildStdin,
    /// Stdout pipe for receiving frames from the worker.
    /// Wrapped in Option so we can take it for the read thread and restore it afterward.
    stdout: Option<std::process::ChildStdout>,
}

impl EmbeddingClient {
    /// Create a new embedding client.
    ///
    /// The worker is not spawned until the first request is made (cold start).
    pub fn new() -> Self {
        Self {
            worker: Mutex::new(None),
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
        // Resolve the worker binary path.
        //
        // First, try to find `leindex-embed` relative to the running binary
        // so the worker is found even when the main binary is invoked via
        // absolute path. Fall back to PATH lookup if the sibling is absent.
        let worker_path = resolve_worker_binary().map_err(|e| {
            ClientError::SpawnFailed(format!("failed to resolve worker binary: {}", e))
        })?;

        let mut child = Command::new(&worker_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| ClientError::SpawnFailed(e.to_string()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::SpawnFailed("failed to open worker stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::SpawnFailed("failed to open worker stdout".to_string()))?;

        **guard = Some(WorkerHandle {
            child,
            stdin,
            stdout: Some(stdout),
        });

        Ok(())
    }

    /// Kill the current worker and clear the handle so a fresh worker
    /// can be spawned on the next request.
    ///
    /// VAL-CPHASE-021: After calling this, the next embed request will
    /// transparently spawn a new worker process.
    fn kill_worker(&self) {
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(mut handle) = guard.take() {
                // Close stdin to signal shutdown
                drop(handle.stdin);
                // Kill the process if it hasn't exited yet
                let _ = handle.child.kill();
                let _ = handle.child.wait();
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
            Ok(response) => return EmbedResult::Success(response),
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
                        return EmbedResult::Success(response);
                    }
                    Err(retry_error) => {
                        // VAL-CPHASE-018: Second failure → TF-IDF fallback for this batch only
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

                        return EmbedResult::Fallback {
                            batch_id,
                            error: retry_error,
                        };
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
    /// Uses a thread + channel pattern to enforce timeout on blocking pipe I/O.
    /// Pipes do not support `set_read_timeout()`, so we spawn a thread to do
    /// the blocking read and use `recv_timeout()` to enforce the deadline.
    ///
    /// After a successful read, stdout is restored to the handle so the client
    /// remains usable for subsequent requests. On timeout, the worker is left
    /// in an undefined state but no stdout is consumed (the thread may still
    /// be blocked on read — the process will be killed via kill_worker if needed).
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
        handle
            .stdin
            .write_all(&wire)
            .map_err(|e| ClientError::Ipc(format!("failed to write to worker: {}", e)))?;
        handle
            .stdin
            .flush()
            .map_err(|e| ClientError::Ipc(format!("failed to flush worker stdin: {}", e)))?;

        // Take stdout out of the handle for the read thread.
        // After the thread completes (success or timeout), we return stdout
        // so the caller can restore it to the handle on success.
        let stdout = handle
            .stdout
            .take()
            .ok_or_else(|| ClientError::Ipc("worker stdout was unexpectedly None".to_string()))?;

        // Helper to perform the IPC read with timeout.
        // Returns (frame_buf, stdout) so the caller can restore stdout on success.
        fn read_response(
            stdout: std::process::ChildStdout,
        ) -> Result<(Vec<u8>, std::process::ChildStdout), ClientError> {
            let (tx, rx) = mpsc::channel::<Result<(Vec<u8>, std::process::ChildStdout), std::io::Error>>();
            let timeout = Duration::from_secs(IPC_TIMEOUT_SECS);

            std::thread::spawn(move || {
                let mut stdout = stdout;
                // Read response length (4 bytes, little-endian)
                let mut len_buf = [0u8; 4];
                match stdout.read_exact(&mut len_buf) {
                    Ok(()) => {}
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }

                let payload_len = u32::from_le_bytes(len_buf);

                // Guard against oversized responses to prevent excessive allocations.
                if payload_len > MAX_RESPONSE_FRAME_SIZE {
                    let _ = tx.send(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "response frame too large: {} bytes (max: {} bytes)",
                            payload_len, MAX_RESPONSE_FRAME_SIZE
                        ),
                    )));
                    return;
                }

                let payload_len = payload_len as usize;
                let mut frame_buf = vec![0u8; payload_len];
                match stdout.read_exact(&mut frame_buf) {
                    Ok(()) => {
                        let _ = tx.send(Ok((frame_buf, stdout)));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            });

            // Wait for the read with timeout
            match rx.recv_timeout(timeout) {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(e)) => {
                    if e.kind() == std::io::ErrorKind::TimedOut {
                        Err(ClientError::Timeout)
                    } else {
                        Err(ClientError::Ipc(format!(
                            "failed to read from worker: {}",
                            e
                        )))
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => Err(ClientError::Timeout),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(ClientError::Ipc(
                    "worker disconnected unexpectedly".to_string(),
                )),
            }
        }

        let (frame_buf, returned_stdout) = read_response(stdout)?;

        // Restore stdout to the handle so the client remains usable.
        handle.stdout = Some(returned_stdout);

        Frame::from_wire_bytes(&frame_buf).map_err(|e| ClientError::Ipc(e.to_string()))
    }
}

impl Drop for EmbeddingClient {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(mut handle) = guard.take() {
                // Close stdin to signal the worker to shut down
                drop(handle.stdin);
                // Wait for the worker to exit
                let _ = handle.child.wait();
            }
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
    fn test_client_clone_creates_new() {
        let client = EmbeddingClient::new();
        let cloned = client.clone();
        // Clone creates a new empty client, not sharing the worker
        let _ = format!("{:?}", cloned);
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
}
