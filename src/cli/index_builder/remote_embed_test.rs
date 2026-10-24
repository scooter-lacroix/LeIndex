//! B1 remote-embeddings wiring tests (spec 2026-10-09).
//!
//! All gated behind `remote-embeddings` so default builds skip them entirely.

#[cfg(feature = "remote-embeddings")]
use crate::search::onnx::RemoteEmbeddingConfig;
#[cfg(feature = "remote-embeddings")]
use crate::search::onnx::remote::RemoteEmbeddingProvider;

use super::*;

// ============================================================================
// Config parsing ([indexing.embeddings])
// ============================================================================

#[test]
fn test_embedding_config_parses_remote_keys() {
    let toml = r#"
[indexing.embeddings]
enable_hybrid = true
remote_endpoint = "http://127.0.0.1:8766/v1"
remote_api_key = "test-key"
remote_model = "embeddinggemma-2"
remote_dimension = 768
"#;
    let config: crate::config::LeIndexConfig = toml::from_str(toml).expect("parse user config");
    let embeddings = &config.indexing.embeddings;
    assert!(embeddings.remote_endpoint_configured());
    assert_eq!(
        embeddings.remote_endpoint.as_deref(),
        Some("http://127.0.0.1:8766/v1")
    );
    assert_eq!(embeddings.remote_model.as_deref(), Some("embeddinggemma-2"));
    assert_eq!(embeddings.remote_dimension, Some(768));
}

#[test]
fn test_embedding_config_defaults_are_inert() {
    let config: crate::config::LeIndexConfig = toml::from_str("").expect("empty config parses");
    let embeddings = &config.indexing.embeddings;
    assert!(!embeddings.remote_endpoint_configured());
    assert!(!embeddings.enable_hybrid);
    assert_eq!(embeddings.remote_provider, "custom");
    assert_eq!(embeddings.remote_endpoint, None);
    assert_eq!(embeddings.remote_dimension, None);

    let partial_indexing: crate::config::LeIndexConfig =
        toml::from_str("[indexing]\nbatch_size = 100\n").expect("partial indexing config parses");
    assert!(!partial_indexing.indexing.embeddings.enable_hybrid);
    assert_eq!(
        partial_indexing.indexing.embeddings.remote_provider,
        "custom"
    );

    let partial_embeddings: crate::config::LeIndexConfig =
        toml::from_str("[indexing.embeddings]\nremote_model = \"legacy-model\"\n")
            .expect("partial embeddings config parses");
    assert!(!partial_embeddings.indexing.embeddings.enable_hybrid);
    assert_eq!(
        partial_embeddings.indexing.embeddings.remote_provider,
        "custom"
    );
    assert_eq!(
        partial_embeddings
            .indexing
            .embeddings
            .remote_model
            .as_deref(),
        Some("legacy-model")
    );

    let fresh = crate::config::EmbeddingConfig::default();
    assert!(fresh.enable_hybrid);
    assert_eq!(fresh.remote_preset_label(), "embeddinggemma-2");
    assert_eq!(
        fresh
            .resolved_remote()
            .expect("fresh preset resolves")
            .remote_dimension,
        Some(768)
    );
}

