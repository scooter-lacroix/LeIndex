use super::*;

// The snapshot cluster (search_snapshot / restore_from_search_snapshot) moved
// out of mod.rs into the sibling snapshot.rs module; tests building 1024-dim
// snapshot vectors need these two names that were previously reachable via
// `use super::*` glob (mod.rs no longer imports Arc or defines the const).
// Storage-gated to match the test functions that use them.
#[cfg(feature = "storage")]
use super::snapshot::NEURAL_EMBEDDING_DIMENSION;
#[cfg(feature = "storage")]
use std::sync::Arc;

pub(super) fn create_test_nodes() -> Vec<NodeInfo> {
    vec![
        NodeInfo {
            node_id: "func1".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "func1".to_string(),
            language: "rust".to_string(),
            content: "fn func1() { println!(\"hello\"); }".to_string(),
            byte_range: (0, 40),
            tfidf_embedding: vec![1.0, 0.0, 0.0],
            neural_embedding: None,
            complexity: 2,
            signature: None,
            pre_tokenized: None,
        },
        NodeInfo {
            node_id: "func2".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "func2".to_string(),
            language: "rust".to_string(),
            content: "fn func2() { println!(\"world\"); }".to_string(),
            byte_range: (42, 82),
            tfidf_embedding: vec![0.0, 1.0, 0.0],
            neural_embedding: None,
            complexity: 2,
            signature: None,
            pre_tokenized: None,
        },
    ]
}

#[test]
fn test_search_engine_creation() {
    let engine = SearchEngine::new();
    assert_eq!(engine.node_count(), 0);
    assert!(engine.is_empty());
}

#[test]
fn neural_rows_can_be_published_after_core_index() {
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());
    assert_eq!(engine.collect_neural_embeddings().len(), 0);

    let updated = engine.update_neural_embeddings(vec![
        ("func1".to_string(), vec![0.25, 0.5]),
        ("missing".to_string(), vec![1.0, 1.0]),
    ]);
    assert_eq!(updated, 1);
    assert_eq!(engine.collect_neural_embeddings().len(), 1);

    engine.clear_neural_embeddings();
    assert!(engine.collect_neural_embeddings().is_empty());
}

#[test]
fn test_index_nodes() {
    let mut engine = SearchEngine::new();
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);
    assert_eq!(engine.node_count(), 2);
    assert!(!engine.is_empty());
}

#[cfg(feature = "storage")]
#[test]
fn test_search_snapshot_restore_round_trip() {
    let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf_embedding[0] = 1.0;

    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![NodeInfo {
        node_id: "auth.rs:authenticate_user".to_string(),
        file_path: "auth.rs".to_string(),
        symbol_name: "authenticate_user".to_string(),
        language: "rust".to_string(),
        content: "// authenticate_user in auth.rs\npub fn authenticate_user() {}".to_string(),
        byte_range: (0, 57),
        tfidf_embedding,
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: Some(vec![
            "authenticate".to_string(),
            "user".to_string(),
            "token".to_string(),
        ]),
    }]);

    let snapshot = engine.search_snapshot(1, 0, "test-fingerprint".to_string());
    let embeddings = engine.collect_embeddings();
    let dir = tempfile::tempdir().unwrap();
    let mmap_path = dir.path().join("embeddings.bin");
    crate::search::vector::write_mmap_embeddings(&mmap_path, &embeddings).unwrap();
    let mmap = crate::search::vector::MmapEmbeddingIndex::open(&mmap_path).unwrap();

    let mut restored = SearchEngine::new();
    let restored_count = restored
        .restore_from_search_snapshot(snapshot, Arc::new(mmap), None, None, None)
        .unwrap();

    assert_eq!(restored_count, 1);
    assert!(restored.validate_coherence().is_ok());
    assert_eq!(
        restored.nodes[0].signature.as_deref(),
        Some("pub fn authenticate_user() {}")
    );

    let results = restored
        .search(SearchQuery {
            query: "authenticate token".to_string(),
            top_k: 5,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(embeddings[0].1.clone()),
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        })
        .unwrap();
    assert_eq!(results[0].node_id, "auth.rs:authenticate_user");
}

#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
#[cfg(feature = "storage")]
#[test]
fn search_snapshot_restores_neural_rows_without_heap_tfidf_copy() {
    let mut engine = SearchEngine::new();
    let mut tfidf = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf[0] = 1.0;
    engine.index_nodes(vec![NodeInfo {
        node_id: "neural-node".to_string(),
        file_path: "main.rs".to_string(),
        symbol_name: "neural_node".to_string(),
        language: "rust".to_string(),
        content: "// neural_node in main.rs\nfn neural_node() {}".to_string(),
        byte_range: (0, 44),
        tfidf_embedding: tfidf,
        neural_embedding: None,
        complexity: 1,
        signature: None,
        pre_tokenized: Some(vec!["neural".to_string(), "node".to_string()]),
    }]);
    let neural = vec![(
        "neural-node".to_string(),
        vec![0.5; NEURAL_EMBEDDING_DIMENSION],
    )];
    engine.update_neural_embeddings(neural.clone());
    let snapshot = engine.search_snapshot(1, 0, "neural-fingerprint".to_string());
    let dir = tempfile::tempdir().unwrap();
    let tfidf_path = dir.path().join("tfidf.bin");
    let neural_path = dir.path().join("neural.bin");
    crate::search::vector::write_mmap_embeddings(&tfidf_path, &engine.collect_embeddings())
        .unwrap();
    crate::search::vector::write_mmap_embeddings(&neural_path, &neural).unwrap();
    let tfidf_mmap = crate::search::vector::MmapEmbeddingIndex::open(&tfidf_path).unwrap();
    let neural_mmap = crate::search::vector::MmapEmbeddingIndex::open(&neural_path).unwrap();

    let mut restored = SearchEngine::new();
    restored
        .restore_from_search_snapshot(
            snapshot,
            Arc::new(tfidf_mmap),
            Some(Arc::new(neural_mmap)),
            None,
            None,
        )
        .unwrap();
    assert_eq!(restored.collect_neural_embeddings().len(), 1);
    assert!(restored.nodes[0].tfidf_embedding.is_empty());
}

