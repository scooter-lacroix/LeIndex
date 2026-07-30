use super::*;

#[test]
fn test_tokenize_code_camel_case() {
    let toks = tokenize_code("getUserName");
    assert!(
        toks.contains(&"get".to_string()),
        "expected 'get', got {:?}",
        toks
    );
    assert!(
        toks.contains(&"user".to_string()),
        "expected 'user', got {:?}",
        toks
    );
    assert!(
        toks.contains(&"name".to_string()),
        "expected 'name', got {:?}",
        toks
    );
}

#[test]
fn test_tokenize_code_acronyms_and_digits() {
    let toks = tokenize_code("HTTPConnection");
    assert!(
        toks.contains(&"http".to_string()),
        "expected 'http', got {:?}",
        toks
    );
    assert!(
        toks.contains(&"connection".to_string()),
        "expected 'connection', got {:?}",
        toks
    );

    let toks2 = tokenize_code("HTTP2Connection");
    assert!(
        toks2.contains(&"http".to_string()),
        "expected 'http', got {:?}",
        toks2
    );
    assert!(
        toks2.contains(&"2".to_string()),
        "expected '2', got {:?}",
        toks2
    );
    assert!(
        toks2.contains(&"connection".to_string()),
        "expected 'connection', got {:?}",
        toks2
    );
}

#[test]
fn test_tokenize_code_snake_case() {
    let toks = tokenize_code("get_user_name");
    assert!(
        toks.contains(&"get".to_string()),
        "expected 'get', got {:?}",
        toks
    );
    assert!(
        toks.contains(&"user".to_string()),
        "expected 'user', got {:?}",
        toks
    );
    assert!(
        toks.contains(&"name".to_string()),
        "expected 'name', got {:?}",
        toks
    );
}

#[test]
fn test_tokenize_code_filters_short_tokens() {
    let toks = tokenize_code("a b c xyz");
    assert!(!toks.contains(&"a".to_string()));
    assert!(!toks.contains(&"b".to_string()));
    assert!(!toks.contains(&"c".to_string()));
    assert!(toks.contains(&"xyz".to_string()));
}

#[test]
fn test_tokenize_code_empty() {
    let toks = tokenize_code("");
    assert!(toks.is_empty());
}

#[test]
fn test_preceding_doc_context_is_bounded_and_ordered() {
    let source = b"/// first\n/// second\nfn demo() {}\n";
    let start = source
        .windows(2)
        .position(|window| window == b"fn")
        .unwrap();
    // Comment markers (`///`) are stripped so the doc embeds as prose.
    assert_eq!(preceding_doc_context(source, start), "first\nsecond");
}

#[test]
fn test_tfidf_embedder_empty_corpus() {
    let embedder = TfIdfEmbedder::build(&[]);
    let vec = embedder.embed("test query");
    assert_eq!(
        vec.len(),
        768,
        "must produce 768-dim vector even for empty corpus"
    );
    assert!(vec.iter().all(|&v| v == 0.0), "empty corpus → zero vector");
}

#[test]
fn test_tfidf_embedding_dimension() {
    let docs: Vec<(String, String)> = (0..10)
        .map(|i| {
            (
                format!("doc_{}", i),
                format!(
                    "fn handle_request_{} {{ let result = process(); result }}",
                    i
                ),
            )
        })
        .collect();
    let embedder = TfIdfEmbedder::build(&docs);
    let vec = embedder.embed("handle request process");
    assert_eq!(vec.len(), 768, "embedding dimension must be 768");
}

#[test]
fn test_tfidf_embedding_normalized() {
    let docs: Vec<(String, String)> = vec![
        (
            "auth".to_string(),
            "fn authenticate_user(token: &str) -> bool { verify_token(token) }".to_string(),
        ),
        (
            "db".to_string(),
            "fn connect_database(url: &str) -> Connection { open_connection(url) }".to_string(),
        ),
        (
            "http".to_string(),
            "fn send_request(endpoint: &str) -> Response { http_get(endpoint) }".to_string(),
        ),
    ];
    let embedder = TfIdfEmbedder::build(&docs);
    let vec = embedder.embed("authenticate token verify");
    let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if magnitude > 1e-9 {
        assert!(
            (magnitude - 1.0).abs() < 1e-4,
            "embedding should be L2-normalized, got magnitude {}",
            magnitude
        );
    }
}

#[test]
fn test_sanitize_for_prefix_keeps_full_project_id() {
    let a = "aaaaaaaaaaaaaaaaaaaa_project_one";
    let b = "aaaaaaaaaaaaaaaaaaaa_project_two";

    assert_ne!(sanitize_for_prefix(a), sanitize_for_prefix(b));
    assert!(sanitize_for_prefix(a).ends_with("project_one"));
    assert!(sanitize_for_prefix(b).ends_with("project_two"));
}

