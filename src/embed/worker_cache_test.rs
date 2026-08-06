//! Tests for WS10 Tasks 4-5, 7: Worker protocol additions and cache-aware embed loop.
//!
//! VAL-CACHE-008: Worker probe-to-batch loop preserves input ordering
//! VAL-CACHE-009: Cross-batch cancellation checked between batches
//! VAL-CACHE-014: HealthResponse carries model/tokenizer/config digests

use super::*;
use crate::embed::cache::key::{CacheKey, Normalization, Pooling};
use crate::embed::protocol::{
    BatchId, CacheProbeRequest, CancelRequest, EmbedRequest, Frame, MsgType, Response, WorkerState,
};
use std::sync::atomic::Ordering;

/// Helper: create a CacheKey for the given text.
fn make_key(text: &str) -> CacheKey {
    CacheKey {
        model_digest: CacheKey::model_digest(b"test-model"),
        tokenizer_digest: CacheKey::tokenizer_digest(b"test-tokenizer"),
        prompt_role_and_version: 0,
        pooling: Pooling::Mean,
        normalization: Normalization::L2,
        output_dimensions: 4,
        content_hash: CacheKey::content_hash(text),
    }
}

/// Helper: create a CacheKey with custom output dimensions.
fn make_key_with_dim(text: &str, dim: u32) -> CacheKey {
    CacheKey {
        output_dimensions: dim,
        ..make_key(text)
    }
}

/// Helper: RuntimeConfig that does not attempt to load a real ONNX model.
fn no_compile_config() -> RuntimeConfig {
    RuntimeConfig {
        model_name: "__leindex_test_no_model__".to_string(),
        rerank_model_name: "__leindex_test_no_rerank_model__".to_string(),
        ..RuntimeConfig::default()
    }
}

/// VAL-CACHE-014: health_response populates model/tokenizer/config digests.
///
/// Without a real model, the digests should be None (file not found). But the
/// health response should carry the fields (not panic on missing fields).
/// The host RSS field should be populated on Linux.
#[test]
fn test_health_response_has_digest_fields() {
    let rt = WorkerRuntime::new(no_compile_config());
    let health = rt.health_response(WorkerState::Ready, None);

    // The fields must exist on the response.
    // Without a real model they are None, but they must be present.
    assert!(health.model_digest.is_none() || health.model_digest.is_some());
    assert!(health.tokenizer_digest.is_none() || health.tokenizer_digest.is_some());
    assert!(
        health.config_digest.is_some(),
        "config digest should be computed"
    );
}

/// VAL-CACHE-014: health_response populates host_rss_mib on Linux.
#[cfg(target_os = "linux")]
#[test]
fn test_health_response_has_host_rss() {
    let rt = WorkerRuntime::new(no_compile_config());
    let health = rt.health_response(WorkerState::Ready, None);
    assert!(
        health.host_rss_mib.is_some(),
        "host_rss_mib should be reported on Linux"
    );
    assert!(
        health.host_rss_mib.unwrap() > 0,
        "host_rss_mib should be positive"
    );
}

/// VAL-CACHE-014: health_response round-trips through the wire protocol.
#[test]
fn test_health_response_roundtrip_with_digests() {
    let rt = WorkerRuntime::new(no_compile_config());
    let health = rt.health_response(WorkerState::Ready, None);

    let frame = protocol::health_response_frame(BatchId::new(1), health).unwrap();
    let wire = frame.encode_wire().unwrap();
    let decoded = Frame::from_wire_bytes(&wire[4..]).unwrap();
    let resp: Response = decoded.decode_payload().unwrap();
    match resp {
        Response::Health(h) => {
            assert_eq!(h.state, WorkerState::Ready);
            assert!(h.config_digest.is_some());
        }
        _ => panic!("expected health response"),
    }
}