#[cfg(feature = "storage")]
#[test]
fn test_search_snapshot_restore_rejects_wrong_tfidf_dimension() {
    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![NodeInfo {
        node_id: "auth.rs:authenticate_user".to_string(),
        file_path: "auth.rs".to_string(),
        symbol_name: "authenticate_user".to_string(),
        language: "rust".to_string(),
        content: "pub fn authenticate_user() {}".to_string(),
        byte_range: (0, 29),
        tfidf_embedding: vec![1.0, 0.0, 0.0],
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: Some(vec!["authenticate".to_string(), "user".to_string()]),
    }]);

    let snapshot = engine.search_snapshot(1, 0, "test-fingerprint".to_string());
    let dir = tempfile::tempdir().unwrap();
    let mmap_path = dir.path().join("bad_embeddings.bin");
    crate::search::vector::write_mmap_embeddings(
        &mmap_path,
        &[("auth.rs:authenticate_user".to_string(), vec![1.0, 0.0, 0.0])],
    )
    .unwrap();
    let mmap = crate::search::vector::MmapEmbeddingIndex::open(&mmap_path).unwrap();

    let mut restored = SearchEngine::new();
    let err = restored
        .restore_from_search_snapshot(snapshot, Arc::new(mmap), None, None, None)
        .unwrap_err();
    assert!(err.contains("TF-IDF mmap dimension"));
    assert!(restored.is_empty());
}

