use super::*;
use crate::embed::cache::key::{Normalization, Pooling};

fn sample_key(content: &str, dim: u32) -> CacheKey {
    CacheKey {
        model_digest: CacheKey::model_digest(b"model-bytes"),
        tokenizer_digest: CacheKey::tokenizer_digest(b"tokenizer-config"),
        prompt_role_and_version: 0,
        pooling: Pooling::Mean,
        normalization: Normalization::L2,
        output_dimensions: dim,
        content_hash: CacheKey::content_hash(content),
    }
}

fn sample_vector(dim: usize, seed: f32) -> Vec<f32> {
    (0..dim).map(|i| seed + i as f32 * 0.01).collect()
}

/// VAL-CACHE-003: probe returns hits and misses correctly.
#[test]
fn test_probe_returns_hits_and_misses() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key1 = sample_key("text one", 4);
    let key2 = sample_key("text two", 4);
    let key3 = sample_key("text three", 4);

    let vec1 = sample_vector(4, 0.1);
    let vec2 = sample_vector(4, 0.2);

    cache.put(&key1, &vec1, None).unwrap();
    cache.put(&key2, &vec2, None).unwrap();

    let result = cache.probe(&[key1, key2, key3]).unwrap();
    assert_eq!(result.hits.len(), 2);
    assert_eq!(result.misses, vec![2]);

    // Verify vectors are bit-identical.
    let hit1 = result.hits.get(&0).unwrap();
    let hit2 = result.hits.get(&1).unwrap();
    assert_eq!(hit1, &vec1);
    assert_eq!(hit2, &vec2);
}

/// VAL-CACHE-003: Hits return bit-identical vectors to what was stored.
#[test]
fn test_put_then_get_is_bit_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("exact text", 8);
    let original = sample_vector(8, 0.5);

    cache.put(&key, &original, None).unwrap();

    let gotten = cache.get(&key).unwrap().unwrap();
    assert_eq!(gotten.len(), original.len());
    for (a, b) in gotten.iter().zip(original.iter()) {
        assert_eq!(a.to_bits(), b.to_bits(), "f32 bits must match");
    }
}

/// VAL-CACHE-004: Corruption detection on read.
#[test]
fn test_corruption_detected_on_read() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("text to corrupt", 4);
    cache.put(&key, &sample_vector(4, 0.1), None).unwrap();

    // Corrupt by flipping one byte in the vector payload area.
    let path = cache.row_path(&key.fingerprint());
    let mut data = fs::read(&path).unwrap();
    // Flip a byte deep in the payload (well past the header).
    let corrupt_offset = ROW_HEADER_LEN + 4;
    data[corrupt_offset] ^= 0xFF;
    fs::write(&path, &data).unwrap();

    // probe should treat it as a miss.
    let result = cache.probe(std::slice::from_ref(&key)).unwrap();
    assert!(result.hits.is_empty());
    assert_eq!(result.misses, vec![0]);

    // get should return an error.
    assert!(cache.get(&key).is_err());
}

/// VAL-CACHE-005: Cross-project deduplication — same content = one row.
#[test]
fn test_cross_project_dedup_single_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    // Both projects embed the same text under the same model.
    let key = sample_key("shared content across projects", 4);
    let vec = sample_vector(4, 0.3);

    // Project A puts the vector.
    cache.put(&key, &vec, None).unwrap();
    assert_eq!(cache.row_count().unwrap(), 1);

    // Project B puts the same vector (same key) — dedup, no second row.
    cache.put(&key, &vec, None).unwrap();
    assert_eq!(cache.row_count().unwrap(), 1);
}

/// VAL-CACHE-005: Two projects adding references, then one drops, row stays.
#[test]
fn test_cross_project_ref_keeps_row_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("shared text", 4);
    cache.put(&key, &sample_vector(4, 0.1), None).unwrap();

    let fp = key.fingerprint();
    cache.add_reference(&fp, "project-a", 1);
    cache.add_reference(&fp, "project-b", 1);

    // Remove project-a's reference; row should survive (project-b still refs).
    cache.remove_reference(&fp, "project-a", 1);
    assert!(cache.is_referenced(&fp));

    // GC should retain the row.
    let report = cache.gc().unwrap();
    assert_eq!(report.rows_removed, 0);
    assert_eq!(report.rows_retained, 1);
}

/// VAL-CACHE-010: Byte-budgeted compaction removes unreferenced rows.
#[test]
fn test_gc_removes_unreferenced_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key_live = sample_key("live text", 4);
    let key_dead = sample_key("dead text", 4);

    cache.put(&key_live, &sample_vector(4, 0.1), None).unwrap();
    cache.put(&key_dead, &sample_vector(4, 0.2), None).unwrap();

    // Mark key_live as referenced by a project.
    let fp_live = key_live.fingerprint();
    cache.add_reference(&fp_live, "project-a", 1);

    // key_dead has no references — should be removed by GC.
    let report = cache.gc().unwrap();
    assert_eq!(report.rows_removed, 1);
    assert_eq!(report.rows_retained, 1);
    assert!(
        report.reclaimed_bytes > 0,
        "should report non-zero reclaimed bytes"
    );

    // Verify the live row survives and dead is gone.
    assert_eq!(cache.row_count().unwrap(), 1);
    assert!(cache.get(&key_live).unwrap().is_some());
    assert!(cache.get(&key_dead).unwrap().is_none());
}