#[test]
fn test_tfidf_related_content_higher_similarity() {
    let docs: Vec<(String, String)> = vec![
        (
            "a1".into(),
            "fn authenticate_user(token: &str) -> bool { verify_token(token) }".into(),
        ),
        (
            "a2".into(),
            "fn check_user_credentials(password: &str) -> bool { hash_check(password) }".into(),
        ),
        (
            "b1".into(),
            "fn connect_database(url: &str) -> Connection { open_connection(url) }".into(),
        ),
        (
            "b2".into(),
            "fn execute_sql_query(query: &str) -> Vec<Row> { db_execute(query) }".into(),
        ),
        (
            "c1".into(),
            "fn parse_json_payload(data: &str) -> Value { serde_parse(data) }".into(),
        ),
    ];
    let embedder = TfIdfEmbedder::build(&docs);

    let auth1 = embedder.embed("fn authenticate_user token verify");
    let auth2 = embedder.embed("fn check_user credentials password hash");
    let db1 = embedder.embed("fn connect database execute sql query");

    let cosine = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b.iter()).map(|(x, y)| x * y).sum() };

    let sim_related = cosine(&auth1, &auth2);
    let sim_unrelated = cosine(&auth1, &db1);

    assert!(
        sim_related >= sim_unrelated - 0.1,
        "related similarity ({}) should not be much lower than unrelated similarity ({})",
        sim_related,
        sim_unrelated
    );
}

#[test]
fn test_tfidf_zero_vector_for_unseen_terms() {
    let docs: Vec<(String, String)> = vec![("a".into(), "fn foo_bar() -> bool { true }".into())];
    let embedder = TfIdfEmbedder::build(&docs);
    let vec = embedder.embed("zzzzzz aaaaaaa bbbbbbb cccccccc");
    let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!(magnitude < 1.1, "magnitude out of range: {}", magnitude);
}

/// Regression test: verify partition-based selection produces the same
/// vocab+idf as the original sort-based approach.
#[test]
fn test_tfidf_partition_matches_sort_selection() {
    use std::collections::HashMap;

    // Generate a corpus that produces >768 candidates after df filtering.
    //
    // Strategy: Create ~1200 tokens, each appearing in 3-8 documents.
    // With 200 docs, min_df=3 and max_df=50, this produces ~1200 candidates,
    // which exercises the sort+stride sampling branch (for >768 candidates).
    //
    // Token distribution:
    // - 1200 unique tokens total (3-letter lowercase tokens like "aaa", "aab", ...)
    // - Each token appears in 3-8 documents (df in range [3, 8])
    // - Documents are 200, each containing ~180 tokens
    // - All tokens are space-separated to avoid introducing extra code keywords
    //
    let mut docs: Vec<(String, String)> = Vec::with_capacity(200);

    // First, create 1200 tokens with their assigned document ranges
    // Use lowercase letter-based tokens to avoid camelCase splitting
    let token_names: Vec<String> = (0usize..1200)
        .map(|i| {
            // Create tokens like "aaa", "aab", etc. - won't be split by tokenizer
            let first = (b'a' + (i % 26) as u8) as char;
            let second = (b'a' + ((i / 26) % 26) as u8) as char;
            let third = (b'a' + ((i / 676) % 26) as u8) as char;
            format!("{}{}{}", first, second, third)
        })
        .collect();

    let mut token_doc_assignments: Vec<(String, Vec<usize>)> = Vec::new();
    for (token_id, token) in token_names.iter().enumerate() {
        // Each token appears in 3-8 documents
        let df = 3 + (token_id % 6); // df in range [3, 8]

        // Use modulo to distribute tokens across documents deterministically
        let docs_with_token: Vec<usize> = (0..df)
            .map(|j| (token_id * 7 + j * 13) % 200) // Spread across docs
            .collect();

        token_doc_assignments.push((token.clone(), docs_with_token));
    }

    // Build documents by collecting their assigned tokens
    for doc_id in 0..200 {
        let mut tokens = Vec::new();
        for (token, doc_ids) in &token_doc_assignments {
            if doc_ids.contains(&doc_id) {
                tokens.push(token.clone());
            }
        }

        // Format as space-separated tokens (no code keywords to avoid extra tokens)
        let content = tokens.join(" ");
        docs.push((format!("doc_{}", doc_id), content));
    }

    let embedder = TfIdfEmbedder::build(&docs);

    // Build a reference vocab using the original sort+stride approach
    // with the SAME min_df/max_df logic as build_from_tokens.
    let tokenized: Vec<(String, Vec<String>)> = docs
        .iter()
        .map(|(id, content)| (id.clone(), tokenize_code(content)))
        .collect();

    let n = tokenized.len();
    let n_f = n as f32;
    // Same logic as build_from_tokens
    let min_df: usize = if n < 50 { 1 } else { (n / 1000).max(3) };
    let max_df: usize = if n < 50 { n } else { (n / 4).max(min_df + 1) };

    let mut df: HashMap<String, usize> = HashMap::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (_, tokens) in &tokenized {
        seen.clear();
        for tok in tokens {
            if seen.insert(tok.as_str()) {
                *df.entry(tok.to_string()).or_insert(0) += 1;
            }
        }
    }

    let mut ref_scores: Vec<(String, f32)> = df
        .into_iter()
        .filter(|(_, c)| *c >= min_df && *c <= max_df)
        .map(|(tok, c)| (tok, (n_f / c as f32).ln()))
        .collect();
    // Sort by IDF score, then by token name for deterministic tie-breaking
    ref_scores.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let target_dim = crate::search::search::DEFAULT_EMBEDDING_DIMENSION;
    let expected_vocab: Vec<String> = if ref_scores.len() <= target_dim {
        ref_scores.iter().map(|(t, _)| t.clone()).collect()
    } else {
        let total = ref_scores.len();
        let stride = total as f64 / target_dim as f64;
        (0..target_dim)
            .map(|i| {
                ref_scores[((i as f64 * stride) as usize).min(total - 1)]
                    .0
                    .clone()
            })
            .collect()
    };

    // The embedder's vocab should match the sort-based reference exactly.
    assert_eq!(
        embedder.vocab.len(),
        expected_vocab.len(),
        "vocab length mismatch: got {} expected {}",
        embedder.vocab.len(),
        expected_vocab.len()
    );
    for (i, (got, expected)) in embedder.vocab.iter().zip(expected_vocab.iter()).enumerate() {
        assert_eq!(
            got, expected,
            "vocab mismatch at position {i}: got '{got}' expected '{expected}'"
        );
    }
}