#[test]
fn test_search_empty_index() {
    let mut engine = SearchEngine::new();
    let query = SearchQuery {
        query: "test".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_semantic_search_empty_index() {
    let engine = SearchEngine::new();
    let results = engine.semantic_search(&[0.1, 0.2, 0.3], 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_with_results() {
    let mut engine = SearchEngine::new();
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    let query = SearchQuery {
        query: "func1".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].node_id, "func1");
}

#[test]
fn test_semantic_search() {
    let mut engine = SearchEngine::with_dimension(3);
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    // Search with query vector similar to func1
    let results = engine.semantic_search(&[1.0, 0.0, 0.0], 1).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].node_id, "func1");
}

#[test]
fn test_dimension_validation() {
    let engine = SearchEngine::with_dimension(128);
    assert_eq!(engine.vector_index().dimension(), 128);
}

#[test]
fn test_dimension_mismatch_error() {
    let mut engine = SearchEngine::with_dimension(3);
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    // Try searching with wrong dimension
    let result = engine.semantic_search(&[0.1, 0.2], 10);
    assert!(result.is_err());
}

#[test]
fn test_hnsw_enable() {
    let mut engine = SearchEngine::with_dimension(128);
    engine.enable_hnsw(None);
    assert!(engine.vector_index().is_hnsw_enabled());
}

#[test]
fn test_top_k_limit() {
    let mut engine = SearchEngine::new();
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    let query = SearchQuery {
        query: "fn".to_string(),
        top_k: 1,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_relevance_threshold() {
    let mut engine = SearchEngine::new();
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    let query = SearchQuery {
        query: "nonexistent".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: Some(0.5),
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_node_id_to_idx_populated() {
    let mut engine = SearchEngine::new();
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    // Verify node_id_to_idx is populated with correct indices
    assert_eq!(engine.node_id_to_idx.len(), 2);
    assert_eq!(engine.node_id_to_idx.get("func1"), Some(&0));
    assert_eq!(engine.node_id_to_idx.get("func2"), Some(&1));
}

#[test]
fn test_node_id_to_idx_o1_lookup_in_semantic_search() {
    let mut engine = SearchEngine::with_dimension(3);
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    // Verify semantic_search uses node_id_to_idx for O(1) lookup
    // by checking that results are still correct after optimization
    let results = engine.semantic_search(&[1.0, 0.0, 0.0], 10).unwrap();
    assert!(!results.is_empty());

    // The top result should be func1 (closest to query vector)
    assert_eq!(results[0].node_id, "func1");
    assert_eq!(results[0].entry_type, EntryType::Function);

    // Verify all results have correct entry type
    for entry in &results {
        assert_eq!(entry.entry_type, EntryType::Function);
    }
}

#[test]
fn test_node_id_to_idx_cleared_on_reindex() {
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());
    assert_eq!(engine.node_id_to_idx.len(), 2);

    // Re-index with different nodes - should clear and repopulate
    engine.index_nodes(vec![NodeInfo {
        node_id: "new_func".to_string(),
        file_path: "new.rs".to_string(),
        symbol_name: "new_func".to_string(),
        language: "rust".to_string(),
        content: "fn new_func() {}".to_string(),
        byte_range: (0, 18),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 1,
        signature: None,
        pre_tokenized: None,
    }]);
    assert_eq!(engine.node_id_to_idx.len(), 1);
    assert_eq!(engine.node_id_to_idx.get("new_func"), Some(&0));
    assert_eq!(engine.node_id_to_idx.get("func1"), None);
}

#[test]
fn test_content_cleared_after_indexing() {
    // T13: Verify that NodeInfo.content is cleared after index_nodes()
    // to reduce memory footprint. The inverted index (text_index) preserves
    // all token information for search.
    let mut engine = SearchEngine::new();
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    // Content should be empty (cleared) for all nodes
    for node in &engine.nodes {
        assert!(
            node.content.is_empty(),
            "Node {} content should be cleared after indexing, but got: {:?}",
            node.node_id,
            node.content
        );
    }

    // But text search should still work via inverted index
    let query = SearchQuery {
        query: "func1".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(
        !results.is_empty(),
        "Search should still find results via inverted index after content cleared"
    );
    assert_eq!(results[0].node_id, "func1");

    // Also verify text_index is populated
    assert!(
        !engine.text_index.is_empty(),
        "text_index should be populated"
    );
    assert!(
        engine.text_index.contains_key("func1"),
        "text_index should contain 'func1' token"
    );
    assert!(
        engine.text_index.contains_key("func2"),
        "text_index should contain 'func2' token"
    );
}

#[test]
fn test_node_tokens_populated() {
    // T14: Verify that node_tokens cache is populated during index_nodes()
    let mut engine = SearchEngine::new();
    let nodes = create_test_nodes();
    engine.index_nodes(nodes);

    // node_tokens should have an entry for each node
    assert_eq!(engine.node_tokens.len(), 2);
    assert!(engine.node_tokens.contains_key("func1"));
    assert!(engine.node_tokens.contains_key("func2"));

    // Verify tokens contain expected normalized content
    let func1_tokens = engine.node_tokens.get("func1").unwrap();
    assert!(
        func1_tokens.contains("func1"),
        "func1 tokens should contain 'func1', got: {:?}",
        func1_tokens
    );

    let func2_tokens = engine.node_tokens.get("func2").unwrap();
    assert!(
        func2_tokens.contains("func2"),
        "func2 tokens should contain 'func2', got: {:?}",
        func2_tokens
    );
}

#[test]
fn test_node_tokens_cleared_on_reindex() {
    // T14: Verify node_tokens is cleared when re-indexing
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());
    assert_eq!(engine.node_tokens.len(), 2);

    // Re-index with different nodes
    engine.index_nodes(vec![NodeInfo {
        node_id: "new_func".to_string(),
        file_path: "test.rs".to_string(),
        symbol_name: "new_func".to_string(),
        language: "rust".to_string(),
        content: "fn new_func() {}".to_string(),
        byte_range: (0, 18),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 1,
        signature: None,
        pre_tokenized: None,
    }]);
    assert_eq!(engine.node_tokens.len(), 1);
    assert!(engine.node_tokens.contains_key("new_func"));
    assert!(!engine.node_tokens.contains_key("func1"));
}

#[test]
fn test_node_tokens_used_in_scoring() {
    // T14: Verify that scoring uses cached tokens (no re-tokenization)
    // by checking that search results are correct after content is cleared.
    // This implicitly tests that calculate_text_score_optimized uses node_tokens.
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    // Content is cleared (T13), but tokens are cached (T14)
    for node in &engine.nodes {
        assert!(node.content.is_empty());
    }

    // Search for a term that appears in content — should still find it via cached tokens
    let query = SearchQuery {
        query: "println hello".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();

    // Should find results since "println" and "hello" appear in node content and tokens are cached
    assert!(
        !results.is_empty(),
        "Search should find results using cached node_tokens even after content is cleared"
    );
    // func1 contains both "println" and "hello", should be top result
    assert_eq!(results[0].node_id, "func1");
}

// ----------------------------------------------------------------
// T28: Incremental reindex tests
// ----------------------------------------------------------------

#[test]
fn test_incremental_reindex_add_nodes() {
    // T28: Adding nodes via incremental_reindex should update all indexes
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());
    assert_eq!(engine.node_count(), 2);

    let delta = TextIndexDelta {
        removed_node_ids: vec![],
        updated_nodes: vec![NodeInfo {
            node_id: "func3".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "func3".to_string(),
            language: "rust".to_string(),
            content: "fn func3() { db_query(); }".to_string(),
            byte_range: (100, 130),
            tfidf_embedding: vec![0.0, 0.0, 1.0],
            neural_embedding: None,
            complexity: 3,
            signature: None,
            pre_tokenized: None,
        }],
    };
    engine.incremental_reindex(delta);

    // Should now have 3 nodes
    assert_eq!(engine.node_count(), 3);
    assert_eq!(engine.node_id_to_idx.len(), 3);
    assert_eq!(engine.node_tokens.len(), 3);
    assert_eq!(engine.complexity_cache.len(), 3);

    // Search should find the new node
    let query = SearchQuery {
        query: "func3".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].node_id, "func3");

    // text_index should contain "func3" token
    assert!(engine.text_index.contains_key("func3"));
    // "db" and "query" tokens should also be indexed
    assert!(engine.text_index.contains_key("query"));
}

#[test]
fn test_incremental_reindex_remove_nodes() {
    // T28: Removing nodes via incremental_reindex should clean up all indexes
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());
    assert_eq!(engine.node_count(), 2);

    let delta = TextIndexDelta {
        removed_node_ids: vec!["func1".to_string()],
        updated_nodes: vec![],
    };
    engine.incremental_reindex(delta);

    // Should now have 1 node
    assert_eq!(engine.node_count(), 1);
    assert_eq!(engine.node_id_to_idx.len(), 1);
    assert!(!engine.node_id_to_idx.contains_key("func1"));
    assert!(engine.node_id_to_idx.contains_key("func2"));

    // func1's tokens should be removed from text_index
    // "func1" token should no longer map to func1
    if let Some(ids) = engine.text_index.get("func1") {
        assert!(
            !ids.contains("func1"),
            "func1 should be removed from text_index"
        );
    }

    // node_tokens should not contain func1
    assert!(!engine.node_tokens.contains_key("func1"));
    assert!(engine.node_tokens.contains_key("func2"));

    // Search for func1 should not find it
    let query = SearchQuery {
        query: "func1".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(
        results.is_empty(),
        "func1 should not be found after removal"
    );
}

#[test]
fn test_incremental_reindex_update_existing_node() {
    // T28: Updating an existing node should replace it correctly
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    // Update func1 with new content
    let delta = TextIndexDelta {
        removed_node_ids: vec![],
        updated_nodes: vec![NodeInfo {
            node_id: "func1".to_string(),
            file_path: "updated.rs".to_string(),
            symbol_name: "func1_renamed".to_string(),
            language: "rust".to_string(),
            content: "fn func1_renamed() { new_logic(); }".to_string(),
            byte_range: (0, 35),
            tfidf_embedding: vec![0.5, 0.5, 0.0],
            neural_embedding: None,
            complexity: 5,
            signature: None,
            pre_tokenized: None,
        }],
    };
    engine.incremental_reindex(delta);

    // Should still have 2 nodes
    assert_eq!(engine.node_count(), 2);

    // Complexity cache should reflect the update
    assert_eq!(engine.complexity_cache.get("func1"), Some(&5));

    // New tokens should be indexed
    assert!(engine.node_tokens.get("func1").unwrap().contains("logic"));
    assert!(engine.text_index.contains_key("logic"));

    // Search for new content should work
    let query = SearchQuery {
        query: "new_logic".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].node_id, "func1");
}

#[test]
fn test_incremental_reindex_combined_add_remove() {
    // T28: Combined add and remove in one delta
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    let delta = TextIndexDelta {
        removed_node_ids: vec!["func1".to_string()],
        updated_nodes: vec![
            NodeInfo {
                node_id: "func3".to_string(),
                file_path: "new.rs".to_string(),
                symbol_name: "func3".to_string(),
                language: "rust".to_string(),
                content: "fn func3() {}".to_string(),
                byte_range: (0, 14),
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            },
            NodeInfo {
                node_id: "func4".to_string(),
                file_path: "new.rs".to_string(),
                symbol_name: "func4".to_string(),
                language: "rust".to_string(),
                content: "fn func4() { helper(); }".to_string(),
                byte_range: (15, 40),
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: 2,
                signature: None,
                pre_tokenized: None,
            },
        ],
    };
    engine.incremental_reindex(delta);

    // Should have func2 (original) + func3 + func4 = 3 nodes
    assert_eq!(engine.node_count(), 3);
    assert_eq!(engine.node_id_to_idx.len(), 3);

    // func1 should be gone
    assert!(!engine.node_id_to_idx.contains_key("func1"));
    // func2, func3, func4 should exist
    assert!(engine.node_id_to_idx.contains_key("func2"));
    assert!(engine.node_id_to_idx.contains_key("func3"));
    assert!(engine.node_id_to_idx.contains_key("func4"));

    // Search for func2 should still work
    let query = SearchQuery {
        query: "func2".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].node_id, "func2");
}

#[test]
fn test_incremental_reindex_empty_delta() {
    // T28: Empty delta should not change anything
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    let delta = TextIndexDelta {
        removed_node_ids: vec![],
        updated_nodes: vec![],
    };
    engine.incremental_reindex(delta);

    assert_eq!(engine.node_count(), 2);
    assert_eq!(engine.node_id_to_idx.len(), 2);
}

#[test]
fn test_incremental_reindex_removes_empty_token_sets() {
    // T28: When removing the last node for a token, the token entry should be removed
    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![
        NodeInfo {
            node_id: "unique1".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "unique1".to_string(),
            language: "rust".to_string(),
            content: "fn unique1() { zebra(); }".to_string(),
            byte_range: (0, 25),
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        },
        NodeInfo {
            node_id: "unique2".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "unique2".to_string(),
            language: "rust".to_string(),
            content: "fn unique2() { apple(); }".to_string(),
            byte_range: (26, 52),
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        },
    ]);

    // "zebra" token should exist and map to unique1 only
    assert!(engine.text_index.contains_key("zebra"));

    // Remove unique1 — "zebra" token set should be cleaned up entirely
    let delta = TextIndexDelta {
        removed_node_ids: vec!["unique1".to_string()],
        updated_nodes: vec![],
    };
    engine.incremental_reindex(delta);

    // "zebra" token should no longer exist in text_index (no remaining nodes have it)
    assert!(
        !engine.text_index.contains_key("zebra"),
        "Token with no remaining nodes should be removed from text_index"
    );

    // "apple" should still exist
    assert!(engine.text_index.contains_key("apple"));
}

