//! Stdio shim forwarder (spec §4.1).
//!
//! Connects to `leindexd`'s Unix socket and transparently proxies MCP/JSON-RPC
//! frames between the client's stdin/stdout and the daemon. The shim:
//!
//! - Parses/emits no JSON: it is a **byte-faithful** pipe. Client request IDs,
//!   framing (Content-Length or newline-delimited), and ordering are preserved
//!   exactly because the raw byte stream is never inspected or modified.
//! - Holds no SQLite, no PDG, no model runtime, no Tokio worker pool beyond the
//!   basic IO tasks. Target RSS: 5–15 MiB.
//! - Reconnects once after a daemon restart when safe (spec §4.1).
//!
//! The shim is compiled only behind the `daemon-client` feature flag. The
//! default build runs the full inline MCP server (spec §12.3 phase 3).

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use super::endpoint::DaemonEndpoint;

/// Reconnect delay after a daemon restart before retrying (spec §4.1).
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Reconnect timeout: give up forwarding after this duration without a
/// successful reconnection.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Forward MCP/JSON-RPC frames between stdin/stdout and the daemon's Unix
/// socket.
///
/// Connects to `endpoint.socket_path`, spawns two tasks (stdin→socket and
/// socket→stdout), and waits for both to complete. If the socket connection
/// drops mid-stream and the daemon is still reachable, the shim reconnects
/// once and continues forwarding (spec §4.1).
///
/// # Errors
///
/// Returns an error if the initial connection fails or the forward loop
/// encounters an unrecoverable IO error.
pub async fn forward_stdio_to_daemon(endpoint: &DaemonEndpoint) -> Result<()> {
    forward_stdio_to_daemon_with_reconnect(endpoint, true).await
}

/// Core forwarder with configurable reconnect behaviour. When `allow_reconnect`
/// is false, the shim exits after the first connection drop (used by tests).
pub async fn forward_stdio_to_daemon_with_reconnect(
    endpoint: &DaemonEndpoint,
    allow_reconnect: bool,
) -> Result<()> {
    let stream = tokio::net::UnixStream::connect(&endpoint.socket_path)
        .await
        .with_context(|| {
            format!(
                "failed to connect to daemon socket at {}",
                endpoint.socket_path.display()
            )
        })?;

    debug!(
        "shim connected to daemon at {} (pid {})",
        endpoint.socket_path.display(),
        endpoint.pid
    );

    forward_stream_with_reconnect(
        stream,
        tokio::io::stdin(),
        tokio::io::stdout(),
        &endpoint.socket_path,
        allow_reconnect,
    )
    .await
}

/// Generic stream forwarder: copies bytes bidirectionally between a client-side
/// reader/writer pair and a daemon Unix socket stream. Extracted from
/// [`forward_stdio_to_daemon`] so integration tests can exercise the forwarding
/// logic with in-memory pipes instead of real stdin/stdout.
///
/// Two tasks run concurrently:
/// - **Client→Daemon**: reads from `client_read` and writes to the socket's
///   write half.
/// - **Daemon→Client**: reads from the socket's read half and writes to
///   `client_write`.
///
/// If the daemon socket closes and `allow_reconnect` is true, the forwarder
/// attempts one reconnect (spec §4.1) before giving up.
pub async fn forward_stream_with_reconnect<R, W>(
    mut stream: tokio::net::UnixStream,
    mut client_read: R,
    mut client_write: W,
    socket_path: &std::path::Path,
    allow_reconnect: bool,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    loop {
        let (mut socket_read, mut socket_write) = stream.split();

        let client_to_socket = async {
            let result = tokio::io::copy(&mut client_read, &mut socket_write).await;
            // Shut down the write half so the daemon sees EOF on this direction.
            let _ = socket_write.shutdown().await;
            result
        };

        let socket_to_client = async {
            let result = tokio::io::copy(&mut socket_read, &mut client_write).await;
            // Flush any remaining buffered output.
            let _ = client_write.flush().await;
            result
        };

        // Run both directions concurrently. The pair completes when either
        // direction closes (EOF on client read or socket close from the daemon).
        let (client_result, socket_result) = tokio::join!(client_to_socket, socket_to_client);

        // Determine whether we should exit or reconnect.
        // client_result is Ok(0) when the client read closes (normal shutdown).
        let client_closed = matches!(&client_result, Ok(0));
        // socket_result is Ok(0) when the daemon closed the connection.
        let socket_closed = matches!(&socket_result, Ok(0));

        if client_closed {
            debug!("shim: client stream closed, shutting down");
            return Ok(());
        }

        // If the socket closed but the client is still sending, the daemon may
        // have restarted. Attempt a single reconnect (spec §4.1).
        if socket_closed && allow_reconnect {
            debug!("shim: daemon socket closed, attempting reconnect");
            stream = match try_reconnect(socket_path).await {
                Some(s) => s,
                None => {
                    warn!(
                        "shim: failed to reconnect to daemon within {:?}; exiting",
                        RECONNECT_TIMEOUT
                    );
                    anyhow::bail!("daemon connection lost and reconnect failed");
                }
            };
            debug!("shim: reconnected to daemon, resuming forward loop");
            continue;
        }

        // Any IO error at this point is unrecoverable.
        if let Err(e) = client_result {
            return Err(anyhow::anyhow!("shim client read error: {e}"));
        }
        if let Err(e) = socket_result {
            return Err(anyhow::anyhow!("shim socket read error: {e}"));
        }

        return Ok(());
    }
}

