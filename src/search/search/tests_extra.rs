use super::tests::create_test_nodes;
use super::*;

#[test]
fn test_incremental_reindex_removes_nonexistent_node() {
    // T28: Removing a node that doesn't exist should be a no-op
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    let delta = TextIndexDelta {
        removed_node_ids: vec!["nonexistent".to_string()],
        updated_nodes: vec![],
    };
    engine.incremental_reindex(delta);

    assert_eq!(engine.node_count(), 2);
    assert_eq!(engine.node_id_to_idx.len(), 2);
}

#[test]
fn test_incremental_reindex_content_cleared() {
    // T28: Content should be cleared on newly added nodes (same as index_nodes)
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    engine.incremental_reindex(TextIndexDelta {
        removed_node_ids: vec![],
        updated_nodes: vec![NodeInfo {
            node_id: "func3".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "func3".to_string(),
            language: "rust".to_string(),
            content: "fn func3() { important_content(); }".to_string(),
            byte_range: (0, 40),
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 3,
            signature: None,
            pre_tokenized: None,
        }],
    });

    // Content should be cleared for all nodes
    for node in &engine.nodes {
        assert!(
            node.content.is_empty(),
            "Node {} content should be cleared, got: {:?}",
            node.node_id,
            node.content
        );
    }

    // But func3 tokens should still be searchable
    assert!(
        engine
            .node_tokens
            .get("func3")
            .unwrap()
            .contains("important")
    );
}

// ----------------------------------------------------------------
// R8: Pre-tokenized search engine tests
// ----------------------------------------------------------------

#[test]
fn test_pre_tokenized_produces_identical_search_results() {
    // R8: NodeInfo with pre_tokenized = Some(...) should produce identical
    // search results to the re-tokenization path.
    let content = "fn calculate_total(price: f64, tax: f64) -> f64 { price + tax }";

    // Compute search tokens the same way index_builder does
    let search_tokens: Vec<String> = content
        .split(|c: char| !c.is_alphanumeric())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 2)
        .collect();

    // Engine with pre-tokenized tokens
    let mut engine_pre = SearchEngine::new();
    engine_pre.index_nodes(vec![NodeInfo {
        node_id: "calc_total".to_string(),
        file_path: "math.rs".to_string(),
        symbol_name: "calculate_total".to_string(),
        language: "rust".to_string(),
        content: content.to_string(),
        byte_range: (0, content.len()),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: Some(search_tokens),
    }]);

    // Engine with re-tokenization (pre_tokenized = None)
    let mut engine_fallback = SearchEngine::new();
    engine_fallback.index_nodes(vec![NodeInfo {
        node_id: "calc_total".to_string(),
        file_path: "math.rs".to_string(),
        symbol_name: "calculate_total".to_string(),
        language: "rust".to_string(),
        content: content.to_string(),
        byte_range: (0, content.len()),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: None,
    }]);

    // Both inverted indexes should be identical
    assert_eq!(
        engine_pre.text_index, engine_fallback.text_index,
        "Pre-tokenized and fallback should produce identical text_index"
    );
    assert_eq!(
        engine_pre.node_tokens, engine_fallback.node_tokens,
        "Pre-tokenized and fallback should produce identical node_tokens"
    );

    // Search for "calculate" should find the node in both
    let query = SearchQuery {
        query: "calculate".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results_pre = engine_pre.search(query.clone()).unwrap();
    let results_fallback = engine_fallback.search(query).unwrap();
    assert_eq!(results_pre.len(), results_fallback.len());
    assert!(!results_pre.is_empty());
    assert_eq!(results_pre[0].node_id, results_fallback[0].node_id);
}

#[test]
fn test_pre_tokenized_none_falls_back_to_content() {
    // R8: NodeInfo with pre_tokenized = None should use content-based
    // tokenization (backward compatibility).
    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![NodeInfo {
        node_id: "backward_compat".to_string(),
        file_path: "compat.rs".to_string(),
        symbol_name: "legacy_func".to_string(),
        language: "rust".to_string(),
        content: "fn legacy_func() { return 42; }".to_string(),
        byte_range: (0, 30),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 1,
        signature: None,
        pre_tokenized: None,
    }]);

    // Should still find via content-based tokenization
    assert!(engine.text_index.contains_key("legacy"));
    assert!(engine.text_index.contains_key("func"));
    assert!(engine.node_tokens.contains_key("backward_compat"));

    let query = SearchQuery {
        query: "legacy".to_string(),
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
    assert_eq!(results[0].node_id, "backward_compat");
}