#[test]
fn test_embedding_config_disabled_flag_stays_inert() {
    let toml = r#"
[indexing.embeddings]
enable_hybrid = false
remote_endpoint = "http://127.0.0.1:8766/v1"
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
fn test_oversized_input_rejection_is_recognized() {
    use crate::search::onnx::remote::is_oversized_input_rejection;
    // llama-server's exact wording for an input over `-ub`.
    assert!(is_oversized_input_rejection(
        500,
        "input (21846 tokens) is too large to process. increase the physical batch size"
    ));
    // Case-insensitive: the server's casing is not a contract.
    assert!(is_oversized_input_rejection(
        500,
        "Input (999) is TOO LARGE TO PROCESS"
    ));
    // An unclassifiable 500 is an outage, NOT a document verdict. This is the
    // distinction that decides whether the caller's failure budget is spent.
    assert!(!is_oversized_input_rejection(500, "internal server error"));
    assert!(!is_oversized_input_rejection(503, "too large to process"));
    assert!(!is_oversized_input_rejection(400, "bad request"));
}

/// A batch refused entirely because of document SIZE must be handled, not Err.
///
/// Returning `Err` here is what let oversized inputs alone burn the caller's
/// sustained-failure budget and freeze the neural layer: every size rejection
/// looked like an endpoint outage. The batch must come back `Ok` with empty
/// rows, so the generation stays alive and later documents still get vectors.
#[test]
fn test_all_oversized_documents_do_not_count_as_an_outage() {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_count = request_count.clone();
    let endpoint = spawn_mock_embeddings_server_scripted(
        |_| 8,
        move |_| {
            server_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // The mock answers 500 with llama-server's oversized-input wording.
            500
        },
        None,
        // One send for the whole batch, then one per half.
        3,
    );
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        remote_dimension: Some(8),
        // No re-sends, so each document reaches exactly one terminal verdict
        // and the request count below is a direct read of the code paths.
        max_retries: 0,
        ..RemoteEmbeddingConfig::default()
    })
    .expect("remote provider");

    let rows = block_on_test(provider.embed_batch_optional(vec!["big one", "big two"])).expect(
        "documents refused individually for size must not fail the batch: that would \
             spend the sustained-failure budget on inputs the server will never accept",
    );
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(Option::is_none),
        "a rejected document carries no vector, and none may be fabricated"
    );
    // One request for the whole batch, then one per half: a size rejection is
    // narrowable, so it is bisected like any other, and each leaf ends in a
    // skip. The point of this assertion is that the leaves did NOT become
    // whole-batch errors, not that the request count is minimal.
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn test_tfidf_only_ladder_terminal() {
    let embedder = HybridEmbedder::tfidf_only(corpus_tfidf());
    assert!(!embedder.has_neural());
    assert_eq!(embedder.neural_status(), "absent");
}

/// An endpoint LeIndex does not own must never be "healed".
///
/// The restart escalation is only safe because a managed service is a process
/// this machine launched and can legitimately stop/start. Treating a
/// third-party endpoint as restartable would mean either shelling out to
/// nothing useful or blocking the index on a restart that can never help; so
/// `try_revive_endpoint` must decline and let the freeze/fallback policy run.
#[test]
fn test_unmanaged_endpoint_declines_to_restart() {
    // Custom endpoint with no `service_name`: nothing local to restart.
    let unmanaged =
        crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
            provider: crate::search::onnx::RemoteProvider::Custom {
                endpoint: "http://127.0.0.1:1/v1".to_string(),
            },
            ..RemoteEmbeddingConfig::default()
        })
        .expect("custom provider");
    assert!(
        !unmanaged.try_revive_endpoint(),
        "an endpoint with no service_name must never attempt a restart"
    );

    // Non-custom providers have no custom provider behind `as_custom` at all.
    let third_party = crate::search::onnx::GenericRemoteProvider::from_config(
        RemoteEmbeddingConfig::openai("test-key".to_string(), None),
    )
    .expect("openai provider");
    assert!(
        !third_party.try_revive_endpoint(),
        "a third-party provider must never attempt a restart"
    );
}

