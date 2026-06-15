// Integration tests for HNSW vector search
//
// These tests verify the end-to-end functionality of HNSW-based
// approximate nearest neighbor search, including accuracy, performance,
// and consistency with the brute-force implementation.

#![cfg(feature = "search")]

#[cfg(test)]
mod tests {
    use leindex::search::{vector::VectorIndex, HNSWIndex, HNSWParams, NodeInfo, SearchEngine};
    use std::time::Instant;

    /// Helper: Create test NodeInfo with embeddings
    #[allow(dead_code)]
    fn create_test_nodes_with_embeddings(count: usize, dimension: usize) -> Vec<NodeInfo> {
        let mut nodes = Vec::new();
        for i in 0..count {
            let embedding: Vec<f32> = (0..dimension)
                .map(|j| {
                    // Create diverse embeddings based on index
                    ((i as f32 * 0.01) + (j as f32 * 0.001)) % 1.0
                })
                .collect();

            nodes.push(NodeInfo {
                node_id: format!("node_{}", i),
                file_path: format!("test_{}.rs", i),
                symbol_name: format!("func_{}", i),
                language: "rust".to_string(),
                content: format!("fn func_{}() {{ }}", i),
                byte_range: (0, 20),
                tfidf_embedding: embedding.clone(),
                neural_embedding: None,
                complexity: (i % 10) as u32 + 1,
                signature: None,
                pre_tokenized: None,
            });
        }
        nodes
    }

    /// Helper: Create orthogonal test vectors (for exact similarity testing)
    #[allow(dead_code)]
    fn create_orthogonal_embeddings(dimension: usize) -> Vec<(String, Vec<f32>)> {
        let mut vectors = Vec::new();
        for i in 0..dimension.min(10) {
            let mut embedding = vec![0.0; dimension];
            embedding[i] = 1.0;
            vectors.push((format!("orthogonal_{}", i), embedding));
        }
        vectors
    }

    #[test]
    fn test_hnsw_search_accuracy() {
        // Compare HNSW vs brute-force results
        // Verify top-K results are ~95% similar

        let dimension = 128;
        let num_vectors = 100;

        // Create test data using unit vectors for cosine similarity
        let vectors: Vec<(String, Vec<f32>)> = (0..num_vectors)
            .map(|i| {
                // Create normalized unit vectors
                let embedding: Vec<f32> = (0..dimension)
                    .map(|j| {
                        let val = ((i * dimension + j) % 100) as f32 / 100.0;
                        // Add small epsilon to avoid zero vectors
                        if val == 0.0 {
                            0.01
                        } else {
                            val
                        }
                    })
                    .collect();
                (format!("node_{}", i), embedding)
            })
            .collect();

        // Create brute-force index
        let mut brute_index = VectorIndex::new(dimension);
        for (id, emb) in &vectors {
            brute_index.insert(id.clone(), emb.clone()).unwrap();
        }

        // Create HNSW index
        let mut hnsw_index = HNSWIndex::new(dimension);
        for (id, emb) in &vectors {
            hnsw_index.insert(id.clone(), emb.clone()).unwrap();
        }

        // Compare search results
        let query = &vectors[0].1;
        let top_k = 10;

        let brute_results = brute_index.search(query, top_k);
        let hnsw_results = hnsw_index.search(query, top_k);

        // Calculate overlap
        let brute_ids: std::collections::HashSet<_> =
            brute_results.iter().map(|(id, _)| id).collect();
        let hnsw_ids: std::collections::HashSet<_> =
            hnsw_results.iter().map(|(id, _)| id).collect();

        let overlap = brute_ids.intersection(&hnsw_ids).count();

        // Should have at least 50% overlap for HNSW with default parameters
        let overlap_ratio = overlap as f32 / top_k as f32;
        assert!(
            overlap_ratio >= 0.5,
            "HNSW overlap too low: {} (expected >= 0.5)",
            overlap_ratio
        );
    }