#[test]
fn test_gc_on_empty_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();
    let report = cache.gc().unwrap();
    assert_eq!(report.rows_removed, 0);
    assert_eq!(report.rows_retained, 0);
    assert_eq!(report.reclaimed_bytes, 0);
}

/// Idempotent put: writing the same key twice does not duplicate the row.
#[test]
fn test_put_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("idempotent text", 4);
    cache.put(&key, &[0.1, 0.2, 0.3, 0.4], None).unwrap();
    cache.put(&key, &[0.1, 0.2, 0.3, 0.4], None).unwrap();

    assert_eq!(cache.row_count().unwrap(), 1);
}

#[test]
fn test_open_creates_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_root = tmp.path().join("nested").join("cache");
    let mut cache = GlobalEmbeddingCache::open(&cache_root).unwrap();
    assert!(cache_root.join("rows").exists());

    let key = sample_key("after open", 2);
    cache.put(&key, &[1.0, 2.0], None).unwrap();
    assert_eq!(cache.row_count().unwrap(), 1);
}

#[test]
fn test_persist_and_reload_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("persist test", 4);
    cache.put(&key, &[0.5, 0.6, 0.7, 0.8], None).unwrap();

    let fp = key.fingerprint();
    cache.add_reference(&fp, "proj", 42);
    cache.persist_refs().unwrap();

    // Reload.
    let cache2 = GlobalEmbeddingCache::open(tmp.path()).unwrap();
    assert!(cache2.is_referenced(&fp));

    // GC should retain the row.
    let mut cache2 = cache2;
    let report = cache2.gc().unwrap();
    assert_eq!(report.rows_retained, 1);
    assert_eq!(report.rows_removed, 0);
}

#[test]
fn test_multiple_projects_add_and_remove_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("multi-ref text", 4);
    cache.put(&key, &[0.1, 0.2, 0.3, 0.4], None).unwrap();
    let fp = key.fingerprint();

    cache.add_reference(&fp, "proj-a", 1);
    cache.add_reference(&fp, "proj-b", 2);
    assert!(cache.is_referenced(&fp));

    cache.remove_reference(&fp, "proj-a", 1);
    assert!(cache.is_referenced(&fp));

    cache.remove_reference(&fp, "proj-b", 2);
    assert!(!cache.is_referenced(&fp));
}

#[test]
fn test_total_bytes_reporting() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let cycles_before = cache.total_bytes().unwrap();
    assert_eq!(cycles_before, 0);

    let key = sample_key("bytes test", 4);
    cache.put(&key, &[1.0, 2.0, 3.0, 4.0], None).unwrap();
    let bytes_after = cache.total_bytes().unwrap();
    assert_eq!(bytes_after, (ROW_HEADER_LEN + 4 * 4) as u64);
}

/// Privacy: no source text stored in the row file.
#[test]
fn test_privacy_no_source_text_in_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let secret_text = "this is a secret symbol name fn_do_not_store_me";
    let key = sample_key(secret_text, 4);
    cache.put(&key, &[0.1, 0.2, 0.3, 0.4], None).unwrap();

    // Inspect the raw row file on disk.
    let path = cache.row_path(&key.fingerprint());
    let raw = fs::read(&path).unwrap();

    // The source text must NOT appear anywhere in the row file.
    assert!(
        !raw.windows(secret_text.len())
            .any(|w| w == secret_text.as_bytes()),
        "source text must not be stored in the cache row"
    );
}

#[test]
fn test_row_path_uses_2_hex_prefix() {
    let key = sample_key("path test", 4);
    let fp = key.fingerprint();
    let path = GlobalEmbeddingCache::open(tempfile::tempdir().unwrap().path())
        .unwrap()
        .row_path(&fp);
    let hex = hex_encode(&fp);
    assert!(
        path.to_string_lossy()
            .contains(&format!("rows/{}/{}", &hex[0..2], hex))
    );
}

// ── WS10 Task 6: Byte-budgeted compaction + telemetry (§10.3) ──────

/// Task 6: Every cache has byte accounting (total_bytes, cache_stats).
#[test]
fn test_cache_stats_reports_byte_accounting() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key1 = sample_key("stats one", 4);
    let key2 = sample_key("stats two", 4);

    cache.put(&key1, &[0.1, 0.2, 0.3, 0.4], None).unwrap();
    cache.put(&key2, &[0.5, 0.6, 0.7, 0.8], None).unwrap();

    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.row_count, 2);
    assert_eq!(
        stats.cache_bytes,
        (ROW_HEADER_LEN + 4 * 4) as u64 * 2,
        "byte accounting must report actual disk bytes"
    );
    assert!(stats.max_bytes > 0, "max_bytes must be set");
    assert!(stats.max_entry_bytes > 0, "max_entry_bytes must be set");
    // model_identity should report the actual model digest hex, not a
    // fixed opaque string.
    let expected_model = hex_encode(&CacheKey::model_digest(b"model-bytes"));
    assert_eq!(stats.model_identity, expected_model);
}