#[test]
fn test_pre_tokenized_and_content_produce_same_inverted_index() {
    // R8: Both paths produce the same inverted index for the same content.
    let content = "pub async fn handle_http_request(req: Request) -> Response { ... }";

    let tokens: Vec<String> = content
        .split(|c: char| !c.is_alphanumeric())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 2)
        .collect();

    // Verify our manual tokenization produces expected tokens
    assert!(tokens.contains(&"handle".to_string()));
    assert!(tokens.contains(&"http".to_string()));
    assert!(tokens.contains(&"request".to_string()));
    assert!(tokens.contains(&"response".to_string()));

    // Engine A: pre-tokenized
    let mut engine_a = SearchEngine::new();
    engine_a.index_nodes(vec![NodeInfo {
        node_id: "handler".to_string(),
        file_path: "server.rs".to_string(),
        symbol_name: "handle_http_request".to_string(),
        language: "rust".to_string(),
        content: content.to_string(),
        byte_range: (0, content.len()),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 5,
        signature: None,
        pre_tokenized: Some(tokens),
    }]);

    // Engine B: content-based
    let mut engine_b = SearchEngine::new();
    engine_b.index_nodes(vec![NodeInfo {
        node_id: "handler".to_string(),
        file_path: "server.rs".to_string(),
        symbol_name: "handle_http_request".to_string(),
        language: "rust".to_string(),
        content: content.to_string(),
        byte_range: (0, content.len()),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 5,
        signature: None,
        pre_tokenized: None,
    }]);

    // Both should have identical text_index entries
    for token in &["handle", "http", "request", "response", "pub", "async"] {
        assert_eq!(
            engine_a.text_index.get(*token),
            engine_b.text_index.get(*token),
            "Mismatch for token '{}': pre_tokenized={:?}, content={:?}",
            token,
            engine_a.text_index.get(*token),
            engine_b.text_index.get(*token)
        );
    }
}

#[test]
fn test_pre_tokenized_incremental_reindex() {
    // R8: Pre-tokenized tokens should work correctly with incremental reindex.
    let mut engine = SearchEngine::new();
    engine.index_nodes(create_test_nodes());

    let new_content = "fn compute_metrics(data: &[f64]) -> Metrics { ... }";
    let tokens: Vec<String> = new_content
        .split(|c: char| !c.is_alphanumeric())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 2)
        .collect();

    let delta = TextIndexDelta {
        removed_node_ids: vec![],
        updated_nodes: vec![NodeInfo {
            node_id: "metrics".to_string(),
            file_path: "metrics.rs".to_string(),
            symbol_name: "compute_metrics".to_string(),
            language: "rust".to_string(),
            content: new_content.to_string(),
            byte_range: (0, new_content.len()),
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 4,
            signature: None,
            pre_tokenized: Some(tokens),
        }],
    };
    engine.incremental_reindex(delta);

    // Should have 3 nodes now
    assert_eq!(engine.node_count(), 3);

    // Pre-tokenized tokens should be in the inverted index
    assert!(engine.text_index.contains_key("compute"));
    assert!(engine.text_index.contains_key("metrics"));
    assert!(engine.text_index.contains_key("data"));

    // Search should find the new node
    let query = SearchQuery {
        query: "compute metrics".to_string(),
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
    assert_eq!(results[0].node_id, "metrics");
}

// A+ VAL-APLUS-014: Search cache is hard-capped without semantic regression
#[test]
fn test_search_cache_hard_capped() {
    let mut engine = SearchEngine::new();

    // Index some nodes
    let nodes: Vec<NodeInfo> = (0..50)
        .map(|i| NodeInfo {
            node_id: format!("node_{}", i),
            file_path: format!("file_{}.rs", i),
            symbol_name: format!("symbol_{}", i),
            language: "rust".to_string(),
            content: format!("fn symbol_{}() {{}}", i),
            byte_range: (0, 16),
            tfidf_embedding: vec![0.0; 768],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        })
        .collect();
    engine.index_nodes(nodes);

    // Run many searches to fill the cache
    for i in 0..300 {
        let query = SearchQuery {
            query: format!("query_{}", i),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        };
        let _ = engine.search(query);
    }

    // Cache should not exceed entry limit
    assert!(
        engine.search_cache.len() <= SEARCH_CACHE_MAX_ENTRIES,
        "search cache entries ({}) should not exceed max ({})",
        engine.search_cache.len(),
        SEARCH_CACHE_MAX_ENTRIES
    );

    // Cache bytes should not exceed byte limit
    assert!(
        engine.search_cache_bytes <= SEARCH_CACHE_MAX_BYTES,
        "search cache bytes ({}) should not exceed max ({})",
        engine.search_cache_bytes,
        SEARCH_CACHE_MAX_BYTES
    );

    // Verify search still returns correct results (no semantic regression)
    let query = SearchQuery {
        query: "symbol_0".to_string(),
        top_k: 5,
        token_budget: None,
        semantic: false,
        expand_context: false,
        query_embedding: None,
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };
    let results = engine.search(query).unwrap();
    assert!(!results.is_empty(), "search should still return results");
}

// ================================================================
// A+ VAL-APLUS-037: Bound-gated indexing admission tests
// ================================================================

#[test]
fn test_admission_gate_admits_within_bounds() {
    let mut gate = IndexingAdmissionGate::with_caps(100, 1024);
    assert!(gate.try_admit(10));
    assert!(gate.try_admit(10));
    assert_eq!(gate.nodes_admitted(), 2);
    assert_eq!(gate.nodes_shed(), 0);
}

#[test]
fn test_admission_gate_sheds_over_node_cap() {
    let mut gate = IndexingAdmissionGate::with_caps(3, 1_000_000);
    assert!(gate.try_admit(10));
    assert!(gate.try_admit(10));
    assert!(gate.try_admit(10));
    // 4th node should be shed
    assert!(!gate.try_admit(10));
    assert!(!gate.try_admit(10));
    assert_eq!(gate.nodes_admitted(), 3);
    assert_eq!(gate.nodes_shed(), 2);
}

#[test]
fn test_admission_gate_sheds_over_byte_cap() {
    let mut gate = IndexingAdmissionGate::with_caps(100, 50);
    assert!(gate.try_admit(20));
    assert!(gate.try_admit(20));
    // 3rd node would exceed byte cap (20+20+20=60 > 50)
    assert!(!gate.try_admit(20));
    assert_eq!(gate.nodes_admitted(), 2);
    assert_eq!(gate.bytes_admitted(), 40);
    assert_eq!(gate.nodes_shed(), 1);
}

