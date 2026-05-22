// Integration tests for INT8 quality gate (Plan 2)
//
// VAL-BPHASE-030: INT8 path is not promoted unless the quality gate passes.
// VAL-BPHASE-031: INT8 quality gate enforces NDCG@10 drop ≤ 1%.
// VAL-BPHASE-032: INT8 quality gate enforces p50 latency ≤ baseline + 5%.
// VAL-BPHASE-033: INT8 quality gate enforces p99 latency ≤ baseline + 10%.
// VAL-BPHASE-034: FP32 comparison path remains available.

#![cfg(feature = "search")]

#[cfg(test)]
mod tests {
    use leindex::search::quantization::Int8HnswParams;
    use leindex::search::{
        Int8PromotionDecision, Int8QualityGate, Int8QualityThresholds, NodeInfo, SearchEngine,
    };
    use std::collections::HashSet;
    use std::time::Instant;

    // ========================================================================
    // Helpers
    // ========================================================================

    /// Create a corpus of test nodes with known embedding structure.
    ///
    /// Each node gets a deterministic embedding based on its index.
    /// The embeddings are designed so that:
    /// - Nodes in the same "cluster" (i % 5) have similar embeddings
    /// - Nodes in different clusters have dissimilar embeddings
    /// - This creates clear ground-truth relevance sets for NDCG computation
    fn create_corpus_nodes(count: usize, dimension: usize) -> Vec<NodeInfo> {
        let mut nodes = Vec::new();
        for i in 0..count {
            let cluster = i % 5;
            let embedding: Vec<f32> = (0..dimension)
                .map(|j| {
                    // Base value from cluster centroid
                    let base = if j == cluster { 0.9 } else { 0.01 };
                    // Small per-node variation
                    base + ((i as f32 * 0.001) + (j as f32 * 0.0001)) % 0.05
                })
                .collect();

            // Content keywords align with clusters for text relevance
            let content = match cluster {
                0 => format!("fn authenticate_{}() {{ verify_password(); }}", i),
                1 => format!("fn parse_config_{}() {{ read_file(); }}", i),
                2 => format!("fn handle_request_{}() {{ route(); }}", i),
                3 => format!("fn compute_hash_{}() {{ sha256(); }}", i),
                _ => format!("fn format_output_{}() {{ serialize(); }}", i),
            };

            nodes.push(NodeInfo {
                node_id: format!("node_{}", i),
                file_path: format!("module_{}.rs", cluster),
                symbol_name: format!("symbol_{}", i),
                language: "rust".to_string(),
                content,
                byte_range: (0, 50),
                tfidf_embedding: embedding,
                neural_embedding: None,
                complexity: (i % 10) as u32 + 1,
                signature: None,
                pre_tokenized: None,
            });
        }
        nodes
    }

    /// Build a set of "relevant" node IDs for a query targeting a specific
    /// cluster. For NDCG computation, nodes in the target cluster are
    /// considered relevant.
    fn relevant_ids_for_cluster(corpus_size: usize, cluster: usize) -> HashSet<String> {
        (0..corpus_size)
            .filter(|i| i % 5 == cluster)
            .map(|i| format!("node_{}", i))
            .collect()
    }

    /// Build a query embedding that targets a specific cluster centroid.
    fn query_embedding_for_cluster(dimension: usize, cluster: usize) -> Vec<f32> {
        (0..dimension)
            .map(|j| if j == cluster { 0.95f32 } else { 0.01 })
            .collect()
    }

    /// Run semantic search on an engine and return the ranked node IDs.
    #[allow(dead_code)]
    fn search_ranked_ids(engine: &SearchEngine, query_emb: &[f32], top_k: usize) -> Vec<String> {
        engine
            .semantic_search(query_emb, top_k)
            .unwrap()
            .into_iter()
            .map(|e| e.node_id)
            .collect()
    }