/// Task 6: Telemetry tracks hit/miss/eviction counts.
#[test]
fn test_telemetry_tracks_hits_and_misses() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key1 = sample_key("telemetry hit", 4);
    let key2 = sample_key("telemetry miss", 4);

    cache.put(&key1, &[1.0, 2.0, 3.0, 4.0], None).unwrap();

    // Probe with one hit, one miss.
    let result = cache.probe(&[key1, key2]).unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.misses.len(), 1);

    let telemetry = cache.telemetry();
    assert_eq!(telemetry.hits, 1);
    assert_eq!(telemetry.misses, 1);
    assert!((telemetry.hit_ratio() - 0.5).abs() < 0.001);
}

/// Task 6: Entry-size rejection — entries exceeding max_entry_bytes are rejected.
#[test]
fn test_entry_size_rejection() {
    let tmp = tempfile::tempdir().unwrap();
    let config = CacheConfig {
        max_bytes: 0,       // unlimited budget
        max_entry_bytes: 8, // tiny: ROW_HEADER_LEN alone is 80 bytes
        debug_mode: false,
    };
    let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

    let key = sample_key("too big", 4);
    cache.put(&key, &[1.0, 2.0, 3.0, 4.0], None).unwrap();

    // The entry should have been rejected (not stored).
    assert_eq!(cache.row_count().unwrap(), 0);

    let telemetry = cache.telemetry();
    assert_eq!(telemetry.entry_size_rejections, 1);
    assert!(telemetry.bytes_rejected > 0);
}

/// Task 6: Byte-budget enforcement evicts unreferenced rows.
#[test]
fn test_byte_budget_eviction_removes_unreferenced() {
    let tmp = tempfile::tempdir().unwrap();
    let dim = 4u32;
    // max_bytes fits about 1 entry (header=80 + payload=16 = 96 bytes).
    // Set max_bytes to 100 to allow one entry, then overflow on second.
    let config = CacheConfig {
        max_bytes: 100,
        max_entry_bytes: u64::MAX,
        debug_mode: false,
    };
    let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

    let key1 = sample_key("first entry", dim);
    let key2 = sample_key("second entry", dim);

    // Put first entry.
    cache.put(&key1, &[1.0, 2.0, 3.0, 4.0], None).unwrap();
    assert_eq!(cache.row_count().unwrap(), 1);

    // Put second entry — should trigger eviction of key1 (unreferenced).
    cache.put(&key2, &[5.0, 6.0, 7.0, 8.0], None).unwrap();
    assert_eq!(
        cache.row_count().unwrap(),
        1,
        "eviction should maintain count"
    );

    // key1 should have been evicted.
    assert!(cache.get(&key1).unwrap().is_none());
    // key2 should be present.
    assert!(cache.get(&key2).unwrap().is_some());

    let telemetry = cache.telemetry();
    assert!(telemetry.evictions >= 1, "should have recorded evictions");
    assert!(telemetry.bytes_evicted > 0);
}

/// Task 6: Byte-budget enforcement does NOT evict referenced rows.
#[test]
fn test_byte_budget_eviction_preserves_referenced() {
    let tmp = tempfile::tempdir().unwrap();
    let dim = 4u32;
    let config = CacheConfig {
        max_bytes: 100,
        max_entry_bytes: u64::MAX,
        debug_mode: false,
    };
    let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

    let key1 = sample_key("referenced", dim);
    cache.put(&key1, &[1.0, 2.0, 3.0, 4.0], None).unwrap();
    let fp = key1.fingerprint();
    cache.add_reference(&fp, "project-a", 1);

    let key2 = sample_key("newcomer", dim);
    cache.put(&key2, &[5.0, 6.0, 7.0, 8.0], None).unwrap();

    // key1 must survive — it has a live project reference.
    assert!(cache.get(&key1).unwrap().is_some());
}

/// Task 6: Telemetry persists across open/reopen.
#[test]
fn test_telemetry_persists_across_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("persist telemetry", 4);
    cache.put(&key, &[1.0, 2.0, 3.0, 4.0], None).unwrap();
    cache.probe(&[key]).unwrap();

    let telemetry_before = cache.telemetry().clone();
    assert_eq!(telemetry_before.hits, 1);

    // Persist and reopen.
    cache.persist_telemetry().unwrap();
    let cache2 = GlobalEmbeddingCache::open(tmp.path()).unwrap();
    let telemetry_after = cache2.telemetry().clone();
    assert_eq!(telemetry_after.hits, 1);
}

/// Task 6: GC compaction telemetry tracks evictions with byte accounting.
#[test]
fn test_gc_compaction_telemetry() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    // Add many unreferenced rows.
    for i in 0..10 {
        let key = sample_key(&format!("dead row {i}"), 4);
        cache.put(&key, &[1.0, 2.0, 3.0, 4.0], None).unwrap();
    }
    assert_eq!(cache.row_count().unwrap(), 10);

    // GC should evict all unreferenced rows.
    let report = cache.gc().unwrap();
    assert_eq!(report.rows_removed, 10);
    assert!(report.reclaimed_bytes > 0);

    let telemetry = cache.telemetry();
    assert_eq!(
        telemetry.evictions, 10,
        "should have telemetry for each eviction"
    );
    assert_eq!(telemetry.bytes_evicted, report.reclaimed_bytes);
}