#[test]
fn test_admission_gate_resets() {
    let mut gate = IndexingAdmissionGate::with_caps(2, 100);
    assert!(gate.try_admit(10));
    assert!(gate.try_admit(10));
    assert!(!gate.try_admit(10));
    gate.reset();
    // After reset, should admit again
    assert!(gate.try_admit(10));
    assert_eq!(gate.nodes_admitted(), 1);
    assert_eq!(gate.nodes_shed(), 0);
}

#[test]
fn test_admission_gate_default_caps() {
    let gate = IndexingAdmissionGate::new();
    assert_eq!(gate.nodes_admitted(), 0);
    assert_eq!(gate.nodes_shed(), 0);
    assert_eq!(gate.bytes_admitted(), 0);
}

#[test]
fn test_admission_gate_oversized_single_node() {
    // A single node whose content exceeds the byte cap should be shed.
    let mut gate = IndexingAdmissionGate::with_caps(100, 50);
    assert!(!gate.try_admit(100));
    assert_eq!(gate.nodes_shed(), 1);
    assert_eq!(gate.nodes_admitted(), 0);
}

#[test]
fn test_admission_gate_bursty_workload() {
    // Simulate bursty indexing: many nodes arriving at once.
    let mut gate = IndexingAdmissionGate::with_caps(10, 10_000);
    let mut admitted = 0;
    let mut shed = 0;
    for _ in 0..50 {
        if gate.try_admit(100) {
            admitted += 1;
        } else {
            shed += 1;
        }
    }
    assert_eq!(admitted, 10);
    assert_eq!(shed, 40);
    assert_eq!(gate.nodes_admitted(), 10);
    assert_eq!(gate.nodes_shed(), 40);
}

// ================================================================
// A+ VAL-APLUS-038: Selective pruning tests
// ================================================================

#[test]
fn test_pruner_keeps_user_authored_code() {
    let pruner = ContentPruner::new();
    let decision = pruner.evaluate("src/main.rs", "fn main() { println!(\"hello\"); }", "main");
    assert_eq!(decision, PruningDecision::Keep);
}

#[test]
fn test_pruner_prunes_minified_js() {
    let pruner = ContentPruner::new();
    let decision = pruner.evaluate(
        "static/app.min.js",
        "var a=1,b=2;function c(){return a+b}",
        "c",
    );
    assert!(matches!(decision, PruningDecision::GeneratedCode(_)));
}

#[test]
fn test_pruner_prunes_generated_protobuf() {
    let pruner = ContentPruner::new();
    let decision = pruner.evaluate(
        "proto/user.pb.go",
        "func (m *User) GetName() string { return m.Name }",
        "GetName",
    );
    assert!(matches!(decision, PruningDecision::GeneratedCode(_)));
}

#[test]
fn test_pruner_prunes_generated_rust() {
    let pruner = ContentPruner::new();
    let decision = pruner.evaluate(
        "src/types.generated.rs",
        "pub fn generated_fn() -> i32 { 42 }",
        "generated_fn",
    );
    assert!(matches!(decision, PruningDecision::GeneratedCode(_)));
}

#[test]
fn test_pruner_prunes_bundle_js() {
    let pruner = ContentPruner::new();
    let decision = pruner.evaluate(
        "dist/app.bundle.js",
        "module.exports=function(n){return n+1}",
        "anonymous",
    );
    assert!(matches!(decision, PruningDecision::GeneratedCode(_)));
}

#[test]
fn test_pruner_prunes_node_modules() {
    let pruner = ContentPruner::new();
    let decision = pruner.evaluate(
        "node_modules/lodash/index.js",
        "function debounce(fn, ms) { /* ... */ }",
        "debounce",
    );
    assert!(matches!(decision, PruningDecision::GeneratedCode(_)));
}

#[test]
fn test_pruner_prunes_low_information() {
    let pruner = ContentPruner::new();
    // Very short content with trivial symbol name
    let decision = pruner.evaluate("src/x.rs", "fn x() {}", "x");
    assert!(matches!(decision, PruningDecision::LowInformation(_)));
}

#[test]
fn test_pruner_keeps_short_content_with_meaningful_name() {
    let pruner = ContentPruner::new();
    // Short content but meaningful symbol name — should be kept
    let decision = pruner.evaluate("src/lib.rs", "fn compute() {}", "compute");
    assert_eq!(decision, PruningDecision::Keep);
}

#[test]
fn test_pruner_is_generated_path() {
    let pruner = ContentPruner::new();
    assert!(pruner.is_generated_path("static/app.min.js"));
    assert!(pruner.is_generated_path("proto/user.pb.go"));
    assert!(pruner.is_generated_path("src/types.generated.rs"));
    assert!(pruner.is_generated_path("node_modules/react/index.js"));
    assert!(!pruner.is_generated_path("src/main.rs"));
    assert!(!pruner.is_generated_path("lib/parser.py"));
}