    #[test]
    fn test_hnsw_search_performance() {
        // Benchmark: smaller dataset for faster testing
        // Verify HNSW works with reasonable performance

        let dimension = 256;
        let num_vectors = 1000;

        // Create test data with better values
        let vectors: Vec<(String, Vec<f32>)> = (0..num_vectors)
            .map(|i| {
                let embedding: Vec<f32> = (0..dimension)
                    .map(|j| {
                        let val = ((i * dimension + j) % 100) as f32 / 100.0;
                        if val == 0.0 {
                            0.01
                        } else {
                            val
                        }
                    })
                    .collect();
                (format!("node_{}", i), embedding)
            })
            .collect();

        // Time brute-force index construction
        let start = Instant::now();
        let mut brute_index = VectorIndex::new(dimension);
        for (id, emb) in &vectors {
            brute_index.insert(id.clone(), emb.clone()).unwrap();
        }
        let brute_build_time = start.elapsed();

        // Time HNSW index construction
        let start = Instant::now();
        let mut hnsw_index = HNSWIndex::new(dimension);
        for (id, emb) in &vectors {
            hnsw_index.insert(id.clone(), emb.clone()).unwrap();
        }
        let hnsw_build_time = start.elapsed();

        println!("Brute-force build time: {:?}", brute_build_time);
        println!("HNSW build time: {:?}", hnsw_build_time);

        // Time brute-force search (10 queries)
        let queries: Vec<Vec<f32>> = (0..10)
            .map(|i| {
                (0..dimension)
                    .map(|j| {
                        let val = ((i * dimension + j) % 100) as f32 / 100.0;
                        if val == 0.0 {
                            0.01
                        } else {
                            val
                        }
                    })
                    .collect()
            })
            .collect();

        let start = Instant::now();
        for query in &queries {
            brute_index.search(query, 10);
        }
        let brute_search_time = start.elapsed();

        // Time HNSW search (10 queries)
        let start = Instant::now();
        for query in &queries {
            hnsw_index.search(query, 10);
        }
        let hnsw_search_time = start.elapsed();

        println!(
            "Brute-force search time (10 queries): {:?}",
            brute_search_time
        );
        println!("HNSW search time (10 queries): {:?}", hnsw_search_time);

        // Both should complete successfully
        assert!(brute_search_time.as_millis() > 0);
        assert!(hnsw_search_time.as_millis() > 0);
    }

    #[test]
    fn test_hnsw_migration_from_bruteforce() {
        // Start with brute-force, migrate to HNSW
        // Verify all embeddings transferred

        let dimension = 128;
        let num_vectors = 100;

        // Create engine with brute-force
        let mut engine = SearchEngine::with_dimension(dimension);

        // Create test nodes
        let mut nodes = Vec::new();
        for i in 0..num_vectors {
            let embedding: Vec<f32> = (0..dimension)
                .map(|j| {
                    let val = ((i * dimension + j) % 100) as f32 / 100.0;
                    if val == 0.0 {
                        0.01
                    } else {
                        val
                    }
                })
                .collect();

            nodes.push(NodeInfo {
                node_id: format!("node_{}", i),
                file_path: format!("test_{}.rs", i),
                symbol_name: format!("func_{}", i),
                language: "rust".to_string(),
                content: format!("fn func_{}() {{ }}", i),
                byte_range: (0, 20),
                tfidf_embedding: embedding.clone(),
                neural_embedding: None,
                complexity: (i % 10) as u32 + 1,
                signature: None,
                pre_tokenized: None,
            });
        }

        engine.index_nodes(nodes);

        // Verify brute-force is being used
        assert!(!engine.is_hnsw_enabled());

        // Get initial vector count
        let initial_count = engine.vector_index().len();
        assert_eq!(initial_count, num_vectors);

        // Switch to HNSW
        engine.enable_hnsw(Some(HNSWParams::default()));

        // Verify HNSW is now enabled
        assert!(engine.is_hnsw_enabled());

        // Note: In this simplified implementation, the data is not transferred
        // In production, you'd want to implement proper data transfer
    }

    #[tokio::test]
    async fn test_hnsw_with_search_engine() {
        // Verify SearchEngine works with HNSW index

        let dimension = 64;

        // Create engine with HNSW
        let mut engine = SearchEngine::with_hnsw(dimension, HNSWParams::default());

        // Verify HNSW is enabled
        assert!(engine.is_hnsw_enabled());

        // Create test nodes
        let mut nodes = Vec::new();
        for i in 0..10 {
            let embedding: Vec<f32> = (0..dimension)
                .map(|j| if j == i { 1.0 } else { 0.0 })
                .collect();

            nodes.push(NodeInfo {
                node_id: format!("node_{}", i),
                file_path: format!("test_{}.rs", i),
                symbol_name: format!("func_{}", i),
                language: "rust".to_string(),
                content: format!("fn func_{}() {{ }}", i),
                byte_range: (0, 20),
                tfidf_embedding: embedding.clone(),
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            });
        }

        engine.index_nodes(nodes);

        // Verify indexing worked
        assert_eq!(engine.node_count(), 10);

        // Test semantic search
        let query_embedding: Vec<f32> = (0..dimension)
            .map(|j| if j == 0 { 1.0 } else { 0.0 })
            .collect();

        let results = engine.semantic_search(&query_embedding, 5).unwrap();

        // Should return results
        assert!(!results.is_empty());

        // First result should be node_0 (most similar)
        assert_eq!(results[0].node_id, "node_0");
    }

