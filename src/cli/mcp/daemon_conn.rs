//! One client connection on the daemon socket (wire v2).
//!
//! Compared with the original connection loop this one
//! - answers `tools/call` concurrently, so a slow tool never queues `ping` or
//!   the next request behind it (the same fix the stdio transport got);
//! - never times out an idle connection: an editor that sits quiet for an hour
//!   is still a client, and dropping it silently was indistinguishable from a
//!   hang;
//! - reads the shim's hello line, starts warming the client's project at once
//!   and fills in `project_path` for tool calls that omit it;
//! - counts itself as an active client, so the daemon never idles out from
//!   under a connected editor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tracing::debug;

use super::{
    ProcessIdleClock, SERVER_STATE, handle_socket_message, read_bounded_line, write_socket_frame,
};
use crate::cli::daemon::proto::{self, Ack, WIRE_VERSION};

/// Upper bound on tool calls executing at once on one connection.
const MAX_CONCURRENT_CALLS: usize = 64;

/// Longest single frame accepted (matches the stdio transport).
const MAX_FRAME_BYTES: usize = 10 * 1024 * 1024;

/// A frame that has started must finish within this long.
const FRAME_BODY_TIMEOUT: Duration = Duration::from_secs(30);

/// Shutdown grace for calls still running when the client hangs up.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Milliseconds since the Unix epoch at daemon start; reported in the ack.
pub(super) fn daemon_started_ms() -> u64 {
    static STARTED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *STARTED.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// Clients currently attached to this daemon.
pub(super) fn attached_clients() -> &'static AtomicUsize {
    static CLIENTS: AtomicUsize = AtomicUsize::new(0);
    &CLIENTS
}

/// Counts a connection while alive, and restarts the idle timer when it ends.
struct Attached {
    idle_clock: ProcessIdleClock,
}

impl Attached {
    fn new(idle_clock: ProcessIdleClock) -> Self {
        attached_clients().fetch_add(1, Ordering::AcqRel);
        idle_clock.touch();
        Self { idle_clock }
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        attached_clients().fetch_sub(1, Ordering::AcqRel);
        self.idle_clock.touch();
    }
}

enum Frame {
    /// A JSON payload; `framed` records `Content-Length` framing.
    Message { payload: String, framed: bool },
    /// The peer sent something we cannot recover from.
    Fatal { response: String },
}

/// Read one frame. `Ok(None)` is a clean end of stream. Waiting for the *start*
/// of a frame is unbounded; once a frame has started it must complete.
async fn read_frame<R>(reader: &mut R) -> std::io::Result<Option<Frame>>
where
    R: AsyncBufRead + AsyncRead + Unpin,
{
    let Some(line) = (match read_bounded_line(reader, MAX_FRAME_BYTES).await {
        Ok(line) => line,
        Err(_) => {
            return Ok(Some(Frame::Fatal {
                response: oversize_response(),
            }));
        }
    }) else {
        return Ok(None);
    };
    let trimmed = line.trim_end();
    if !trimmed.to_ascii_lowercase().starts_with("content-length:") {
        return Ok(Some(Frame::Message {
            payload: trimmed.to_string(),
            framed: false,
        }));
    }
    let length = trimmed
        .split(':')
        .nth(1)
        .and_then(|value| value.trim().parse::<usize>().ok());
    let Some(length) = length.filter(|length| *length <= MAX_FRAME_BYTES) else {
        return Ok(Some(Frame::Fatal {
            response: oversize_response(),
        }));
    };
    // Remaining headers up to the blank line.
    loop {
        match tokio::time::timeout(FRAME_BODY_TIMEOUT, read_bounded_line(reader, 8192)).await {
            Ok(Ok(Some(header))) if header.trim().is_empty() => break,
            Ok(Ok(Some(_))) => {}
            _ => return Ok(None),
        }
    }
    let mut body = vec![0u8; length];
    match tokio::time::timeout(FRAME_BODY_TIMEOUT, reader.read_exact(&mut body)).await {
        Ok(Ok(_)) => Ok(Some(Frame::Message {
            payload: String::from_utf8_lossy(&body).into_owned(),
            framed: true,
        })),
        _ => Ok(None),
    }
}

fn oversize_response() -> String {
    use super::super::protocol::{JsonRpcError, JsonRpcResponse};
    serde_json::to_string(&JsonRpcResponse::error(
        serde_json::Value::Null,
        JsonRpcError::new(-32600, "request payload exceeds maximum size"),
    ))
    .unwrap_or_default()
}