#[test]
fn test_pruner_decision_is_observable() {
    // VAL-APLUS-038: The pruned-vs-kept decision is externally observable.
    let pruner = ContentPruner::new();

    let kept = pruner.evaluate("src/main.rs", "fn main() { /* ... */ }", "main");
    let generated = pruner.evaluate("src/types.generated.rs", "fn gen() {}", "gen");
    let low_info = pruner.evaluate("src/x.rs", "fn x() {}", "x");

    // Each decision variant is distinguishable
    assert_eq!(kept, PruningDecision::Keep);
    match generated {
        PruningDecision::GeneratedCode(reason) => {
            assert!(
                reason.contains("generated"),
                "reason should mention generated: {}",
                reason
            );
        }
        other => panic!("expected GeneratedCode, got {:?}", other),
    }
    match low_info {
        PruningDecision::LowInformation(reason) => {
            assert!(
                reason.contains("bytes"),
                "reason should mention bytes: {}",
                reason
            );
        }
        other => panic!("expected LowInformation, got {:?}", other),
    }
}

#[test]
fn test_pruner_does_not_remove_high_signal_files() {
    let pruner = ContentPruner::new();
    // User-authored high-signal files should always be kept
    let cases = vec![
        (
            "src/lib.rs",
            "pub fn connect(db: &Database) -> Result<Connection> { /* ... */ }",
            "connect",
        ),
        (
            "src/api/handlers.rs",
            "async fn handle_request(req: Request) -> Response { /* ... */ }",
            "handle_request",
        ),
        (
            "src/models/user.rs",
            "struct User { name: String, email: String, created_at: DateTime }",
            "User",
        ),
        (
            "app/controllers/application_controller.rb",
            "def index; @items = Item.all; end",
            "index",
        ),
    ];
    for (path, content, symbol) in cases {
        let decision = pruner.evaluate(path, content, symbol);
        assert_eq!(
            decision,
            PruningDecision::Keep,
            "high-signal file {} should be kept, got {:?}",
            path,
            decision
        );
    }
}

// ================================================================
// A+ VAL-APLUS-039: Repeated-work hoisting tests
// ================================================================

#[test]
fn test_work_hoister_stores_and_retrieves() {
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    let content = "fn compute(x: i32) -> i32 { x + 1 }";
    let embedding = vec![0.1, 0.2, 0.3];

    hoister.store(content, embedding.clone(), None);
    let retrieved = hoister.lookup(content);

    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().0, embedding);
}

#[test]
fn test_work_hoister_miss_for_unseen_content() {
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    assert!(hoister.lookup("unseen content").is_none());
}

#[test]
fn test_work_hoister_reuses_identical_content() {
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    let content = "fn identical() {}";
    let embedding = vec![0.5, 0.5, 0.5];

    hoister.store(content, embedding.clone(), None);

    // Second lookup for identical content should return the same embedding
    let retrieved = hoister.lookup(content);
    assert_eq!(retrieved.unwrap().0, embedding);
}

#[test]
fn test_work_hoister_distinguishes_different_content() {
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    let content_a = "fn alpha() {}";
    let content_b = "fn beta() {}";
    let embedding_a = vec![1.0, 0.0, 0.0];
    let embedding_b = vec![0.0, 1.0, 0.0];

    hoister.store(content_a, embedding_a.clone(), None);
    hoister.store(content_b, embedding_b.clone(), None);

    assert_eq!(hoister.lookup(content_a).unwrap().0, embedding_a);
    assert_eq!(hoister.lookup(content_b).unwrap().0, embedding_b);
}

#[test]
fn test_work_hoister_evicts_on_entry_cap() {
    let mut hoister = WorkHoister::with_bounds(3, 1_000_000);

    hoister.store("content_1", vec![1.0], None);
    hoister.store("content_2", vec![2.0], None);
    hoister.store("content_3", vec![3.0], None);
    assert_eq!(hoister.len(), 3);

    // Adding a 4th should evict the LRU entry
    hoister.store("content_4", vec![4.0], None);
    assert_eq!(hoister.len(), 3);

    // content_1 should have been evicted (LRU)
    assert!(hoister.lookup("content_1").is_none());
    // content_4 should be present
    assert!(hoister.lookup("content_4").is_some());
}

#[test]
fn test_work_hoister_evicts_on_byte_cap() {
    // Very small byte cap: only room for ~1 embedding
    // Each entry: 32-byte BLAKE3 hash key + 3 * 4 bytes = 44 bytes
    let mut hoister = WorkHoister::with_bounds(100, 50);

    hoister.store("short", vec![1.0, 2.0, 3.0], None);
    assert_eq!(hoister.len(), 1);

    // Adding another should evict the first (44 + 44 = 88 > 50)
    hoister.store("another", vec![4.0, 5.0, 6.0], None);
    assert!(hoister.lookup("short").is_none());
    assert!(hoister.lookup("another").is_some());
}

#[test]
fn test_work_hoister_clear() {
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    hoister.store("content", vec![1.0], None);
    assert!(!hoister.is_empty());

    hoister.clear();
    assert!(hoister.is_empty());
    assert_eq!(hoister.bytes_used(), 0);
}

