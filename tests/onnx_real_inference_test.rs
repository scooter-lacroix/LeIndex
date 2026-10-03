//! Real-inference regression tests for the ONNX embed worker's input feeding.
//!
//! REGRESSION GUARD: the Qwen3 embed export declares 56 `past_key_values.*`
//! KV-cache inputs that the worker historically never fed, so EVERY inference
//! failed with `Missing Input: past_key_values.0.value` and neural search
//! silently degraded to TF-IDF. These tests assert real inference succeeds
//! end-to-end when a model is present, and skip (with a loud reason) on
//! machines without one — CI has no models by design.
//!
//! Run with: `cargo test -p leindex --features onnx --test
//! onnx_real_inference_test -- --ignored --nocapture`

#![cfg(feature = "onnx")]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use leindex::embed::protocol::EmbedResponse;
use leindex::embed::runtime::{RuntimeConfig, WorkerRuntime};

/// Resolve the default model exactly as the worker does; `None` when absent.
fn default_model_available() -> bool {
    // RuntimeConfig::from_env resolves model name + provider the same way the
    // spawned worker does; WorkerRuntime::new logs loudly when the model is
    // missing. We probe via ModelResolver directly for a clean skip reason.
    let config = RuntimeConfig::from_env();
    leindex::embed::model_path::ModelResolver::resolve(&config.model_name).is_ok()
}

/// VAL-KV-001: a REAL embed through the in-process worker runtime succeeds —
/// no `Missing Input: past_key_values.*` failure, real vectors, finite values.
#[test]
#[ignore = "requires the real ONNX model + ORT library (absent in CI by design)"]
fn test_real_inference_feeds_past_key_values() {
    if !default_model_available() {
        eprintln!("SKIP: default embed model not present on this machine");
        return;
    }

    let config = RuntimeConfig::from_env();
    let runtime = WorkerRuntime::new(config);
    assert!(
        runtime.is_neural_ready(),
        "worker runtime must initialize session + tokenizer when the model exists"
    );

    let session = runtime
        .bench_session()
        .expect("neural-ready runtime must expose a session");
    let tokenizer = runtime
        .bench_tokenizer()
        .expect("neural-ready runtime must expose a tokenizer");
    let cancel = Arc::new(AtomicBool::new(false));

    let response: EmbedResponse = runtime
        .bench_run_onnx_embed(
            &session,
            &tokenizer,
            &[
                "fn authenticate(user: &str, token: &str) -> Result<Session>".to_string(),
                "pub struct ProjectRegistry { projects: HashMap<String, Handle> }".to_string(),
            ],
            runtime.bench_embed_dim(),
            &cancel,
        )
        .expect("real ONNX inference must succeed (past_key_values must be fed)");

    assert_eq!(response.count, 2, "one vector per input text");
    assert_eq!(
        response.dimension,
        runtime.bench_embed_dim(),
        "embedding dimension must match the configured model"
    );
    assert!(
        response.vectors.iter().all(|v| v.is_finite()),
        "all embedding values must be finite (no NaN from a broken forward pass)"
    );
    assert!(
        response.vectors.iter().any(|v| *v != 0.0),
        "vectors must be non-degenerate (a zero vector means the pooling path is broken)"
    );

    // Distinct inputs must produce distinct embeddings — the strongest signal
    // that the forward pass is real rather than constant garbage.
    let dim = response.dimension;
    let a = &response.vectors[..dim];
    let b = &response.vectors[dim..];
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    let cosine = dot / (norm_a * norm_b);
    assert!(
        cosine < 0.999,
        "two semantically different snippets must not embed identically (cosine={cosine:.4})"
    );
}

/// VAL-KV-002: the runtime's KV detection sees the shipped model's cache
/// inputs. Catches silent detection breakage even without a full inference.
#[test]
#[ignore = "requires the real ONNX model + ORT library (absent in CI by design)"]
fn test_kv_detection_finds_declared_cache_inputs() {
    if !default_model_available() {
        eprintln!("SKIP: default embed model not present on this machine");
        return;
    }

    let config = RuntimeConfig::from_env();
    let runtime = WorkerRuntime::new(config);
    let session = runtime
        .bench_session()
        .expect("neural-ready runtime must expose a session");

    let guard = session.lock().unwrap();
    let declared: Vec<String> = guard
        .inputs()
        .iter()
        .map(|i| i.name().to_string())
        .collect();
    let declared_kv = declared
        .iter()
        .filter(|n| n.starts_with("past_key_values"))
        .count();
    drop(guard);

    if declared_kv > 0 {
        // Decoder-style export: the worker MUST have detected every one,
        // through the same code path the embed inference uses.
        let detected = runtime.bench_detect_kv_inputs();
        assert_eq!(
            detected.len(),
            declared_kv,
            "KV detection must find every declared past_key_values input"
        );
    } else {
        eprintln!("NOTE: model declares no KV inputs (BERT/GTE-style); no-op path");
    }
}