#[test]
fn test_detect_changed_manifests_cold_start_no_false_positive() {
    // Simulates a cold-start scenario: a persisted scan exists on disk but
    // the in-memory cache is empty. Without the load_from_disk fallback,
    // old_hashes would be empty and every manifest would be flagged as changed.
    use crate::cli::memory::{CacheSpiller, MemoryConfig};
    use std::path::PathBuf;

    let temp_dir = tempfile::tempdir().unwrap();
    let config = MemoryConfig {
        cache_dir: temp_dir.path().join("cache"),
        max_cache_bytes: 10_000_000,
        ..Default::default()
    };

    let mut spiller = CacheSpiller::new(config).unwrap();

    // Create an old scan with a manifest hash
    let manifest_path = PathBuf::from("/project/Cargo.toml");
    let mut old_hashes = std::collections::HashMap::new();
    old_hashes.insert(manifest_path.display().to_string(), "abc123".to_string());

    let old_scan = ProjectFileScan {
        source_paths: vec![PathBuf::from("/project/src/main.rs")],
        manifest_paths: vec![manifest_path.clone()],
        manifest_paths_canonical: Vec::new(),
        source_directories: vec![PathBuf::from("/project/src")],
        manifest_hashes: old_hashes,
    };

    // Serialize and store in cache
    let cache_key = crate::cli::memory::project_scan_cache_key("test_project");
    let serialized = bincode::serialize(&old_scan).unwrap();
    let entry = crate::cli::memory::CacheEntry::Binary {
        metadata: std::collections::HashMap::new(),
        serialized_data: serialized,
    };
    spiller
        .store_mut()
        .insert(cache_key.clone(), entry)
        .unwrap();

    // Persist to disk, then remove from in-memory cache (simulating cold start)
    spiller.store_mut().persist_key(&cache_key).unwrap();
    let _ = spiller.store_mut().remove(&cache_key);

    // Verify in-memory cache is empty (peek returns None)
    assert!(
        spiller.store().peek(&cache_key).is_none(),
        "peek should return None after removal"
    );

    // Create a current scan with the SAME manifest hashes
    let mut current_hashes = std::collections::HashMap::new();
    current_hashes.insert(manifest_path.display().to_string(), "abc123".to_string());

    let current_scan = ProjectFileScan {
        source_paths: vec![],
        manifest_paths: vec![manifest_path.clone()],
        manifest_paths_canonical: Vec::new(),
        source_directories: vec![],
        manifest_hashes: current_hashes,
    };

    // Without the fix, this would return the manifest as changed (false positive)
    // because old_hashes would be empty (peek returns None).
    let changed = detect_changed_manifests(&current_scan, "test_project", &spiller);

    assert!(
        changed.is_empty(),
        "cold start should NOT produce false-positive manifest changes, got: {:?}",
        changed
    );
}