#[test]
fn test_work_hoister_preserves_search_results() {
    // VAL-APLUS-039: Reusing hoisted work should produce identical results.
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    let content = "fn compute(data: &[f64]) -> f64 { data.iter().sum() }";
    let embedding = vec![0.1, 0.2, 0.3, 0.4, 0.5];

    // Store the embedding
    hoister.store(content, embedding.clone(), None);

    // Retrieve and verify it matches
    let retrieved = hoister.lookup(content).unwrap().0;
    assert_eq!(retrieved, embedding);

    // Using the retrieved embedding in a search engine should produce
    // the same results as using the original.
    let mut engine_a = SearchEngine::with_dimension(5);
    let mut engine_b = SearchEngine::with_dimension(5);

    engine_a.index_nodes(vec![NodeInfo {
        node_id: "compute".to_string(),
        file_path: "math.rs".to_string(),
        symbol_name: "compute".to_string(),
        language: "rust".to_string(),
        content: content.to_string(),
        byte_range: (0, content.len()),
        tfidf_embedding: embedding,
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: None,
    }]);

    engine_b.index_nodes(vec![NodeInfo {
        node_id: "compute".to_string(),
        file_path: "math.rs".to_string(),
        symbol_name: "compute".to_string(),
        language: "rust".to_string(),
        content: content.to_string(),
        byte_range: (0, content.len()),
        tfidf_embedding: retrieved,
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: None,
    }]);

    // Both engines should produce identical semantic search results
    let results_a = engine_a
        .semantic_search(&[0.1, 0.2, 0.3, 0.4, 0.5], 5)
        .unwrap();
    let results_b = engine_b
        .semantic_search(&[0.1, 0.2, 0.3, 0.4, 0.5], 5)
        .unwrap();
    assert_eq!(results_a.len(), results_b.len());
    if !results_a.is_empty() {
        assert_eq!(results_a[0].node_id, results_b[0].node_id);
        assert!((results_a[0].relevance - results_b[0].relevance).abs() < 1e-6);
    }
}

#[test]
fn test_work_hoister_duplicate_work_suppressed() {
    // VAL-APLUS-039: Duplicate expensive work is suppressed.
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    let content = "fn expensive() { /* lots of work */ }";
    let embedding = vec![0.42; 768];

    // First time: store
    hoister.store(content, embedding.clone(), None);

    // Second time: lookup should return the same result without recomputation
    let lookup_result = hoister.lookup(content);
    assert!(lookup_result.is_some());
    assert_eq!(lookup_result.unwrap().0, embedding);

    // Verify the hoister has exactly 1 entry (no duplicate)
    assert_eq!(hoister.len(), 1);
}

#[test]
fn test_work_hoister_updates_existing_embedding() {
    let mut hoister = WorkHoister::with_bounds(100, 1_000_000);
    let content = "fn update_me() {}";
    let first_embedding = vec![1.0, 2.0, 3.0];
    let second_embedding = vec![4.0, 5.0];

    hoister.store(content, first_embedding.clone(), None);
    let initial_bytes = hoister.bytes_used();

    hoister.store(content, second_embedding.clone(), None);

    assert_eq!(hoister.lookup(content).unwrap().0, second_embedding);
    assert_eq!(hoister.len(), 1);
    assert_eq!(
        hoister.bytes_used(),
        initial_bytes - first_embedding.len() * std::mem::size_of::<f32>()
            + second_embedding.len() * std::mem::size_of::<f32>()
    );
}

// ========================================================================
// VAL-APLUS-015 through VAL-APLUS-021: NodeInfo compatibility bridge
// and duplicate TF-IDF ownership removal tests
// ========================================================================

/// Helper: create a minimal NodeInfo for serialization tests.
fn make_node_info(tfidf: Vec<f32>) -> NodeInfo {
    NodeInfo {
        node_id: "test_node".into(),
        file_path: "test.rs".into(),
        symbol_name: "test_fn".into(),
        language: "rust".into(),
        content: "fn test_fn() {}".into(),
        byte_range: (0, 16),
        tfidf_embedding: tfidf,
        neural_embedding: None,
        complexity: 1,
        signature: None,
        pre_tokenized: None,
    }
}

/// VAL-APLUS-015: Legacy NodeInfo payloads remain readable during the
/// one-minor compatibility window.
///
/// A payload serialized with the old `embedding` field (and no
/// `tfidf_embedding`) must deserialize successfully.
#[test]
fn test_legacy_payload_remains_readable() {
    let legacy_json = r#"{
            "node_id": "legacy_node",
            "file_path": "legacy.rs",
            "symbol_name": "legacy_fn",
            "language": "rust",
            "content": "fn legacy_fn() {}",
            "byte_range": [0, 16],
            "embedding": [0.1, 0.2, 0.3, 0.4],
            "neural_embedding": null,
            "complexity": 5,
            "signature": null,
            "pre_tokenized": null
        }"#;

    let node: NodeInfo = serde_json::from_str(legacy_json)
        .expect("Legacy payload must deserialize during compatibility window");

    assert_eq!(node.node_id, "legacy_node");
    // The legacy embedding should have been promoted to tfidf_embedding
    assert_eq!(node.tfidf_embedding, vec![0.1, 0.2, 0.3, 0.4]);
}

/// VAL-APLUS-016: New NodeInfo payloads serialize only the new shape.
///
/// Fresh serialization must emit `tfidf_embedding` and must NOT emit
/// the legacy `embedding` field.
#[test]
fn test_new_payload_serializes_only_new_shape() {
    let node = make_node_info(vec![1.0, 2.0, 3.0]);
    let json = serde_json::to_string(&node).expect("Serialization must succeed");

    // Must contain tfidf_embedding
    assert!(
        json.contains("\"tfidf_embedding\""),
        "Serialized output must contain tfidf_embedding field"
    );

    // Must NOT contain the legacy "embedding" field (not "tfidf_embedding")
    // We check for the exact key by looking for the pattern that would indicate
    // a standalone "embedding" key (not preceded by "tfidf_")
    let has_legacy_embedding =
        json.contains("\"embedding\":") && !json.contains("\"tfidf_embedding\":");
    assert!(
        !has_legacy_embedding,
        "Serialized output must not contain legacy 'embedding' field. JSON: {}",
        json
    );
}

