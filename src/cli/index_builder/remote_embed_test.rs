//! B1 remote-embeddings wiring tests (spec 2026-10-09).
//!
//! All gated behind `remote-embeddings` so default builds skip them entirely.

use super::*;

// ============================================================================
// Config parsing ([indexing.embeddings])
// ============================================================================

#[test]
fn test_embedding_config_parses_remote_keys() {
    let toml = r#"
[indexing.embeddings]
enable_hybrid = true
remote_endpoint = "http://127.0.0.1:8765/v1"
remote_api_key = "test-key"
remote_model = "embeddinggemma-2"
remote_dimension = 768
"#;
    let config: crate::config::LeIndexConfig = toml::from_str(toml).expect("parse user config");
    let embeddings = &config.indexing.embeddings;
    assert!(embeddings.remote_endpoint_configured());
    assert_eq!(
        embeddings.remote_endpoint.as_deref(),
        Some("http://127.0.0.1:8765/v1")
    );
    assert_eq!(embeddings.remote_model.as_deref(), Some("embeddinggemma-2"));
    assert_eq!(embeddings.remote_dimension, Some(768));
}

#[test]
fn test_embedding_config_defaults_are_inert() {
    let config: crate::config::LeIndexConfig = toml::from_str("").expect("empty config parses");
    let embeddings = &config.indexing.embeddings;
    assert!(!embeddings.remote_endpoint_configured());
    assert_eq!(embeddings.remote_endpoint, None);
    assert_eq!(embeddings.remote_dimension, None);
}

#[test]
fn test_embedding_config_disabled_flag_stays_inert() {
    let toml = r#"
[indexing.embeddings]
enable_hybrid = false
remote_endpoint = "http://127.0.0.1:8765/v1"
"#;
    let config: crate::config::LeIndexConfig = toml::from_str(toml).expect("parse");
    // Endpoint set but master switch off: must NOT be treated as configured.
    assert!(!config.indexing.embeddings.remote_endpoint_configured());
}

// ============================================================================
// Construction ladder (remote → hybrid_local → tfidf_only)
// ============================================================================

fn corpus_tfidf() -> TfIdfEmbedder {
    TfIdfEmbedder::build(&[(
        "a.rs".to_string(),
        "fn sample_function() -> u32 { 42 }".to_string(),
    )])
}

#[test]
fn test_hybrid_remote_construction_custom_provider() {
    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: "http://127.0.0.1:1/v1".to_string(), // nothing listens here
        },
        model: Some("test-model".to_string()),
        ..RemoteEmbeddingConfig::default()
    };
    let embedder = HybridEmbedder::hybrid_remote(corpus_tfidf(), remote_cfg, None);
    // Construction is config-only: it must succeed even with no live server.
    let embedder = embedder.expect("hybrid_remote construction");
    assert!(embedder.has_neural());
    assert!(matches!(embedder, HybridEmbedder::HybridRemote { .. }));
    assert_eq!(embedder.neural_status(), "ready");
}

#[test]
fn test_tfidf_only_ladder_terminal() {
    let embedder = HybridEmbedder::tfidf_only(corpus_tfidf());
    assert!(!embedder.has_neural());
    assert_eq!(embedder.neural_status(), "absent");
}

#[test]
fn test_try_remote_neural_embedder_ladder_arms() {
    use crate::config::EmbeddingConfig;
    // Endpoint unset: helper returns None without constructing anything.
    assert!(
        try_remote_neural_embedder(corpus_tfidf(), &EmbeddingConfig::default()).is_none(),
        "no endpoint configured must yield None"
    );
    // Endpoint configured: config-only construction succeeds and is remote.
    let cfg = EmbeddingConfig {
        enable_hybrid: true,
        remote_endpoint: Some("http://127.0.0.1:1/v1".to_string()),
        ..EmbeddingConfig::default()
    };
    let embedder =
        try_remote_neural_embedder(corpus_tfidf(), &cfg).expect("configured endpoint builds");
    assert!(matches!(embedder, HybridEmbedder::HybridRemote { .. }));
}

// ============================================================================
// Mock /v1/embeddings server (std TcpListener + serde_json body parsing)
// ============================================================================

/// Serve exactly one OpenAI-compatible /v1/embeddings response on an ephemeral
/// port; the closure chooses the vector length per request. Extra requests are
/// not answered (the client sees a reset → fallback path in the caller).
fn spawn_mock_embeddings_server(
    dim_for_request: impl Fn(usize) -> usize + Send + 'static,
) -> String {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener
        .local_addr()
        .expect("mock server local addr")
        .to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 65536];
            let read = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]).to_string();
            let body_start = request
                .find("\r\n\r\n")
                .map(|i| i + 4)
                .unwrap_or(request.len());
            let payload: serde_json::Value =
                serde_json::from_str(request[body_start..].trim()).unwrap_or(serde_json::json!({}));
            let input_count = payload["input"].as_array().map(|a| a.len()).unwrap_or(1);
            let dim = dim_for_request(input_count);
            let data: Vec<String> = (0..input_count)
                .map(|i| {
                    let vector: Vec<String> = (0..dim)
                        .map(|j| (0.5 + i as f32 * 0.001 + j as f32 * 0.0001).to_string())
                        .collect();
                    format!(
                        r#"{{"object":"embedding","index":{i},"embedding":[{}]}}"#,
                        vector.join(",")
                    )
                })
                .collect();
            let body = format!(r#"{{"object":"list","data":[{}]}}"#, data.join(","));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/v1")
}