#[test]
fn test_try_remote_neural_embedder_ladder_arms() {
    use crate::config::EmbeddingConfig;
    let _guard = crate::feature_flags::lock_flag_tests();
    let _reset = crate::feature_flags::FlagOverrideReset;
    // Fresh construction selects the preset; it does not require a live server.
    let fresh = try_remote_neural_embedder(corpus_tfidf(), &EmbeddingConfig::default())
        .expect("fresh preset constructs remote provider without a server");
    assert!(matches!(fresh, HybridEmbedder::HybridRemote { .. }));
    assert_eq!(fresh.neural_dimension(), Some(768));

    // A parsed existing/partial config is legacy and remains inert.
    let legacy: EmbeddingConfig = toml::from_str("").expect("legacy config parses");
    assert!(
        try_remote_neural_embedder(corpus_tfidf(), &legacy).is_none(),
        "missing endpoint and legacy provider must yield None"
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

#[test]
fn test_remote_kill_switch_flag_off_skips_remote_despite_config() {
    use crate::config::EmbeddingConfig;
    let _guard = crate::feature_flags::lock_flag_tests();
    let _reset = crate::feature_flags::FlagOverrideReset;
    let cfg = EmbeddingConfig {
        enable_hybrid: true,
        remote_endpoint: Some("http://127.0.0.1:1/v1".to_string()),
        ..EmbeddingConfig::default()
    };
    crate::feature_flags::set_flag_override_for_test(
        crate::feature_flags::FeatureFlag::RemoteEmbeddings,
        false,
    );
    assert!(
        try_remote_neural_embedder(corpus_tfidf(), &cfg).is_none(),
        "flag off + remote config present must not construct HybridRemote"
    );
    assert!(
        try_remote_neural_embedder(corpus_tfidf(), &EmbeddingConfig::default()).is_none(),
        "flag off must also disable the fresh preset"
    );
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
    spawn_mock_embeddings_server_with_capture(dim_for_request, None)
}

fn spawn_mock_embeddings_server_with_capture(
    dim_for_request: impl Fn(usize) -> usize + Send + 'static,
    captured_request: Option<std::sync::mpsc::Sender<serde_json::Value>>,
) -> String {
    spawn_mock_embeddings_server_responses(dim_for_request, captured_request, 1)
}

pub(crate) fn spawn_mock_embeddings_server_responses(
    dim_for_request: impl Fn(usize) -> usize + Send + 'static,
    captured_request: Option<std::sync::mpsc::Sender<serde_json::Value>>,
    responses: usize,
) -> String {
    spawn_mock_embeddings_server_scripted(dim_for_request, |_| 200, captured_request, responses)
}

fn spawn_mock_embeddings_server_scripted(
    dim_for_request: impl Fn(usize) -> usize + Send + 'static,
    status_for_request: impl Fn(&serde_json::Value) -> u16 + Send + 'static,
    captured_request: Option<std::sync::mpsc::Sender<serde_json::Value>>,
    responses: usize,
) -> String {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener
        .local_addr()
        .expect("mock server local addr")
        .to_string();
    std::thread::spawn(move || {
        for _ in 0..responses {
            let (mut stream, _) = listener.accept().expect("mock request");
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let payload: serde_json::Value = serde_json::from_slice(
                &request[header_end..header_end + content_length.min(request.len() - header_end)],
            )
            .unwrap_or(serde_json::json!({}));
            if let Some(sender) = captured_request.as_ref() {
                let _ = sender.send(payload.clone());
            }
            let status = status_for_request(&payload);
            if status == 0 {
                continue;
            }
            if status != 200 {
                let reason = match status {
                    400 => "Bad Request",
                    413 => "Payload Too Large",
                    422 => "Unprocessable Entity",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "Mock Error",
                };
                // The body matters: the provider classifies a 500 by whether it
                // names a per-input token limit, so an empty-bodied mock would
                // make every 500 look like an outage and hide the distinction
                // the classification exists to draw. 500 carries llama-server's
                // real wording; every other status carries an unclassifiable
                // body (a test that wants an *outage* 500 must not use 500).
                let body = match status {
                    500 => "input (32768 tokens) is too large to process. \
                            increase the physical batch size"
                        .to_string(),
                    _ => format!("mock error status {status}"),
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("mock status response");
                continue;
            }
            let input_count = payload["input"].as_array().map(|a| a.len()).unwrap_or(1);
            let dim = dim_for_request(input_count);
            if dim == 0 {
                stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").expect("mock outage response");
                continue;
            }
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
fn test_generation_remote_death_after_success_persists_only_remote_space() {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_count = request_count.clone();
    let endpoint = spawn_mock_embeddings_server_responses(
        move |_| {
            match server_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                // Request 0 seeds the remote space with a real 768-dim vector.
                0 => 768,
                // The outage: exactly the budgeted failures answer with an
                // empty vector, which the provider rejects as unusable.
                n if n <= MAX_CONSECUTIVE_REMOTE_FAILURES => 0,
                // ...and then the endpoint is healthy again, so a NEW
                // generation must be able to use it.
                _ => 768,
            }
        },
        None,
        // One priming request, one request per budgeted failure, and one more
        // for the fresh generation that retries after the endpoint recovers.
        MAX_CONSECUTIVE_REMOTE_FAILURES + 2,
    );
    let remote_config = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        remote_dimension: Some(768),
        model: Some("embeddinggemma-2".to_string()),
        // This test is about the generation's liveness policy, not the
        // transport's: no re-sends, so each batch that fails is exactly one
        // whole-batch failure and the budget can be counted directly.
        max_retries: 0,
        ..RemoteEmbeddingConfig::default()
    };
    let generation = GenerationNeuralEmbedder::new(
        HybridEmbedder::hybrid_remote(corpus_tfidf(), remote_config.clone(), None).expect("remote"),
    );
    // Batch shapes mirror the production flush: the first request carries the
    // whole batch (so rows land), the next one survives the outage.
    let landed = |ids: &[&str], texts: &[&str]| -> Vec<(String, Vec<f32>)> {
        let owned: Vec<String> = texts.iter().map(|text| text.to_string()).collect();
        let titles = vec![None; owned.len()];
        generation
            .embed_neural_batch_blocking_titled(&owned, titles)
            .into_iter()
            .zip(ids)
            .filter_map(|(row, node_id)| {
                row.filter(|vector| !vector.is_empty())
                    .map(|vector| ((*node_id).to_string(), vector))
            })
            .collect()
    };
    let mut rows = landed(&["a", "b"], &["first document", "second document"]);
    assert_eq!(
        rows.len(),
        2,
        "remote successes must land before the outage"
    );

    // The endpoint is gone. The generation must absorb the first two whole-batch
    // failures, then freeze — and it must NEVER switch to 1024-dimensional Qwen
    // once remote rows have landed, because mixing spaces is worse than a
    // partial neural layer.
    for attempt in 1..MAX_CONSECUTIVE_REMOTE_FAILURES {
        rows.extend(landed(&["x"], &["outage"]));
        assert!(
            !generation.document_embedding_stopped(),
            "whole-batch failure {attempt} of {MAX_CONSECUTIVE_REMOTE_FAILURES} must not freeze"
        );
    }
    rows.extend(landed(&["c"], &["outage"]));
    assert!(
        generation.document_embedding_stopped(),
        "the budget must be finite: sustained failure has to end the generation"
    );
    // One priming request plus one request per failed batch: max_retries is 0,
    // so this counts the budget exactly. A dead endpoint must not be re-probed
    // more than the budget allows.
    let spent = request_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        spent,
        MAX_CONSECUTIVE_REMOTE_FAILURES + 1,
        "the generation must spend exactly one request per budgeted failure"
    );

    // A frozen generation spends nothing further.
    rows.extend(landed(&["d"], &["remainder"]));
    assert_eq!(
        request_count.load(std::sync::atomic::Ordering::SeqCst),
        spent,
        "remaining batches must not retry a frozen generation"
    );
    assert!(matches!(
        generation.into_embedder(),
        HybridEmbedder::HybridRemote { .. }
    ));
    assert_eq!(rows.len(), 2, "only the pre-outage rows may persist");

    let dir = tempfile::tempdir().expect("mmap fixture");
    let path = dir.path().join("neural_embeddings.bin");
    crate::search::vector::write_mmap_embeddings(&path, &rows).expect("persist generation");
    let mmap = crate::search::vector::MmapEmbeddingIndex::open(&path).expect("reopen generation");
    let stored = mmap.entries().expect("stored rows");
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|(_, vector)| vector.len() == 768));
    assert!(!stored.iter().any(|(_, vector)| vector.len() == 1024));

    // A new generation gets a fresh decision and retries remote successfully.
    let next = GenerationNeuralEmbedder::new(
        HybridEmbedder::hybrid_remote(corpus_tfidf(), remote_config, None).expect("next remote"),
    );
    let next_rows = next.embed_neural_batch_blocking(&["retry"]);
    assert_eq!(next_rows[0].as_ref().map(Vec::len), Some(768));
}

#[test]
fn test_remote_batch_bisects_one_bad_input_without_freezing() {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_count = request_count.clone();
    let endpoint = spawn_mock_embeddings_server_scripted(
        |_| 8,
        move |payload| {
            server_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let inputs = payload["input"].as_array().expect("input array");
            if inputs.iter().any(|input| {
                input
                    .as_str()
                    .is_some_and(|text| text.contains("BAD_INPUT"))
            }) {
                400
            } else {
                200
            }
        },
        None,
        6,
    );
    let generation = GenerationNeuralEmbedder::new(
        HybridEmbedder::hybrid_remote(
            corpus_tfidf(),
            RemoteEmbeddingConfig {
                provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
                remote_dimension: Some(8),
                ..RemoteEmbeddingConfig::default()
            },
            None,
        )
        .expect("remote provider"),
    );

    let batch = ["good 0", "good 1", "BAD_INPUT", "good 3"];
    let rows = generation.embed_neural_batch_blocking(&batch);
    assert_eq!(rows.len(), 4);
    assert!(rows[0].is_some());
    assert!(rows[1].is_some());
    assert!(rows[2].is_none());
    assert!(rows[3].is_some());
    assert!(!generation.document_embedding_stopped());

    let next = generation.embed_neural_batch_blocking(&["good after partial"]);
    assert!(
        next[0].is_some(),
        "partial success must not freeze the generation"
    );
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 6);
}

#[test]
fn test_remote_batch_all_bad_freezes_only_after_sustained_failure() {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_count = request_count.clone();
    let batch_sizes = std::sync::Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let sizes = batch_sizes.clone();
    let endpoint = spawn_mock_embeddings_server_scripted(
        |_| 8,
        move |payload| {
            server_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let inputs = payload["input"].as_array().expect("input array");
            sizes.lock().expect("batch sizes").push(inputs.len());
            if inputs.iter().any(|input| input.as_str() == Some("seed")) {
                200
            } else {
                400
            }
        },
        None,
        20,
    );
    let generation = GenerationNeuralEmbedder::new(
        HybridEmbedder::hybrid_remote(
            corpus_tfidf(),
            RemoteEmbeddingConfig {
                provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
                remote_dimension: Some(8),
                ..RemoteEmbeddingConfig::default()
            },
            None,
        )
        .expect("remote provider"),
    );

    assert!(generation.embed_neural_batch_blocking(&["seed"])[0].is_some());
    let all_bad = ["bad 0", "bad 1", "bad 2", "bad 3"];

    // One rejected batch must not end the generation. Note the 500 status the
    // provider maps this to is retryable AND bisectable: with the retry ladder
    // it is re-sent max_retries times, and each attempt then bisects.
    let first = generation.embed_neural_batch_blocking(&all_bad);
    assert!(first.iter().all(Option::is_none));
    assert!(
        !generation.document_embedding_stopped(),
        "a single failed batch must not freeze the generation"
    );

    // The endpoint is still answering, so the generation keeps trying. Walk the
    // remaining budget instead of hardcoding a count, so the policy constant
    // stays the single source of truth for when the generation acts.
    for attempt in 2..super::hybrid::MAX_CONSECUTIVE_REMOTE_FAILURES {
        let after_previous = request_count.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            generation
                .embed_neural_batch_blocking(&[format!("still trying {attempt}")])
                .iter()
                .all(Option::is_none)
        );
        assert!(!generation.document_embedding_stopped());
        assert!(
            request_count.load(std::sync::atomic::Ordering::SeqCst) > after_previous,
            "attempt {attempt} must still reach the endpoint"
        );
    }

    // The last consecutive whole-batch failure exhausts the budget. This
    // endpoint is unmanaged (no `[services.*]` entry, so no restart is
    // possible): the escalation falls straight through to the freeze policy.
    assert!(
        generation
            .embed_neural_batch_blocking(&["budget spent"])
            .iter()
            .all(Option::is_none)
    );
    assert!(
        generation.document_embedding_stopped(),
        "sustained failure after landed rows must freeze the generation"
    );

    // A frozen generation stops spending requests. Assert on the batch sizes
    // the endpoint saw rather than the raw request count: the count depends on
    // the retry budget, the size sequence does not.
    let before = batch_sizes.lock().expect("batch sizes").len();
    assert!(
        generation
            .embed_neural_batch_blocking(&["must not retry"])
            .iter()
            .all(Option::is_none)
    );
    assert_eq!(
        batch_sizes.lock().expect("batch sizes").len(),
        before,
        "a frozen generation must not contact the endpoint again"
    );
    assert!(
        batch_sizes.lock().expect("batch sizes").contains(&4),
        "the original 4-document batch shape must be preserved"
    );
}