/// Task 6: cache_stats report is serializable and includes generation/model
/// invalidation key info (spec section 10.3 — count-only prohibited).
#[test]
fn test_cache_stats_report_includes_telemetry_and_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key1 = sample_key("report key 1", 4);
    let key2 = sample_key("report key 2", 4);
    cache.put(&key1, &[0.1, 0.2, 0.3, 0.4], None).unwrap();
    cache.put(&key2, &[0.5, 0.6, 0.7, 0.8], None).unwrap();

    // Probe for telemetry.
    cache.probe(&[key1.clone(), key2.clone()]).unwrap();

    // Also create a miss.
    let key3 = sample_key("miss key", 4);
    cache.probe(&[key3]).unwrap();

    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.row_count, 2);
    assert_eq!(stats.telemetry.hits, 2);
    assert_eq!(stats.telemetry.misses, 1);
    assert!(stats.cache_bytes > 0);
    assert!(stats.max_bytes > 0);
    assert!(stats.max_entry_bytes > 0);
    // model_identity reports the actual model digest hex.
    let expected_model = hex_encode(&CacheKey::model_digest(b"model-bytes"));
    assert_eq!(stats.model_identity, expected_model);

    // Verify the report is serializable (for JSON output).
    let json = serde_json::to_string(&stats).unwrap();
    assert!(json.contains("cache_bytes"));
    assert!(json.contains("hit_ratio"));
    assert!(json.contains("telemetry"));
    assert!(json.contains("max_bytes"));
    assert!(json.contains("model_identity"));
}

// ── WS10 Task 8: Cache-effectiveness measurement (§13 scenario 22) ──

/// Task 8 / VAL-CACHE-012: Two-worktree cache hit ratio.
///
/// Index two projects with substantial content overlap (simulating two
/// git worktrees from the same repo). The second index should see cache
/// hits for all shared content.
#[test]
fn test_two_worktree_cache_hit_ratio() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let dim = 4u32;
    // Shared content: 10 identical texts across two "worktrees."
    let shared_texts: Vec<String> = (0..10)
        .map(|i| format!("fn func_{i}(x: i32) -> i32 {{ x + {i} }}"))
        .collect();

    let keys_a: Vec<CacheKey> = shared_texts.iter().map(|t| sample_key(t, dim)).collect();

    // Simulate embedding project A (first worktree).
    for key in &keys_a {
        cache.put(key, &[0.1, 0.2, 0.3, 0.4], None).unwrap();
    }

    // Simulate probing project B (second worktree with same content).
    let keys_b: Vec<CacheKey> = shared_texts.iter().map(|t| sample_key(t, dim)).collect();

    let result = cache.probe(&keys_b).unwrap();

    // All entries should be cache hits — zero duplicate embeddings.
    assert_eq!(result.hits.len(), 10, "all shared content should hit");
    assert_eq!(result.misses.len(), 0, "no misses for identical content");
    assert_eq!(cache.row_count().unwrap(), 10, "no duplicated rows");

    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.hit_ratio, 1.0, "100% hit ratio for shared content");
    assert_eq!(stats.row_count, 10);

    // Bytes saved = vectors_that_would_have_been_computed * dim * sizeof(f32)
    let bytes_saved = result.hits.len() as u64 * dim as u64 * 4;
    assert_eq!(bytes_saved, 160, "correct bytes saved calculation");
}

/// Task 8 / VAL-CACHE-012: Partially overlapping worktrees.
#[test]
fn test_partially_overlapping_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let dim = 4u32;

    // 8 shared files, 2 unique to each worktree.
    let shared: Vec<String> = (0..8).map(|i| format!("shared_func_{i}")).collect();
    let unique_a: Vec<String> = vec!["unique_a_0".into(), "unique_a_1".into()];
    let unique_b: Vec<String> = vec!["unique_b_0".into(), "unique_b_1".into()];

    // Project A: shared + unique_a.
    let keys_a: Vec<CacheKey> = shared
        .iter()
        .chain(unique_a.iter())
        .map(|t| sample_key(t, dim))
        .collect();
    for key in &keys_a {
        cache.put(key, &[1.0, 2.0, 3.0, 4.0], None).unwrap();
    }

    // Project B: shared + unique_b.
    let keys_b: Vec<CacheKey> = shared
        .iter()
        .chain(unique_b.iter())
        .map(|t| sample_key(t, dim))
        .collect();
    let result = cache.probe(&keys_b).unwrap();

    assert_eq!(result.hits.len(), 8, "8 shared should hit");
    assert_eq!(result.misses.len(), 2, "2 unique should miss");

    let stats = cache.cache_stats().unwrap();
    let hit_ratio = stats.hit_ratio;
    assert!(
        (hit_ratio - (8.0 / 10.0)).abs() < 0.01,
        "hit ratio should be 0.8, got {hit_ratio}"
    );
}