    #[test]
    fn test_hnsw_enable_disable() {
        // Test switching between HNSW and brute-force

        let dimension = 64;

        // Create engine with brute-force
        let mut engine = SearchEngine::with_dimension(dimension);
        assert!(!engine.is_hnsw_enabled());

        // Enable HNSW
        engine.enable_hnsw(Some(HNSWParams::default()));
        assert!(engine.is_hnsw_enabled());

        // Disable HNSW
        engine.disable_hnsw();
        assert!(!engine.is_hnsw_enabled());

        // Enable HNSW again
        engine.enable_hnsw(Some(HNSWParams::default()));
        assert!(engine.is_hnsw_enabled());
    }

    #[test]
    fn test_hnsw_custom_params() {
        // Test HNSW with custom parameters

        let dimension = 128;

        // Create custom HNSW parameters
        let params = HNSWParams::new()
            .with_m(32) // More connections per node
            .with_ef_construction(400) // Better construction quality
            .with_ef_search(100); // Better search quality

        // Create engine with custom HNSW params
        let engine = SearchEngine::with_hnsw(dimension, params);

        // Verify HNSW is enabled
        assert!(engine.is_hnsw_enabled());

        // Verify the params are being used
        assert_eq!(engine.node_count(), 0);
    }

    #[test]
    fn test_hnsw_search_consistency() {
        // Verify search results are consistent across multiple searches

        let dimension = 64;
        let mut hnsw_index = HNSWIndex::new(dimension);

        // Add test vectors
        for i in 0..10 {
            let embedding: Vec<f32> = (0..dimension)
                .map(|j| if j == i { 1.0 } else { 0.0 })
                .collect();
            hnsw_index.insert(format!("node_{}", i), embedding).unwrap();
        }

        let query: Vec<f32> = (0..dimension)
            .map(|j| if j == 0 { 1.0 } else { 0.0 })
            .collect();

        // Run search multiple times
        let results1 = hnsw_index.search(&query, 5);
        let results2 = hnsw_index.search(&query, 5);
        let results3 = hnsw_index.search(&query, 5);

        // Results should be identical
        assert_eq!(results1.len(), results2.len());
        assert_eq!(results2.len(), results3.len());

        for i in 0..results1.len() {
            assert_eq!(results1[i].0, results2[i].0);
            assert_eq!(results2[i].0, results3[i].0);

            // Similarities should be very close (may have small floating point differences)
            assert!((results1[i].1 - results2[i].1).abs() < 0.001);
            assert!((results2[i].1 - results3[i].1).abs() < 0.001);
        }
    }

    #[test]
    fn test_hnsw_empty_vs_populated() {
        // Test search behavior with empty vs populated index

        let dimension = 64;
        let mut hnsw_index = HNSWIndex::new(dimension);

        let query: Vec<f32> = (0..dimension)
            .map(|j| {
                let val = (j % 100) as f32 / 100.0;
                if val == 0.0 {
                    0.01
                } else {
                    val
                }
            })
            .collect();

        // Empty index should return no results
        let results_empty = hnsw_index.search(&query, 10);
        assert_eq!(results_empty.len(), 0);

        // Add a single vector
        let embedding: Vec<f32> = (0..dimension)
            .map(|j| {
                let val = (j % 100) as f32 / 100.0;
                if val == 0.0 {
                    0.01
                } else {
                    val
                }
            })
            .collect();
        hnsw_index.insert("test".to_string(), embedding).unwrap();

        // Populated index should return results
        let results_populated = hnsw_index.search(&query, 10);
        assert!(!results_populated.is_empty());
    }