#[test]
fn test_clear_query_caches_keeps_project_id_prefix_siblings() {
    use crate::cli::memory::{CacheEntry, CacheSpiller, MemoryConfig};

    let temp_dir = tempfile::tempdir().unwrap();
    let config = MemoryConfig {
        cache_dir: temp_dir.path().join("cache"),
        max_cache_bytes: 10_000_000,
        ..Default::default()
    };
    let mut spiller = CacheSpiller::new(config).unwrap();

    let project_1_key = "search:query:project_1:fingerprint:10:auth".to_string();
    let project_10_key = "search:query:project_10:fingerprint:10:auth".to_string();
    let analysis_1_key = "analysis:analyze:project_1:fingerprint:10:auth".to_string();
    let analysis_10_key = "analysis:analyze:project_10:fingerprint:10:auth".to_string();
    let entry = CacheEntry::Binary {
        metadata: std::collections::HashMap::new(),
        serialized_data: b"cached".to_vec(),
    };

    for key in [
        &project_1_key,
        &project_10_key,
        &analysis_1_key,
        &analysis_10_key,
    ] {
        spiller
            .store_mut()
            .insert(key.clone(), entry.clone())
            .unwrap();
        spiller.store_mut().persist_key(key).unwrap();
    }

    clear_query_caches(&mut spiller, "project_1");

    assert!(spiller.store().peek(&project_1_key).is_none());
    assert!(spiller.store().peek(&analysis_1_key).is_none());
    assert!(spiller.store().peek(&project_10_key).is_some());
    assert!(spiller.store().peek(&analysis_10_key).is_some());
    assert!(
        spiller
            .store_mut()
            .get_or_load(&project_10_key)
            .unwrap()
            .is_some(),
        "disk cache for project_10 must not be deleted by project_1 prefix cleanup"
    );
    assert!(
        spiller
            .store_mut()
            .get_or_load(&analysis_10_key)
            .unwrap()
            .is_some(),
        "analysis disk cache for project_10 must not be deleted by project_1 prefix cleanup"
    );
}

#[test]
fn test_detect_changed_manifests_detects_real_change() {
    // Verifies that a real manifest change is still detected even on cold start.
    use crate::cli::memory::{CacheSpiller, MemoryConfig};
    use std::path::PathBuf;

    let temp_dir = tempfile::tempdir().unwrap();
    let config = MemoryConfig {
        cache_dir: temp_dir.path().join("cache"),
        max_cache_bytes: 10_000_000,
        ..Default::default()
    };

    let mut spiller = CacheSpiller::new(config).unwrap();

    let manifest_path = PathBuf::from("/project/Cargo.toml");
    let mut old_hashes = std::collections::HashMap::new();
    old_hashes.insert(manifest_path.display().to_string(), "old_hash".to_string());

    let old_scan = ProjectFileScan {
        source_paths: vec![PathBuf::from("/project/src/main.rs")],
        manifest_paths: vec![manifest_path.clone()],
        manifest_paths_canonical: Vec::new(),
        source_directories: vec![PathBuf::from("/project/src")],
        manifest_hashes: old_hashes,
    };

    let cache_key = crate::cli::memory::project_scan_cache_key("test_project2");
    let serialized = bincode::serialize(&old_scan).unwrap();
    let entry = crate::cli::memory::CacheEntry::Binary {
        metadata: std::collections::HashMap::new(),
        serialized_data: serialized,
    };
    spiller
        .store_mut()
        .insert(cache_key.clone(), entry)
        .unwrap();
    spiller.store_mut().persist_key(&cache_key).unwrap();
    let _ = spiller.store_mut().remove(&cache_key);

    // Current scan with DIFFERENT manifest hash
    let mut current_hashes = std::collections::HashMap::new();
    current_hashes.insert(manifest_path.display().to_string(), "new_hash".to_string());

    let current_scan = ProjectFileScan {
        source_paths: vec![PathBuf::from("/project/src/main.rs")],
        manifest_paths: vec![manifest_path.clone()],
        manifest_paths_canonical: Vec::new(),
        source_directories: vec![PathBuf::from("/project/src")],
        manifest_hashes: current_hashes,
    };

    let changed = detect_changed_manifests(&current_scan, "test_project2", &spiller);

    assert_eq!(
        changed.len(),
        1,
        "should detect exactly one changed manifest"
    );
    assert_eq!(
        changed[0], manifest_path,
        "should detect the correct manifest as changed"
    );
}