/// Task 8 / VAL-CROSS-003: Global cache + streaming fragment dedup across
/// projects — zero duplicate ONNX inference calls for shared content.
#[test]
fn test_cross_project_zero_duplicate_embeddings() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let dim = 4u32;

    // Project A content.
    let content_a = ["file_one".to_string(), "file_two".to_string()];
    let keys_a: Vec<CacheKey> = content_a.iter().map(|t| sample_key(t, dim)).collect();

    // Simulate embedding all of project A.
    for (i, key) in keys_a.iter().enumerate() {
        cache.put(key, &[(i as f32), 1.0, 2.0, 3.0], None).unwrap();
    }

    // Project B has the SAME content (worktree of the same repo).
    let keys_b: Vec<CacheKey> = content_a.iter().map(|t| sample_key(t, dim)).collect();

    // Probe project B — all should be hits.
    let result = cache.probe(&keys_b).unwrap();
    assert_eq!(result.hits.len(), 2, "zero duplicate embeddings needed");
    assert_eq!(result.misses.len(), 0);
    assert_eq!(cache.row_count().unwrap(), 2, "no duplicate rows");
}

/// Task 8 / VAL-CACHE-007: Cache hits return bit-equivalent vectors to
/// fresh embeds (anti-cheat section 2.1 — no precision loss from caching).
#[test]
fn test_cache_hit_bit_equivalent_to_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let dim = 8;
    let key = sample_key("bit equivalence", dim as u32);
    let original: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.123_456_79).collect();

    // Store the "freshly embedded" vector.
    cache.put(&key, &original, None).unwrap();

    // Retrieve (simulating a cache hit).
    let cached = cache.get(&key).unwrap().unwrap();

    // Bit-for-bit identical (anti-cheat: no precision loss from cache).
    assert_eq!(cached.len(), original.len());
    for (a, b) in cached.iter().zip(original.iter()) {
        assert_eq!(a.to_bits(), b.to_bits(), "f32 bits must be identical");
    }
}

/// VAL-CACHE-015: No source text stored after hashing.
/// (Verify with various complex source texts.)
#[test]
fn test_val_cache_015_no_source_text_in_cache_files() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let secret_texts = vec![
        "fn process_payment(credit_card: &str) -> Result<(), Error>",
        "const API_KEY = \"sk-1234567890abcdef\"",
        "SELECT password_hash FROM users WHERE email = 'admin@test.com'",
        "private data that should never be persisted in plaintext",
    ];

    for text in &secret_texts {
        let key = sample_key(text, 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4], None).unwrap();

        // Inspect the raw row file.
        let path = cache.row_path(&key.fingerprint());
        let raw = fs::read(&path).unwrap();

        assert!(
            !raw.windows(text.len()).any(|w| w == text.as_bytes()),
            "source text '{}' must not appear in cache file",
            text
        );
    }
}

// ── VAL-CACHE-015 privacy escape hatch (LEINDEX_EMBED_CACHE_DEBUG) ──

/// Privacy gate (default): when debug_mode is OFF and no env var or feature
/// flag enables it, source text passed to `put` is dropped — never written
/// to the row file.
#[test]
fn test_debug_escape_hatch_default_private_drops_source_text() {
    // Ensure no env var or feature flag leaks in from the outside.
    let _g = crate::feature_flags::lock_flag_tests();
    crate::feature_flags::clear_flag_overrides_for_test();
    // SAFETY (env-var mutation): tests are serialized via FLAG_TEST_LOCK
    // and we restore the var at the end. std::env::set_var is safe on the
    // Linux CI where this test runs.
    // Explicitly clear the legacy debug env var.
    // SAFETY: serialized by FLAG_TEST_LOCK; documented test-only mutation.
    unsafe {
        std::env::remove_var(DEBUG_ENV_VAR);
    }

    let tmp = tempfile::tempdir().unwrap();
    let config = CacheConfig {
        debug_mode: false,
        ..CacheConfig::default()
    };
    let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

    let secret = "fn debug_should_not_store_me() { todo!() }";
    let key = sample_key(secret, 4);

    // Even though we pass Some(secret), privacy gate drops it.
    cache
        .put(&key, &[0.1, 0.2, 0.3, 0.4], Some(secret))
        .unwrap();

    let path = cache.row_path(&key.fingerprint());
    let raw = fs::read(&path).unwrap();

    assert!(
        !raw.windows(secret.len()).any(|w| w == secret.as_bytes()),
        "default-private mode must not persist source text"
    );

    // The helper should agree.
    assert!(!is_debug_escape_hatch_active(false));
}