/// Serve one client until it hangs up.
pub(super) async fn serve(
    stream: tokio::net::UnixStream,
    session_id: String,
    session_handshakes: Arc<DashMap<Arc<str>, (bool, Instant)>>,
    handshake_complete: Arc<AtomicBool>,
    idle_clock: ProcessIdleClock,
) {
    use tokio::io::BufReader;

    debug!("Daemon connection accepted (session {session_id})");
    let _attached = Attached::new(idle_clock.clone());
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<(String, bool)>();
    let writer = tokio::spawn(async move {
        while let Some((response, framed)) = out_rx.recv().await {
            if !write_socket_frame(&mut write_half, &response, framed).await {
                break;
            }
        }
    });

    let limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_CALLS));
    let mut calls: JoinSet<()> = JoinSet::new();
    let mut default_project: Option<String> = None;
    let mut first_frame = true;

    loop {
        // Reap finished calls so the set does not grow for the connection's life.
        while calls.try_join_next().is_some() {}
        let frame = match read_frame(&mut reader).await {
            Ok(Some(frame)) => frame,
            Ok(None) | Err(_) => break,
        };
        let (mut payload, framed) = match frame {
            Frame::Message { payload, framed } => (payload, framed),
            Frame::Fatal { response } => {
                let _ = out_tx.send((response, false));
                break;
            }
        };
        if payload.is_empty() {
            continue;
        }

        if first_frame {
            first_frame = false;
            if let Some(hello) = proto::parse_hello(&payload) {
                let compatible = hello.wire == WIRE_VERSION;
                let ack = Ack {
                    wire: WIRE_VERSION,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    pid: std::process::id(),
                    started_ms: daemon_started_ms(),
                    clients: attached_clients().load(Ordering::Acquire),
                    ok: compatible,
                    error: (!compatible).then(|| {
                        format!(
                            "wire v{} client cannot talk to wire v{} daemon",
                            hello.wire, WIRE_VERSION
                        )
                    }),
                };
                // The ack is a bare line, never Content-Length framed.
                let _ = out_tx.send((proto::ack_line(&ack).trim_end().to_string(), false));
                if !compatible {
                    break;
                }
                if let Some(cwd) = hello.cwd.filter(|cwd| !cwd.is_empty()) {
                    if let Some(state) = SERVER_STATE.get() {
                        state.spawn_prewarm_at(Some(std::path::PathBuf::from(&cwd)));
                    }
                    default_project = Some(cwd);
                }
                continue;
            }
        }

        idle_clock.touch();
        if let Some(cwd) = default_project.as_deref() {
            if let Some(patched) = proto::with_default_project(&payload, cwd) {
                payload = patched;
            }
        }

        let is_tool_call = payload.contains("\"tools/call\"");
        let respond_framed = framed;
        let out = out_tx.clone();
        let session = session_id.clone();
        let handshakes = Arc::clone(&session_handshakes);
        let complete = Arc::clone(&handshake_complete);
        if is_tool_call {
            let Ok(permit) = Arc::clone(&limiter).acquire_owned().await else {
                break;
            };
            let clock = idle_clock.clone();
            calls.spawn(async move {
                let _permit = permit;
                if let Some(response) =
                    handle_socket_message(&payload, &session, &handshakes, &complete).await
                {
                    let _ = out.send((response, respond_framed));
                }
                clock.touch();
            });
        } else if let Some(response) =
            handle_socket_message(&payload, &session, &handshakes, &complete).await
        {
            let _ = out.send((response, respond_framed));
        }
    }

    // Answer everything already accepted before hanging up.
    let drain = async { while calls.join_next().await.is_some() {} };
    if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
        calls.abort_all();
    }
    drop(out_tx);
    let _ = writer.await;
    session_handshakes.remove(session_id.as_str());
    debug!("Daemon connection closed (session {session_id})");
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn test_read_frame_newline_and_content_length() {
        let mut input: &[u8] = b"{\"a\":1}\nContent-Length: 7\r\nX-Y: z\r\n\r\n{\"b\":2}";
        let mut reader = tokio::io::BufReader::new(&mut input);
        match read_frame(&mut reader).await.unwrap() {
            Some(Frame::Message { payload, framed }) => {
                assert_eq!(payload, "{\"a\":1}");
                assert!(!framed);
            }
            _ => panic!("expected a newline frame"),
        }
        match read_frame(&mut reader).await.unwrap() {
            Some(Frame::Message { payload, framed }) => {
                assert_eq!(payload, "{\"b\":2}");
                assert!(framed);
            }
            _ => panic!("expected a framed frame"),
        }
        assert!(read_frame(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_read_frame_rejects_oversized_declared_length() {
        let mut input: &[u8] = b"Content-Length: 99999999999\r\n\r\n";
        let mut reader = tokio::io::BufReader::new(&mut input);
        assert!(matches!(
            read_frame(&mut reader).await.unwrap(),
            Some(Frame::Fatal { .. })
        ));
    }

    #[test]
    fn test_attached_guard_counts_and_releases() {
        let before = attached_clients().load(Ordering::Acquire);
        let clock = ProcessIdleClock::new();
        let guard = Attached::new(clock.clone());
        assert_eq!(attached_clients().load(Ordering::Acquire), before + 1);
        drop(guard);
        assert_eq!(attached_clients().load(Ordering::Acquire), before);
    }
}