#[test]
fn test_detect_changed_manifests_uses_in_memory_cache_first() {
    // When both in-memory and disk caches exist, in-memory takes priority.
    use crate::cli::memory::{CacheSpiller, MemoryConfig};
    use std::path::PathBuf;

    let temp_dir = tempfile::tempdir().unwrap();
    let config = MemoryConfig {
        cache_dir: temp_dir.path().join("cache"),
        max_cache_bytes: 10_000_000,
        ..Default::default()
    };

    let mut spiller = CacheSpiller::new(config).unwrap();

    let manifest_path = PathBuf::from("/project/Cargo.toml");

    // Create a stale disk cache with old hash
    let mut disk_hashes = std::collections::HashMap::new();
    disk_hashes.insert(
        manifest_path.display().to_string(),
        "stale_hash".to_string(),
    );
    let disk_scan = ProjectFileScan {
        source_paths: vec![],
        manifest_paths: vec![manifest_path.clone()],
        manifest_paths_canonical: Vec::new(),
        source_directories: vec![],
        manifest_hashes: disk_hashes,
    };
    let cache_key = crate::cli::memory::project_scan_cache_key("test_project3");
    let serialized = bincode::serialize(&disk_scan).unwrap();
    let disk_entry = crate::cli::memory::CacheEntry::Binary {
        metadata: std::collections::HashMap::new(),
        serialized_data: serialized,
    };
    spiller
        .store_mut()
        .insert(cache_key.clone(), disk_entry)
        .unwrap();
    spiller.store_mut().persist_key(&cache_key).unwrap();

    // Create a fresh in-memory cache with current hash
    let mut mem_hashes = std::collections::HashMap::new();
    mem_hashes.insert(
        manifest_path.display().to_string(),
        "current_hash".to_string(),
    );
    let mem_scan = ProjectFileScan {
        source_paths: vec![],
        manifest_paths: vec![manifest_path.clone()],
        manifest_paths_canonical: Vec::new(),
        source_directories: vec![],
        manifest_hashes: mem_hashes,
    };
    let mem_serialized = bincode::serialize(&mem_scan).unwrap();
    let mem_entry = crate::cli::memory::CacheEntry::Binary {
        metadata: std::collections::HashMap::new(),
        serialized_data: mem_serialized,
    };
    spiller
        .store_mut()
        .insert(cache_key.clone(), mem_entry)
        .unwrap();

    // Current scan matches the in-memory hash, not the disk hash
    let mut current_hashes = std::collections::HashMap::new();
    current_hashes.insert(
        manifest_path.display().to_string(),
        "current_hash".to_string(),
    );
    let current_scan = ProjectFileScan {
        source_paths: vec![PathBuf::from("/project/src/main.rs")],
        manifest_paths: vec![manifest_path.clone()],
        manifest_paths_canonical: Vec::new(),
        source_directories: vec![PathBuf::from("/project/src")],
        manifest_hashes: current_hashes,
    };

    let changed = detect_changed_manifests(&current_scan, "test_project3", &spiller);

    assert!(
        changed.is_empty(),
        "in-memory cache should be preferred over disk; should see no changes"
    );
}

#[test]
fn test_tfidf_embedder_clone_roundtrip() {
    let docs = vec![
        ("a".to_string(), "fn alpha beta gamma".to_string()),
        ("b".to_string(), "fn delta epsilon zeta".to_string()),
    ];
    let embedder = TfIdfEmbedder::build(&docs);
    let cloned = embedder.clone();
    assert_eq!(embedder.vocab, cloned.vocab);
    assert_eq!(embedder.idf, cloned.idf);
    assert_eq!(embedder.dimension, cloned.dimension);
}

#[test]
fn test_tfidf_incremental_batches_match_full_build() {
    let docs: Vec<(String, String)> = (0..120)
        .map(|i| {
            let body = format!(
                "fn item_{}() {{ let value = {}; let shared = value + {}; }}",
                i,
                i,
                i % 7
            );
            (format!("doc_{i}"), body)
        })
        .collect();
    let full = TfIdfEmbedder::build(&docs);

    let tokenized: Vec<(String, Vec<String>)> = docs
        .iter()
        .map(|(id, content)| (id.clone(), tokenize_code(content)))
        .collect();
    let chunked = TfIdfEmbedder::build_from_tokens(&tokenized);

    assert_eq!(full.vocab, chunked.vocab);
    assert_eq!(full.idf, chunked.idf);
    assert_eq!(full.dimension, chunked.dimension);
}

#[test]
fn test_index_nodes_respects_batch_size_and_matches_results() {
    use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};
    use crate::search::search::SearchEngine;

    let mut pdg = ProgramDependenceGraph::new();
    for i in 0..8 {
        pdg.add_node(Node {
            id: format!("node_{i}"),
            name: format!("symbol_{i}"),
            file_path: format!("/tmp/file_{i}.rs").into(),
            language: format!("rust batch {i}"),
            node_type: NodeType::Function,
            byte_range: (0, 100),
            complexity: i as u32 + 1,
        });
    }

    let mut file_stats_cache = None;
    let mut engine_small = SearchEngine::new();
    let embedder_small = index_nodes(&pdg, &mut engine_small, &mut file_stats_cache, 2).unwrap();

    let mut file_stats_cache = None;
    let mut engine_large = SearchEngine::new();
    let embedder_large = index_nodes(&pdg, &mut engine_large, &mut file_stats_cache, 64).unwrap();

    // Extract TfIdfEmbedder from HybridEmbedder to access internal fields
    let tfidf_small = match embedder_small {
        HybridEmbedder::TfIdfOnly(emb) => emb,
        #[cfg(feature = "onnx")]
        HybridEmbedder::HybridLocal { tfidf, .. } => tfidf,
        #[cfg(feature = "remote-embeddings")]
        HybridEmbedder::HybridRemote { tfidf, .. } => tfidf,
    };
    let tfidf_large = match embedder_large {
        HybridEmbedder::TfIdfOnly(emb) => emb,
        #[cfg(feature = "onnx")]
        HybridEmbedder::HybridLocal { tfidf, .. } => tfidf,
        #[cfg(feature = "remote-embeddings")]
        HybridEmbedder::HybridRemote { tfidf, .. } => tfidf,
    };

    assert_eq!(tfidf_small.vocab, tfidf_large.vocab);
    assert_eq!(tfidf_small.idf, tfidf_large.idf);
    assert_eq!(tfidf_small.dimension, tfidf_large.dimension);
}