    /// Run semantic search on an engine and collect latency samples.
    fn search_with_latency(
        engine: &SearchEngine,
        query_emb: &[f32],
        top_k: usize,
        repetitions: usize,
    ) -> (Vec<String>, Vec<u64>) {
        // Warm-up
        let _ = engine.semantic_search(query_emb, top_k);

        let mut latencies = Vec::with_capacity(repetitions);
        let mut last_ids = Vec::new();

        for _ in 0..repetitions {
            let start = Instant::now();
            let results = engine.semantic_search(query_emb, top_k).unwrap();
            latencies.push(start.elapsed().as_nanos() as u64);
            if last_ids.is_empty() {
                last_ids = results.into_iter().map(|e| e.node_id).collect();
            }
        }

        (last_ids, latencies)
    }

    // ========================================================================
    // VAL-BPHASE-030: INT8 path is not promoted unless quality gate passes
    // ========================================================================

    #[test]
    fn test_int8_not_promoted_when_quality_gate_fails() {
        // Construct a scenario where INT8 quality is degraded.
        // Use custom thresholds that are very tight so that even small
        // quantization loss fails the gate.
        let tight_thresholds = Int8QualityThresholds {
            ndcg10_max_drop: 0.001, // 0.1% — extremely tight
            p50_max_increase: 0.01, // 1%
            p99_max_increase: 0.02, // 2%
        };
        let gate = Int8QualityGate::new(tight_thresholds);

        // Simulate metrics where INT8 has a meaningful NDCG drop
        let report = gate.evaluate(
            0.95,                         // baseline NDCG@10
            0.90,                         // INT8 NDCG@10 — 5.3% drop, well above 0.1% threshold
            &[100_000, 110_000, 120_000], // baseline latencies
            &[105_000, 115_000, 125_000], // INT8 latencies
        );

        // NDCG should fail
        assert!(!report.ndcg10_passed, "NDCG@10 should fail with 5.3% drop");

        // Overall should fail
        assert!(
            !report.overall_passed,
            "Gate should not pass when NDCG fails"
        );

        // Promotion should be blocked
        match report.promotion_decision() {
            Int8PromotionDecision::Block(reason) => {
                assert!(
                    reason.contains("NDCG"),
                    "Block reason should mention NDCG: {}",
                    reason
                );
            }
            Int8PromotionDecision::Promote => {
                panic!("INT8 should NOT be promoted when quality gate fails");
            }
        }
    }

    #[test]
    fn test_int8_not_promoted_when_latency_exceeds_p50_threshold() {
        let gate = Int8QualityGate::with_default_thresholds();

        // NDCG is fine, but p50 latency is way over
        let report = gate.evaluate(
            0.95,       // baseline NDCG
            0.95,       // INT8 NDCG — no drop
            &[100_000], // baseline p50 = 100µs
            &[200_000], // INT8 p50 = 200µs — 100% increase, way over 5%
        );

        assert!(report.ndcg10_passed, "NDCG should pass");
        assert!(!report.p50_passed, "p50 should fail with 100% increase");
        assert!(!report.overall_passed, "Gate should fail on p50");

        match report.promotion_decision() {
            Int8PromotionDecision::Block(reason) => {
                assert!(
                    reason.contains("p50"),
                    "Block reason should mention p50: {}",
                    reason
                );
            }
            Int8PromotionDecision::Promote => panic!("Should not promote"),
        }
    }

    #[test]
    fn test_int8_not_promoted_when_latency_exceeds_p99_threshold() {
        let gate = Int8QualityGate::with_default_thresholds();

        // NDCG and p50 are fine, but p99 latency is over
        let report = gate.evaluate(
            0.95,
            0.95,
            &[100_000, 100_000, 100_000, 100_000, 100_000], // baseline
            &[100_000, 100_000, 100_000, 100_000, 500_000], // INT8: p99 is 500µs
        );

        assert!(report.ndcg10_passed, "NDCG should pass");
        assert!(report.p50_passed, "p50 should pass");
        assert!(!report.p99_passed, "p99 should fail");
        assert!(!report.overall_passed, "Gate should fail on p99");
    }

    // ========================================================================
    // VAL-BPHASE-031: INT8 quality gate enforces NDCG@10 drop ≤ 1%
    // ========================================================================

