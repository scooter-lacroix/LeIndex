// Integration tests for borrowed mmap vector access, row lookup,
// owned-copy compatibility, and invalid-row clean failure.
//
// Validates: VAL-BPHASE-001, VAL-BPHASE-002, VAL-BPHASE-003, VAL-BPHASE-004

use leindex::search::vector::{cosine_similarity, write_mmap_embeddings, MmapEmbeddingIndex};

/// Helper: create a temp mmap file with known embeddings and return the index.
fn create_test_index() -> (tempfile::TempDir, MmapEmbeddingIndex) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("embeddings.bin");

    let embeddings: Vec<(String, Vec<f32>)> = vec![
        ("func_a".to_string(), vec![1.0, 0.0, 0.0]),
        ("func_b".to_string(), vec![0.0, 1.0, 0.0]),
        ("func_c".to_string(), vec![0.0, 0.0, 1.0]),
        ("func_d".to_string(), vec![0.9, 0.1, 0.0]),
    ];

    write_mmap_embeddings(&path, &embeddings).unwrap();
    let index = MmapEmbeddingIndex::open(&path).unwrap();
    (dir, index)
}

// ===========================================================================
// VAL-BPHASE-001: Borrowed mmap row access returns stable vector content
// ===========================================================================

#[test]
fn test_borrowed_slice_returns_correct_content() {
    let (_dir, index) = create_test_index();

    // Row 0 should be func_a = [1.0, 0.0, 0.0]
    let slice = index.embedding_slice_by_index(0).unwrap();
    assert_eq!(slice, &[1.0, 0.0, 0.0]);

    // Row 1 should be func_b = [0.0, 1.0, 0.0]
    let slice = index.embedding_slice_by_index(1).unwrap();
    assert_eq!(slice, &[0.0, 1.0, 0.0]);

    // Row 2 should be func_c = [0.0, 0.0, 1.0]
    let slice = index.embedding_slice_by_index(2).unwrap();
    assert_eq!(slice, &[0.0, 0.0, 1.0]);

    // Row 3 should be func_d = [0.9, 0.1, 0.0]
    let slice = index.embedding_slice_by_index(3).unwrap();
    assert_eq!(slice, &[0.9, 0.1, 0.0]);
}

#[test]
fn test_borrowed_slice_has_correct_dimension() {
    let (_dir, index) = create_test_index();

    for i in 0..4 {
        let slice = index.embedding_slice_by_index(i).unwrap();
        assert_eq!(slice.len(), 3, "Row {} should have dimension 3", i);
    }
}

#[test]
fn test_borrowed_slice_stable_across_repeated_reads() {
    let (_dir, index) = create_test_index();

    // VAL-BPHASE-001: repeated reads of the same row remain identical while
    // the mmap file is unchanged.
    let first = index.embedding_slice_by_index(0).unwrap().to_vec();
    let second = index.embedding_slice_by_index(0).unwrap().to_vec();
    let third = index.embedding_slice_by_index(0).unwrap().to_vec();

    assert_eq!(first, second);
    assert_eq!(second, third);
    assert_eq!(first, vec![1.0, 0.0, 0.0]);
}

#[test]
fn test_borrowed_slice_all_rows_stable() {
    let (_dir, index) = create_test_index();

    let expected = vec![
        vec![1.0, 0.0, 0.0],
        vec![0.0, 1.0, 0.0],
        vec![0.0, 0.0, 1.0],
        vec![0.9, 0.1, 0.0],
    ];

    // Read each row twice and verify stability
    for (row_idx, expected_vec) in expected.iter().enumerate() {
        let first = index
            .embedding_slice_by_index(row_idx as u32)
            .unwrap()
            .to_vec();
        let second = index
            .embedding_slice_by_index(row_idx as u32)
            .unwrap()
            .to_vec();
        assert_eq!(first, second, "Row {} not stable across reads", row_idx);
        assert_eq!(&first, expected_vec, "Row {} content mismatch", row_idx);
    }
}

#[test]
fn test_borrowed_slice_cosine_similarity_correct() {
    let (_dir, index) = create_test_index();

    // Verify that borrowed slices produce correct cosine similarity values
    let slice_a = index.embedding_slice_by_index(0).unwrap();
    let slice_b = index.embedding_slice_by_index(1).unwrap();
    let slice_d = index.embedding_slice_by_index(3).unwrap();

    // func_a vs func_a should be 1.0
    let sim_aa = cosine_similarity(slice_a, slice_a);
    assert!(
        (sim_aa - 1.0).abs() < 1e-6,
        "cosine(a,a) should be 1.0, got {}",
        sim_aa
    );

    // func_a vs func_b should be 0.0 (orthogonal)
    let sim_ab = cosine_similarity(slice_a, slice_b);
    assert!(
        sim_ab.abs() < 1e-6,
        "cosine(a,b) should be 0.0, got {}",
        sim_ab
    );

    // func_a vs func_d should be close to 0.995
    let sim_ad = cosine_similarity(slice_a, slice_d);
    assert!(
        sim_ad > 0.99,
        "cosine(a,d) should be > 0.99, got {}",
        sim_ad
    );
}