#[test]
fn test_remote_batch_availability_status_fails_fast_without_bisecting() {
    for status in [502, 503, 504] {
        let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_count = request_count.clone();
        // Budget > 1 so a (wrong) bisect would be answered and observed
        // instead of hanging until the client timeout.
        let endpoint = spawn_mock_embeddings_server_scripted(
            |_| 8,
            move |_| {
                server_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                status
            },
            None,
            8,
        );
        let provider =
            crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
                provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
                remote_dimension: Some(8),
                ..RemoteEmbeddingConfig::default()
            })
            .expect("remote provider");

        let error = block_on_test(
            provider.embed_batch_optional(vec!["first", "second", "third", "fourth"]),
        )
        .expect_err("availability failure must fail the whole batch");
        assert!(matches!(
            error,
            crate::search::onnx::RemoteEmbeddingError::HttpStatus { status: got, .. } if got == status
        ));
        // The retry ladder re-sends an availability status (it can clear on its
        // own), but the batch is NEVER split: a 5xx is not evidence that one
        // input is bad, so bisecting would spend four more doomed requests
        // guessing which. One original send plus `max_retries` re-sends.
        assert_eq!(
            request_count.load(std::sync::atomic::Ordering::SeqCst),
            RemoteEmbeddingConfig::default().max_retries + 1,
            "HTTP {status} must be retried but never bisected"
        );
    }
}

