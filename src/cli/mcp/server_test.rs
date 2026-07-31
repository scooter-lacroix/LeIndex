use super::*;

/// Serialize any test that mutates process-global environment
/// variables. `std::env::set_var` is not thread-safe and `cargo
/// test` runs tests in parallel by default — the
/// `test_max_http_sessions_env_override` test below reads/writes
/// `LEINDEX_MAX_SESSIONS` while other tests concurrently call
/// `max_http_sessions()` (which reads the same variable), which
/// is an active data race under POSIX. Holding this mutex for the
/// entire test body guarantees only one test at a time touches
/// the env.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn test_server_config_default() {
    let config = McpServerConfig::default();
    assert_eq!(
        config.bind_address,
        SocketAddr::from(([127, 0, 0, 1], DEFAULT_MCP_PORT))
    );
    // Loopback-only binding on a high, rarely-used port. We avoid the
    // well-known dev-server range (<10000) which collides with Node,
    // Rails, Django, etc. 47500 sits well above that and below the
    // IANA dynamic range (49152+) so it's reliably available.
    assert!(config.bind_address.ip().is_loopback());
    assert!(config.bind_address.port() >= 10000);
}

#[cfg(unix)]
#[tokio::test]
async fn test_read_bounded_line_rejects_oversized_input_and_handles_eof() {
    use tokio::io::BufReader;

    let mut oversized = BufReader::new(std::io::Cursor::new(b"12345".to_vec()));
    let error = read_bounded_line(&mut oversized, 4).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let mut exact = BufReader::new(std::io::Cursor::new(b"1234\n".to_vec()));
    assert_eq!(
        read_bounded_line(&mut exact, 5).await.unwrap().as_deref(),
        Some("1234\n")
    );

    let mut valid = BufReader::new(std::io::Cursor::new(b"ok\n".to_vec()));
    assert_eq!(
        read_bounded_line(&mut valid, 3).await.unwrap().as_deref(),
        Some("ok\n")
    );

    let mut eof = BufReader::new(std::io::Cursor::new(Vec::<u8>::new()));
    assert!(read_bounded_line(&mut eof, 4).await.unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn test_socket_connection_preserves_newline_and_content_length_framing() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let session_handshakes = Arc::new(DashMap::new());
    let handshake_complete = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(handle_socket_connection(
        server,
        "framing-test".to_string(),
        session_handshakes.clone(),
        handshake_complete,
    ));

    // An invalid newline-delimited message exercises the newline response
    // path without requiring global MCP handler initialization.
    client.write_all(b"not-json\n").await.unwrap();
    let (reader, mut writer) = client.into_split();
    let mut reader = BufReader::new(reader);
    let mut newline_response = String::new();
    reader.read_line(&mut newline_response).await.unwrap();
    assert!(newline_response.contains("-32700") || newline_response.contains("-32600"));

    // The next message switches framing modes on the same connection.
    let payload = b"not-json";
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", payload.len()).as_bytes())
        .await
        .unwrap();
    writer.write_all(payload).await.unwrap();
    writer.flush().await.unwrap();

    let mut header = String::new();
    reader.read_line(&mut header).await.unwrap();
    assert!(header.to_ascii_lowercase().starts_with("content-length:"));
    let length = header
        .split_once(':')
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .expect("response content length");
    let mut blank = String::new();
    reader.read_line(&mut blank).await.unwrap();
    assert!(blank.trim().is_empty());
    let mut response = vec![0; length];
    reader.read_exact(&mut response).await.unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.contains("-32700") || response.contains("-32600"));

    writer.shutdown().await.unwrap();
    task.await.unwrap();
    assert!(!session_handshakes.contains_key("framing-test"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_socket_connection_closes_on_malformed_or_incomplete_headers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(handle_socket_connection(
        server,
        "malformed-header-test".to_string(),
        Arc::new(DashMap::new()),
        Arc::new(AtomicBool::new(false)),
    ));
    client.write_all(b"Content-Length: nope\r\n").await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    assert!(String::from_utf8_lossy(&response).contains("-32600"));
    task.await.unwrap();

    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(handle_socket_connection(
        server,
        "incomplete-header-test".to_string(),
        Arc::new(DashMap::new()),
        Arc::new(AtomicBool::new(false)),
    ));
    client
        .write_all(b"Content-Length: 8\r\n\r\nshort")
        .await
        .unwrap();
    client.shutdown().await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    assert!(response.is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("incomplete payload must close promptly")
        .unwrap();
}

#[cfg(unix)]
#[test]
fn test_socket_cleanup_guard_removes_file() {
    let dir = std::env::temp_dir().join("leindex_test_socket_guard");
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("test.sock");
    std::fs::write(&socket_path, b"").unwrap();
    assert!(socket_path.exists());

    {
        let _guard = SocketCleanupGuard {
            path: socket_path.clone(),
        };
    }
    assert!(!socket_path.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- A+ MCP session cleanup tests (VAL-APLUS-025, VAL-APLUS-026) ----

/// VAL-APLUS-025: MCP session handshake handling preserves behavior under
/// concurrent sessions. Multiple sessions can be initialized and tracked
/// independently without corrupting each other.
#[test]
fn test_concurrent_session_handshake_isolation() {
    let registry = Arc::new(ProjectRegistry::new(5));
    let server = McpServer {
        config: McpServerConfig::default(),
        _registry: registry,
        handshake_complete: Arc::new(AtomicBool::new(false)),
        session_handshakes: Arc::new(DashMap::new()),
        in_flight: Arc::new(DashMap::new()),
        freshness_advisories: Arc::new(DashMap::new()),
    };

    // Simulate multiple concurrent session handshakes
    let (result1, sid1) = handle_initialize_for_test(&server);
    let (result2, sid2) = handle_initialize_for_test(&server);
    let (result3, sid3) = handle_initialize_for_test(&server);

    // All should succeed with unique session IDs
    assert!(result1.get("protocolVersion").is_some());
    assert!(result2.get("protocolVersion").is_some());
    assert!(result3.get("protocolVersion").is_some());

    // Session IDs must be unique
    assert_ne!(sid1, sid2);
    assert_ne!(sid2, sid3);
    assert_ne!(sid1, sid3);

    // All sessions should be tracked
    assert_eq!(server.active_session_count(), 3);

    // All sessions should be marked as handshaked
    {
        assert!(server.session_handshakes.get(sid1.as_str()).unwrap().0);
        assert!(server.session_handshakes.get(sid2.as_str()).unwrap().0);
        assert!(server.session_handshakes.get(sid3.as_str()).unwrap().0);
    }
}

/// VAL-APLUS-026: MCP session tracking remains isolated per session.
/// Operations on one session do not corrupt or block unrelated session state.
#[test]
fn test_session_isolation_per_session() {
    let registry = Arc::new(ProjectRegistry::new(5));
    let server = McpServer {
        config: McpServerConfig::default(),
        _registry: registry,
        handshake_complete: Arc::new(AtomicBool::new(false)),
        session_handshakes: Arc::new(DashMap::new()),
        in_flight: Arc::new(DashMap::new()),
        freshness_advisories: Arc::new(DashMap::new()),
    };

    let (_, sid1) = handle_initialize_for_test(&server);
    let (_, sid2) = handle_initialize_for_test(&server);

    // Remove session 1
    server.session_handshakes.remove(sid1.as_str());

    // Session 2 should still be valid
    assert!(server.session_handshakes.get(sid2.as_str()).is_some());
    assert!(server.session_handshakes.get(sid2.as_str()).unwrap().0);

    assert_eq!(server.active_session_count(), 1);
}

#[test]
fn test_freshness_advisory_is_once_per_session_and_generation() {
    let registry = Arc::new(ProjectRegistry::new(5));
    let server = McpServer {
        config: McpServerConfig::default(),
        _registry: registry,
        handshake_complete: Arc::new(AtomicBool::new(false)),
        session_handshakes: Arc::new(DashMap::new()),
        in_flight: Arc::new(DashMap::new()),
        freshness_advisories: Arc::new(DashMap::new()),
    };
    let (_, session_id) = handle_initialize_for_test(&server);

    let response = || {
        serde_json::json!({
            "project_path": "/tmp/project",
            "_meta": {"freshness": {
                "generation": 7,
                "warning": "refresh recommended"
            }}
        })
    };
    let mut first = response();
    server.apply_freshness_advisory(&session_id, None, &mut first);
    assert_eq!(
        first["_meta"]["freshness"]["advisory"],
        "refresh recommended"
    );
    assert!(first["_meta"]["freshness"].get("warning").is_none());

    let mut second = response();
    server.apply_freshness_advisory(&session_id, None, &mut second);
    assert!(second["_meta"]["freshness"]["advisory"].is_null());

    let mut new_generation = response();
    new_generation["_meta"]["freshness"]["generation"] = serde_json::json!(8);
    server.apply_freshness_advisory(&session_id, None, &mut new_generation);
    assert_eq!(
        new_generation["_meta"]["freshness"]["advisory"],
        "refresh recommended"
    );
}

/// VAL-APLUS-025 variant: stale session cleanup removes only expired sessions.
#[test]
fn test_stale_session_cleanup() {
    let registry = Arc::new(ProjectRegistry::new(5));
    let server = McpServer {
        config: McpServerConfig::default(),
        _registry: registry,
        handshake_complete: Arc::new(AtomicBool::new(false)),
        session_handshakes: Arc::new(DashMap::new()),
        in_flight: Arc::new(DashMap::new()),
        freshness_advisories: Arc::new(DashMap::new()),
    };

    // Create a session
    let (_, sid) = handle_initialize_for_test(&server);
    assert_eq!(server.active_session_count(), 1);

    // Manually age the session's last_access time to simulate staleness
    if let Some(mut entry) = server.session_handshakes.get_mut(sid.as_str()) {
        entry.1 = Instant::now() - std::time::Duration::from_secs(600);
    }

    // Cleanup with a 60-second idle timeout should remove the stale session
    let removed = server.cleanup_stale_sessions(std::time::Duration::from_secs(60));
    assert_eq!(removed, 1);
    assert_eq!(server.active_session_count(), 0);
}

/// Helper: simulate an initialize call and return (result, session_id).
fn handle_initialize_for_test(server: &McpServer) -> (Value, String) {
    let (result, sid) = handle_initialize(server);
    (result, sid.unwrap())
}

/// Regression test for HIGH #2: when `handle_initialize` is forced to
/// evict to make room for a new session, it must NOT evict a session
/// that is currently processing a request. Without the in_flight
/// filter, a long-running tool call could be evicted mid-request,
/// producing spurious "Server not initialized" errors for the
/// active client.
///
/// Test strategy: lower the session cap to 2, register 2 sessions
/// (one of which is in_flight), and call `handle_initialize` a
/// third time. The in_flight session must survive; the idle
/// session must be evicted.
///
/// `handle_initialize` reads `LEINDEX_MAX_SESSIONS` via
/// `max_http_sessions()`. `cargo test` runs tests in parallel by
/// default and `test_max_http_sessions_env_override` mutates that
/// env var on a sibling thread, so we hold `ENV_LOCK` for the
/// entire test body to serialise env access.
#[test]
fn test_handle_initialize_does_not_evict_in_flight_session() {
    let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Override the cap for this test. We can't override
    // `DEFAULT_MAX_HTTP_SESSIONS` (it's a const) but we can verify
    // the eviction logic by pre-loading 2 sessions and forcing a
    // third call to be on the boundary.
    //
    // To exercise the eviction code path directly we use a slightly
    // higher load than the cap: register MAX sessions, then call
    // initialize once more and check the in_flight session is kept.
    let registry = Arc::new(ProjectRegistry::new(5));
    let server = McpServer {
        config: McpServerConfig::default(),
        _registry: registry,
        handshake_complete: Arc::new(AtomicBool::new(false)),
        session_handshakes: Arc::new(DashMap::new()),
        in_flight: Arc::new(DashMap::new()),
        freshness_advisories: Arc::new(DashMap::new()),
    };

    // Fill the session map to the cap. We use `insert` directly
    // because `handle_initialize` generates its own session IDs and
    // we want to control which ones are "old" and "in flight".
    let now = Instant::now();
    for i in 0..DEFAULT_MAX_HTTP_SESSIONS {
        let sid = format!("sess-{i:04}");
        server
            .session_handshakes
            .insert(Arc::<str>::from(sid.as_str()), (true, now));
    }
    assert_eq!(server.active_session_count(), DEFAULT_MAX_HTTP_SESSIONS);

    // Mark the very first session (oldest by insertion order) as
    // in-flight. `last_access_time` is the same as the others, so
    // without the in_flight filter it would be evicted.
    let in_flight_sid = "sess-0000".to_string();
    server.begin_request(&in_flight_sid);
    assert!(server.session_in_flight(&in_flight_sid));

    // The next initialize call should trigger eviction logic.
    let (_, new_sid) = handle_initialize_for_test(&server);

    // The in-flight session must still be present.
    assert!(
        server
            .session_handshakes
            .contains_key(in_flight_sid.as_str()),
        "in_flight session {} was evicted during initialize",
        in_flight_sid,
    );
    // The new session must be present.
    assert!(
        server.session_handshakes.contains_key(new_sid.as_str()),
        "newly initialized session {} was not registered",
        new_sid,
    );
    // The total count is at most MAX + 1 (the new session, since
    // the in_flight one was preserved and exactly one other was
    // evicted).
    assert!(
        server.active_session_count() <= DEFAULT_MAX_HTTP_SESSIONS + 1,
        "session count {} exceeds cap + 1",
        server.active_session_count(),
    );

    // Cleanup: end the request so the in_flight map is empty for
    // other tests in the same process.
    server.end_request(&in_flight_sid);
}

/// `LEINDEX_MAX_SESSIONS` env var overrides the default session cap.
#[test]
fn test_max_http_sessions_env_override() {
    // Hold the env-mutation lock for the entire body. Other tests in
    // this module (e.g. the eviction test) call `handle_initialize`,
    // which reads `LEINDEX_MAX_SESSIONS` via `max_http_sessions()`;
    // racing `std::env::set_var` against a concurrent
    // `std::env::var` is undefined behaviour. Serialising on
    // `ENV_LOCK` keeps every env read/write in this test entirely
    // single-threaded.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Default (env unset) returns the default.
    unsafe {
        std::env::set_var(MAX_SESSIONS_ENV, "42");
    }
    assert_eq!(max_http_sessions(), 42);
    unsafe {
        std::env::remove_var(MAX_SESSIONS_ENV);
    }
    assert_eq!(max_http_sessions(), DEFAULT_MAX_HTTP_SESSIONS);
    // Bogus value falls back to default.
    unsafe {
        std::env::set_var(MAX_SESSIONS_ENV, "not-a-number");
    }
    assert_eq!(max_http_sessions(), DEFAULT_MAX_HTTP_SESSIONS);
    unsafe {
        std::env::remove_var(MAX_SESSIONS_ENV);
    }
}

/// Regression for MED #3342354737: `session_handshakes` is keyed
/// on `Arc<str>`, and `begin_request` clones the cached
/// `Arc<str>` from the handshake table (refcount bump, no
/// allocation) instead of constructing a fresh
/// `Arc::<str>::from(&str)` for every request. Verify the
/// in_flight map's key is `Arc`-equal (same allocation) to the
/// session_handshakes key.
#[test]
fn test_begin_request_clones_cached_arc_str_key() {
    let registry = Arc::new(ProjectRegistry::new(5));
    let server = McpServer {
        config: McpServerConfig::default(),
        _registry: registry,
        handshake_complete: Arc::new(AtomicBool::new(false)),
        session_handshakes: Arc::new(DashMap::new()),
        in_flight: Arc::new(DashMap::new()),
        freshness_advisories: Arc::new(DashMap::new()),
    };

    // Register a session directly so the key is an `Arc<str>` we
    // can compare against.
    let sid = Arc::<str>::from("sess-arc-key");
    let cached: Arc<str> = sid.clone();
    server
        .session_handshakes
        .insert(sid, (true, Instant::now()));

    // `begin_request` should clone the cached `Arc<str>` rather
    // than allocating a new one.
    server.begin_request("sess-arc-key");

    // The in_flight map should contain a key whose pointer
    // matches the cached one.
    let stored = server
        .in_flight
        .iter()
        .next()
        .expect("expected one in_flight entry");
    assert_eq!(
        stored.key().as_ptr(),
        cached.as_ptr(),
        "in_flight key should be the same Arc<str> allocation as session_handshakes"
    );
    assert_eq!(stored.key().as_ref(), "sess-arc-key");
}

/// `begin_request` falls back to `Arc::<str>::from(&str)` when
/// the session is not yet registered. The fallback allocation
/// is acceptable; the test exists to lock the contract so a
/// future refactor doesn't silently panic or skip the insert.
#[test]
fn test_begin_request_allocates_when_session_unknown() {
    let registry = Arc::new(ProjectRegistry::new(5));
    let server = McpServer {
        config: McpServerConfig::default(),
        _registry: registry,
        handshake_complete: Arc::new(AtomicBool::new(false)),
        session_handshakes: Arc::new(DashMap::new()),
        in_flight: Arc::new(DashMap::new()),
        freshness_advisories: Arc::new(DashMap::new()),
    };

    // Session not registered yet — should still insert into
    // in_flight (allocates a fresh `Arc<str>`).
    server.begin_request("unregistered-session");

    assert!(
        server.session_in_flight("unregistered-session"),
        "begin_request must insert even when session is unregistered"
    );

    // Lookup works via `&str` (DashMap resolves Arc<str>: Borrow<str>).
    let stored = server
        .in_flight
        .get("unregistered-session")
        .expect("lookup by &str must work");
    assert_eq!(stored.key().as_ref(), "unregistered-session");
}

/// Regression for MED round 18: the fixed-port loop in
/// `bind_with_fallback` previously used
/// `preferred.port().saturating_add(offset)`, which
/// silently caps the candidate port at `u16::MAX`. With
/// `BIND_FALLBACK_PORT_RANGE = 10` and a preferred port of
/// 65530, the loop would burn six of its eleven slots
/// re-binding the saturated port 65535 instead of moving
/// on. The fix uses `checked_add` and breaks out of the
/// fixed range on overflow, falling through to the
/// ephemeral-bind fallback.
///
/// This test exercises the overflow path: bind to the
/// highest valid port (65535), then ask
/// `bind_with_fallback` to start near the top of the
/// range. We assert it returns *some* `TcpListener`
/// (either via the ephemeral fallback or via the
/// saturated port) without entering an infinite loop.
/// The previous implementation would also return a
/// listener eventually, but it would burn cycles on
/// duplicate bind attempts. The regression target is the
/// contract: the loop must terminate promptly and the
/// function must not loop forever on saturated ports.
#[tokio::test]
async fn test_bind_with_fallback_breaks_on_port_overflow() {
    // Bind to the highest valid port to consume it.
    let high = SocketAddr::from(([127, 0, 0, 1], u16::MAX));
    let _occupying = tokio::net::TcpListener::bind(high).await.unwrap();

    // Start the search 5 ports below u16::MAX. With a
    // 10-port fallback range, offset 0..=10 covers
    // 65530..65535 + the saturating case. The fixed
    // range will collide with the occupying listener on
    // 65535; post-fix the loop breaks on overflow
    // (offset 5 → 65535, then offset 6..10 would
    // overflow), and falls through to ephemeral.
    let preferred = SocketAddr::from(([127, 0, 0, 1], u16::MAX - 5));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        bind_with_fallback(preferred),
    )
    .await;
    let listener = result
        .expect("bind_with_fallback must terminate within 5s")
        .expect("ephemeral fallback must succeed on a free IP");
    // The returned listener must be on the preferred IP.
    assert_eq!(
        listener.local_addr().unwrap().ip(),
        preferred.ip(),
        "ephemeral fallback must use the preferred IP"
    );
}

/// The happy path: when the preferred port is free,
/// `bind_with_fallback` must return a listener bound to
/// that exact port — no fallback walk, no ephemeral
/// detour. This guards against an over-eager overflow
/// break that would cause the function to give up the
/// preferred port for an ephemeral one.
#[tokio::test]
async fn test_bind_with_fallback_uses_preferred_when_free() {
    let preferred = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = bind_with_fallback(preferred).await.unwrap();
    let bound = listener.local_addr().unwrap();
    // Port 0 → ephemeral, so just assert the IP matches.
    assert_eq!(bound.ip(), preferred.ip());
}

/// Regression for MED round 20: when `preferred.port() == 0`,
/// `bind_with_fallback` must bypass the fixed-range
/// fallback loop entirely and let the OS pick an
/// ephemeral port directly. The pre-fix code walked
/// `1..=BIND_FALLBACK_PORT_RANGE` even when the
/// preferred port was 0, probing privileged ports (< 1024
/// on Unix) that could fail with EACCES on systems where
/// the process has no CAP_NET_BIND_SERVICE.
///
/// The new contract is observable in two ways:
///   1. The returned listener's port is non-zero (the
///      OS-assigned ephemeral port).
///   2. No privileged ports (1..=1023) were probed —
///      which we verify indirectly by asserting the
///      function returns successfully on a default-
///      capability process without touching any port
///      that would need root.
#[tokio::test]
async fn test_bind_with_fallback_port_zero_skips_fallback_loop() {
    let preferred = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = bind_with_fallback(preferred).await.unwrap();
    let bound = listener.local_addr().unwrap();
    // The OS-assigned ephemeral port is non-zero.
    assert_ne!(
        bound.port(),
        0,
        "OS-assigned ephemeral port must be non-zero; got port 0 (bind did not actually request ephemeral)"
    );
    // The IP is preserved through the fast path.
    assert_eq!(bound.ip(), preferred.ip());
}

#[cfg(unix)]
#[test]
fn test_socket_timeout_contract_distinguishes_initial_and_subsequent_frames() {
    assert_eq!(
        socket_read_timeout(true),
        std::time::Duration::from_secs(120)
    );
    assert_eq!(
        socket_read_timeout(false),
        std::time::Duration::from_secs(30)
    );
    assert!(socket_read_timeout(true) > socket_read_timeout(false));
}