// ===========================================================================
// VAL-BPHASE-002: Node ID lookup resolves to a stable row index
// ===========================================================================

#[test]
fn test_node_id_lookup_returns_correct_row() {
    let (_dir, index) = create_test_index();

    // Each node ID should resolve to the expected row index
    assert_eq!(index.find_node_row("func_a"), Some(0));
    assert_eq!(index.find_node_row("func_b"), Some(1));
    assert_eq!(index.find_node_row("func_c"), Some(2));
    assert_eq!(index.find_node_row("func_d"), Some(3));
}

#[test]
fn test_node_id_lookup_stable_across_repeated_calls() {
    let (_dir, index) = create_test_index();

    // VAL-BPHASE-002: lookup returns the same row index for the lifetime of
    // the mmap file unless explicit compaction occurs.
    for _ in 0..10 {
        assert_eq!(index.find_node_row("func_a"), Some(0));
        assert_eq!(index.find_node_row("func_b"), Some(1));
        assert_eq!(index.find_node_row("func_c"), Some(2));
        assert_eq!(index.find_node_row("func_d"), Some(3));
    }
}

#[test]
fn test_node_id_lookup_unknown_returns_none() {
    let (_dir, index) = create_test_index();

    assert_eq!(index.find_node_row("nonexistent"), None);
    assert_eq!(index.find_node_row(""), None);
    assert_eq!(index.find_node_row("func_A"), None); // case-sensitive
}

#[test]
fn test_node_id_to_row_then_slice_roundtrip() {
    let (_dir, index) = create_test_index();

    // For each known node, resolve row index, then read the borrowed slice
    // and verify it matches the expected embedding.
    let expected: Vec<(&str, Vec<f32>)> = vec![
        ("func_a", vec![1.0, 0.0, 0.0]),
        ("func_b", vec![0.0, 1.0, 0.0]),
        ("func_c", vec![0.0, 0.0, 1.0]),
        ("func_d", vec![0.9, 0.1, 0.0]),
    ];

    for (node_id, expected_embedding) in &expected {
        let row = index
            .find_node_row(node_id)
            .unwrap_or_else(|| panic!("node '{}' should have a row index", node_id));
        let slice = index
            .embedding_slice_by_index(row)
            .unwrap_or_else(|| panic!("row {} for node '{}' should return a slice", row, node_id));
        assert_eq!(
            slice,
            expected_embedding.as_slice(),
            "embedding mismatch for node '{}' at row {}",
            node_id,
            row
        );
    }
}

// ===========================================================================
// VAL-BPHASE-003: Legacy owned-copy vector access remains compatibility-equivalent
// ===========================================================================

#[test]
fn test_owned_copy_matches_borrowed_slice() {
    let (_dir, index) = create_test_index();

    // VAL-BPHASE-003: the deprecated owned-copy accessor returns contents
    // identical to the borrowed row-slice path for the same logical embedding.
    for i in 0..4u32 {
        let borrowed = index.embedding_slice_by_index(i).unwrap();
        let owned = index.get_embedding_by_row(i).unwrap();
        assert_eq!(
            borrowed,
            owned.as_slice(),
            "owned copy at row {} should match borrowed slice",
            i
        );
    }
}

#[test]
fn test_owned_copy_via_node_id_matches_borrowed_slice() {
    let (_dir, index) = create_test_index();

    let node_ids = ["func_a", "func_b", "func_c", "func_d"];

    for node_id in &node_ids {
        let row = index.find_node_row(node_id).unwrap();
        let borrowed = index.embedding_slice_by_index(row).unwrap();
        let owned = index.get_embedding(node_id).unwrap();
        assert_eq!(
            borrowed,
            owned.as_slice(),
            "owned copy for node '{}' should match borrowed slice at row {}",
            node_id,
            row
        );
    }
}

#[test]
fn test_owned_copy_returns_vec_f32() {
    let (_dir, index) = create_test_index();

    // Verify the owned copy is a proper Vec<f32> with correct length
    let owned = index.get_embedding_by_row(0).unwrap();
    assert_eq!(owned.len(), 3);
    assert_eq!(owned, vec![1.0, 0.0, 0.0]);
}