#[test]
fn test_remote_batch_aborted_mid_flight_keeps_already_embedded_rows() {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_count = request_count.clone();
    // Request 0 (the whole 4-document batch) is rejected so the provider
    // bisects. Request 1 is the FIRST half, which succeeds and lands two rows.
    // Every later request drops the connection, so the second half never
    // answers — that is the moment the old code returned Err and threw away the
    // two rows it had already paid for.
    let endpoint = spawn_mock_embeddings_server_scripted(
        |_| 8,
        move |_| match server_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => 400,
            1 => 200,
            _ => 0,
        },
        None,
        3,
    );
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        remote_dimension: Some(8),
        max_retries: 0,
        ..RemoteEmbeddingConfig::default()
    })
    .expect("remote provider");

    let rows =
        block_on_test(provider.embed_batch_optional(vec!["first", "second", "third", "fourth"]))
            .expect("a mid-flight abort must return the rows that landed, not an error");

    assert_eq!(
        rows.iter().filter(|row| row.is_some()).count(),
        2,
        "the half that was embedded before the abort must survive"
    );
    assert_eq!(
        rows.iter().filter(|row| row.is_none()).count(),
        2,
        "the unreachable half must be reported empty, not fabricated"
    );
}

#[test]
fn test_remote_batch_http_then_transport_error_propagates() {
    let request_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_count = request_count.clone();
    let endpoint = spawn_mock_embeddings_server_scripted(
        |_| 8,
        move |_| match server_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => 400,
            _ => 0,
        },
        None,
        2,
    );
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        remote_dimension: Some(8),
        ..RemoteEmbeddingConfig::default()
    })
    .expect("remote provider");

    let error =
        block_on_test(provider.embed_batch_optional(vec!["first", "second", "third", "fourth"]))
            .expect_err("transport failure after HTTP error must propagate");
    // The endpoint never answered this slice, so this must surface as a
    // transport failure — that is the variant the retry ladder and the
    // generation policy both key off of. Anything else (ApiError, HttpStatus)
    // would claim the endpoint gave a verdict it never gave.
    assert!(
        matches!(
            error,
            crate::search::onnx::RemoteEmbeddingError::Transport(_)
        ),
        "expected a transport error, got {error:?}"
    );
    assert_eq!(request_count.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Which endpoints get a whole-request deadline and which rely on connection
/// liveness instead. This decides the timeout policy for the entire run, so a
/// regression here is silent: a loopback endpoint that stops being recognised
/// as local silently regains the wall-clock cap that aborted valid batches.
#[test]
fn test_local_endpoint_classification_covers_loopback_forms() {
    let local = |endpoint: &str| {
        RemoteEmbeddingConfig {
            provider: crate::search::onnx::RemoteProvider::Custom {
                endpoint: endpoint.to_string(),
            },
            ..RemoteEmbeddingConfig::default()
        }
        .is_local_endpoint()
    };
    for endpoint in [
        "http://127.0.0.1:8766/v1",
        "http://127.0.0.2:8080/v1",
        "http://localhost:11434/v1",
        "http://[::1]:8766/v1",
        "https://localhost",
        // No scheme at all: still a local authority.
        "127.0.0.1:8766",
    ] {
        assert!(local(endpoint), "{endpoint} must be treated as local");
    }
    for endpoint in [
        "https://api.openai.com/v1",
        "http://192.168.1.10:8766/v1",
        // The host NAME contains "localhost" but the authority does not: a
        // substring match would wrongly call this local and drop its deadline.
        "https://localhost.example.com/v1",
        "https://notlocalhost.com/v1",
    ] {
        assert!(
            !local(endpoint),
            "{endpoint} must keep the whole-request deadline"
        );
    }
    // A provider with no Custom endpoint has no URL to inspect.
    assert!(
        !RemoteEmbeddingConfig::default().is_local_endpoint(),
        "a preset provider must not be classified as a local endpoint"
    );
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
    assert_eq!(provider.observed_dimension(), None);
    let embedding = block_on_test(provider.embed("hello")).expect("embed");
    assert_eq!(embedding.len(), 512);
    assert_eq!(provider.observed_dimension(), Some(512));
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
fn test_remote_query_template_is_applied_verbatim_on_wire() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let endpoint = spawn_mock_embeddings_server_with_capture(|_| 768, Some(sender));
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        model: Some("embeddinggemma-2".to_string()),
        remote_dimension: Some(768),
        query_template: Some("task: code retrieval | query: {text}".to_string()),
        ..RemoteEmbeddingConfig::default()
    })
    .expect("provider construction");

    block_on_test(provider.embed("find cache invalidation")).expect("query embedding");
    let request = receiver.recv().expect("captured query request");
    assert_eq!(
        request["input"][0],
        "task: code retrieval | query: find cache invalidation"
    );
    assert_eq!(request["model"], "embeddinggemma-2");
}