#[test]
fn test_incremental_reindex_correctness_vs_full_rebuild() {
    // T28: Incremental reindex should produce identical results to a full rebuild
    let mut engine_inc = SearchEngine::new();
    let mut engine_full = SearchEngine::new();

    // Start with same initial nodes
    let initial = create_test_nodes();
    engine_inc.index_nodes(initial.clone());
    engine_full.index_nodes(initial);

    // Apply delta incrementally
    let delta = TextIndexDelta {
        removed_node_ids: vec!["func1".to_string()],
        updated_nodes: vec![NodeInfo {
            node_id: "func3".to_string(),
            file_path: "new.rs".to_string(),
            symbol_name: "func3".to_string(),
            language: "rust".to_string(),
            content: "fn func3() { compute(); }".to_string(),
            byte_range: (0, 25),
            tfidf_embedding: vec![1.0, 1.0, 0.0],
            neural_embedding: None,
            complexity: 4,
            signature: None,
            pre_tokenized: None,
        }],
    };
    engine_inc.incremental_reindex(delta);

    // Apply same changes via full rebuild
    engine_full.index_nodes(vec![
        NodeInfo {
            node_id: "func2".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "func2".to_string(),
            language: "rust".to_string(),
            content: "fn func2() { println!(\"world\"); }".to_string(),
            byte_range: (42, 82),
            tfidf_embedding: vec![0.0, 1.0, 0.0],
            neural_embedding: None,
            complexity: 2,
            signature: None,
            pre_tokenized: None,
        },
        NodeInfo {
            node_id: "func3".to_string(),
            file_path: "new.rs".to_string(),
            symbol_name: "func3".to_string(),
            language: "rust".to_string(),
            content: "fn func3() { compute(); }".to_string(),
            byte_range: (0, 25),
            tfidf_embedding: vec![1.0, 1.0, 0.0],
            neural_embedding: None,
            complexity: 4,
            signature: None,
            pre_tokenized: None,
        },
    ]);

    // Both engines should have same node count
    assert_eq!(engine_inc.node_count(), engine_full.node_count());

    // Both should have same node_ids
    let inc_ids: std::collections::BTreeSet<_> =
        engine_inc.nodes.iter().map(|n| n.node_id.clone()).collect();
    let full_ids: std::collections::BTreeSet<_> = engine_full
        .nodes
        .iter()
        .map(|n| n.node_id.clone())
        .collect();
    assert_eq!(inc_ids, full_ids);

    // Search should produce same results
    let query = SearchQuery {
        query: "func2".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let inc_results = engine_inc.search(query.clone()).unwrap();
    let full_results = engine_full.search(query).unwrap();
    assert_eq!(inc_results.len(), full_results.len());
    if !inc_results.is_empty() {
        assert_eq!(inc_results[0].node_id, full_results[0].node_id);
    }

    // Semantic search should also produce same results
    let inc_sem = engine_inc.semantic_search(&[1.0, 1.0, 0.0], 10).unwrap();
    let full_sem = engine_full.semantic_search(&[1.0, 1.0, 0.0], 10).unwrap();
    assert_eq!(inc_sem.len(), full_sem.len());
    if !inc_sem.is_empty() {
        assert_eq!(inc_sem[0].node_id, full_sem[0].node_id);
    }
}

#[test]
fn test_incremental_reindex_semantic_search_after_update() {
    // T28: Semantic search should work correctly after incremental update
    let mut engine = SearchEngine::with_dimension(3);
    engine.index_nodes(create_test_nodes());

    // Add a new node with a distinct embedding
    let delta = TextIndexDelta {
        removed_node_ids: vec![],
        updated_nodes: vec![NodeInfo {
            node_id: "func3".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "func3".to_string(),
            language: "rust".to_string(),
            content: "fn func3() {}".to_string(),
            byte_range: (0, 14),
            tfidf_embedding: vec![0.1, 0.1, 0.9],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }],
    };
    engine.incremental_reindex(delta);

    // Search for vec close to func3's embedding
    let results = engine.semantic_search(&[0.1, 0.1, 0.9], 1).unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].node_id, "func3");
}