    #[test]
    fn test_ndcg10_drop_within_threshold_passes() {
        let gate = Int8QualityGate::with_default_thresholds();

        // 0.5% drop — within 1% threshold
        let report = gate.evaluate(
            0.950, // baseline
            0.945, // INT8 — 0.53% drop
            &[100_000],
            &[100_000],
        );

        assert!(report.ndcg10_passed, "NDCG@10 drop of 0.53% should pass");
        assert!(
            report.ndcg10_drop <= 0.01,
            "NDCG drop should be ≤ 0.01, got {:.6}",
            report.ndcg10_drop
        );
    }

    #[test]
    fn test_ndcg10_drop_exceeding_threshold_fails() {
        let gate = Int8QualityGate::with_default_thresholds();

        // 1.5% drop — exceeds 1% threshold
        let report = gate.evaluate(
            0.950,
            0.935, // 1.58% drop
            &[100_000],
            &[100_000],
        );

        assert!(
            !report.ndcg10_passed,
            "NDCG@10 drop of 1.58% should fail the 1% threshold"
        );
        assert!(report.ndcg10_drop > 0.01);
    }

    #[test]
    fn test_ndcg10_no_drop_always_passes() {
        let gate = Int8QualityGate::with_default_thresholds();

        let report = gate.evaluate(0.95, 0.96, &[100_000], &[100_000]);
        // INT8 is actually better — should definitely pass
        assert!(report.ndcg10_passed);
        assert!(report.ndcg10_drop <= 0.0);
    }

    #[test]
    fn test_ndcg_at_10_computation() {
        // Verify the NDCG@10 computation against a known example.
        let returned: Vec<String> = (0..10).map(|i| format!("doc_{}", i)).collect();

        // Case 1: All returned docs are relevant
        let all_relevant: HashSet<String> = returned.iter().cloned().collect();
        let ndcg = Int8QualityGate::ndcg_at_10(&returned, &all_relevant);
        assert!(
            (ndcg - 1.0).abs() < 1e-6,
            "NDCG@10 should be 1.0 when all returned are relevant, got {:.6}",
            ndcg
        );

        // Case 2: No returned docs are relevant
        let no_relevant: HashSet<String> = HashSet::new();
        let ndcg_zero = Int8QualityGate::ndcg_at_10(&returned, &no_relevant);
        assert!(
            ndcg_zero.abs() < 1e-6,
            "NDCG@10 should be 0.0 when none are relevant, got {:.6}",
            ndcg_zero
        );

        // Case 3: First 5 are relevant, last 5 are not
        let partial_relevant: HashSet<String> = (0..5).map(|i| format!("doc_{}", i)).collect();
        let ndcg_partial = Int8QualityGate::ndcg_at_10(&returned, &partial_relevant);
        // When the 5 relevant docs are all in the top-5 positions, DCG equals
        // ideal DCG (which also assumes 5 relevant docs in the best positions),
        // so NDCG = 1.0.
        assert!(
            (ndcg_partial - 1.0).abs() < 1e-6,
            "NDCG@10 should be 1.0 when all relevant docs are at the top, got {:.6}",
            ndcg_partial
        );
    }

    // ========================================================================
    // VAL-BPHASE-032: p50 latency ≤ baseline + 5%
    // VAL-BPHASE-033: p99 latency ≤ baseline + 10%
    // ========================================================================

    #[test]
    fn test_p50_latency_within_threshold() {
        let gate = Int8QualityGate::with_default_thresholds();

        // INT8 p50 is 3% above baseline — within 5%
        let report = gate.evaluate(
            0.95,
            0.95,
            &[100_000, 100_000, 100_000],
            &[103_000, 103_000, 103_000],
        );

        assert!(
            report.p50_passed,
            "p50 increase of 3% should pass the 5% threshold"
        );
        assert!(report.p50_increase <= 0.05);
    }

    #[test]
    fn test_p50_latency_exceeding_threshold() {
        let gate = Int8QualityGate::with_default_thresholds();

        // INT8 p50 is 8% above baseline — exceeds 5%
        let report = gate.evaluate(
            0.95,
            0.95,
            &[100_000, 100_000, 100_000],
            &[108_000, 108_000, 108_000],
        );

        assert!(
            !report.p50_passed,
            "p50 increase of 8% should fail the 5% threshold"
        );
    }