#[test]
fn test_remote_document_template_is_applied_verbatim_on_wire() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let endpoint = spawn_mock_embeddings_server_with_capture(|_| 768, Some(sender));
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        model: Some("embeddinggemma-2".to_string()),
        remote_dimension: Some(768),
        doc_template: Some("title: none | text: {text}".to_string()),
        ..RemoteEmbeddingConfig::default()
    })
    .expect("provider construction");

    block_on_test(provider.embed_batch(vec!["fn cache_key() {}"])).expect("document embedding");
    let request = receiver.recv().expect("captured document request");
    assert_eq!(request["input"][0], "title: none | text: fn cache_key() {}");
}

#[test]
fn test_remote_document_template_threads_titles_on_wire() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let endpoint = spawn_mock_embeddings_server_responses(|_| 768, Some(sender), 2);
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        model: Some("embeddinggemma-2".to_string()),
        remote_dimension: Some(768),
        doc_template: Some("title: {title} | text: {text}".to_string()),
        ..RemoteEmbeddingConfig::default()
    })
    .expect("provider construction");

    block_on_test(provider.embed_batch_titled(
        vec!["fn cache_key() {}"],
        vec![Some("src/lib.rs".to_string())].into_iter().peekable(),
    ))
    .expect("titled document embedding");
    let request = receiver.recv().expect("captured titled document request");
    assert_eq!(
        request["input"][0],
        "title: src/lib.rs | text: fn cache_key() {}"
    );

    block_on_test(
        provider.embed_batch_titled(vec!["fn other() {}"], vec![None].into_iter().peekable()),
    )
    .expect("title-less document embedding");
    let request = receiver.recv().expect("captured title-less request");
    assert_eq!(request["input"][0], "title: none | text: fn other() {}");
}