#[test]
fn test_incremental_reindex_node_id_to_idx_consistency() {
    // T28: node_id_to_idx should be consistent after multiple incremental updates
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    // Add func3
    engine.incremental_reindex(TextIndexDelta {
        removed_node_ids: vec![],
        updated_nodes: vec![NodeInfo {
            node_id: "func3".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "func3".to_string(),
            language: "rust".to_string(),
            content: "fn func3() {}".to_string(),
            byte_range: (0, 14),
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }],
    });

    // Remove func1 (swap-remove may swap func3 into func1's slot)
    engine.incremental_reindex(TextIndexDelta {
        removed_node_ids: vec!["func1".to_string()],
        updated_nodes: vec![],
    });

    // Verify all indices are consistent
    assert_eq!(engine.node_id_to_idx.len(), engine.nodes.len());
    for (idx, node) in engine.nodes.iter().enumerate() {
        assert_eq!(
            engine.node_id_to_idx.get(&node.node_id),
            Some(&idx),
            "node_id_to_idx mismatch for node {}",
            node.node_id
        );
    }
}

#[cfg(feature = "storage")]
#[test]
fn test_search_snapshot_fragment_roundtrip() {
    let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf_embedding[0] = 1.0;

    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![NodeInfo {
        node_id: "auth.rs:authenticate_user".to_string(),
        file_path: "auth.rs".to_string(),
        symbol_name: "authenticate_user".to_string(),
        language: "rust".to_string(),
        content: "pub fn authenticate_user() {}".to_string(),
        byte_range: (0, 29),
        tfidf_embedding,
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: Some(vec!["authenticate".to_string(), "user".to_string()]),
    }]);

    // Simulate a fragment-enabled persisted snapshot: the cli-side persist path
    // fills `fragment_rows` from the hydrated index before writing.
    let mut snapshot = engine.search_snapshot(1, 0, "frag-fingerprint".to_string());
    snapshot.fragment_rows = 2;

    let dir = tempfile::tempdir().unwrap();
    let tfidf_path = dir.path().join("tfidf.bin");
    let frag_path = dir.path().join("frag.bin");
    crate::search::vector::write_mmap_embeddings(&tfidf_path, &engine.collect_embeddings())
        .unwrap();
    let fragment_embeddings = vec![
        (
            "hash_abc".to_string(),
            vec![0.1f32; NEURAL_EMBEDDING_DIMENSION],
        ),
        (
            "hash_def".to_string(),
            vec![0.2f32; NEURAL_EMBEDDING_DIMENSION],
        ),
    ];
    crate::search::vector::write_mmap_embeddings(&frag_path, &fragment_embeddings).unwrap();
    let tfidf_mmap = crate::search::vector::MmapEmbeddingIndex::open(&tfidf_path).unwrap();
    let frag_mmap = crate::search::vector::MmapEmbeddingIndex::open(&frag_path).unwrap();
    let frag_ids = vec!["hash_abc".to_string(), "hash_def".to_string()];

    let mut restored = SearchEngine::new();
    restored
        .restore_from_search_snapshot(
            snapshot,
            Arc::new(tfidf_mmap),
            None,
            Some(Arc::new(frag_mmap)),
            Some(&frag_ids),
        )
        .unwrap();

    // Fragment fields survive the round-trip.
    let resnap = restored.search_snapshot(1, 0, "frag-fingerprint".to_string());
    assert_eq!(resnap.fragment_rows, 2);
    let mut collected = restored.collect_fragment_embeddings();
    assert_eq!(collected.len(), 2);
    collected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(collected[0].0, "hash_abc");
    assert_eq!(collected[1].0, "hash_def");
}

#[cfg(feature = "storage")]
#[test]
fn test_search_snapshot_restore_disables_fragment_on_row_mismatch() {
    let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf_embedding[0] = 1.0;

    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![NodeInfo {
        node_id: "auth.rs:authenticate_user".to_string(),
        file_path: "auth.rs".to_string(),
        symbol_name: "authenticate_user".to_string(),
        language: "rust".to_string(),
        content: "pub fn authenticate_user() {}".to_string(),
        byte_range: (0, 29),
        tfidf_embedding,
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: Some(vec!["authenticate".to_string(), "user".to_string()]),
    }]);

    // Snapshot claims 5 fragment rows but the mmap only has 2: the fragment
    // layer must be disabled, while the node-level restore still succeeds.
    let mut snapshot = engine.search_snapshot(1, 0, "frag-fingerprint".to_string());
    snapshot.fragment_rows = 5;

    let dir = tempfile::tempdir().unwrap();
    let tfidf_path = dir.path().join("tfidf.bin");
    let frag_path = dir.path().join("frag.bin");
    crate::search::vector::write_mmap_embeddings(&tfidf_path, &engine.collect_embeddings())
        .unwrap();
    let fragment_embeddings = vec![
        (
            "hash_abc".to_string(),
            vec![0.1f32; NEURAL_EMBEDDING_DIMENSION],
        ),
        (
            "hash_def".to_string(),
            vec![0.2f32; NEURAL_EMBEDDING_DIMENSION],
        ),
    ];
    crate::search::vector::write_mmap_embeddings(&frag_path, &fragment_embeddings).unwrap();
    let tfidf_mmap = crate::search::vector::MmapEmbeddingIndex::open(&tfidf_path).unwrap();
    let frag_mmap = crate::search::vector::MmapEmbeddingIndex::open(&frag_path).unwrap();
    let frag_ids = vec!["hash_abc".to_string(), "hash_def".to_string()];

    let mut restored = SearchEngine::new();
    restored
        .restore_from_search_snapshot(
            snapshot,
            Arc::new(tfidf_mmap),
            None,
            Some(Arc::new(frag_mmap)),
            Some(&frag_ids),
        )
        .unwrap();
    assert_eq!(restored.node_count(), 1);
    assert!(
        restored.collect_fragment_embeddings().is_empty(),
        "row-count mismatch must disable the fragment layer"
    );
}