/// Debug-visible path: when `CacheConfig::debug_mode` is `true` AND the
/// caller supplies source text, the text IS written after the vector
/// payload. The vector is still read back bit-identical.
#[test]
fn test_debug_escape_hatch_config_debug_mode_appends_source_text() {
    let _g = crate::feature_flags::lock_flag_tests();
    crate::feature_flags::clear_flag_overrides_for_test();
    // SAFETY: serialized by FLAG_TEST_LOCK; documented test-only mutation.
    unsafe {
        std::env::remove_var(DEBUG_ENV_VAR);
    }

    let tmp = tempfile::tempdir().unwrap();
    let config = CacheConfig {
        debug_mode: true,
        ..CacheConfig::default()
    };
    let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

    let source = "fn debug_visible_function(x: i32) -> i32 { x + 1 }";
    let key = sample_key(source, 4);
    let vector = [0.5, 0.6, 0.7, 0.8];

    cache.put(&key, &vector, Some(source)).unwrap();

    // 1. The source text appears in the raw row file.
    let path = cache.row_path(&key.fingerprint());
    let raw = fs::read(&path).unwrap();
    assert!(
        raw.windows(source.len()).any(|w| w == source.as_bytes()),
        "debug_mode must persist source text after the vector payload"
    );

    // 2. The vector still reads back bit-identical (debug suffix is ignored).
    let recovered = cache.get(&key).unwrap().unwrap();
    assert_eq!(recovered.len(), vector.len());
    for (a, b) in recovered.iter().zip(vector.iter()) {
        assert_eq!(a.to_bits(), b.to_bits());
    }

    // 3. The total row size includes the debug appendix (len prefix + text).
    let expected_min = ROW_HEADER_LEN + 4 * 4 + 4 + source.len();
    assert!(
        raw.len() >= expected_min,
        "row must include debug appendix: got {} bytes, need at least {expected_min}",
        raw.len()
    );

    assert!(is_debug_escape_hatch_active(true));
}

/// If debug mode is enabled but the caller passes `None` (no source text),
/// no debug appendix is written — the row is the standard layout.
#[test]
fn test_debug_escape_hatch_no_source_text_no_appendix() {
    let tmp = tempfile::tempdir().unwrap();
    let config = CacheConfig {
        debug_mode: true,
        ..CacheConfig::default()
    };
    let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

    let key = sample_key("no source supplied", 4);
    cache.put(&key, &[1.0, 2.0, 3.0, 4.0], None).unwrap();

    let path = cache.row_path(&key.fingerprint());
    let raw = fs::read(&path).unwrap();
    // Should be exactly the standard layout (no debug appendix).
    assert_eq!(raw.len(), ROW_HEADER_LEN + 4 * 4);
}

/// The feature flag (`LEINDEX_FEATURE_EMBED_CACHE_DEBUG`) also enables the
/// debug escape hatch without touching `CacheConfig::debug_mode`.
#[test]
fn test_debug_escape_hatch_feature_flag_enables_debug() {
    let _g = crate::feature_flags::lock_flag_tests();
    crate::feature_flags::clear_flag_overrides_for_test();
    // SAFETY: serialized by FLAG_TEST_LOCK; documented test-only mutation.
    unsafe {
        std::env::remove_var(DEBUG_ENV_VAR);
    }

    let tmp = tempfile::tempdir().unwrap();
    let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();
    let mut cache = cache;

    let source = "fn flag_enabled_debug() -> u32 { 42 }";
    let key = sample_key(source, 4);

    // Override the feature flag ON.
    crate::feature_flags::set_flag_override_for_test(
        crate::feature_flags::FeatureFlag::DebugEscapeHatch,
        true,
    );

    let vector = [0.9, 0.8, 0.7, 0.6];
    cache.put(&key, &vector, Some(source)).unwrap();

    let path = cache.row_path(&key.fingerprint());
    let raw = fs::read(&path).unwrap();
    assert!(
        raw.windows(source.len()).any(|w| w == source.as_bytes()),
        "FeatureFlag::DebugEscapeHatch must enable source text persistence"
    );

    // Vector still intact.
    let recovered = cache.get(&key).unwrap().unwrap();
    assert_eq!(recovered, vector);

    // Helper reflects the flag.
    assert!(is_debug_escape_hatch_active(false));

    crate::feature_flags::clear_flag_overrides_for_test();
}

/// The legacy `LEINDEX_EMBED_CACHE_DEBUG` env var also enables the hatch.
/// SAFETY: env-var mutation is serialized behind FLAG_TEST_LOCK; we restore
/// the var at the end of the test.
#[test]
fn test_debug_escape_hatch_env_var_enables_debug() {
    let _g = crate::feature_flags::lock_flag_tests();
    crate::feature_flags::clear_flag_overrides_for_test();
    // Snapshot the prior value so we can restore it.
    let prior = std::env::var(DEBUG_ENV_VAR).ok();
    // SAFETY: serialized by FLAG_TEST_LOCK; documented test-only mutation.
    unsafe {
        std::env::set_var(DEBUG_ENV_VAR, "1");
    }

    // Cache was opened with default config (debug_mode = false), but env
    // var flips the hatch at put() time.
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    // Sanity: the helper sees the env var.
    assert!(
        is_debug_escape_hatch_active(false),
        "LEINDEX_EMBED_CACHE_DEBUG=1 should activate the escape hatch"
    );

    let source = "fn env_var_debug_path() {}";
    let key = sample_key(source, 2);
    cache.put(&key, &[0.1, 0.2], Some(source)).unwrap();

    let path = cache.row_path(&key.fingerprint());
    let raw = fs::read(&path).unwrap();
    assert!(
        raw.windows(source.len()).any(|w| w == source.as_bytes()),
        "LEINDEX_EMBED_CACHE_DEBUG=1 must persist source text"
    );

    // Restore.
    // SAFETY: serialized by FLAG_TEST_LOCK; documented test-only mutation.
    match prior {
        Some(v) => unsafe { std::env::set_var(DEBUG_ENV_VAR, v) },
        None => unsafe { std::env::remove_var(DEBUG_ENV_VAR) },
    }
}