// ===========================================================================
// VAL-BPHASE-004: Invalid row access fails cleanly
// ===========================================================================

#[test]
fn test_invalid_row_returns_none() {
    let (_dir, index) = create_test_index();

    // Row 4 is out of range (only 0..3 are valid)
    assert!(
        index.embedding_slice_by_index(4).is_none(),
        "out-of-range row should return None"
    );
}

#[test]
fn test_large_invalid_row_returns_none() {
    let (_dir, index) = create_test_index();

    // Very large row index
    assert!(
        index.embedding_slice_by_index(u32::MAX).is_none(),
        "u32::MAX row should return None"
    );
    assert!(
        index.embedding_slice_by_index(1_000_000).is_none(),
        "large row should return None"
    );
}

#[test]
fn test_owned_copy_invalid_row_returns_none() {
    let (_dir, index) = create_test_index();

    assert!(
        index.get_embedding_by_row(4).is_none(),
        "owned copy for out-of-range row should return None"
    );
    assert!(
        index.get_embedding_by_row(u32::MAX).is_none(),
        "owned copy for u32::MAX row should return None"
    );
}

#[test]
fn test_empty_index_invalid_row_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.bin");
    write_mmap_embeddings(&path, &[]).unwrap();
    let index = MmapEmbeddingIndex::open(&path).unwrap();

    // Any row in an empty index should return None
    assert!(index.embedding_slice_by_index(0).is_none());
    assert!(index.get_embedding_by_row(0).is_none());
    assert!(index.find_node_row("anything").is_none());
}

#[test]
fn test_invalid_row_does_not_crash() {
    let (_dir, index) = create_test_index();

    // VAL-BPHASE-004: requesting an out-of-range row does not return garbage
    // data and is surfaced as a clean miss rather than a crash or silent
    // corruption.
    for bad_row in [4, 5, 100, 1000, u32::MAX] {
        let result = std::panic::catch_unwind(|| index.embedding_slice_by_index(bad_row));
        assert!(
            result.is_ok(),
            "embedding_slice_by_index({}) should not panic",
            bad_row
        );
        assert!(
            result.unwrap().is_none(),
            "embedding_slice_by_index({}) should return None",
            bad_row
        );
    }
}

// ===========================================================================
// Additional coverage: search consistency with borrowed path
// ===========================================================================

#[test]
fn test_search_results_consistent_with_borrowed_slices() {
    let (_dir, index) = create_test_index();

    // Search for a vector close to func_a
    let query = vec![1.0, 0.0, 0.0];
    let results = index.search(&query, 4);

    assert!(!results.is_empty());

    // The top result should be func_a (identical vector)
    assert_eq!(results[0].0, "func_a");

    // Verify each search result's embedding via the borrowed path
    for (node_id, score) in &results {
        let row = index
            .find_node_row(node_id)
            .unwrap_or_else(|| panic!("search result '{}' should have a row", node_id));
        let slice = index
            .embedding_slice_by_index(row)
            .unwrap_or_else(|| panic!("row {} for '{}' should have a slice", row, node_id));

        // Verify the score matches manual cosine similarity
        let manual_sim = cosine_similarity(&query, slice);
        assert!(
            (score - manual_sim).abs() < 1e-5,
            "score {} for '{}' doesn't match manual cosine {}",
            score,
            node_id,
            manual_sim
        );
    }
}

#[test]
fn test_large_dimension_borrowed_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.bin");

    let dim = 768;
    let embeddings: Vec<(String, Vec<f32>)> = (0..10)
        .map(|i| {
            let mut vec = vec![0.0f32; dim];
            vec[i] = 1.0;
            (format!("node_{}", i), vec)
        })
        .collect();

    write_mmap_embeddings(&path, &embeddings).unwrap();
    let index = MmapEmbeddingIndex::open(&path).unwrap();

    // Verify borrowed slices for all rows
    for i in 0..10u32 {
        let slice = index.embedding_slice_by_index(i).unwrap();
        assert_eq!(slice.len(), dim);
        assert_eq!(slice[i as usize], 1.0);

        // All other dimensions should be 0.0
        for j in 0..dim {
            if j != i as usize {
                assert_eq!(slice[j], 0.0, "dim {} should be 0.0 for row {}", j, i);
            }
        }
    }

    // Verify node ID lookup
    for i in 0..10 {
        let node_id = format!("node_{}", i);
        let row = index.find_node_row(&node_id).unwrap();
        assert_eq!(row, i as u32);
    }
}