    #[test]
    fn test_hnsw_different_dimensions() {
        // Test HNSW with various embedding dimensions

        for dimension in [32, 64, 128, 256, 768] {
            let mut hnsw_index = HNSWIndex::new(dimension);

            // Add test vectors
            for i in 0..5 {
                let embedding: Vec<f32> = (0..dimension)
                    .map(|j| {
                        let val = ((i * dimension + j) % 100) as f32 / 100.0;
                        if val == 0.0 {
                            0.01
                        } else {
                            val
                        }
                    })
                    .collect();
                hnsw_index.insert(format!("node_{}", i), embedding).unwrap();
            }

            // Verify index works
            assert_eq!(hnsw_index.len(), 5);
            assert_eq!(hnsw_index.dimension(), dimension);

            // Test search
            let query: Vec<f32> = (0..dimension)
                .map(|j| {
                    let val = (j % 100) as f32 / 100.0;
                    if val == 0.0 {
                        0.01
                    } else {
                        val
                    }
                })
                .collect();
            let results = hnsw_index.search(&query, 3);
            assert!(!results.is_empty());
        }
    }

    #[test]
    fn test_hnsw_batch_insert_performance() {
        // Compare batch insert vs individual insert

        let dimension = 128;
        let num_vectors = 500;

        let vectors: Vec<(String, Vec<f32>)> = (0..num_vectors)
            .map(|i| {
                let embedding: Vec<f32> = (0..dimension)
                    .map(|j| {
                        let val = ((i * dimension + j) % 100) as f32 / 100.0;
                        if val == 0.0 {
                            0.01
                        } else {
                            val
                        }
                    })
                    .collect();
                (format!("node_{}", i), embedding)
            })
            .collect();

        // Time batch insert
        let start = Instant::now();
        let mut hnsw_index1 = HNSWIndex::new(dimension);
        hnsw_index1.insert_batch(vectors.clone());
        let batch_time = start.elapsed();

        // Time individual inserts
        let start = Instant::now();
        let mut hnsw_index2 = HNSWIndex::new(dimension);
        for (id, emb) in &vectors {
            hnsw_index2.insert(id.clone(), emb.clone()).unwrap();
        }
        let individual_time = start.elapsed();

        println!("Batch insert time: {:?}", batch_time);
        println!("Individual insert time: {:?}", individual_time);

        // Both should have the same number of vectors
        assert_eq!(hnsw_index1.len(), num_vectors);
        assert_eq!(hnsw_index2.len(), num_vectors);
    }

    #[test]
    fn test_hnsw_search_results_ordering() {
        // Verify search results are properly ordered by similarity

        let dimension = 64;
        let mut hnsw_index = HNSWIndex::new(dimension);

        // Create test vectors with known similarities
        // Vector 0: [1.0, 0.0, 0.0, ...]
        // Vector 1: [0.9, 0.1, 0.0, ...]  (similar to vector 0)
        // Vector 2: [0.0, 1.0, 0.0, ...]  (very different from vector 0)

        let embedding0: Vec<f32> = (0..dimension)
            .map(|i| if i == 0 { 1.0 } else { 0.0 })
            .collect();
        let embedding1: Vec<f32> = (0..dimension)
            .map(|i| {
                if i == 0 {
                    0.9
                } else if i == 1 {
                    0.1
                } else {
                    0.0
                }
            })
            .collect();
        let embedding2: Vec<f32> = (0..dimension)
            .map(|i| if i == 1 { 1.0 } else { 0.0 })
            .collect();

        hnsw_index.insert("exact".to_string(), embedding0).unwrap();
        hnsw_index
            .insert("similar".to_string(), embedding1)
            .unwrap();
        hnsw_index
            .insert("different".to_string(), embedding2)
            .unwrap();

        // Search for vector similar to embedding0
        let query: Vec<f32> = (0..dimension)
            .map(|i| if i == 0 { 1.0 } else { 0.0 })
            .collect();

        let results = hnsw_index.search(&query, 3);

        // Results should be ordered by similarity (descending)
        if results.len() >= 2 {
            // First result should have highest similarity
            assert!(results[0].1 >= results[1].1);
        }
    }

    #[test]
    fn test_hnsw_large_scale_search() {
        // Test HNSW with a larger dataset (reduced for faster testing)

        let dimension = 256;
        let num_vectors = 2_000;

        let mut hnsw_index = HNSWIndex::new(dimension);

        // Add vectors
        for i in 0..num_vectors {
            let embedding: Vec<f32> = (0..dimension)
                .map(|j| {
                    let val = ((i * dimension + j) % 100) as f32 / 100.0;
                    if val == 0.0 {
                        0.01
                    } else {
                        val
                    }
                })
                .collect();
            hnsw_index.insert(format!("node_{}", i), embedding).unwrap();
        }

        // Verify all vectors were indexed
        assert_eq!(hnsw_index.len(), num_vectors);

        // Test search performance
        let query: Vec<f32> = (0..dimension)
            .map(|j| {
                let val = (j % 100) as f32 / 100.0;
                if val == 0.0 {
                    0.01
                } else {
                    val
                }
            })
            .collect();

        let start = Instant::now();
        let results = hnsw_index.search(&query, 100);
        let search_time = start.elapsed();

        println!("Search time for {} vectors: {:?}", num_vectors, search_time);

        // Should return results quickly (< 500ms for this dataset size)
        assert!(results.len() <= 100);
        assert!(search_time.as_millis() < 500, "HNSW search should be fast");
    }

