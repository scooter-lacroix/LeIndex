// Worker client for delegating ONNX inference to the leindex-embed process
//
// VAL-CPHASE-002: The main crate no longer owns ONNX runtime deps directly.
// This client communicates with the leindex-embed worker over local IPC.
//
// The full worker lifecycle (cold start, reuse, idle teardown, restart,
// retry-once fallback) will be implemented in the runtime lifecycle feature.
// This module provides the protocol client foundation.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use leindex_embed::protocol::{
    self, BatchId, EmbedRequest, EmbedResponse, ErrorKind, Frame, MsgType, Request, RerankDocument,
    RerankRequest, RerankResponse, Response, WorkerError,
};

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
}

/// Client for the leindex-embed worker process.
///
/// Manages the worker lifecycle and provides methods for sending embed
/// and rerank requests over local IPC.
///
/// The full lifecycle (cold start, reuse, idle teardown, restart) will be
/// implemented in the runtime lifecycle feature. This struct provides the
/// protocol client foundation.
pub struct EmbeddingClient {
    /// Worker process handle, if currently running.
    worker: Mutex<Option<WorkerHandle>>,
}

/// Handle to a running worker process with its stdin/stdout pipes.
struct WorkerHandle {
    /// The child process.
    child: Child,
    /// Stdin pipe for sending frames to the worker.
    stdin: std::process::ChildStdin,
    /// Stdout pipe for receiving frames from the worker.
    stdout: std::process::ChildStdout,
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

    /// Ensure the worker is running, spawning it if necessary.
    fn ensure_worker(&self) -> Result<(), ClientError> {
        let mut guard = self
            .worker
            .lock()
            .map_err(|e| ClientError::Ipc(format!("failed to lock worker handle: {}", e)))?;

        if guard.is_some() {
            return Ok(());
        }

        let mut child = Command::new("leindex-embed")
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

        *guard = Some(WorkerHandle {
            child,
            stdin,
            stdout,
        });

        Ok(())
    }

    /// Send an embed request to the worker and return the response.
    ///
    /// Spawns the worker on first call (cold start). The full lifecycle
    /// (reuse, idle teardown, retry-once) will be added in the runtime
    /// lifecycle feature.
    pub fn embed(
        &self,
        texts: &[String],
        expected_dim: usize,
    ) -> Result<EmbedResponse, ClientError> {
        self.ensure_worker()?;

        let batch_id = BatchId::new(std::process::id() as u64);
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

        let batch_id = BatchId::new(std::process::id() as u64);
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

        // Read the response
        let mut len_buf = [0u8; 4];
        handle
            .stdout
            .read_exact(&mut len_buf)
            .map_err(|e| ClientError::Ipc(format!("failed to read response length: {}", e)))?;

        let payload_len = u32::from_le_bytes(len_buf) as usize;
        let mut frame_buf = vec![0u8; payload_len];
        handle
            .stdout
            .read_exact(&mut frame_buf)
            .map_err(|e| ClientError::Ipc(format!("failed to read response payload: {}", e)))?;

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

    #[test]
    fn test_client_creation() {
        let _client = EmbeddingClient::new();
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
}