#[test]
fn test_remote_templates_none_preserves_raw_text_on_wire() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let endpoint = spawn_mock_embeddings_server_with_capture(|_| 768, Some(sender));
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        remote_dimension: Some(768),
        ..RemoteEmbeddingConfig::default()
    })
    .expect("provider construction");

    block_on_test(provider.embed("raw query")).expect("query embedding");
    let request = receiver.recv().expect("captured request");
    assert_eq!(request["input"][0], "raw query");
}

#[test]
fn test_remote_document_template_without_placeholder_prepends_prefix() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let endpoint = spawn_mock_embeddings_server_with_capture(|_| 768, Some(sender));
    let provider = crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
        remote_dimension: Some(768),
        doc_template: Some("document task".to_string()),
        ..RemoteEmbeddingConfig::default()
    })
    .expect("provider construction");

    block_on_test(provider.embed_batch(vec!["source text"])).expect("document embedding");
    let request = receiver.recv().expect("captured request");
    assert_eq!(request["input"][0], "document task source text");
}

#[test]
fn test_remote_template_changes_identity() {
    let make_provider = |template: &str| {
        crate::search::onnx::GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
            provider: crate::search::onnx::RemoteProvider::Custom {
                endpoint: "http://127.0.0.1:1/v1".to_string(),
            },
            doc_template: Some(template.to_string()),
            ..RemoteEmbeddingConfig::default()
        })
        .expect("provider construction")
    };
    assert_ne!(
        make_provider("document A").identity(),
        make_provider("document B").identity()
    );
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