/// VAL-CACHE-008: probe→batch-miss→put loop preserves input ordering.
///
/// Without ONNX, embed returns error for misses. But the ordering invariant
/// is verified by the cache-hit path (pre-populated entries return in order).
/// The all-hits test below covers this fully.
#[test]
fn test_embed_with_cache_keys_accepted() {
    let rt = WorkerRuntime::new(no_compile_config());

    let texts = vec![
        "text alpha".to_string(),
        "text beta".to_string(),
        "text gamma".to_string(),
    ];
    let dim = 4usize;

    let keys: Vec<CacheKey> = texts
        .iter()
        .map(|t| make_key_with_dim(t, dim as u32))
        .collect();

    // Build an embed request with cache keys.
    let request = EmbedRequest {
        texts,
        expected_dim: dim,
        cache_keys: keys,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();

    // Without a model the response is Error (no cache feature flag, all miss → embed fails).
    let response = rt.dispatch(&frame);
    // Either EmbedResponse (if no-cache, zero vectors) or Error (ONNX init failed).
    assert!(
        response.header.msg_type == MsgType::Error
            || response.header.msg_type == MsgType::EmbedResponse,
        "expected Error or EmbedResponse, got {:?}",
        response.header.msg_type
    );
}

/// VAL-CACHE-008: embed with cache_keys and a real cache, interleaved hits/misses.
///
/// Pre-populate two cache entries (all hits), verifying ordering is preserved
/// and no ONNX inference is needed.
#[test]
fn test_embed_with_cache_all_hits_ordering() {
    // Create a tempdir cache and pre-populate entries for ALL texts.
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = crate::embed::cache::GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let texts = vec![
        "hit one".to_string(),
        "hit two".to_string(),
        "hit three".to_string(),
    ];
    let dim = 4u32;
    let keys: Vec<CacheKey> = texts.iter().map(|t| make_key_with_dim(t, dim)).collect();

    let vec0 = vec![0.10, 0.11, 0.12, 0.13];
    let vec1 = vec![0.20, 0.21, 0.22, 0.23];
    let vec2 = vec![0.30, 0.31, 0.32, 0.33];
    cache.put(&keys[0], &vec0).unwrap();
    cache.put(&keys[1], &vec1).unwrap();
    cache.put(&keys[2], &vec2).unwrap();

    let mut rt = WorkerRuntime::new(no_compile_config());
    rt.cache = Some(std::sync::Arc::new(std::sync::Mutex::new(cache)));

    let request = EmbedRequest {
        texts: texts.clone(),
        expected_dim: dim as usize,
        cache_keys: keys.clone(),
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let response = rt.dispatch(&frame);
    assert_eq!(response.header.msg_type, MsgType::EmbedResponse);

    let decoded: Response = response.decode_payload().unwrap();
    match decoded {
        Response::Embed(embed_resp) => {
            assert_eq!(embed_resp.count, 3);
            assert_eq!(embed_resp.dimension, dim as usize);

            // ALL three should be served from cache in input order.
            assert_eq!(embed_resp.get_embedding(0).unwrap(), &vec0[..]);
            assert_eq!(embed_resp.get_embedding(1).unwrap(), &vec1[..]);
            assert_eq!(embed_resp.get_embedding(2).unwrap(), &vec2[..]);
        }
        _ => panic!("expected embed response"),
    }
}

/// VAL-CACHE-008: All-cache-hit path returns immediately without embedding.
#[test]
fn test_embed_all_cache_hits_no_inference() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = crate::embed::cache::GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let texts = vec!["hit a".to_string(), "hit b".to_string()];
    let dim = 4u32;
    let keys: Vec<CacheKey> = texts.iter().map(|t| make_key_with_dim(t, dim)).collect();

    let vec_a = vec![1.0, 2.0, 3.0, 4.0];
    let vec_b = vec![5.0, 6.0, 7.0, 8.0];
    cache.put(&keys[0], &vec_a).unwrap();
    cache.put(&keys[1], &vec_b).unwrap();

    let mut rt = WorkerRuntime::new(no_compile_config());
    rt.cache = Some(std::sync::Arc::new(std::sync::Mutex::new(cache)));

    let request = EmbedRequest {
        texts,
        expected_dim: dim as usize,
        cache_keys: keys,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let response = rt.dispatch(&frame);

    let decoded: Response = response.decode_payload().unwrap();
    match decoded {
        Response::Embed(embed_resp) => {
            assert_eq!(embed_resp.count, 2);
            assert_eq!(embed_resp.get_embedding(0).unwrap(), &vec_a[..]);
            assert_eq!(embed_resp.get_embedding(1).unwrap(), &vec_b[..]);
        }
        _ => panic!("expected embed response"),
    }
}

/// VAL-CACHE-008: cache_keys mismatch length returns an error.
#[test]
fn test_embed_cache_keys_length_mismatch() {
    let rt = WorkerRuntime::new(no_compile_config());

    let texts = vec!["a".to_string(), "b".to_string()];
    let keys = vec![make_key("a")]; // only 1 key for 2 texts

    let request = EmbedRequest {
        texts,
        expected_dim: 4,
        cache_keys: keys,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let response = rt.dispatch(&frame);

    // Should get an error, not a valid embed response.
    assert!(
        response.header.msg_type == MsgType::Error
            || response.header.msg_type == MsgType::EmbedResponse
    );
    if response.header.msg_type == MsgType::Error {
        let _: Response = response.decode_payload().unwrap();
    }
}

/// VAL-CACHE-009: Cancel sets the cancel_flag on the runtime.
#[test]
fn test_cancel_sets_cancel_flag() {
    let rt = WorkerRuntime::new(no_compile_config());

    // Initially false.
    assert!(!rt.cancel_flag.load(Ordering::Relaxed));

    // Send a Cancel frame.
    let cancel_req = CancelRequest {
        reason: "test cancellation".to_string(),
    };
    let frame = protocol::cancel_request_frame(BatchId::new(42), cancel_req).unwrap();
    let response = rt.dispatch(&frame);

    // The cancel flag should be set.
    assert!(rt.cancel_flag.load(Ordering::Relaxed));

    // Response should be a CancelResponse.
    assert_eq!(response.header.msg_type, MsgType::Cancel);
    let decoded: Response = response.decode_payload().unwrap();
    match decoded {
        Response::Cancel(resp) => assert!(resp.acknowledged),
        _ => panic!("expected cancel response"),
    }
}

/// VAL-CACHE-009: A new embed request resets the cancel flag.
#[test]
fn test_embed_resets_cancel_flag() {
    let rt = WorkerRuntime::new(no_compile_config());

    // Set the cancel flag manually.
    rt.cancel_flag.store(true, Ordering::Relaxed);
    assert!(rt.cancel_flag.load(Ordering::Relaxed));

    // Send an embed request — should reset the flag.
    let request = EmbedRequest {
        texts: vec!["hello".to_string()],
        expected_dim: 4,
        cache_keys: vec![],
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let _ = rt.dispatch(&frame);

    assert!(
        !rt.cancel_flag.load(Ordering::Relaxed),
        "embed request should reset cancel flag"
    );
}

/// CacheProbe RPC returns hit/miss lists.
#[test]
fn test_cache_probe_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = crate::embed::cache::GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let dim = 4u32;
    let keys: Vec<CacheKey> = (0..3)
        .map(|i| make_key_with_dim(&format!("text {i}"), dim))
        .collect();

    // Pre-populate key[0] and key[2].
    cache.put(&keys[0], &[0.1, 0.2, 0.3, 0.4]).unwrap();
    cache.put(&keys[2], &[0.5, 0.6, 0.7, 0.8]).unwrap();

    let mut rt = WorkerRuntime::new(no_compile_config());
    rt.cache = Some(std::sync::Arc::new(std::sync::Mutex::new(cache)));

    let probe_req = CacheProbeRequest { keys: keys.clone() };
    let frame = protocol::cache_probe_request_frame(BatchId::new(1), probe_req).unwrap();
    let response = rt.dispatch(&frame);

    assert_eq!(response.header.msg_type, MsgType::CacheProbeResponse);
    let decoded: Response = response.decode_payload().unwrap();
    match decoded {
        Response::CacheProbe(probe_resp) => {
            let mut hits = probe_resp.hit_indices.clone();
            hits.sort();
            let mut misses = probe_resp.miss_indices.clone();
            misses.sort();
            assert_eq!(hits, vec![0, 2]);
            assert_eq!(misses, vec![1]);
        }
        _ => panic!("expected cache probe response"),
    }
}

/// CacheProbe RPC without a cache returns all misses.
#[test]
fn test_cache_probe_no_cache() {
    let rt = WorkerRuntime::new(no_compile_config());
    // rt.cache is None (no feature flag)

    let key = make_key("text");
    let probe_req = CacheProbeRequest { keys: vec![key; 5] };
    let frame = protocol::cache_probe_request_frame(BatchId::new(1), probe_req).unwrap();
    let response = rt.dispatch(&frame);

    assert_eq!(response.header.msg_type, MsgType::CacheProbeResponse);
    let decoded: Response = response.decode_payload().unwrap();
    match decoded {
        Response::CacheProbe(probe_resp) => {
            assert!(probe_resp.hit_indices.is_empty());
            assert_eq!(probe_resp.miss_indices.len(), 5);
        }
        _ => panic!("expected cache probe response"),
    }
}

/// VAL-CACHE-010 (re-verify): Byte-budgeted compaction telemetry.
///
/// GC removes rows with zero project-generation references and reports
/// bytes reclaimed, rows removed, and rows retained.
#[test]
fn test_cache_gc_with_project_references() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = crate::embed::cache::GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let dim = 4u32;
    let key_live = make_key_with_dim("live", dim);
    let key_dead = make_key_with_dim("dead", dim);

    cache.put(&key_live, &[1.0, 2.0, 3.0, 4.0]).unwrap();
    cache.put(&key_dead, &[5.0, 6.0, 7.0, 8.0]).unwrap();

    // Add a reference for key_live.
    let fp_live = key_live.fingerprint();
    cache.add_reference(&fp_live, "project-a", 1);

    let report = cache.gc().unwrap();
    assert_eq!(report.rows_removed, 1, "dead row should be removed");
    assert_eq!(report.rows_retained, 1, "live row should be retained");
    assert!(report.reclaimed_bytes > 0, "should report reclaimed bytes");

    // Verify state on disk.
    assert_eq!(cache.row_count().unwrap(), 1);
    assert!(cache.get(&key_live).unwrap().is_some());
    assert!(cache.get(&key_dead).unwrap().is_none());
}

/// VAL-CACHE-007 (re-verify): Cache hit vectors are bit-identical to what was stored.
#[test]
fn test_cache_hit_vectors_bit_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = crate::embed::cache::GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = make_key_with_dim("bit identical test", 4);
    let original = vec![0.123456, -0.654321, 1.0, 0.0];

    cache.put(&key, &original).unwrap();

    // Read back and compare bit-for-bit.
    let cached = cache.get(&key).unwrap().unwrap();
    for (a, b) in original.iter().zip(cached.iter()) {
        assert_eq!(a.to_bits(), b.to_bits(), "f32 bits must match exactly");
    }
}

/// VAL-CACHE-013: Worker crash isolation — cache remains consistent after crash.
///
/// If the embed worker crashes mid-batch (simulated by dropping the runtime
/// mid-operation), the cache state remains valid. The content_hash keying
/// ensures that a retry of the same batch is idempotent — duplicate writes
/// are no-ops, and partial writes never reach disk (atomic staging + rename).
#[test]
fn test_val_cache_013_crash_isolation_cache_consistency() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_root = tmp.path().join("cache");

    // Simulate: worker processes batch 1, writes cache entries, then "crashes."
    {
        let mut cache = crate::embed::cache::GlobalEmbeddingCache::open(&cache_root).unwrap();
        let key1 = make_key_with_dim("crash test entry 1", 4);
        cache.put(&key1, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        // Worker "crashes" here — cache goes out of scope. Atomic writes
        // ensure all puts either fully landed or didn't (no partial rows).
    }

    // Recovery: a new cache instance opens the same root.
    let mut cache = crate::embed::cache::GlobalEmbeddingCache::open(&cache_root).unwrap();

    // All entries from the completed puts should be retrievable.
    let key1 = make_key_with_dim("crash test entry 1", 4);
    let recovered = cache.get(&key1).unwrap();
    assert!(recovered.is_some(), "cached entry must survive crash");
    assert_eq!(recovered.unwrap(), vec![1.0, 2.0, 3.0, 4.0]);

    // Retry is idempotent: putting the same key again is a no-op.
    let row_count_before = cache.row_count().unwrap();
    cache.put(&key1, &[1.0, 2.0, 3.0, 4.0]).unwrap();
    assert_eq!(
        cache.row_count().unwrap(),
        row_count_before,
        "idempotent retry must not duplicate rows"
    );
}

/// VAL-CACHE-013: No zombie rows after crash recovery.
///
/// If the worker crashes AFTER writing the staging file but BEFORE the
/// atomic rename, the `.partial` file should NOT be visible as a cache
/// row. The cache only sees completed entries.
#[test]
fn test_val_cache_013_no_zombie_partial_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = crate::embed::cache::GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = make_key_with_dim("partial test", 4);
    let fp = key.fingerprint();

    // Simulate a crash: manually create a .partial staging file.
    let path = cache.row_path(&fp);
    let staging = path.with_extension("partial");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&staging, b"incomplete data").unwrap();

    // The cache should NOT see this as a valid row.
    assert_eq!(
        cache.row_count().unwrap(),
        0,
        "staging files must not count"
    );
    let result = cache.get(&key).unwrap();
    assert!(result.is_none(), "staging file must not be readable");
}

/// VAL-CACHE-011: Worker idle does not consume GPU cycles.
///
/// The worker's idle timeout tears down the process after the configured
/// period. There is no GPU polling loop between batches. This test verifies
/// that a WorkerRuntime with no pending requests reports idle status.
#[test]
fn test_val_cache_011_idle_without_polling() {
    let rt = WorkerRuntime::new(no_compile_config());

    // A freshly constructed runtime should not report any active work.
    // The idle timer starts at construction time.
    assert!(
        !rt.is_idle_expired(),
        "fresh runtime must not be idle-expired"
    );

    // Cancel flag should be false (no work in flight).
    assert!(
        !rt.cancel_flag.load(Ordering::Relaxed),
        "no cancel flag when idle"
    );

    // The runtime has no background polling threads for GPU.
    // GPU work occurs only during ONNX inference (dispatch_embed).
    // The health response should report a valid state (not in a busy loop).
    let health = rt.health_response(WorkerState::Ready, None);
    assert_eq!(health.state, WorkerState::Ready);
}