/// Attempt to reconnect to the daemon socket with a bounded timeout.
/// Returns `Some(stream)` on success, `None` if the daemon never comes back.
async fn try_reconnect(socket_path: &std::path::Path) -> Option<tokio::net::UnixStream> {
    let deadline = tokio::time::Instant::now() + RECONNECT_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        if let Ok(stream) = tokio::net::UnixStream::connect(socket_path).await {
            return Some(stream);
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::path::PathBuf;

    /// A `DaemonEndpoint` for testing with an unreachable socket path. Used
    /// only to verify the error path (connection failure).
    #[test]
    fn test_forward_fails_on_missing_socket() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let endpoint = DaemonEndpoint {
            socket_path: PathBuf::from("/tmp/leindex-nonexistent-test-shim.sock"),
            pid: 999_999,
            pid_start_time_ms: 0,
            protocol_version: 1,
            leindex_version: "test".into(),
        };
        let result = rt.block_on(forward_stdio_to_daemon_with_reconnect(&endpoint, false));
        assert!(
            result.is_err(),
            "forwarder must error when socket does not exist"
        );
    }

    /// Byte-faithful passthrough test using the generic forwarder and an
    /// in-memory echo server. Verifies that data flows client→daemon and
    /// daemon→client without modification.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_shim_byte_faithful_passthrough() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("passthrough.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();

        // Spawn an echo server as a mock daemon.
        let server_task = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let n = conn.read(&mut buf).await.unwrap();
            conn.write_all(&buf[..n]).await.unwrap();
            conn.flush().await.unwrap();
            // Close to signal the shim.
            drop(conn);
        });

        // Give the listener a moment to be ready.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();

        // Create a duplex pipe: client_tx feeds client_read, client_write feeds client_rx.
        let (mut client_tx, client_read) = tokio::io::duplex(4096);
        let (client_write, mut client_rx) = tokio::io::duplex(4096);

        // Spawn the forwarder.
        let forward_task = tokio::spawn(async move {
            forward_stream_with_reconnect(
                stream,
                client_read,
                client_write,
                PathBuf::from("/nonexistent-for-reconnect.sock").as_path(),
                false,
            )
            .await
        });

        // Send test data through the client side.
        let payload = b"{\"jsonrpc\":\"2.0\",\"id\":42,\"method\":\"test\"}\n";
        client_tx.write_all(payload).await.unwrap();
        client_tx.flush().await.unwrap();

        // Read the echoed response.
        let mut response = vec![0u8; payload.len()];
        client_rx.read_exact(&mut response).await.unwrap();

        // Assert byte-faithful passthrough.
        assert_eq!(&response[..], payload, "shim must forward bytes unmodified");

        // Close the client write side to trigger the forwarder's clean exit.
        drop(client_tx);

        // Wait for forwarder to complete.
        let _ = forward_task.await;
        let _ = server_task.await;
    }
}