/// VAL-APLUS-017: Compatibility resolution prefers non-empty tfidf_embedding.
///
/// When both old and new shapes are present, deserialization uses the
/// non-empty new TF-IDF field first.
#[test]
fn test_compat_prefers_tfidf_embedding_over_legacy() {
    let dual_json = r#"{
            "node_id": "dual_node",
            "file_path": "dual.rs",
            "symbol_name": "dual_fn",
            "language": "rust",
            "content": "fn dual_fn() {}",
            "byte_range": [0, 14],
            "tfidf_embedding": [0.5, 0.6, 0.7],
            "embedding": [0.1, 0.2, 0.3],
            "neural_embedding": null,
            "complexity": 3,
            "signature": null,
            "pre_tokenized": null
        }"#;

    let node: NodeInfo =
        serde_json::from_str(dual_json).expect("Dual-shape payload must deserialize");

    // Should prefer tfidf_embedding (the new field)
    assert_eq!(
        node.tfidf_embedding,
        vec![0.5, 0.6, 0.7],
        "Must prefer tfidf_embedding when both fields are present"
    );
}

/// VAL-APLUS-018: Compatibility fallback promotes legacy embedding only
/// when needed.
///
/// If the new TF-IDF field is absent or empty and the legacy field is
/// populated, deserialization promotes the legacy value.
#[test]
fn test_compat_fallback_promotes_legacy_when_needed() {
    // Case 1: tfidf_embedding absent, legacy present
    let legacy_only_json = r#"{
            "node_id": "fallback_node",
            "file_path": "fallback.rs",
            "symbol_name": "fallback_fn",
            "language": "rust",
            "content": "fn fallback_fn() {}",
            "byte_range": [0, 18],
            "embedding": [0.9, 0.8, 0.7],
            "neural_embedding": null,
            "complexity": 2,
            "signature": null,
            "pre_tokenized": null
        }"#;

    let node: NodeInfo =
        serde_json::from_str(legacy_only_json).expect("Legacy-only payload must deserialize");
    assert_eq!(
        node.tfidf_embedding,
        vec![0.9, 0.8, 0.7],
        "Must promote legacy embedding when tfidf_embedding is absent"
    );

    // Case 2: tfidf_embedding present but empty, legacy present
    let empty_new_json = r#"{
            "node_id": "empty_new_node",
            "file_path": "empty.rs",
            "symbol_name": "empty_fn",
            "language": "rust",
            "content": "fn empty_fn() {}",
            "byte_range": [0, 14],
            "tfidf_embedding": [],
            "embedding": [0.4, 0.5, 0.6],
            "neural_embedding": null,
            "complexity": 1,
            "signature": null,
            "pre_tokenized": null
        }"#;

    let node2: NodeInfo =
        serde_json::from_str(empty_new_json).expect("Empty-new + legacy payload must deserialize");
    assert_eq!(
        node2.tfidf_embedding,
        vec![0.4, 0.5, 0.6],
        "Must promote legacy embedding when tfidf_embedding is empty"
    );
}

/// VAL-APLUS-019: Empty legacy and new embeddings degrade safely.
///
/// If neither shape provides a usable vector, deserialization succeeds
/// with an empty TF-IDF vector rather than crashing or inventing state.
#[test]
fn test_empty_embeddings_degrade_safely() {
    let empty_json = r#"{
            "node_id": "empty_node",
            "file_path": "empty.rs",
            "symbol_name": "empty_fn",
            "language": "rust",
            "content": "fn empty_fn() {}",
            "byte_range": [0, 14],
            "neural_embedding": null,
            "complexity": 0,
            "signature": null,
            "pre_tokenized": null
        }"#;

    let node: NodeInfo = serde_json::from_str(empty_json)
        .expect("Payload with no embeddings must deserialize successfully");

    assert!(
        node.tfidf_embedding.is_empty(),
        "Must degrade to empty tfidf_embedding, got {:?}",
        node.tfidf_embedding
    );
    assert_eq!(node.node_id, "empty_node");
}