    #[test]
    fn test_p99_latency_within_threshold() {
        let gate = Int8QualityGate::with_default_thresholds();

        // INT8 p99 is 8% above baseline — within 10%
        let report = gate.evaluate(0.95, 0.95, &[100_000; 100], &[108_000; 100]);

        assert!(
            report.p99_passed,
            "p99 increase of 8% should pass the 10% threshold"
        );
    }

    #[test]
    fn test_p99_latency_exceeding_threshold() {
        let gate = Int8QualityGate::with_default_thresholds();

        // INT8 p99 is 15% above baseline — exceeds 10%
        let report = gate.evaluate(0.95, 0.95, &[100_000; 100], &[115_000; 100]);

        assert!(
            !report.p99_passed,
            "p99 increase of 15% should fail the 10% threshold"
        );
    }

    #[test]
    fn test_latency_percentile_edge_cases() {
        // Empty samples
        assert_eq!(Int8QualityGate::latency_percentile(&[], 0.5), 0);

        // Single sample
        assert_eq!(Int8QualityGate::latency_percentile(&[42], 0.5), 42);
        assert_eq!(Int8QualityGate::latency_percentile(&[42], 0.99), 42);

        // Two samples
        assert_eq!(Int8QualityGate::latency_percentile(&[10, 20], 0.0), 10);
        // p50: index = floor(2 * 0.5) = 1 → value at index 1 = 20
        assert_eq!(Int8QualityGate::latency_percentile(&[10, 20], 0.5), 20);
        assert_eq!(Int8QualityGate::latency_percentile(&[10, 20], 1.0), 20);
    }

    // ========================================================================
    // VAL-BPHASE-034: FP32 comparison path remains available
    // ========================================================================

    #[test]
    fn test_fp32_path_remains_available_after_int8_gate() {
        // Verify that the FP32 (brute-force / HNSW f32) search path is still
        // usable even after INT8 quality gate work has landed.

        let dimension = 64;
        let corpus_size = 200;

        // Build FP32 brute-force engine
        let mut fp32_engine = SearchEngine::with_dimension(dimension);
        fp32_engine.index_nodes(create_corpus_nodes(corpus_size, dimension));

        // Build INT8 HNSW engine
        let mut int8_engine = SearchEngine::with_dimension(dimension);
        int8_engine.enable_int8_hnsw(Some(
            Int8HnswParams::new()
                .with_m(8)
                .with_ef_construction(50)
                .with_ef_search(50),
        ));
        int8_engine.index_nodes(create_corpus_nodes(corpus_size, dimension));

        // Both engines should return results for the same query
        let query = query_embedding_for_cluster(dimension, 0);

        let fp32_results = fp32_engine.semantic_search(&query, 10).unwrap();
        let int8_results = int8_engine.semantic_search(&query, 10).unwrap();

        // FP32 path must still work
        assert!(
            !fp32_results.is_empty(),
            "FP32 search path must return results"
        );

        // INT8 path should also return results
        assert!(
            !int8_results.is_empty(),
            "INT8 search path must return results"
        );

        // Both paths should find relevant results (from cluster 0)
        let relevant = relevant_ids_for_cluster(corpus_size, 0);

        let fp32_relevant_count = fp32_results
            .iter()
            .filter(|r| relevant.contains(&r.node_id))
            .count();
        let int8_relevant_count = int8_results
            .iter()
            .filter(|r| relevant.contains(&r.node_id))
            .count();

        assert!(
            fp32_relevant_count > 0,
            "FP32 path must find relevant results"
        );
        assert!(
            int8_relevant_count > 0,
            "INT8 path must find relevant results"
        );
    }

    #[test]
    fn test_fp32_hnsw_path_remains_available() {
        // Verify that the FP32 HNSW path also remains available.
        use leindex::search::HNSWParams;

        let dimension = 64;
        let corpus_size = 200;

        let mut fp32_hnsw_engine = SearchEngine::with_hnsw(dimension, HNSWParams::default());
        fp32_hnsw_engine.index_nodes(create_corpus_nodes(corpus_size, dimension));

        let query = query_embedding_for_cluster(dimension, 2);
        let results = fp32_hnsw_engine.semantic_search(&query, 10).unwrap();

        assert!(!results.is_empty(), "FP32 HNSW path must return results");
    }