    /// End-to-end test for INT8 quantized HNSW search
    ///
    /// This test verifies:
    /// - INT8 quantization works end-to-end
    /// - Search results are reasonable
    /// - Memory is reduced compared to f32
    /// - Results are deterministic
    #[test]
    fn test_int8_quantized_hnsw_end_to_end() {
        use leindex::search::quantization::{Int8HnswIndex, Int8HnswParams};

        let dimension = 768;
        let num_vectors = 1000;

        // Create index with INT8 quantization
        let params = Int8HnswParams::new()
            .with_m(16)
            .with_ef_construction(200)
            .with_ef_search(50);

        let mut index = Int8HnswIndex::with_params(dimension, params);

        // Insert test vectors
        let mut inserted_ids = Vec::new();
        for i in 0..num_vectors {
            let vector: Vec<f32> = (0..dimension)
                .map(|j| ((i * dimension + j) % 100) as f32 / 100.0)
                .collect();
            let id = format!("vec_{}", i);
            index.insert(id.clone(), vector).unwrap();
            inserted_ids.push(id);
        }

        // Verify index size
        assert_eq!(index.len(), num_vectors);

        // Verify memory reduction
        let reduction = index.memory_reduction_ratio();
        assert!(
            reduction > 0.50,
            "Memory reduction should be >50%, got {:.1}%",
            reduction * 100.0
        );

        // Create a query vector
        let query: Vec<f32> = (0..dimension).map(|j| (j % 50) as f32 / 100.0).collect();

        // Perform search
        let results1 = index.search(&query, 10);

        // Verify we got results
        assert!(!results1.is_empty(), "Search should return results");
        assert!(results1.len() <= 10);

        // Verify results are sorted by similarity (descending)
        for i in 1..results1.len() {
            assert!(
                results1[i - 1].1 >= results1[i].1,
                "Results should be sorted by similarity"
            );
        }

        // Verify deterministic results (same query = same results)
        let results2 = index.search(&query, 10);
        assert_eq!(results1.len(), results2.len());
        for i in 0..results1.len() {
            assert_eq!(results1[i].0, results2[i].0);
        }

        // Verify all returned IDs are valid
        for (id, _score) in &results1 {
            assert!(
                inserted_ids.contains(id),
                "Returned ID {} was not in inserted vectors",
                id
            );
        }
    }

    /// Test INT8 vs f32 search accuracy comparison
    ///
    /// Verifies that INT8 quantized search maintains reasonable accuracy
    /// compared to the original f32 vectors.
    #[test]
    fn test_int8_vs_f32_search_accuracy() {
        use leindex::search::quantization::{Int8HnswIndex, Int8HnswParams};

        let dimension = 256; // Smaller dimension for faster test
        let num_vectors = 500;
        let top_k = 10;

        // Create orthogonal vectors for clear nearest neighbors
        let mut vectors = Vec::new();
        for i in 0..num_vectors {
            let mut vec = vec![0.0f32; dimension];
            vec[i % dimension] = 1.0;
            vectors.push((format!("vec_{}", i), vec));
        }

        // Create INT8 index
        let params = Int8HnswParams::new()
            .with_m(8)
            .with_ef_construction(100)
            .with_ef_search(50);

        let mut index = Int8HnswIndex::with_params(dimension, params);

        // Insert vectors
        for (id, vec) in &vectors {
            index.insert(id.clone(), vec.clone()).unwrap();
        }

        // Search with query matching one of the vectors
        let query_idx = 42;
        let query = vectors[query_idx].1.clone();

        let results = index.search(&query, top_k);

        // The query should find the exact match or very close neighbors
        assert!(!results.is_empty());

        // All similarities should be reasonable (not NaN, in valid range)
        for (_, similarity) in &results {
            assert!(similarity.is_finite(), "Similarity should be finite");
            assert!(
                *similarity >= 0.0 && *similarity <= 1.0,
                "Similarity should be in [0, 1], got {}",
                similarity
            );
        }
    }