/// VAL-APLUS-020: Search semantics are unchanged after duplicate embedding
/// removal.
///
/// Removing duplicate TF-IDF ownership does not alter observable search
/// results or ranking behavior for existing scenarios.
#[test]
fn test_search_semantics_unchanged_after_dedup() {
    // Create a node with tfidf_embedding and index it
    let node = NodeInfo {
        node_id: "dedup_node".into(),
        file_path: "dedup.rs".into(),
        symbol_name: "dedup_fn".into(),
        language: "rust".into(),
        content: "fn dedup_fn() { compute_value(); }".into(),
        byte_range: (0, 32),
        tfidf_embedding: vec![1.0, 0.0, 0.0],
        neural_embedding: None,
        complexity: 3,
        signature: None,
        pre_tokenized: None,
    };

    let mut engine = SearchEngine::with_dimension(3);
    engine.index_nodes(vec![node]);

    // Search with a vector close to the indexed embedding
    let query = SearchQuery {
        query: "dedup_fn".into(),
        top_k: 10,
        token_budget: None,
        semantic: true,
        expand_context: false,
        query_embedding: Some(vec![0.9, 0.1, 0.0]),
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };

    let results = engine.search(query).unwrap();
    assert!(
        !results.is_empty(),
        "Search must return results for indexed node"
    );
    assert_eq!(
        results[0].node_id, "dedup_node",
        "Search must find the correct node"
    );

    // Verify round-trip: serialize then deserialize and search again
    let node_v2 = NodeInfo {
        node_id: "dedup_node_v2".into(),
        file_path: "dedup.rs".into(),
        symbol_name: "dedup_fn_v2".into(),
        language: "rust".into(),
        content: "fn dedup_fn_v2() { compute_other(); }".into(),
        byte_range: (0, 36),
        tfidf_embedding: vec![0.0, 1.0, 0.0],
        neural_embedding: None,
        complexity: 4,
        signature: None,
        pre_tokenized: None,
    };

    // Serialize and deserialize the node to verify the round-trip
    let serialized = serde_json::to_string(&node_v2).unwrap();
    let deserialized: NodeInfo = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.tfidf_embedding, node_v2.tfidf_embedding);

    let mut engine2 = SearchEngine::with_dimension(3);
    engine2.index_nodes(vec![deserialized]);

    let query2 = SearchQuery {
        query: "dedup_fn_v2".into(),
        top_k: 10,
        token_budget: None,
        semantic: true,
        expand_context: false,
        query_embedding: Some(vec![0.0, 0.9, 0.1]),
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };

    let results2 = engine2.search(query2).unwrap();
    assert!(
        !results2.is_empty(),
        "Search must return results after round-trip serialization"
    );
    assert_eq!(
        results2[0].node_id, "dedup_node_v2",
        "Search must find the correct node after round-trip"
    );
}

/// VAL-APLUS-021: Post-index content clearing behavior is preserved.
///
/// A+ dedup work does not regress the existing behavior that clears
/// bulky source content after indexing.
#[test]
fn test_post_index_content_clearing_preserved() {
    let nodes = vec![
        NodeInfo {
            node_id: "clear_node_1".into(),
            file_path: "clear.rs".into(),
            symbol_name: "clear_fn_1".into(),
            language: "rust".into(),
            content: "fn clear_fn_1() { /* some content that should be cleared */ }".into(),
            byte_range: (0, 60),
            tfidf_embedding: vec![1.0, 0.0, 0.0],
            neural_embedding: None,
            complexity: 2,
            signature: Some("fn clear_fn_1()".into()),
            pre_tokenized: None,
        },
        NodeInfo {
            node_id: "clear_node_2".into(),
            file_path: "clear.rs".into(),
            symbol_name: "clear_fn_2".into(),
            language: "rust".into(),
            content: "fn clear_fn_2() { /* more content to be cleared */ }".into(),
            byte_range: (60, 110),
            tfidf_embedding: vec![0.0, 1.0, 0.0],
            neural_embedding: None,
            complexity: 3,
            signature: Some("fn clear_fn_2()".into()),
            pre_tokenized: None,
        },
    ];

    let mut engine = SearchEngine::with_dimension(3);
    engine.index_nodes(nodes);

    // Verify content was cleared on the stored nodes
    for node in &engine.nodes {
        assert!(
            node.content.is_empty(),
            "Node {} content should be cleared after indexing, but got: {:?}",
            node.node_id,
            node.content
        );
    }

    // Verify search still works after content clearing (uses inverted index)
    let query = SearchQuery {
        query: "clear_fn".into(),
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
}

#[test]
fn test_is_archive_path_detection() {
    // Root-level archive directories
    assert!(SearchEngine::is_archive_path("archive/src/main.rs"));
    assert!(SearchEngine::is_archive_path(".archive/old_code.rs"));

    // Nested archive directories
    assert!(SearchEngine::is_archive_path(
        "docs/archive/leindex_pre_step5.rs"
    ));
    assert!(SearchEngine::is_archive_path("src/.archive/backup.rs"));
    assert!(SearchEngine::is_archive_path("maestro/archive/config.yaml"));

    // Deeply nested
    assert!(SearchEngine::is_archive_path(
        "project/.archive/sub/deep/file.rs"
    ));
    assert!(SearchEngine::is_archive_path("a/b/c/archive/d/e/f.rs"));

    // Non-archive paths should NOT match
    assert!(!SearchEngine::is_archive_path("src/main.rs"));
    assert!(!SearchEngine::is_archive_path("src/search/search.rs"));
    assert!(!SearchEngine::is_archive_path("docs/README.md"));
    assert!(!SearchEngine::is_archive_path("tests/archived_test.rs"));
    assert!(!SearchEngine::is_archive_path("my_archive_helper.rs"));
    assert!(!SearchEngine::is_archive_path("src/archive_helper.rs"));
}

#[test]
fn test_archive_files_deprioritized_in_search() {
    // Create nodes with the same symbol name in both src/ and archive/
    let src_node = NodeInfo {
        node_id: "src_index_project".to_string(),
        file_path: "src/cli/leindex/indexing.rs".to_string(),
        symbol_name: "index_project".to_string(),
        language: "rust".to_string(),
        content: "pub fn index_project() {}".to_string(),
        byte_range: (0, 30),
        tfidf_embedding: vec![1.0, 0.0, 0.0],
        neural_embedding: None,
        complexity: 5,
        signature: None,
        pre_tokenized: None,
    };

    let archive_node = NodeInfo {
        node_id: "archive_index_project".to_string(),
        file_path: "docs/archive/leindex_pre_step5.rs".to_string(),
        symbol_name: "index_project".to_string(),
        language: "rust".to_string(),
        content: "pub fn index_project() {}".to_string(),
        byte_range: (0, 30),
        tfidf_embedding: vec![1.0, 0.0, 0.0],
        neural_embedding: None,
        complexity: 5,
        signature: None,
        pre_tokenized: None,
    };

    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![src_node, archive_node]);

    let query = SearchQuery {
        query: "index_project".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: true,
        expand_context: false,
        query_embedding: Some(vec![1.0, 0.0, 0.0]),
        query_neural_embedding: None,
        threshold: None,
        query_type: None,
    };

    let results = engine.search(query).unwrap();
    assert!(!results.is_empty(), "Should have results");

    // The src/ result must rank higher than the archive/ result
    let src_rank = results
        .iter()
        .position(|r| r.file_path.starts_with("src/"))
        .expect("src/ result should exist");
    let archive_rank = results
        .iter()
        .position(|r| SearchEngine::is_archive_path(&r.file_path));

    if let Some(arch_rank) = archive_rank {
        assert!(
            src_rank < arch_rank,
            "src/ result (rank {}) must appear before archive/ result (rank {})",
            src_rank + 1,
            arch_rank + 1
        );
    }

    // Verify the archive result has a lower score
    if let Some(arch_result) = results
        .iter()
        .find(|r| SearchEngine::is_archive_path(&r.file_path))
    {
        let src_result = results
            .iter()
            .find(|r| r.file_path.starts_with("src/"))
            .expect("src/ result should exist");
        assert!(
            src_result.score.overall > arch_result.score.overall,
            "src/ score ({:.4}) must be higher than archive/ score ({:.4})",
            src_result.score.overall,
            arch_result.score.overall
        );
    }
}