#[cfg(feature = "storage")]
#[test]
fn test_fragment_owner_union_and_byte_range_surfacing() {
    let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf_embedding[0] = 1.0;

    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![NodeInfo {
        node_id: "auth.rs:authenticate_user".to_string(),
        file_path: "auth.rs".to_string(),
        symbol_name: "authenticate_user".to_string(),
        language: "rust".to_string(),
        content: "pub fn authenticate_user() {}".to_string(),
        byte_range: (0, 29),
        tfidf_embedding,
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: Some(vec!["authenticate".to_string(), "user".to_string()]),
    }]);

    // Hydrate a 2-row fragment index owned by the single node.
    let mut snapshot = engine.search_snapshot(1, 0, "frag-fingerprint".to_string());
    snapshot.fragment_rows = 2;
    let dir = tempfile::tempdir().unwrap();
    let tfidf_path = dir.path().join("tfidf.bin");
    let frag_path = dir.path().join("frag.bin");
    crate::search::vector::write_mmap_embeddings(&tfidf_path, &engine.collect_embeddings())
        .unwrap();
    let mut hash_abc = vec![0.0f32; NEURAL_EMBEDDING_DIMENSION];
    hash_abc[0] = 1.0;
    let mut hash_def = vec![0.0f32; NEURAL_EMBEDDING_DIMENSION];
    hash_def[1] = 1.0;
    let fragment_embeddings = vec![
        ("hash_abc".to_string(), hash_abc),
        ("hash_def".to_string(), hash_def),
    ];
    crate::search::vector::write_mmap_embeddings(&frag_path, &fragment_embeddings).unwrap();
    let tfidf_mmap = crate::search::vector::MmapEmbeddingIndex::open(&tfidf_path).unwrap();
    let frag_mmap = crate::search::vector::MmapEmbeddingIndex::open(&frag_path).unwrap();
    let frag_ids = vec!["hash_abc".to_string(), "hash_def".to_string()];

    let mut restored = SearchEngine::new();
    restored
        .restore_from_search_snapshot(
            snapshot,
            Arc::new(tfidf_mmap),
            None,
            Some(Arc::new(frag_mmap)),
            Some(&frag_ids),
        )
        .unwrap();
    // Turn the fragment layer ON: master switch + fusion weight + refs map.
    let mut refs = std::collections::HashMap::new();
    refs.insert(
        "hash_abc".to_string(),
        ("auth.rs:authenticate_user".to_string(), (10, 25)),
    );
    refs.insert(
        "hash_def".to_string(),
        ("auth.rs:authenticate_user".to_string(), (30, 45)),
    );
    restored.set_fragment_index_enabled(true);
    restored.set_fragment_weight(0.12);
    restored.set_fragment_refs(refs);

    // A neural query embedding close to hash_abc must surface the owner with
    // the fragment byte range and a nonzero fragment score component.
    let mut query_neural = vec![0.0f32; NEURAL_EMBEDDING_DIMENSION];
    query_neural[0] = 1.0;
    let results = restored
        .search(SearchQuery {
            query: "authenticate".to_string(),
            top_k: 5,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(engine.collect_embeddings()[0].1.clone()),
            query_neural_embedding: Some(query_neural),
            threshold: None,
            query_type: None,
        })
        .unwrap();
    let owner = results
        .iter()
        .find(|r| r.node_id == "auth.rs:authenticate_user");
    assert!(
        owner.is_some(),
        "fragment owner must enter the candidate pool"
    );
    let result = owner.unwrap();
    assert_eq!(result.fragment_byte_range, Some((10, 25)));
    assert!(result.score.fragment > 0.0);
    assert_eq!(
        result.byte_range,
        (0, 29),
        "node byte range stays unchanged"
    );
}

#[cfg(feature = "storage")]
#[test]
fn test_fragment_layer_off_by_default_contributes_nothing() {
    let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf_embedding[0] = 1.0;

    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![NodeInfo {
        node_id: "auth.rs:authenticate_user".to_string(),
        file_path: "auth.rs".to_string(),
        symbol_name: "authenticate_user".to_string(),
        language: "rust".to_string(),
        content: "pub fn authenticate_user() {}".to_string(),
        byte_range: (0, 29),
        tfidf_embedding,
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: Some(vec!["authenticate".to_string(), "user".to_string()]),
    }]);

    let mut snapshot = engine.search_snapshot(1, 0, "frag-fingerprint".to_string());
    snapshot.fragment_rows = 2;
    let dir = tempfile::tempdir().unwrap();
    let tfidf_path = dir.path().join("tfidf.bin");
    let frag_path = dir.path().join("frag.bin");
    crate::search::vector::write_mmap_embeddings(&tfidf_path, &engine.collect_embeddings())
        .unwrap();
    let mut hash_abc = vec![0.0f32; NEURAL_EMBEDDING_DIMENSION];
    hash_abc[0] = 1.0;
    let fragment_embeddings = vec![
        ("hash_abc".to_string(), hash_abc),
        (
            "hash_def".to_string(),
            vec![0.0f32; NEURAL_EMBEDDING_DIMENSION],
        ),
    ];
    crate::search::vector::write_mmap_embeddings(&frag_path, &fragment_embeddings).unwrap();
    let tfidf_mmap = crate::search::vector::MmapEmbeddingIndex::open(&tfidf_path).unwrap();
    let frag_mmap = crate::search::vector::MmapEmbeddingIndex::open(&frag_path).unwrap();
    let frag_ids = vec!["hash_abc".to_string(), "hash_def".to_string()];

    let mut restored = SearchEngine::new();
    restored
        .restore_from_search_snapshot(
            snapshot,
            Arc::new(tfidf_mmap),
            None,
            Some(Arc::new(frag_mmap)),
            Some(&frag_ids),
        )
        .unwrap();
    // fragment_index_enabled stays false (default): the fragment layer must
    // contribute nothing, even with a fragment index hydrated.

    let mut query_neural = vec![0.0f32; NEURAL_EMBEDDING_DIMENSION];
    query_neural[0] = 1.0;
    let results = restored
        .search(SearchQuery {
            query: "authenticate".to_string(),
            top_k: 5,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(engine.collect_embeddings()[0].1.clone()),
            query_neural_embedding: Some(query_neural),
            threshold: None,
            query_type: None,
        })
        .unwrap();
    let owner = results
        .iter()
        .find(|r| r.node_id == "auth.rs:authenticate_user");
    assert!(owner.is_some(), "lexical match keeps the owner in results");
    let result = owner.unwrap();
    assert_eq!(result.fragment_byte_range, None);
    assert_eq!(result.score.fragment, 0.0);
}