/// Probe still returns bit-identical vectors for rows that carry a debug
/// appendix (read_row must ignore trailing bytes when re-hashing).
#[test]
fn test_probe_round_trips_through_debug_row() {
    let _g = crate::feature_flags::lock_flag_tests();
    crate::feature_flags::clear_flag_overrides_for_test();
    // SAFETY: serialized by FLAG_TEST_LOCK; documented test-only mutation.
    unsafe {
        std::env::remove_var(DEBUG_ENV_VAR);
    }

    let tmp = tempfile::tempdir().unwrap();
    let config = CacheConfig {
        debug_mode: true,
        ..CacheConfig::default()
    };
    let mut cache = GlobalEmbeddingCache::open_with_config(tmp.path(), config).unwrap();

    let source = "fn probe_with_debug(row: &[u8]) -> bool { true }";
    let key = sample_key(source, 4);
    let original = [0.123_456_79, -0.654_321, 1.0, 0.0];
    cache.put(&key, &original, Some(source)).unwrap();

    // Reopen so we exercise the on-disk read path (not in-memory state).
    let mut reopened = GlobalEmbeddingCache::open(tmp.path()).unwrap();
    // But reopened uses the default config (debug_mode = false). The debug
    // appendix must still be ignored on read.
    let result = reopened.probe(std::slice::from_ref(&key)).unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.misses.len(), 0);
    let v = result.hits.get(&0).unwrap();
    for (a, b) in v.iter().zip(original.iter()) {
        assert_eq!(a.to_bits(), b.to_bits());
    }
}

// ── Model identity (fix-sp5-model-identity) ──────────────────────

/// Helper: create a CacheKey with a specific model digest.
fn key_with_model(model_bytes: &[u8], content: &str, dim: u32) -> CacheKey {
    CacheKey {
        model_digest: CacheKey::model_digest(model_bytes),
        tokenizer_digest: CacheKey::tokenizer_digest(b"tokenizer-config"),
        prompt_role_and_version: 0,
        pooling: Pooling::Mean,
        normalization: Normalization::L2,
        output_dimensions: dim,
        content_hash: CacheKey::content_hash(content),
    }
}

/// model_identity is "none" when the cache has no rows.
#[test]
fn test_model_identity_none_for_empty_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();
    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.model_identity, "none");
}

/// model_identity reports the actual model digest hex when all rows
/// share the same model.
#[test]
fn test_model_identity_single_model_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let model_bytes = b"my-specific-model-v1";
    let expected_hex = hex_encode(&CacheKey::model_digest(model_bytes));

    // Put multiple rows under the same model.
    let key1 = key_with_model(model_bytes, "text one", 4);
    let key2 = key_with_model(model_bytes, "text two", 4);
    let key3 = key_with_model(model_bytes, "text three", 4);

    cache.put(&key1, &[0.1, 0.2, 0.3, 0.4], None).unwrap();
    cache.put(&key2, &[0.5, 0.6, 0.7, 0.8], None).unwrap();
    cache.put(&key3, &[0.9, 1.0, 1.1, 1.2], None).unwrap();

    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.row_count, 3);
    assert_eq!(
        stats.model_identity, expected_hex,
        "model_identity must report the actual model digest hex"
    );
}

/// model_identity is "multiple" when rows from different models exist.
#[test]
fn test_model_identity_multiple_models() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key_a = key_with_model(b"model-alpha", "shared text", 4);
    let key_b = key_with_model(b"model-beta", "shared text", 4);

    cache.put(&key_a, &[1.0, 2.0, 3.0, 4.0], None).unwrap();
    cache.put(&key_b, &[5.0, 6.0, 7.0, 8.0], None).unwrap();

    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.row_count, 2);
    assert_eq!(
        stats.model_identity, "multiple",
        "model_identity must be 'multiple' when different model digests are cached"
    );
}

/// model_identity survives cache reopen (model_index persisted).
#[test]
fn test_model_identity_persists_across_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let model_bytes = b"persist-model-test";
    let expected_hex = hex_encode(&CacheKey::model_digest(model_bytes));

    {
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();
        let key = key_with_model(model_bytes, "persist content", 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4], None).unwrap();
    }

    // Reopen and check model_identity is still the correct digest.
    let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();
    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.model_identity, expected_hex);
}

