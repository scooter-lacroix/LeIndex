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
fn test_cancel_batch_rejects_pipe_mode_without_touching_worker() {
    let client = EmbeddingClient::new_pipe();
    let error = client
        .cancel_batch(BatchId::new(99), "test")
        .expect_err("pipe mode must reject cancellation");
    assert!(
        error
            .to_string()
            .contains("pipe-mode cancellation is unavailable")
    );
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

    let socket = daemon_socket_path(Some("migraphx"), Some("qwen3-embed-0.6b-dynamic")).unwrap();
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