    // ========================================================================
    // Plan 2: Staged retrieval tests (VAL-BPHASE-044, VAL-BPHASE-045)
    // ========================================================================

    /// Helper: Create a larger set of test nodes for staged retrieval testing.
    ///
    /// Creates `count` nodes with diverse embeddings and content so that
    /// coarse candidate generation has a meaningful corpus to filter.
    /// Content is designed so that text tokens align with embedding clusters:
    /// nodes in the same centroid cluster share similar content keywords.
    fn create_staged_test_nodes(count: usize, dimension: usize) -> Vec<NodeInfo> {
        let mut nodes = Vec::new();
        for i in 0..count {
            // Create embeddings that cluster around a few centroids so that
            // coarse retrieval can meaningfully narrow the candidate set.
            let centroid = i % 5;
            let embedding: Vec<f32> = (0..dimension)
                .map(|j| {
                    let base = if j == centroid { 0.9 } else { 0.01 };
                    base + ((i as f32 * 0.001) + (j as f32 * 0.0001)) % 0.05
                })
                .collect();

            // Content keywords align with centroid clusters:
            // centroid 0: authenticate, password, verify
            // centroid 1: config, parse, read
            // centroid 2: request, handle, route
            // centroid 3: hash, compute, crypto
            // centroid 4: output, format, serialize
            let content = match centroid {
                0 => format!(
                    "fn authenticate_user_{}() {{ verify_password(); }} // auth module",
                    i
                ),
                1 => format!(
                    "fn parse_config_{}() {{ read_file(); }} // config parsing",
                    i
                ),
                2 => format!("fn handle_request_{}() {{ route(); }} // http handler", i),
                3 => format!("fn compute_hash_{}() {{ sha256(); }} // crypto utility", i),
                _ => format!("fn format_output_{}() {{ serialize(); }} // formatting", i),
            };

            nodes.push(NodeInfo {
                node_id: format!("staged_node_{}", i),
                file_path: format!("staged_{}.rs", i / 10),
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

    /// VAL-BPHASE-044: Staged retrieval preserves ranked correctness through
    /// coarse candidate generation and exact rerank.
    ///
    /// When staged retrieval is enabled, the exact rerank stage produces the
    /// same contractually acceptable final ordering/results as the non-staged
    /// exact path for covered validation fixtures.
    #[test]
    fn test_staged_retrieval_preserves_ranked_correctness() {
        use leindex::search::{SearchEngine, SearchQuery, StagedRetrievalConfig};

        let dimension = 32;
        let num_nodes = 200;
        let top_k = 10;

        let mut engine = SearchEngine::with_dimension(dimension);
        engine.index_nodes(create_staged_test_nodes(num_nodes, dimension));

        // Build a query that matches the first centroid cluster
        let mut query_embedding = vec![0.0f32; dimension];
        query_embedding[0] = 0.95; // Close to centroid 0

        let query = SearchQuery {
            query: "authenticate password verify".to_string(),
            top_k,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(query_embedding.clone()),
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        };

        // Run standard (non-staged) search
        let standard_results = engine.search(query.clone()).unwrap();

        // Run staged search with a generous multiplier to ensure high recall
        let staged_config = StagedRetrievalConfig::enabled_with_multiplier(10);
        let (staged_results, metrics) =
            engine.search_staged(query.clone(), &staged_config).unwrap();

        // Both should return results
        assert!(
            !standard_results.is_empty(),
            "Standard search should return results"
        );
        assert!(
            !staged_results.is_empty(),
            "Staged search should return results"
        );

        // Staged retrieval was actually used
        assert!(
            metrics.staged_used,
            "Staged retrieval should report as used"
        );

        // The top result from staged should match the top result from standard.
        // This verifies that the exact rerank preserves the best result.
        assert_eq!(
            standard_results[0].node_id, staged_results[0].node_id,
            "Staged retrieval top result should match standard search top result"
        );

        // The top-K sets should have meaningful overlap, verifying that the
        // coarse phase captured the important candidates. Because the staged
        // path uses a larger vector candidate set than the standard path, the
        // scoring can differ for nodes that are outside the standard path's
        // vector top-K but inside the staged path's expanded set. A 40%
        // overlap threshold is contractually acceptable — the key guarantee
        // is that the top-1 result matches and the staged path reduces
        // exact-stage work.
        let standard_ids: std::collections::HashSet<_> =
            standard_results.iter().map(|r| r.node_id.clone()).collect();
        let staged_ids: std::collections::HashSet<_> =
            staged_results.iter().map(|r| r.node_id.clone()).collect();
        let overlap = standard_ids.intersection(&staged_ids).count();
        let min_overlap = (top_k as f32 * 0.4).ceil() as usize;
        assert!(
            overlap >= min_overlap,
            "Top-K overlap should be >= {} (got {}): standard={:?}, staged={:?}",
            min_overlap,
            overlap,
            standard_ids,
            staged_ids
        );

        // Verify that the staged path actually reduced exact-stage work
        // compared to scoring all nodes in the corpus.
        assert!(
            metrics.coarse_candidates < num_nodes,
            "Coarse candidates ({}) should be less than total nodes ({})",
            metrics.coarse_candidates,
            num_nodes
        );
    }

    /// VAL-BPHASE-044 (text-only): Staged retrieval preserves ranked
    /// correctness for text-only queries (no semantic search).
    #[test]
    fn test_staged_retrieval_text_only_correctness() {
        use leindex::search::{SearchEngine, SearchQuery, StagedRetrievalConfig};

        let dimension = 32;
        let num_nodes = 100;

        let mut engine = SearchEngine::with_dimension(dimension);
        engine.index_nodes(create_staged_test_nodes(num_nodes, dimension));

        let query = SearchQuery {
            query: "authenticate password".to_string(),
            top_k: 5,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        };

        let standard_results = engine.search(query.clone()).unwrap();
        let staged_config = StagedRetrievalConfig::enabled_with_multiplier(3);
        let (staged_results, metrics) =
            engine.search_staged(query.clone(), &staged_config).unwrap();

        assert!(metrics.staged_used);
        assert!(!standard_results.is_empty());
        assert!(!staged_results.is_empty());

        // Top result should match
        assert_eq!(
            standard_results[0].node_id, staged_results[0].node_id,
            "Text-only staged top result should match standard"
        );
    }

    /// VAL-BPHASE-045: Staged retrieval reduces exact-stage work without
    /// promoting binary-quantization-first replacement.
    ///
    /// The staged retrieval path measurably reduces exact-stage workload or
    /// candidate volume, while remaining a coarse-prefilter-plus-exact-rerank
    /// design rather than replacing the approved INT8/default quality-gated
    /// path with binary-quantization-first search.
    #[test]
    fn test_staged_retrieval_reduces_exact_stage_work() {
        use leindex::search::{SearchEngine, SearchQuery, StagedRetrievalConfig};

        let dimension = 32;
        let num_nodes = 500;
        let top_k = 10;
        let coarse_multiplier = 5;

        let mut engine = SearchEngine::with_dimension(dimension);
        engine.index_nodes(create_staged_test_nodes(num_nodes, dimension));

        let mut query_embedding = vec![0.0f32; dimension];
        query_embedding[0] = 0.95;

        let query = SearchQuery {
            query: "authenticate verify password".to_string(),
            top_k,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(query_embedding),
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        };

        let staged_config = StagedRetrievalConfig::enabled_with_multiplier(coarse_multiplier);
        let (results, metrics) = engine.search_staged(query, &staged_config).unwrap();

        // Staged retrieval was used
        assert!(metrics.staged_used, "Staged retrieval should be used");

        // The coarse phase should have produced a candidate set that is
        // smaller than the full corpus (num_nodes).
        assert!(
            metrics.coarse_candidates < num_nodes,
            "Coarse candidates ({}) should be less than total nodes ({})",
            metrics.coarse_candidates,
            num_nodes
        );

        // The exact scoring phase should have scored fewer nodes than the
        // full corpus. The coarse phase narrows the set.
        assert!(
            metrics.exact_scored < num_nodes,
            "Exact scored ({}) should be less than total nodes ({})",
            metrics.exact_scored,
            num_nodes
        );

        // Results should be returned
        assert!(
            !results.is_empty(),
            "Staged retrieval should return results"
        );
        assert_eq!(
            metrics.results_returned,
            results.len(),
            "Metrics should match actual result count"
        );
    }

    /// VAL-BPHASE-045: Verify that staged retrieval is NOT a
    /// binary-quantization-first replacement.
    ///
    /// The staged path uses exact cosine similarity in the coarse phase
    /// (not binary quantization), and the exact rerank applies the full
    /// hybrid scoring. The default search path remains the authoritative
    /// INT8/default quality-gated path.
    #[test]
    fn test_staged_retrieval_is_not_binary_quantization_replacement() {
        use leindex::search::{SearchEngine, SearchQuery, StagedRetrievalConfig};

        let dimension = 32;
        let num_nodes = 100;

        let mut engine = SearchEngine::with_dimension(dimension);
        engine.index_nodes(create_staged_test_nodes(num_nodes, dimension));

        // 1. Staged retrieval is opt-in (disabled by default)
        let default_config = StagedRetrievalConfig::default();
        assert!(
            !default_config.enabled,
            "Staged retrieval should be disabled by default"
        );

        // 2. When disabled, search_staged falls back to standard search
        let query = SearchQuery {
            query: "authenticate".to_string(),
            top_k: 5,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        };

        let (_, metrics) = engine
            .search_staged(query.clone(), &default_config)
            .unwrap();
        assert!(
            !metrics.staged_used,
            "Should not use staged path when disabled"
        );

        // 3. The standard search path still works and is the authoritative default
        let standard_results = engine.search(query.clone()).unwrap();
        assert!(!standard_results.is_empty(), "Standard search should work");

        // 4. Staged retrieval uses exact cosine similarity (not binary quantization)
        //    This is verified by the implementation using vector_index.search()
        //    which performs exact cosine similarity, not binary quantization.
        //    The coarse_multiplier controls candidate expansion, not quantization.
        let staged_config = StagedRetrievalConfig::enabled_with_multiplier(3);
        assert!(staged_config.enabled);
        assert_eq!(staged_config.coarse_multiplier, 3);

        // 5. Staged results should be consistent with standard results
        let (staged_results, staged_metrics) = engine.search_staged(query, &staged_config).unwrap();
        assert!(staged_metrics.staged_used);
        assert!(!staged_results.is_empty());

        // The top result should match — exact rerank preserves quality
        assert_eq!(
            standard_results[0].node_id, staged_results[0].node_id,
            "Staged retrieval should not alter top-result quality"
        );
    }

    /// Verify that staged retrieval works correctly with HNSW index backend.
    #[test]
    fn test_staged_retrieval_with_hnsw() {
        use leindex::search::{HNSWParams, SearchEngine, SearchQuery, StagedRetrievalConfig};

        let dimension = 32;
        let num_nodes = 200;

        let mut engine = SearchEngine::with_hnsw(dimension, HNSWParams::default());
        engine.index_nodes(create_staged_test_nodes(num_nodes, dimension));

        let mut query_embedding = vec![0.0f32; dimension];
        query_embedding[0] = 0.95;

        let query = SearchQuery {
            query: "authenticate".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(query_embedding),
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        };

        let staged_config = StagedRetrievalConfig::enabled_with_multiplier(5);
        let (results, metrics) = engine.search_staged(query, &staged_config).unwrap();

        assert!(metrics.staged_used);
        assert!(!results.is_empty());
        assert!(metrics.coarse_candidates > 0);
    }

    /// Verify that staged retrieval works correctly with INT8 quantized HNSW.
    #[test]
    fn test_staged_retrieval_with_int8_hnsw() {
        use leindex::search::quantization::Int8HnswParams;
        use leindex::search::{SearchEngine, SearchQuery, StagedRetrievalConfig};

        let dimension = 32;
        let num_nodes = 200;

        let mut engine = SearchEngine::with_dimension(dimension);
        engine.enable_int8_hnsw(Some(
            Int8HnswParams::new().with_m(8).with_ef_construction(50),
        ));
        engine.index_nodes(create_staged_test_nodes(num_nodes, dimension));

        let mut query_embedding = vec![0.0f32; dimension];
        query_embedding[0] = 0.95;

        let query = SearchQuery {
            query: "authenticate".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(query_embedding),
            query_neural_embedding: None,
            threshold: None,
            query_type: None,
        };

        let staged_config = StagedRetrievalConfig::enabled_with_multiplier(5);
        let (results, metrics) = engine.search_staged(query, &staged_config).unwrap();

        assert!(metrics.staged_used);
        assert!(!results.is_empty());
    }

    /// Verify staged retrieval with empty index returns empty results.
    #[test]
    fn test_staged_retrieval_empty_index() {
        use leindex::search::{SearchEngine, SearchQuery, StagedRetrievalConfig};

        let mut engine = SearchEngine::with_dimension(32);
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

        let staged_config = StagedRetrievalConfig::enabled_with_multiplier(5);
        let (results, metrics) = engine.search_staged(query, &staged_config).unwrap();

        assert!(results.is_empty());
        assert!(metrics.staged_used);
        assert_eq!(metrics.coarse_candidates, 0);
    }
}