    #[test]
    fn test_fp32_brute_force_path_remains_available() {
        // Verify that the brute-force path remains available.
        let dimension = 64;
        let corpus_size = 100;

        let mut engine = SearchEngine::with_dimension(dimension);
        assert!(
            !engine.is_hnsw_enabled(),
            "Default engine should use brute-force"
        );
        assert!(
            !engine.is_quantized(),
            "Default engine should not be quantized"
        );

        engine.index_nodes(create_corpus_nodes(corpus_size, dimension));

        let query = query_embedding_for_cluster(dimension, 1);
        let results = engine.semantic_search(&query, 10).unwrap();

        assert!(
            !results.is_empty(),
            "FP32 brute-force path must return results"
        );
    }

    // ========================================================================
    // End-to-end quality gate evaluation with real search
    // ========================================================================

    #[test]
    fn test_quality_gate_e2e_with_real_search_results() {
        // Run a full end-to-end quality gate evaluation:
        // 1. Build FP32 and INT8 engines with the same corpus
        // 2. Run queries and measure NDCG@10 and latency
        // 3. Evaluate the quality gate

        let dimension = 64;
        let corpus_size = 500;
        let num_queries = 20;
        let top_k = 10;
        let repetitions = 5;

        let corpus = create_corpus_nodes(corpus_size, dimension);

        // FP32 engine
        let mut fp32_engine = SearchEngine::with_dimension(dimension);
        fp32_engine.index_nodes(corpus.clone());

        // INT8 engine
        let mut int8_engine = SearchEngine::with_dimension(dimension);
        int8_engine.enable_int8_hnsw(Some(
            Int8HnswParams::new()
                .with_m(16)
                .with_ef_construction(100)
                .with_ef_search(50),
        ));
        int8_engine.index_nodes(corpus);

        let gate = Int8QualityGate::with_default_thresholds();

        let mut fp32_ndcg_sum = 0.0;
        let mut int8_ndcg_sum = 0.0;
        let mut fp32_all_latencies = Vec::new();
        let mut int8_all_latencies = Vec::new();

        for q in 0..num_queries {
            let cluster = q % 5;
            let query = query_embedding_for_cluster(dimension, cluster);
            let relevant = relevant_ids_for_cluster(corpus_size, cluster);

            // FP32 search with latency
            let (fp32_ids, fp32_lat) =
                search_with_latency(&fp32_engine, &query, top_k, repetitions);
            fp32_all_latencies.extend(fp32_lat);

            // INT8 search with latency
            let (int8_ids, int8_lat) =
                search_with_latency(&int8_engine, &query, top_k, repetitions);
            int8_all_latencies.extend(int8_lat);

            // Compute NDCG@10 for each
            let fp32_ndcg = Int8QualityGate::ndcg_at_10(&fp32_ids, &relevant);
            let int8_ndcg = Int8QualityGate::ndcg_at_10(&int8_ids, &relevant);

            fp32_ndcg_sum += fp32_ndcg;
            int8_ndcg_sum += int8_ndcg;
        }

        let avg_fp32_ndcg = fp32_ndcg_sum / num_queries as f64;
        let avg_int8_ndcg = int8_ndcg_sum / num_queries as f64;

        // Evaluate the gate
        let report = gate.evaluate(
            avg_fp32_ndcg,
            avg_int8_ndcg,
            &fp32_all_latencies,
            &int8_all_latencies,
        );

        // The report should be valid regardless of pass/fail
        assert!(report.baseline_ndcg10 >= 0.0 && report.baseline_ndcg10 <= 1.0);
        assert!(report.int8_ndcg10 >= 0.0 && report.int8_ndcg10 <= 1.0);

        // The promotion decision should be deterministic
        let decision = report.promotion_decision();
        match &decision {
            Int8PromotionDecision::Promote => {
                assert!(report.overall_passed);
            }
            Int8PromotionDecision::Block(reason) => {
                assert!(!report.overall_passed);
                assert!(!reason.is_empty());
            }
        }

        // Regardless of the outcome, the FP32 path must still be usable
        let query = query_embedding_for_cluster(dimension, 0);
        let fp32_still_works = fp32_engine.semantic_search(&query, 5).unwrap();
        assert!(
            !fp32_still_works.is_empty(),
            "FP32 path must remain available after gate evaluation"
        );
    }