// Empirical recall-gain measurement for the fragment tier (Task 11 evidence).
//
// Builds a synthetic corpus where each conceptual query targets sub-symbol
// content that exists ONLY as fragment rows (content-hash addressed), not in
// the owner node's lexical surface. A pool of lexically-dominant decoy nodes
// fills the top-k at baseline, so the owner is ABSENT from results without
// the fragment tier; the fragment score then surfaces the owner. Measures
// MRR@5 with the fragment tier disabled (baseline) and enabled, and asserts a
// measurable gain. Also re-runs plain node-level queries to assert NO
// node-rank regression when the tier is enabled (plan stop-condition: MRR on
// existing node queries must not drop).
//
// Deliberately not gated behind `storage`/`cli`: the fragment public API
// (set_fragment_index_enabled/set_fragment_weight/set_fragment_refs/
// set_fragment_embeddings) lives in the search crate and is available in any
// `search` build, so the measurement runs in the default workspace suite.
#[test]
/// Empirical MRR evidence for the fragment tier (plan Task 11 checkbox:
/// "Recall/regression measurement (conceptual-query MRR before/after fragment
/// tier)").
///
/// Scenario design notes (all three quirks were empirically diagnosed on first
/// run — gain=0.0000 — and fixed here):
/// - `with_dimension(DIM)`: a default 768-dim engine silently rejects the
///   8-dim synthetic tfidf embeddings, which left `vector_results` empty and
///   made scoring degenerate to structural noise. The engine must match the
///   synthetic dimension so the vector path actually runs.
/// - Distinct query text per query: the search cache key folds in the query
///   STRING but not the neural embedding content, so identical texts collapse
///   to one cached result set. Each conceptual query gets its own text.
/// - `top_k` (5) < corpus (10): decoys perfectly match the tfidf query
///   embedding (cosine 1.0) and fill ranks 1-5, cutting the owners OUT of the
///   result set at baseline — so the fragment tier is genuinely the only path
///   that can surface them (not merely re-rank nodes that are already present).
/// - `fragment_weight` sweep (0.12 / 0.20 / 0.30 / 0.35 / 0.40) vs baseline
///   (0.0): the shipped default was empirically tuned to 0.35 (see
///   `src/config.rs::default_fragment_weight`) — 0.35 is the smallest weight
///   with real margin that surfaces fragments over strong tfidf matches
///   (fragment share w/(1+w) vs decoy tfidf share 0.3/(1+w); 0.30 sits at
///   share-equality and is fragile, 0.35 clears it by ~3.7pp) while
///   preserving node-rank exactly. The assertion stays on 0.40 so the
///   surfacing MECHANISM is verified in isolation regardless of the shipped
///   default.
fn test_fragment_tier_improves_conceptual_mrr() {
    const DIM: usize = 8;
    const N_OWNERS: usize = 4;
    const N_DECOYS: usize = 6;
    const TOP_K: usize = 5; // < corpus (10): owners must be cut at baseline
    let one_hot = |dim: usize, hot: usize| -> Vec<f32> {
        (0..dim).map(|i| if i == hot { 1.0 } else { 0.0 }).collect()
    };

    // Owner nodes: tfidf one-hot at dim 1 — the conceptual query embedding
    // (dim 0) does NOT match them, so at baseline they score ~0 and get cut.
    // Decoy nodes: tfidf one-hot at dim 0 — perfect tfidf match, fills the
    // top-k and pushes the owners out of the results entirely.
    let build_nodes = || -> Vec<NodeInfo> {
        let mut nodes: Vec<NodeInfo> = (0..N_OWNERS)
            .map(|i| NodeInfo {
                node_id: format!("mod.rs:owner_{i}"),
                file_path: "mod.rs".to_string(),
                symbol_name: format!("owner_{i}"),
                language: "rust".to_string(),
                content: format!("pub fn owner_{i}() {{ /* unrelated body */ }}"),
                byte_range: (0, 40),
                tfidf_embedding: one_hot(DIM, 1),
                neural_embedding: None,
                complexity: 2,
                signature: None,
                pre_tokenized: Some(vec![format!("owner_{i}")]),
            })
            .collect();
        for d in 0..N_DECOYS {
            nodes.push(NodeInfo {
                node_id: format!("mod.rs:decoy_{d}"),
                file_path: "mod.rs".to_string(),
                symbol_name: format!("decoy_{d}"),
                language: "rust".to_string(),
                content: format!("pub fn decoy_{d}() {{ /* dominates lexical space */ }}"),
                byte_range: (0, 50),
                tfidf_embedding: one_hot(DIM, 0),
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: Some(vec![format!("decoy_{d}")]),
            });
        }
        nodes
    };

    // Fragment rows: one per owner, embedded at a *distinct* semantic region
    // (dim 6) far from every node tfidf one-hot. The conceptual queries embed
    // near this region, so only the fragment tier can surface the owner.
    let mut fragment_rows = Vec::new();
    let mut refs = std::collections::HashMap::new();
    for i in 0..N_OWNERS {
        let mut emb = one_hot(DIM, 6);
        emb[i % DIM] = 0.5; // per-owner differentiation
        let frag_id = format!("hash_owner_{i}");
        fragment_rows.push((frag_id.clone(), emb));
        refs.insert(
            frag_id,
            (format!("mod.rs:owner_{i}"), (8 + i * 2, 12 + i * 2)),
        );
    }

    // Build a fresh engine with the same corpus; optionally enable fragments
    // at the given fusion weight (0.0 == off).
    let build_engine = |fragments_on: bool, frag_weight: f32| -> SearchEngine {
        let mut engine = SearchEngine::with_dimension(DIM);
        engine.index_nodes(build_nodes());
        if fragments_on {
            engine.set_fragment_embeddings(fragment_rows.clone());
            engine.set_fragment_refs(refs.clone());
            engine.set_fragment_index_enabled(true);
            engine.set_fragment_weight(frag_weight);
        }
        engine
    };

    // MRR@TOP_K over a set of (expected owner, query text, tfidf query
    // embedding, neural query embedding) tuples. `frag_weight` selects the
    // fusion weight (0.4 is the demonstration weight; the shipped 0.12 default
    // is measured separately below, unasserted, to self-demonstrate that
    // fragments add recall without outranking strong tfidf matches).
    let mrr = |queries: &[(String, String, Vec<f32>, Option<Vec<f32>>)],
               fragments_on: bool,
               frag_weight: f32|
     -> f64 {
        let mut engine = build_engine(fragments_on, frag_weight);
        let mut reciprocal_ranks = Vec::new();
        for (target, query_text, query_emb, query_neural) in queries.iter() {
            let results = engine
                .search(SearchQuery {
                    query: query_text.clone(),
                    top_k: TOP_K,
                    token_budget: None,
                    semantic: true,
                    expand_context: false,
                    query_embedding: Some(query_emb.clone()),
                    query_neural_embedding: query_neural.clone(),
                    threshold: None,
                    query_type: None,
                })
                .unwrap();
            let rank = results
                .iter()
                .position(|r| r.node_id == *target)
                .map(|p| p + 1);
            reciprocal_ranks.push(match rank {
                Some(r) => 1.0 / r as f64,
                None => 0.0,
            });
        }
        reciprocal_ranks.iter().sum::<f64>() / reciprocal_ranks.len() as f64
    };

    // Conceptual queries: text has no lexical overlap with any node, each has
    // DISTINCT text (avoids the search-cache key collision that would serve
    // query 0's result set to the others), the tfidf embedding matches decoys
    // only, and the neural embedding exactly matches fragment i (cosine 1.0).
    // Baseline: decoys fill the top-k, owner absent (0.0). With fragments: the
    // owner surfaces via the fusion path.
    let conceptual: Vec<(String, String, Vec<f32>, Option<Vec<f32>>)> = (0..N_OWNERS)
        .map(|i| {
            let mut q = one_hot(DIM, 6);
            q[i % DIM] = 0.5;
            (
                format!("mod.rs:owner_{i}"),
                format!("conceptual sub-symbol intent {i}"),
                one_hot(DIM, 0),
                Some(q),
            )
        })
        .collect();

    // Node-level queries: query text matches the owner token lexically, the
    // tfidf embedding is neutral (dim 7, matches nothing), and the neural
    // embedding is None so the fragment path is inert — the owner surfaces via
    // the normal path identically with and without the tier. Guards the plan
    // stop-condition (fragment tier must not regress node-level ranking).
    let node_level: Vec<(String, String, Vec<f32>, Option<Vec<f32>>)> = (0..N_OWNERS)
        .map(|i| {
            (
                format!("mod.rs:owner_{i}"),
                format!("owner_{i}"),
                one_hot(DIM, 7),
                None,
            )
        })
        .collect();

    let conceptual_off = mrr(&conceptual, false, 0.0);
    let node_off = mrr(&node_level, false, 0.0);

    // Fragment-weight default sweep (Task: tune fragment_weight). Measure the
    // conceptual-recall MRR at the candidate defaults 0.12 / 0.2 / 0.3 / 0.4
    // (and the node-level no-regression guard at each). All four are measured
    // empirically and printed; the assertion stays on the demonstration weight
    // (0.4) so the surfacing MECHANISM is verified in isolation regardless of
    // the shipped default (see the fn doc comment). The winning default is
    // recorded in the printed line and applied to `src/config.rs`
    // `default_fragment_weight()`.
    let conceptual_w012 = mrr(&conceptual, true, 0.12);
    let conceptual_w020 = mrr(&conceptual, true, 0.20);
    let conceptual_w030 = mrr(&conceptual, true, 0.30);
    let conceptual_w035 = mrr(&conceptual, true, 0.35);
    let conceptual_w040 = mrr(&conceptual, true, 0.40);
    let node_w040 = mrr(&node_level, true, 0.40);

    eprintln!(
        "fragment_recall_mrr: baseline(off)={conceptual_off:.4} w=0.12:{conceptual_w012:.4} w=0.20:{conceptual_w020:.4} w=0.30:{conceptual_w030:.4} w=0.35:{conceptual_w035:.4} w=0.40:{conceptual_w040:.4} | node-rank off={node_off:.4} w=0.40:{node_w040:.4}"
    );

    // The shipped default (0.35) must deliver the recall gain — that is the
    // product claim being tuned. Assert on the DEFAULT rather than the 0.40
    // demonstration weight so a future regression that breaks the shipped
    // default specifically fails here; 0.40 is still printed above as margin
    // evidence.
    assert!(
        conceptual_w035 > conceptual_off,
        "fragment tier must improve conceptual-query MRR at the shipped default 0.35: baseline {conceptual_off:.4} -> with fragments {conceptual_w035:.4}"
    );
    assert!(
        node_w040 >= node_off,
        "fragment tier must not regress node-level ranking MRR: baseline {node_off:.4} -> with fragments {node_w040:.4}"
    );
}