/// Run one future to completion on a scratch current-thread runtime (mirrors
/// the production bridge; the tests are sync fns).
fn block_on_test<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(fut)
}

#[test]
fn test_remote_embed_success_measures_dimension() {
    // Server returns 768-dim vectors (NOT the 1536 bootstrap default).
    let endpoint = spawn_mock_embeddings_server(|_| 768);
    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: endpoint.clone(),
        },
        model: Some("mock".to_string()),
        ..RemoteEmbeddingConfig::default()
    };
    let provider =
        crate::search::onnx::GenericRemoteProvider::from_config(remote_cfg).expect("ctor");
    // Before any response: bootstrap default (no panic, no enforcement).
    assert_eq!(provider.dimension(), 1536);
    let embedding = block_on_test(provider.embed("hello world")).expect("embed");
    // Runtime-true dimension: measured from the response.
    assert_eq!(embedding.len(), 768);
    assert_eq!(provider.dimension(), 768);
    // Identity must now carry the measured dimension (Engram keying).
    assert!(
        provider.identity().ends_with("|768"),
        "{}",
        provider.identity()
    );
}

#[test]
fn test_remote_embed_dimension_override_wins_over_bootstrap() {
    let endpoint = spawn_mock_embeddings_server(|_| 512);
    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: endpoint.clone(),
        },
        remote_dimension: Some(512),
        ..RemoteEmbeddingConfig::default()
    };
    let provider =
        crate::search::onnx::GenericRemoteProvider::from_config(remote_cfg).expect("ctor");
    assert_eq!(provider.dimension(), 512);
    let embedding = block_on_test(provider.embed("hello")).expect("embed");
    assert_eq!(embedding.len(), 512);
}

#[test]
fn test_remote_embed_dimension_mismatch_rejected_when_known() {
    // Server serves 1536; the override settles the expectation at 768 → the
    // response must be rejected (InvalidDimension), never accepted as ragged.
    let endpoint = spawn_mock_embeddings_server(|_| 1536);
    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: endpoint.clone(),
        },
        remote_dimension: Some(768),
        ..RemoteEmbeddingConfig::default()
    };
    let provider =
        crate::search::onnx::GenericRemoteProvider::from_config(remote_cfg).expect("ctor");
    let err = block_on_test(provider.embed("drift")).unwrap_err();
    assert!(matches!(
        err,
        crate::search::onnx::RemoteEmbeddingError::InvalidDimension {
            expected: 768,
            got: 1536
        }
    ));
}

#[test]
fn test_remote_batch_success() {
    // Batch of 2 → server returns 2 vectors at the overridden dim.
    let endpoint = spawn_mock_embeddings_server(|_| 512);
    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: endpoint.clone(),
        },
        remote_dimension: Some(512),
        ..RemoteEmbeddingConfig::default()
    };
    let provider =
        crate::search::onnx::GenericRemoteProvider::from_config(remote_cfg).expect("ctor");
    let embeddings = block_on_test(provider.embed_batch(vec!["one", "two"])).expect("batch");
    assert_eq!(embeddings.len(), 2);
    assert!(embeddings.iter().all(|v| v.len() == 512));
}

#[test]
fn test_blocking_arm_degrades_to_none_on_connection_refused() {
    // Port 1 on loopback: nothing listens; connect fails fast.
    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: "http://127.0.0.1:1/v1".to_string(),
        },
        timeout_secs: 2,
        ..RemoteEmbeddingConfig::default()
    };
    let embedder = HybridEmbedder::hybrid_remote(corpus_tfidf(), remote_cfg, None)
        .expect("construction must not require a live server");
    // VAL-CPHASE-018: batch API failure degrades to None (TF-IDF fallback),
    // never a panic, never a hard index failure.
    let results = embedder.embed_neural_batch_blocking(&["alpha", "beta"]);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_none()));
}

#[test]
fn test_blocking_single_arm_surfaces_error_on_connection_refused() {
    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: "http://127.0.0.1:1/v1".to_string(),
        },
        timeout_secs: 2,
        ..RemoteEmbeddingConfig::default()
    };
    let embedder = HybridEmbedder::hybrid_remote(corpus_tfidf(), remote_cfg, None).expect("ctor");
    // Query-side path: Some(Err(..)) → caller falls back to TF-IDF.
    let result = embedder.embed_neural_blocking("query text");
    assert!(matches!(result, Some(Err(_))));
}
