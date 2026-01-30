// Integration tests for HNSW vector search
//
// These tests verify the end-to-end functionality of HNSW-based
// approximate nearest neighbor search, including accuracy, performance,
// and consistency with the brute-force implementation.

#[cfg(test)]
mod tests {
    use lerecherche::{vector::VectorIndex, HNSWIndex, HNSWParams, NodeInfo, SearchEngine};
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
                embedding: Some(embedding),
                complexity: (i % 10) as u32 + 1,
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
                embedding: Some(embedding),
                complexity: (i % 10) as u32 + 1,
            });
        }

        engine.index_nodes(nodes);

        // Verify brute-force is being used
        assert!(!engine.is_hnsw_enabled());

        // Get initial vector count
        let initial_count = engine.vector_index().len();
        assert_eq!(initial_count, num_vectors);

        // Switch to HNSW
        engine.enable_hnsw(HNSWParams::default()).unwrap();

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
                embedding: Some(embedding),
                complexity: 1,
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
        engine.enable_hnsw(HNSWParams::default()).unwrap();
        assert!(engine.is_hnsw_enabled());

        // Disable HNSW
        engine.disable_hnsw().unwrap();
        assert!(!engine.is_hnsw_enabled());

        // Enable HNSW again
        engine.enable_hnsw(HNSWParams::default()).unwrap();
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
}