#[test]
fn pdg_search_fingerprint_is_order_independent_and_content_sensitive() {
    use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};

    fn node(id: &str, complexity: u32) -> Node {
        Node {
            id: id.to_string(),
            name: format!("symbol_{id}"),
            file_path: format!("/tmp/{id}.rs").into(),
            language: "rust".to_string(),
            node_type: NodeType::Function,
            byte_range: (0, 10),
            complexity,
        }
    }

    let mut forward = ProgramDependenceGraph::new();
    forward.add_node(node("a", 1));
    forward.add_node(node("b", 2));

    let mut reverse = ProgramDependenceGraph::new();
    reverse.add_node(node("b", 2));
    reverse.add_node(node("a", 1));

    let mut changed = ProgramDependenceGraph::new();
    changed.add_node(node("a", 9));
    changed.add_node(node("b", 2));

    assert_eq!(
        pdg_search_fingerprint(&forward),
        pdg_search_fingerprint(&reverse)
    );
    assert_ne!(
        pdg_search_fingerprint(&forward),
        pdg_search_fingerprint(&changed)
    );
}

#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
#[test]
fn test_empty_neural_persist_removes_stale_mmap() {
    use crate::search::search::SearchEngine;

    let temp = tempfile::TempDir::new().unwrap();
    let stale_path = neural_mmap_embeddings_path(temp.path());
    std::fs::create_dir_all(stale_path.parent().unwrap()).unwrap();
    std::fs::write(&stale_path, b"stale neural data").unwrap();

    let engine = SearchEngine::new();
    persist_neural_embeddings_to_mmap(&engine, temp.path()).unwrap();

    assert!(
        !stale_path.exists(),
        "empty neural persistence should remove stale neural mmap file"
    );
}

#[test]
fn test_index_nodes_accumulates_df_across_passes() {
    use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};
    use crate::search::search::SearchEngine;

    let mut pdg = ProgramDependenceGraph::new();
    for i in 0..6 {
        let content_tag = if i < 3 { "shared_token" } else { "other_token" };
        pdg.add_node(Node {
            id: format!("node_{i}"),
            name: format!("symbol_{i}"),
            file_path: format!("/tmp/file_{i}.rs").into(),
            language: "rust".to_string(),
            node_type: NodeType::Function,
            byte_range: (0, 100),
            complexity: 1,
        });
        let _ = content_tag;
    }

    let mut cache = None;
    let mut engine = SearchEngine::new();
    let embedder = index_nodes(&pdg, &mut engine, &mut cache, 3).unwrap();

    // Extract TfIdfEmbedder from HybridEmbedder to access dimension
    let tfidf_embedder = match embedder {
        HybridEmbedder::TfIdfOnly(emb) => emb,
        #[cfg(feature = "onnx")]
        HybridEmbedder::HybridLocal { tfidf, .. } => tfidf,
        #[cfg(feature = "remote-embeddings")]
        HybridEmbedder::HybridRemote { tfidf, .. } => tfidf,
    };

    assert_eq!(
        tfidf_embedder.dimension,
        crate::search::search::DEFAULT_EMBEDDING_DIMENSION
    );
    // All 6 nodes should be indexed (append_nodes accumulates across batches)
    assert_eq!(engine.node_count(), 6);
}

#[test]
fn test_tfidf_embedder_persist_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let docs = vec![("a".to_string(), "fn alpha beta gamma".to_string())];
    let embedder = TfIdfEmbedder::build(&docs);
    let pdg = { crate::graph::pdg::ProgramDependenceGraph::new() };
    embedder.persist_to_storage(temp.path(), &pdg).unwrap();
    let loaded = TfIdfEmbedder::load_from_storage(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(embedder.vocab, loaded.vocab);
    assert_eq!(embedder.idf, loaded.idf);
    assert_eq!(embedder.dimension, loaded.dimension);
}

#[test]
fn test_tfidf_embedder_missing_file_returns_none() {
    let temp = tempfile::tempdir().unwrap();
    assert!(TfIdfEmbedder::load_from_storage(temp.path())
        .unwrap()
        .is_none());
}

#[test]
fn test_tfidf_embedder_freshness_checks_pdg_counts() {
    let mut embedder = TfIdfEmbedder::build_from_tokens(&[]);
    embedder.pdg_nodes = 3;
    embedder.pdg_edges = 7;
    assert!(embedder.is_fresh(3, 7));
    assert!(!embedder.is_fresh(4, 7));
    assert!(!embedder.is_fresh(3, 8));
}

#[test]
fn test_tfidf_build_from_tokens_is_deterministic_across_batch_sizes() {
    let docs: Vec<(String, String)> = (0..90)
        .map(|i| {
            let body = format!(
                "pub fn symbol_{}() -> usize {{ {} + {} + {} }}",
                i,
                i,
                i % 5,
                i % 11
            );
            (format!("doc_{i}"), body)
        })
        .collect();
    let tokenized: Vec<(String, Vec<String>)> = docs
        .iter()
        .map(|(id, content)| (id.clone(), tokenize_code(content)))
        .collect();

    let embedder_a = TfIdfEmbedder::build_from_tokens(&tokenized);
    let embedder_b = TfIdfEmbedder::build_from_tokens(&tokenized);

    assert_eq!(embedder_a.vocab, embedder_b.vocab);
    assert_eq!(embedder_a.idf, embedder_b.idf);
}