    // ========================================================================
    // Quality gate with synthetic metrics (deterministic)
    // ========================================================================

    #[test]
    fn test_quality_gate_all_pass() {
        let gate = Int8QualityGate::with_default_thresholds();

        // All metrics within thresholds
        let report = gate.evaluate(
            0.95,           // baseline NDCG
            0.945,          // INT8 NDCG — 0.53% drop, within 1%
            &[100_000; 50], // baseline latencies
            &[103_000; 50], // INT8 latencies — 3% increase, within 5%
        );

        assert!(report.ndcg10_passed, "NDCG should pass");
        assert!(report.p50_passed, "p50 should pass");
        assert!(report.p99_passed, "p99 should pass");
        assert!(report.overall_passed, "All thresholds should pass");

        assert_eq!(
            report.promotion_decision(),
            Int8PromotionDecision::Promote,
            "Should promote when all thresholds pass"
        );
    }

    #[test]
    fn test_quality_gate_all_fail() {
        let gate = Int8QualityGate::with_default_thresholds();

        // All metrics exceed thresholds
        let report = gate.evaluate(
            0.95,
            0.90, // 5.3% NDCG drop
            &[100_000; 50],
            &[200_000; 50], // 100% latency increase
        );

        assert!(!report.ndcg10_passed);
        assert!(!report.p50_passed);
        assert!(!report.p99_passed);
        assert!(!report.overall_passed);

        match report.promotion_decision() {
            Int8PromotionDecision::Block(reason) => {
                assert!(reason.contains("NDCG"));
                assert!(reason.contains("p50"));
                assert!(reason.contains("p99"));
            }
            Int8PromotionDecision::Promote => panic!("Should not promote"),
        }
    }

    #[test]
    fn test_quality_gate_ndcg_passes_latency_fails() {
        let gate = Int8QualityGate::with_default_thresholds();

        let report = gate.evaluate(
            0.95,
            0.948, // 0.2% drop — passes
            &[100_000; 50],
            &[150_000; 50], // 50% increase — fails
        );

        assert!(report.ndcg10_passed);
        assert!(!report.p50_passed);
        assert!(!report.overall_passed);
    }

    #[test]
    fn test_quality_thresholds_are_configurable() {
        // Verify that custom thresholds work correctly
        let custom = Int8QualityThresholds {
            ndcg10_max_drop: 0.05,  // 5%
            p50_max_increase: 0.20, // 20%
            p99_max_increase: 0.50, // 50%
        };
        let gate = Int8QualityGate::new(custom);

        // These metrics would fail default thresholds but pass custom ones
        let report = gate.evaluate(
            0.95,
            0.92, // 3.2% drop — passes 5% custom threshold
            &[100_000; 50],
            &[115_000; 50], // 15% increase — passes 20% custom threshold
        );

        assert!(report.ndcg10_passed);
        assert!(report.p50_passed);
        assert!(report.overall_passed);
    }

    #[test]
    fn test_quality_gate_with_zero_baseline_latency() {
        // Edge case: zero baseline latency (should not divide by zero)
        let gate = Int8QualityGate::with_default_thresholds();

        let report = gate.evaluate(0.95, 0.95, &[0, 0, 0], &[0, 0, 0]);

        assert!(report.p50_increase == 0.0);
        assert!(report.p99_increase == 0.0);
        assert!(report.p50_passed);
        assert!(report.p99_passed);
    }