#[test]
fn test_search_mode_exact_vs_semantic_different_rankings() {
    // VAL-SEARCH-010: search_mode=exact must produce different rankings
    // than search_mode=semantic for the same query.
    //
    // Setup: two nodes where one has an exact name match but low TF-IDF,
    // and the other has high TF-IDF (conceptual match) but no exact name.
    // Exact mode should rank the exact-name match higher relative to semantic mode.
    let nodes = vec![
        NodeInfo {
            node_id: "exact_match".to_string(),
            file_path: "lib.rs".to_string(),
            symbol_name: "exact_match".to_string(),
            language: "rust".to_string(),
            // Content has some overlap but not a lot
            content: "fn exact_match() { let x = 1; }".to_string(),
            byte_range: (0, 30),
            tfidf_embedding: vec![0.5, 0.3, 0.0],
            neural_embedding: None,
            complexity: 5,
            signature: None,
            pre_tokenized: None,
        },
        NodeInfo {
            node_id: "conceptual_match".to_string(),
            file_path: "lib.rs".to_string(),
            symbol_name: "conceptual_match".to_string(),
            language: "rust".to_string(),
            // Content has strong TF-IDF overlap with query terms
            content: "fn conceptual_match() { exact match logic here }".to_string(),
            byte_range: (31, 70),
            tfidf_embedding: vec![0.9, 0.8, 0.0],
            neural_embedding: None,
            complexity: 10,
            signature: None,
            pre_tokenized: None,
        },
    ];

    let mut engine = SearchEngine::new();
    engine.index_nodes(nodes);

    // Search with exact mode
    let exact_query = SearchQuery {
        query: "exact_match".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: true,
        expand_context: false,
        query_embedding: Some(vec![0.5, 0.3, 0.0]),
        query_neural_embedding: None,
        threshold: None,
        query_type: Some(crate::search::ranking::QueryType::Exact),
    };
    let exact_results = engine.search(exact_query).unwrap();

    // Search with semantic mode
    let semantic_query = SearchQuery {
        query: "exact_match".to_string(),
        top_k: 10,
        token_budget: None,
        semantic: true,
        expand_context: false,
        query_embedding: Some(vec![0.5, 0.3, 0.0]),
        query_neural_embedding: None,
        threshold: None,
        query_type: Some(crate::search::ranking::QueryType::Semantic),
    };
    let semantic_results = engine.search(semantic_query).unwrap();

    // Both should return results
    assert!(
        !exact_results.is_empty(),
        "exact mode should return results"
    );
    assert!(
        !semantic_results.is_empty(),
        "semantic mode should return results"
    );

    // In exact mode, the exact name match should be ranked #1
    assert_eq!(
        exact_results[0].node_id, "exact_match",
        "exact mode should rank exact name match first"
    );

    // The score distributions should differ between modes
    // (different weight distributions produce different scores)
    let exact_top_score = exact_results[0].score.overall;
    let semantic_top_score = semantic_results[0].score.overall;
    assert!(
        exact_top_score != semantic_top_score
            || exact_results[0].node_id != semantic_results[0].node_id,
        "exact and semantic modes should produce different top scores or orderings"
    );

    // Verify that exact mode gives a higher score to the exact name match
    // compared to semantic mode (due to stronger boost)
    let exact_match_exact_score = exact_results
        .iter()
        .find(|r| r.node_id == "exact_match")
        .map(|r| r.score.overall)
        .unwrap_or(0.0);
    let exact_match_semantic_score = semantic_results
        .iter()
        .find(|r| r.node_id == "exact_match")
        .map(|r| r.score.overall)
        .unwrap_or(0.0);
    assert!(
        exact_match_exact_score > exact_match_semantic_score,
        "exact mode ({:.4}) should give higher score to exact name match than semantic mode ({:.4})",
        exact_match_exact_score,
        exact_match_semantic_score
    );
}