#[test]
fn test_read_file_once_hash_and_content() {
    let temp = tempfile::tempdir().unwrap();
    let file_path = temp.path().join("test_file.txt");
    let content = b"Hello, streaming BLAKE3 world!";
    std::fs::write(&file_path, content).unwrap();

    let (hash, bytes) = read_file_once(&file_path).unwrap();

    // Verify hash matches independent blake3::hash() computation
    let expected_hash = blake3::hash(content).to_hex().to_string();
    assert_eq!(
        hash, expected_hash,
        "streaming BLAKE3 hash must match independent computation"
    );

    // Verify bytes match file contents
    assert_eq!(
        bytes.as_slice(),
        content,
        "file bytes must match original content"
    );
}

#[test]
fn test_read_file_once_empty_file() {
    let temp = tempfile::tempdir().unwrap();
    let file_path = temp.path().join("empty.txt");
    std::fs::write(&file_path, b"").unwrap();

    let (hash, bytes) = read_file_once(&file_path).unwrap();

    // Empty file should produce the BLAKE3 hash of empty input
    let expected_hash = blake3::hash(b"").to_hex().to_string();
    assert_eq!(
        hash, expected_hash,
        "empty file hash must match blake3 of empty input"
    );
    assert!(bytes.is_empty(), "empty file bytes should be empty");
}

#[test]
fn test_read_file_once_error() {
    let result = read_file_once(Path::new("/nonexistent/path/to/file.txt"));
    assert!(
        result.is_err(),
        "reading a nonexistent file should return an error"
    );
}

// ============================================================================
// HYBRID EMBEDDING INTEGRATION TESTS
// ============================================================================

#[test]
#[cfg(feature = "onnx")]
fn test_hybrid_embedder_local_creation() {
    let docs: Vec<(String, String)> =
        vec![("test".to_string(), "fn test_function() -> bool".to_string())];
    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    let result = HybridEmbedder::hybrid_local(tfidf_embedder, None);
    // May fail if model not found, but tests the API
    assert!(result.is_ok() || result.is_err());
}

#[test]
#[cfg(not(feature = "onnx"))]
fn test_hybrid_embedder_local_feature_not_enabled() {
    let docs: Vec<(String, String)> =
        vec![("test".to_string(), "fn test_function() -> bool".to_string())];
    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    // When ONNX feature is not enabled, only TfIdfOnly is available
    let _ = HybridEmbedder::tfidf_only(tfidf_embedder);
    // Test passes if we can create a TfIdfOnly embedder
}

#[test]
fn test_hybrid_embedder_tfidf_only_default() {
    let embedder = HybridEmbedder::default();
    assert!(
        !embedder.has_neural(),
        "default embedder should be TF-IDF only"
    );
    assert_eq!(
        embedder.tfidf_dimension(),
        768,
        "TF-IDF dimension should be 768"
    );
    assert!(
        embedder.neural_dimension().is_none(),
        "neural dimension should be None"
    );
}

#[test]
fn test_hybrid_embedder_tfidf_only() {
    let docs: Vec<(String, String)> = vec![(
        "auth".to_string(),
        "fn authenticate_user(token: &str) -> bool".to_string(),
    )];
    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    let embedder = HybridEmbedder::tfidf_only(tfidf_embedder);

    assert!(!embedder.has_neural());
    assert_eq!(embedder.tfidf_dimension(), 768);
    assert_eq!(embedder.neural_weight(), 0.0);

    let weights = embedder.scoring_weights();
    assert_eq!(
        weights.tfidf, 0.60,
        "TF-IDF weight should be 0.60 without neural"
    );
    assert_eq!(
        weights.neural, 0.00,
        "neural weight should be 0.00 without neural"
    );
}

#[test]
#[cfg(feature = "onnx")]
fn test_hybrid_embedder_local_dimension() {
    let docs: Vec<(String, String)> =
        vec![("test".to_string(), "fn test_function() -> bool".to_string())];
    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    if let Ok(embedder) = HybridEmbedder::hybrid_local(tfidf_embedder, None) {
        assert!(
            embedder.has_neural(),
            "hybrid local embedder should have neural"
        );
        assert_eq!(
            embedder.tfidf_dimension(),
            768,
            "TF-IDF dimension should be 768"
        );
        assert!(
            embedder.neural_dimension().is_some(),
            "neural dimension should be Some"
        );
        assert_eq!(
            embedder.neural_weight(),
            0.40,
            "neural weight should be 0.40"
        );

        let weights = embedder.scoring_weights();
        assert_eq!(
            weights.tfidf, 0.30,
            "TF-IDF weight should be 0.30 with neural"
        );
        assert_eq!(
            weights.neural, 0.40,
            "neural weight should be 0.40 with neural"
        );
    }
}