    #[test]
    fn test_promotion_decision_is_explicit_not_silent() {
        // VAL-BPHASE-030: The system does not silently promote INT8.
        // The promotion decision is always explicit and observable.

        let gate = Int8QualityGate::with_default_thresholds();

        // Case 1: Passing metrics → explicit Promote
        let passing = gate.evaluate(0.95, 0.945, &[100_000; 10], &[103_000; 10]);
        assert!(passing.overall_passed);
        let decision = passing.promotion_decision();
        assert_eq!(decision, Int8PromotionDecision::Promote);

        // Case 2: Failing metrics → explicit Block with reason
        let failing = gate.evaluate(0.95, 0.85, &[100_000; 10], &[200_000; 10]);
        assert!(!failing.overall_passed);
        let decision = failing.promotion_decision();
        match decision {
            Int8PromotionDecision::Block(reason) => {
                // The reason must be non-empty and informative
                assert!(!reason.is_empty());
            }
            Int8PromotionDecision::Promote => panic!("Must not promote failing metrics"),
        }
    }

    // ========================================================================
    // NDCG computation correctness
    // ========================================================================

    #[test]
    fn test_ndcg_perfect_ranking() {
        let relevant: HashSet<String> = (0..10).map(|i| format!("doc_{}", i)).collect();
        let returned: Vec<String> = (0..10).map(|i| format!("doc_{}", i)).collect();

        let ndcg = Int8QualityGate::ndcg_at_10(&returned, &relevant);
        assert!(
            (ndcg - 1.0).abs() < 1e-6,
            "Perfect ranking should give NDCG=1.0, got {:.6}",
            ndcg
        );
    }

    #[test]
    fn test_ndcg_worst_ranking() {
        // All relevant docs are in the set but returned IDs are all irrelevant
        let relevant: HashSet<String> = (0..10).map(|i| format!("rel_{}", i)).collect();
        let returned: Vec<String> = (0..10).map(|i| format!("irrel_{}", i)).collect();

        let ndcg = Int8QualityGate::ndcg_at_10(&returned, &relevant);
        assert!(
            ndcg.abs() < 1e-6,
            "No relevant results should give NDCG=0.0, got {:.6}",
            ndcg
        );
    }

    #[test]
    fn test_ndcg_partial_ranking() {
        // 5 relevant out of 10 returned, but the relevant ones are NOT all at
        // the top — they're interleaved with irrelevant ones.
        // Relevant: doc_0, doc_2, doc_4, doc_6, doc_8
        let relevant: HashSet<String> = [0, 2, 4, 6, 8]
            .iter()
            .map(|i| format!("doc_{}", i))
            .collect();
        // Returned in order: doc_0, doc_1, doc_2, doc_3, doc_4, doc_5, doc_6, doc_7, doc_8, doc_9
        let returned: Vec<String> = (0..10).map(|i| format!("doc_{}", i)).collect();

        let ndcg = Int8QualityGate::ndcg_at_10(&returned, &relevant);
        // Should be less than 1.0 because relevant docs are not all at the top
        assert!(
            ndcg > 0.0 && ndcg < 1.0,
            "NDCG should be between 0 and 1 for interleaved ranking, got {:.6}",
            ndcg
        );

        // If we put all relevant docs at the top, NDCG should be higher
        let good_order: Vec<String> = [0, 2, 4, 6, 8, 1, 3, 5, 7, 9]
            .iter()
            .map(|i| format!("doc_{}", i))
            .collect();
        let good_ndcg = Int8QualityGate::ndcg_at_10(&good_order, &relevant);

        assert!(
            good_ndcg >= ndcg,
            "Relevant docs at top should have higher NDCG ({:.6}) than interleaved ({:.6})",
            good_ndcg,
            ndcg
        );
    }

    #[test]
    fn test_ndcg_with_fewer_returned_than_k() {
        let relevant: HashSet<String> = (0..20).map(|i| format!("doc_{}", i)).collect();
        let returned: Vec<String> = (0..3).map(|i| format!("doc_{}", i)).collect();

        let ndcg = Int8QualityGate::ndcg_at_10(&returned, &relevant);
        // Only 3 returned, all relevant — should be less than 1.0 because
        // ideal has 10 relevant results
        assert!(ndcg > 0.0 && ndcg < 1.0);
    }
}
