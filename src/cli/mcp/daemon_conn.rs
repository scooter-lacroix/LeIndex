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
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tracing::debug;

use super::{
    ProcessIdleClock, SERVER_STATE, handle_socket_message, read_bounded_line, write_socket_frame,
};
use crate::cli::daemon::proto::{self, Ack, WIRE_VERSION};

/// Upper bound on tool calls executing at once on one connection.
const MAX_CONCURRENT_CALLS: usize = 64;

/// Upper bound on responses queued for the socket writer. Bounded on
/// purpose: a client that keeps submitting requests but stops reading
/// responses would otherwise park the writer while this queue accepted
/// every completed result without limit, growing the shared daemon's
/// memory until it died for every attached editor. Once the queue is full,
/// senders park (and the read loop with them), pushing backpressure onto
/// the client through TCP flow control.
const MAX_QUEUED_RESPONSES: usize = 32;

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

/// Queue a response for the writer, when the message produced one
/// (notifications produce none). Awaiting the bounded send is the
/// backpressure point: it parks until the writer has drained enough of the
/// queue, instead of buffering without limit.
async fn send_response(out: mpsc::Sender<(String, bool)>, response: Option<String>, framed: bool) {
    if let Some(response) = response {
        let _ = out.send((response, framed)).await;
    }
}

/// Drain the outbound response queue into the socket until the peer goes away.
async fn drive_writer<W>(mut write_half: W, mut out_rx: mpsc::Receiver<(String, bool)>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    while let Some((response, framed)) = out_rx.recv().await {
        if !write_socket_frame(&mut write_half, &response, framed).await {
            break;
        }
    }
}

/// How the connection loop should treat the client's first frame.
enum HelloOutcome {
    /// Not a hello: handle it as an ordinary request.
    NotHello,
    /// A hello from a compatible client: the ack is sent, keep serving.
    Handled,
    /// A hello from an incompatible client: hang up.
    Incompatible,
}

/// Handle the client's first frame as the shim hello when it is one: reply with
/// the ack, start warming the client's project at once and record its cwd as
/// the connection default.
fn handle_hello_frame(
    payload: &str,
    out_tx: &mpsc::Sender<(String, bool)>,
    default_project: &mut Option<String>,
) -> HelloOutcome {
    let Some(hello) = proto::parse_hello(payload) else {
        return HelloOutcome::NotHello;
    };
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
    // The ack is a bare line, never Content-Length framed. try_send: this
    // is a sync helper and the queue is at its freshest here — a full queue
    // before the ack means the client already is not reading; hang up.
    if out_tx
        .try_send((proto::ack_line(&ack).trim_end().to_string(), false))
        .is_err()
    {
        return HelloOutcome::Incompatible;
    }
    if !compatible {
        return HelloOutcome::Incompatible;
    }
    if let Some(cwd) = hello.cwd.filter(|cwd| !cwd.is_empty()) {
        // Re-validate on the daemon side. The shim canonicalizes and filters
        // the cwd before sending it, but the wire peer is unauthenticated: a
        // relative path, `/`, or a nonexistent tree named by a rogue client
        // must not be prewarmed and injected as the default `project_path`
        // for every tool call on this connection.
        let validated = std::path::PathBuf::from(&cwd)
            .canonicalize()
            .ok()
            .filter(|path| proto::is_projectish_cwd(path))
            .map(|path| path.to_string_lossy().into_owned());
        if let Some(cwd) = validated {
            if let Some(state) = SERVER_STATE.get() {
                state.spawn_prewarm_at(Some(std::path::PathBuf::from(&cwd)));
            }
            *default_project = Some(cwd);
        }
    }
    HelloOutcome::Handled
}

/// Fill in `project_path` for a tool call that omitted it, using the cwd the
/// client announced in its hello.
fn apply_default_project(payload: &mut String, default_project: Option<&str>) {
    let Some(cwd) = default_project else {
        return;
    };
    if let Some(patched) = proto::with_default_project(payload, cwd) {
        *payload = patched;
    }
}

/// Answer everything already accepted before hanging up.
async fn drain_calls(calls: &mut JoinSet<()>) {
    let drain = async { while calls.join_next().await.is_some() {} };
    if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
        calls.abort_all();
    }
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
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let (out_tx, out_rx) = mpsc::channel::<(String, bool)>(MAX_QUEUED_RESPONSES);
    let writer = tokio::spawn(drive_writer(write_half, out_rx));

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
                let _ = out_tx.send((response, false)).await;
                break;
            }
        };
        if payload.is_empty() {
            continue;
        }

        if first_frame {
            first_frame = false;
            match handle_hello_frame(&payload, &out_tx, &mut default_project) {
                HelloOutcome::Handled => continue,
                HelloOutcome::Incompatible => break,
                HelloOutcome::NotHello => {}
            }
        }

        idle_clock.touch();
        apply_default_project(&mut payload, default_project.as_deref());

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
                let response =
                    handle_socket_message(&payload, &session, &handshakes, &complete).await;
                send_response(out, response, respond_framed).await;
                clock.touch();
            });
        } else {
            let response = handle_socket_message(&payload, &session, &handshakes, &complete).await;
            send_response(out_tx.clone(), response, respond_framed).await;
        }
    }

    // Answer everything already accepted before hanging up.
    drain_calls(&mut calls).await;
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