// ============================================================================
// Remote embedding cache: the second pass must not re-request anything
// ============================================================================

/// Measure the remote route's whole point: a repeated document batch performs
/// ZERO embed HTTP requests the second time, and the served vectors are the
/// same bytes the endpoint originally returned.
///
/// The mock server answers exactly one request, so a second request would not
/// merely be counted — it would hang until the client's retry ladder gave up
/// and returned errors. A cache regression therefore fails loudly here rather
/// than silently costing minutes of re-embedding.
#[test]
fn test_remote_batch_is_reembedded_only_once() {
    let endpoint = spawn_mock_embeddings_server_responses(|_| 768, None, 1);
    // `LEINDEX_HOME` selects the shared embed-cache root; point it at a scratch
    // directory so this test neither reads nor pollutes the real store.
    let scratch = std::env::temp_dir().join(format!(
        "leindex-remote-route-cache-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&scratch).expect("scratch home");
    let previous_home = std::env::var("LEINDEX_HOME").ok();
    // SAFETY: this test owns `LEINDEX_HOME` for its own duration; the cache
    // tests in `search::onnx::remote_cache` serialize on their own lock and use
    // distinct roots, so the only shared state is the variable itself.
    unsafe { std::env::set_var("LEINDEX_HOME", &scratch) };

    let remote_cfg = RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: endpoint.clone(),
        },
        timeout_secs: 5,
        model: Some("embeddinggemma-2".to_string()),
        remote_dimension: Some(768),
        ..RemoteEmbeddingConfig::default()
    };
    let texts = vec!["fn alpha() {}", "fn beta() {}", "fn gamma() {}"];

    let first = {
        let embedder =
            HybridEmbedder::hybrid_remote(corpus_tfidf(), remote_cfg.clone(), None).expect("ctor");
        embedder.embed_neural_batch_blocking(&texts)
    };
    assert_eq!(first.len(), texts.len());
    assert!(
        first.iter().all(Option::is_some),
        "the single allowed request must have produced every row"
    );

    // Same provider identity, same texts: served entirely from the cache.
    let second = {
        let embedder =
            HybridEmbedder::hybrid_remote(corpus_tfidf(), remote_cfg, None).expect("ctor");
        embedder.embed_neural_batch_blocking(&texts)
    };

    match previous_home {
        Some(value) => unsafe { std::env::set_var("LEINDEX_HOME", value) },
        None => unsafe { std::env::remove_var("LEINDEX_HOME") },
    }
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!(
        second.len(),
        first.len(),
        "a cached batch must still return a row per input"
    );
    for (index, (cached, fresh)) in second.iter().zip(first.iter()).enumerate() {
        assert_eq!(
            cached.as_ref(),
            fresh.as_ref(),
            "row {index} must be byte-identical to the endpoint's original vector"
        );
    }
}
