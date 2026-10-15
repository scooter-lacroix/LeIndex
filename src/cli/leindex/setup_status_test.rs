use super::*;

fn remote_config() -> LeIndexConfig {
    let mut config = LeIndexConfig::default();
    config.neural.enabled = true;
    config.indexing.embeddings.enable_hybrid = true;
    config.indexing.embeddings.remote_endpoint = Some("http://127.0.0.1:12345/v1".into());
    config.indexing.embeddings.remote_dimension = Some(3);
    config
}

fn response(passed: bool, dimension: Option<usize>) -> SmokeTestResult {
    SmokeTestResult {
        passed,
        skipped: false,
        dimension,
        execution_provider: Some("remote".into()),
        configured_provider_label: None,
        error: None,
        note: None,
    }
}

#[test]
fn test_lexical_off_does_not_probe_or_require_models() {
    let mut config = remote_config();
    config.neural.enabled = false;
    assert_eq!(
        status_with(&config, false, false, false, || panic!(
            "lexical must not probe"
        ))
        .unwrap(),
        Some(true)
    );
}

#[test]
fn test_local_route_delegates_without_remote_probe() {
    let mut config = remote_config();
    config.indexing.embeddings.enable_hybrid = false;
    assert_eq!(
        status_with(&config, true, true, true, || panic!(
            "local must not remote probe"
        ))
        .unwrap(),
        None
    );
}

#[test]
fn test_disabled_build_and_runtime_flags_fail_without_probe() {
    let config = remote_config();
    for (neural, remote, build) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        assert_eq!(
            status_with(&config, neural, remote, build, || panic!(
                "disabled must not probe"
            ))
            .unwrap(),
            Some(false)
        );
    }
}

#[test]
fn test_remote_probe_must_be_nonempty_unskipped_expected_dimension() {
    let config = remote_config();
    assert_eq!(
        status_with(&config, true, true, true, || response(true, Some(3))).unwrap(),
        Some(true)
    );
    for result in [
        response(false, None),
        response(true, Some(0)),
        response(true, Some(2)),
        SmokeTestResult {
            skipped: true,
            ..response(true, Some(3))
        },
    ] {
        assert_eq!(
            status_with(&config, true, true, true, || result).unwrap(),
            Some(false)
        );
    }
}

#[test]
fn test_explicit_endpoint_skips_missing_managed_service() {
    let mut config = remote_config();
    config.indexing.embeddings.remote_service = Some("missing".into());
    assert_eq!(
        status_with(&config, true, true, true, || response(true, Some(3))).unwrap(),
        Some(true)
    );
    config.indexing.embeddings.remote_endpoint = None;
    assert_eq!(
        status_with(&config, true, true, true, || panic!(
            "missing service must not probe"
        ))
        .unwrap(),
        Some(false)
    );
}

#[test]
fn test_endpoint_credentials_and_zero_dimension_rejected_without_probe() {
    let mut config = remote_config();
    config.indexing.embeddings.remote_endpoint = Some("https://user:secret@example.com/v1".into());
    assert!(
        status_with(&config, true, true, true, || panic!(
            "credentials must not probe"
        ))
        .is_err()
    );
    config.indexing.embeddings.remote_endpoint = Some("http://127.0.0.1:12345/v1".into());
    config.indexing.embeddings.remote_dimension = Some(0);
    assert_eq!(
        status_with(&config, true, true, true, || panic!(
            "zero dimension must not probe"
        ))
        .unwrap(),
        Some(false)
    );
}

#[cfg(feature = "remote-embeddings")]
#[test]
fn test_nonstarting_production_probe_against_mock_http() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("mock request missing: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(split) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..split]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= split + 4 + length {
                    break;
                }
            }
            assert!(request.len() < 65536);
        }
        assert!(request.starts_with(b"POST /v1/embeddings "));
        let body = r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0}],"model":"fixture","usage":{"prompt_tokens":2,"total_tokens":2}}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let mut config = remote_config();
    config.indexing.embeddings.remote_endpoint = Some(format!("http://{address}/v1"));
    config.indexing.embeddings.remote_service = Some("must-not-start".into());
    let result = status_with(&config, true, true, true, || {
        super::super::run_remote_smoke_test_inner(
            &config.indexing.embeddings,
            &config.services,
            false,
        )
    });
    server.join().unwrap();
    assert_eq!(result.unwrap(), Some(true));
}