/// model_identity reflects the actual digest, not a fixed opaque string.
#[test]
fn test_model_identity_not_opaque_string() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = key_with_model(b"unique-model-bytes-123", "content", 4);
    cache.put(&key, &[1.0, 2.0, 3.0, 4.0], None).unwrap();

    let stats = cache.cache_stats().unwrap();
    // Must NOT be the old opaque string.
    assert_ne!(
        stats.model_identity, "content-addressed (model-digest namespaced)",
        "model_identity must not be the old fixed opaque string"
    );
    // Must be a 64-character hex string (32-byte blake3 digest).
    assert_eq!(stats.model_identity.len(), 64);
    assert!(
        stats.model_identity.chars().all(|c| c.is_ascii_hexdigit()),
        "model_identity must be a valid hex string"
    );
}

/// After GC removes rows, model_identity is recomputed correctly.
#[test]
fn test_model_identity_after_gc() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    // Two models, one referenced (survives GC), one not (evicted).
    let key_survive = key_with_model(b"survive-model", "survive content", 4);
    let key_evict = key_with_model(b"evict-model", "evict content", 4);

    cache
        .put(&key_survive, &[1.0, 2.0, 3.0, 4.0], None)
        .unwrap();
    cache.put(&key_evict, &[5.0, 6.0, 7.0, 8.0], None).unwrap();

    // Before GC: two models → "multiple".
    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.model_identity, "multiple");

    // Reference the survive key so it isn't collected.
    let fp = key_survive.fingerprint();
    cache.add_reference(&fp, "project-a", 1);

    // GC removes the unreferenced key.
    let report = cache.gc().unwrap();
    assert_eq!(report.rows_removed, 1);

    // After GC: only one model remains → its digest.
    let expected_hex = hex_encode(&CacheKey::model_digest(b"survive-model"));
    let stats = cache.cache_stats().unwrap();
    assert_eq!(stats.row_count, 1);
    assert_eq!(stats.model_identity, expected_hex);
}

// -------- staging naming (round-8 Codex P2 + Kilo WARNING) ----------------

/// Staging names must be unique per writer AND keep the trailing `.partial`:
/// the uniqueness stops two processes caching the same fingerprint from
/// sharing one staging inode (one rename publishing the other's half-written
/// row), and the trailing suffix keeps every name-based staging detector
/// (`row_file_paths`/gc/eviction, `row_count`, `total_bytes`,
/// `compute_model_identity`) recognizing them — a staging file must never be
/// simultaneously invisible as staging and visible as data.
#[test]
fn test_staging_path_is_unique_per_call_and_trailing_partial() {
    let final_path = std::path::Path::new("/cache/rows/ab/0123456789abcdef");
    let first = staging_path_for(final_path);
    let second = staging_path_for(final_path);
    assert_ne!(first, second, "each writer/call gets its own staging name");
    let first = first.to_string_lossy().into_owned();
    assert!(
        first.ends_with(".partial"),
        "trailing .partial keeps the staging detectors working: {first}"
    );
    assert_eq!(
        first.rsplit('.').next(),
        Some("partial"),
        "extension is exactly 'partial'"
    );
    assert!(
        first.contains(&format!(".{}.", std::process::id())),
        "pid is part of the unique suffix: {first}"
    );
}

/// Round-10 Kilo: staging files under BOTH naming conventions must be
/// invisible to every row detector and reclaimable by gc. The intermediate
/// `<hex>.partial.<pid>.<seq>` spelling (extension = seq) used to count as a
/// real row everywhere: its bytes were charged against the byte budget
/// forever while `gc_row`/eviction bailed out in `hex_decode` — eviction
/// freed live rows to make room for a phantom.
#[test]
fn test_staging_files_under_both_conventions_are_invisible_and_reclaimable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

    let key = sample_key("live text", 4);
    cache.put(&key, &sample_vector(4, 0.1), None).unwrap();
    let fingerprint = key.fingerprint();
    cache.add_reference(&fingerprint, "project-a", 1);
    assert_eq!(cache.row_count().unwrap(), 1);

    // Legacy leftovers in the row's shard directory: the pre-rename
    // convention (`<hex>.partial.<pid>.<seq>`) and the current one
    // (`<hex>.<pid>.<seq>.partial`).
    let row_path = cache.row_path(&fingerprint);
    let hex = row_path.file_name().unwrap().to_string_lossy().to_string();
    let shard = row_path.parent().unwrap().to_path_buf();
    let legacy = shard.join(format!("{}.partial.4242.7", hex));
    let current = shard.join(format!("{}.999.1.partial", hex));
    std::fs::write(&legacy, b"stale-staging-bytes").unwrap();
    std::fs::write(&current, b"fresh-staging-bytes").unwrap();

    // The detectors agree: staging is not a row under either convention.
    assert_eq!(
        cache.row_count().unwrap(),
        1,
        "staging files must not be counted as rows"
    );
    let row_bytes = std::fs::metadata(&row_path).unwrap().len();
    assert_eq!(
        cache.total_bytes().unwrap(),
        row_bytes,
        "staging bytes must not be charged against the byte budget"
    );

    // And gc reclaims them without touching the live row.
    let report = cache.gc().unwrap();
    assert_eq!(report.staging_files_removed, 2);
    assert_eq!(report.rows_removed, 0);
    assert_eq!(report.rows_retained, 1);
    assert!(!legacy.exists() && !current.exists());
    assert!(row_path.exists(), "the live row survives");
    assert!(cache.get(&key).unwrap().is_some());
}