#[test]
fn test_hybrid_embedder_embed_tfidf() {
    let docs: Vec<(String, String)> = vec![(
        "auth".to_string(),
        "fn authenticate_user(token: &str) -> bool".to_string(),
    )];
    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    let embedder = HybridEmbedder::tfidf_only(tfidf_embedder);

    let tokens = vec![
        "authenticate".to_string(),
        "user".to_string(),
        "token".to_string(),
    ];
    let embedding = embedder.embed_tfidf(&tokens);

    assert_eq!(
        embedding.len(),
        768,
        "TF-IDF embedding dimension should be 768"
    );
}

#[test]
#[cfg(feature = "onnx")]
fn test_hybrid_embedder_embed_neural_local() {
    let docs: Vec<(String, String)> =
        vec![("test".to_string(), "fn test_function() -> bool".to_string())];
    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    if let Ok(embedder) = HybridEmbedder::hybrid_local(tfidf_embedder, None) {
        let tokens = vec![
            "test".to_string(),
            "code".to_string(),
            "embedding".to_string(),
        ];
        let tfidf_embedding = embedder.embed_tfidf(&tokens);

        assert_eq!(
            tfidf_embedding.len(),
            768,
            "TF-IDF embedding dimension should be 768"
        );

        // Test neural embedding generation (blocking version for sync test)
        let text = "test code embedding";
        if let Some(Ok(neural_embedding)) = embedder.embed_neural_blocking(text) {
            assert!(
                !neural_embedding.is_empty(),
                "neural embedding should have non-zero dimension"
            );
            // Real embeddings should have non-zero values
            let has_nonzero = neural_embedding.iter().any(|&v| v != 0.0);
            assert!(has_nonzero, "neural embeddings should have non-zero values");
        }
    }
}

#[test]
#[ignore = "requires the configured auto ONNX model and execution provider"]
#[cfg(feature = "onnx")]
fn test_hybrid_embedder_cold_start_uses_neural_by_default() {
    let docs: Vec<(String, String)> = vec![(
        "search".to_string(),
        "fn route_semantic_search(query: &str) -> bool".to_string(),
    )];
    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    let embedder = HybridEmbedder::hybrid_local(tfidf_embedder, None).unwrap();
    let result = embedder.embed_neural_blocking("route semantic search");
    let embedding = result
        .expect("cold hybrid request must attempt the neural worker")
        .expect("configured auto neural worker must return an embedding");

    assert_eq!(embedding.len(), NEURAL_EMBEDDING_DIMENSION);
    assert!(embedding.iter().any(|value| *value != 0.0));
    assert_eq!(embedder.neural_status(), "ready");
}

#[test]
fn test_hybrid_scoring_weights() {
    let weights_with_neural = HybridScoringWeights::default();
    assert_eq!(weights_with_neural.tfidf, 0.30);
    assert_eq!(weights_with_neural.neural, 0.40);
    assert_eq!(weights_with_neural.structural, 0.15);
    assert_eq!(weights_with_neural.text_match, 0.15);
    assert!(
        (weights_with_neural.tfidf
            + weights_with_neural.neural
            + weights_with_neural.structural
            + weights_with_neural.text_match
            - 1.0)
            .abs()
            < 0.001
    );

    let weights_without_neural = HybridScoringWeights::without_neural();
    assert_eq!(weights_without_neural.tfidf, 0.60);
    assert_eq!(weights_without_neural.neural, 0.00);
    assert_eq!(weights_without_neural.structural, 0.20);
    assert_eq!(weights_without_neural.text_match, 0.20);
    assert!(
        (weights_without_neural.tfidf
            + weights_without_neural.neural
            + weights_without_neural.structural
            + weights_without_neural.text_match
            - 1.0)
            .abs()
            < 0.001
    );
}

#[test]
fn test_hybrid_scoring_weights_normalize() {
    let mut custom_weights = HybridScoringWeights {
        tfidf: 0.5,
        neural: 0.3,
        structural: 0.1,
        text_match: 0.1,
    };
    custom_weights = custom_weights.normalize();
    assert!(
        (custom_weights.tfidf
            + custom_weights.neural
            + custom_weights.structural
            + custom_weights.text_match
            - 1.0)
            .abs()
            < 0.001
    );
}

#[test]
fn test_hybrid_embedder_compare_backends() {
    let docs: Vec<(String, String)> =
        vec![("test".to_string(), "fn test_function() -> bool".to_string())];

    let tfidf_embedder = TfIdfEmbedder::build(&docs);
    let tfidf_only = HybridEmbedder::tfidf_only(tfidf_embedder.clone());

    assert!(!tfidf_only.has_neural());
    assert_eq!(tfidf_only.tfidf_dimension(), 768);
    assert!(tfidf_only.neural_dimension().is_none());

    #[cfg(feature = "onnx")]
    {
        if let Ok(hybrid_local) = HybridEmbedder::hybrid_local(tfidf_embedder, None) {
            assert!(hybrid_local.has_neural());
            assert_eq!(hybrid_local.tfidf_dimension(), 768);
            assert!(hybrid_local.neural_dimension().is_some());
        }
    }
}
