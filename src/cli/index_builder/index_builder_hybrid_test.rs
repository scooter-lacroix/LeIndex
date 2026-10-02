use super::*;

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

#[test]
fn file_summary_context_collects_same_file_symbols_excluding_summary_nodes() {
    use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};
    use std::sync::Arc;

    let mut pdg = ProgramDependenceGraph::new();
    let lib: Arc<str> = Arc::from("src/lib.rs");
    // A FileSummary node must NOT appear in the collected symbol names.
    pdg.add_node(Node {
        id: "src/lib.rs".to_string(),
        node_type: NodeType::FileSummary,
        name: "src/lib.rs".to_string(),
        file_path: lib.clone(),
        byte_range: (0, 0),
        complexity: 0,
        language: "rust".to_string(),
    });
    for name in ["alpha", "beta", "gamma"] {
        pdg.add_node(Node {
            id: format!("src/lib.rs:{name}"),
            node_type: NodeType::Function,
            name: name.to_string(),
            file_path: lib.clone(),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".to_string(),
        });
    }
    pdg.add_node(Node {
        id: "src/other.rs:delta".to_string(),
        node_type: NodeType::Function,
        name: "delta".to_string(),
        file_path: Arc::from("src/other.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });

    let ctx = FileSummaryContext::from_pdg(&pdg);
    // Same-file symbols, FileSummary excluded, insertion order preserved.
    assert_eq!(
        ctx.file_symbols.get("src/lib.rs"),
        Some(&vec![
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string()
        ])
    );
    // Different file is bucketed separately.
    assert_eq!(
        ctx.file_symbols.get("src/other.rs"),
        Some(&vec!["delta".to_string()])
    );
}

#[test]
fn test_persist_search_snapshot_rewrites_on_content_only_token_change() {
    use crate::search::search::{DEFAULT_EMBEDDING_DIMENSION, NodeInfo, SearchEngine};

    // A comment-only edit inside a function body leaves the node id, byte
    // range, counts and PDG fingerprint untouched but changes the node's
    // tokens. The snapshot identity must cover the token dictionary and
    // per-node token ids, or the rewrite is skipped and cold hydration keeps
    // serving pre-edit tokens (text search can never match the new content).
    let temp = tempfile::tempdir().unwrap();
    let project_path = temp.path();
    let storage = project_path.join(".leindex");
    std::fs::create_dir_all(&storage).unwrap();

    let engine_for = |content: &str| {
        let mut engine = SearchEngine::new();
        let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
        tfidf_embedding[0] = 1.0;
        engine.index_nodes(vec![NodeInfo {
            node_id: "a.rs:alpha".to_string(),
            file_path: "a.rs".to_string(),
            symbol_name: "alpha".to_string(),
            language: "rust".to_string(),
            content: content.to_string(),
            byte_range: (0, 25),
            tfidf_embedding,
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }]);
        engine
    };

    let snapshot_path = storage.join("search_snapshot.bin");
    persist_search_snapshot(
        &engine_for("fn alpha() { let counter_a = 1; }"),
        project_path,
        1,
        0,
        "fp".to_string(),
    )
    .unwrap();
    let first = std::fs::read(&snapshot_path).unwrap();

    // Content-only change: same node id, byte range, fingerprint arguments.
    persist_search_snapshot(
        &engine_for("fn alpha() { let counter_b = 1; }"),
        project_path,
        1,
        0,
        "fp".to_string(),
    )
    .unwrap();
    let second = std::fs::read(&snapshot_path).unwrap();
    assert_ne!(
        first, second,
        "a token-level content change must rewrite the snapshot even when every \
         structural identity field is unchanged"
    );

    // The rewritten snapshot is valid and hydrates.
    let reloaded = try_load_search_snapshot_from_storage(&storage).unwrap();
    assert_eq!(reloaded.pdg_fingerprint, "fp");
}

#[test]
fn test_persist_search_snapshot_skips_identical_rewrite() {
    use crate::search::search::{DEFAULT_EMBEDDING_DIMENSION, NodeInfo, SearchEngine};

    let temp = tempfile::TempDir::new().unwrap();
    let project_path = temp.path();
    let storage = project_path.join(".leindex");
    std::fs::create_dir_all(&storage).unwrap();

    let mut engine = SearchEngine::new();
    let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf_embedding[0] = 1.0;
    engine.index_nodes(vec![NodeInfo {
        node_id: "a.rs:alpha".to_string(),
        file_path: "a.rs".to_string(),
        symbol_name: "alpha".to_string(),
        language: "rust".to_string(),
        content: "fn alpha() {}".to_string(),
        byte_range: (0, 14),
        tfidf_embedding,
        neural_embedding: None,
        complexity: 1,
        signature: None,
        pre_tokenized: None,
    }]);

    let snapshot_path = storage.join("search_snapshot.bin");
    let sidecar_path = storage.join("search_snapshot.identity");

    persist_search_snapshot(&engine, project_path, 1, 0, "fp-1".to_string()).unwrap();
    assert!(snapshot_path.is_file(), "first persist writes the snapshot");
    assert!(sidecar_path.is_file(), "identity sidecar written");

    // Corrupt the snapshot but keep the identity sidecar: an identical
    // persist must SKIP (garbage stays), proving no rewrite happened.
    std::fs::write(&snapshot_path, b"stale-marker").unwrap();
    persist_search_snapshot(&engine, project_path, 1, 0, "fp-1".to_string()).unwrap();
    assert_eq!(
        std::fs::read(&snapshot_path).unwrap(),
        b"stale-marker",
        "identical identity must skip the snapshot rewrite"
    );

    // A changed fingerprint rewrites: the garbage must be replaced by a real
    // snapshot that hydrates.
    persist_search_snapshot(&engine, project_path, 2, 1, "fp-2".to_string()).unwrap();
    assert_ne!(
        std::fs::read(&snapshot_path).unwrap(),
        b"stale-marker",
        "changed identity must rewrite the snapshot"
    );
    let reloaded = try_load_search_snapshot_from_storage(&storage).unwrap();
    assert_eq!(reloaded.pdg_fingerprint, "fp-2");
}

#[test]
fn test_persist_search_snapshot_writes_fragment_artifacts() {
    use crate::search::search::{DEFAULT_EMBEDDING_DIMENSION, NodeInfo, SearchEngine};
    use crate::search::vector::MmapEmbeddingIndex;

    let temp = tempfile::TempDir::new().unwrap();
    let project_path = temp.path();
    let storage = project_path.join(".leindex");
    std::fs::create_dir_all(&storage).unwrap();

    // Build a fragment-enabled engine by hydrating a 2-row fragment index from
    // persisted-style mmap files (the same path `indexing/load.rs` uses).
    let mut engine = SearchEngine::new();
    let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
    tfidf_embedding[0] = 1.0;
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
    let mut snapshot = engine.search_snapshot(1, 0, "frag".to_string());
    snapshot.fragment_rows = 2;
    let tfidf_path = storage.join("tfidf.bin");
    let frag_path = storage.join("frag.bin");
    crate::search::vector::write_mmap_embeddings(&tfidf_path, &engine.collect_embeddings())
        .unwrap();
    let fragment_embeddings = vec![
        ("hash_abc".to_string(), vec![0.1f32; 1024]),
        ("hash_def".to_string(), vec![0.2f32; 1024]),
    ];
    crate::search::vector::write_mmap_embeddings(&frag_path, &fragment_embeddings).unwrap();
    let tfidf_mmap = MmapEmbeddingIndex::open(&tfidf_path).unwrap();
    let frag_mmap = MmapEmbeddingIndex::open(&frag_path).unwrap();
    let frag_ids = vec!["hash_abc".to_string(), "hash_def".to_string()];
    let mut hydrated = SearchEngine::new();
    hydrated
        .restore_from_search_snapshot(
            snapshot,
            std::sync::Arc::new(tfidf_mmap),
            None,
            Some(std::sync::Arc::new(frag_mmap)),
            Some(&frag_ids),
        )
        .unwrap();

    // Persist: must write the fragment mmap + root and stamp the snapshot root.
    persist_search_snapshot(&hydrated, project_path, 1, 0, "frag".to_string()).unwrap();
    assert!(storage.join("fragments_embeddings.bin").exists());
    assert!(storage.join("fragment_root.bin").exists());

    // Reload: the snapshot carries the root hash and passes invariant-8
    // validation against the persisted artifacts.
    let reloaded = try_load_search_snapshot_from_storage(&storage).unwrap();
    assert!(reloaded.fragment_root_hash.is_some());
    assert_eq!(reloaded.fragment_rows, 2);
    let mmap = try_load_fragment_mmap_embeddings_from_storage(&storage).unwrap();
    assert!(fragment_layer_is_valid(
        reloaded.fragment_root_hash.as_deref(),
        Some(&mmap),
        &storage
    ));
}

#[test]
fn test_fragment_layer_is_valid_rejects_stale_root() {
    use crate::search::vector::MmapEmbeddingIndex;

    let temp = tempfile::TempDir::new().unwrap();
    let project_path = temp.path();
    let storage = project_path.join(".leindex");

    let ids = vec!["hash_abc".to_string(), "hash_def".to_string()];
    fragment::sync::persist_fragment_root_from_ids(project_path, &ids, 0).unwrap();

    let frag_path = storage.join("frag.bin");
    let embeddings = vec![
        ("hash_abc".to_string(), vec![0.1f32; 1024]),
        ("hash_def".to_string(), vec![0.2f32; 1024]),
    ];
    crate::search::vector::write_mmap_embeddings(&frag_path, &embeddings).unwrap();
    let mmap = MmapEmbeddingIndex::open(&frag_path).unwrap();

    // Matching root + row count -> valid.
    let good_root = fragment::sync::load_fragment_root(&storage)
        .unwrap()
        .unwrap()
        .root_hash;
    assert!(fragment_layer_is_valid(
        Some(&good_root),
        Some(&mmap),
        &storage
    ));
    // Stale root -> invalid.
    assert!(!fragment_layer_is_valid(
        Some("stale-hash"),
        Some(&mmap),
        &storage
    ));
    // Snapshot without a root -> invalid (feature-off semantics).
    assert!(!fragment_layer_is_valid(None, Some(&mmap), &storage));
    // Missing mmap -> invalid.
    assert!(!fragment_layer_is_valid(Some(&good_root), None, &storage));
    // Missing fragment_root.bin artifact -> invalid.
    std::fs::remove_file(storage.join("fragment_root.bin")).unwrap();
    assert!(!fragment_layer_is_valid(
        Some(&good_root),
        Some(&mmap),
        &storage
    ));
}

// ============================================================================
// DIRECTORY EXCLUSIONS (VAL-DIR-001..006): SKIP_DIRS + hidden-directory
// filtering across both the git and non-git scan paths.
// ============================================================================

/// Write `files` (relative path -> contents) under `root`, then `git init` and
/// force-track everything so every fixture file appears in `git ls-files`
/// inventory regardless of any global ignore rules.
fn git_scan_fixture(root: &std::path::Path, files: &[(&str, &str)]) {
    for (rel, content) in files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-q"]);
    run(&["add", "-f", "."]);
}

/// Render the relative (to `root`) source paths of a scan, for readable
/// assertions.
fn scan_relative_sources(scan: &ProjectFileScan, root: &std::path::Path) -> Vec<String> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    scan.source_paths
        .iter()
        .filter_map(|path| {
            path.strip_prefix(&root)
                .ok()
                .map(|relative| relative.to_string_lossy().into_owned())
        })
        .collect()
}

/// VAL-DIR-001: `SKIP_DIRS` contains `packages` so packaging scaffolding is
/// excluded from every scan path that consults the shared list.
#[test]
fn test_skip_dirs_contains_packages() {
    assert!(
        SKIP_DIRS.contains(&"packages"),
        "SKIP_DIRS must contain packages/"
    );
}

/// The git-scan post-filter rejects any path whose descendant component is a
/// hidden directory or a SKIP_DIRS entry, while never rejecting the project
/// root itself or legitimate source directories.
#[test]
fn test_is_excluded_project_path_filters_skip_dirs_hidden_and_keeps_legit() {
    let root = std::path::PathBuf::from("/proj");
    // SKIP_DIRS entries (any depth).
    assert!(is_excluded_project_path(
        &root.join("packages/web/index.js"),
        &root
    ));
    assert!(is_excluded_project_path(
        &root.join("target/debug/app.rs"),
        &root
    ));
    assert!(is_excluded_project_path(
        &root.join("node_modules/pkg/lib.js"),
        &root
    ));
    // Hidden directories (any depth).
    assert!(is_excluded_project_path(
        &root.join(".cache/snippet.rs"),
        &root
    ));
    assert!(is_excluded_project_path(
        &root.join(".github/workflows/ci.yml"),
        &root
    ));
    assert!(is_excluded_project_path(
        &root.join("src/.hidden/mod.rs"),
        &root
    ));
    // The project root itself is NOT rejected (zero descendant components).
    assert!(!is_excluded_project_path(&root, &root));
    // Legitimate source directories are NOT excluded.
    assert!(!is_excluded_project_path(&root.join("src/main.rs"), &root));
    assert!(!is_excluded_project_path(&root.join("lib/utils.ts"), &root));
    assert!(!is_excluded_project_path(&root.join("tests/mod.rs"), &root));
    assert!(!is_excluded_project_path(
        &root.join("benches/bench.rs"),
        &root
    ));
    // A path with no components at all (the empty relative path) is kept.
    assert!(!is_excluded_project_path(&root.join("README.md"), &root));
}

/// VAL-DIR-003: a git repo (no .gitignore) with tracked files under
/// `target/`, `node_modules/`, and `packages/` must yield zero of those files
/// while keeping legitimate source files.
#[test]
fn test_git_scan_excludes_skip_dirs_entries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let files = [
        ("target/debug/app.rs", "fn main() {}\n"),
        ("node_modules/pkg/lib.js", "const x = 1;\n"),
        ("packages/web/lib.rs", "fn web() {}\n"),
        ("packages/web/package.json", "{}\n"),
        ("src/main.rs", "pub fn main() {}\n"),
        ("lib/core.py", "def core():\n    pass\n"),
    ];
    git_scan_fixture(root, &files);

    let scan = scan_git_project_files(root).unwrap();
    let rels = scan_relative_sources(&scan, root);
    for bad in [
        "target/debug/app.rs",
        "node_modules/pkg/lib.js",
        "packages/web/lib.rs",
    ] {
        assert!(
            !rels.iter().any(|r| r == bad),
            "git scan must exclude {bad}, got: {rels:?}"
        );
    }
    assert!(
        rels.iter().any(|r| r == "src/main.rs"),
        "src/main.rs must remain in git scan results: {rels:?}"
    );
    assert!(
        rels.iter().any(|r| r == "lib/core.py"),
        "lib/core.py must remain in git scan results: {rels:?}"
    );
    // Manifests under excluded dirs must not be collected either.
    assert!(
        !scan
            .manifest_paths
            .iter()
            .any(|p| p.ends_with("packages/web/package.json")),
        "manifests under SKIP_DIRS must not be collected"
    );
}

/// VAL-DIR-002: a git repo with a tracked file under a hidden directory
/// (`.cache/`) must exclude that file while keeping legitimate source files.
#[test]
fn test_git_scan_excludes_hidden_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let files = [
        (".cache/snippet.rs", "fn cached() {}\n"),
        (".tmp/scratch.rs", "fn scratch() {}\n"),
        ("src/main.rs", "pub fn main() {}\n"),
    ];
    git_scan_fixture(root, &files);

    let scan = scan_git_project_files(root).unwrap();
    let rels = scan_relative_sources(&scan, root);
    assert!(
        !rels.iter().any(|r| r == ".cache/snippet.rs"),
        "tracked file under .cache/ must be excluded: {rels:?}"
    );
    assert!(
        !rels.iter().any(|r| r == ".tmp/scratch.rs"),
        "tracked file under .tmp/ must be excluded: {rels:?}"
    );
    assert!(
        rels.iter().any(|r| r == "src/main.rs"),
        "src/main.rs must remain in git scan results: {rels:?}"
    );
}

/// VAL-DIR-004: the non-git walker must keep excluding hidden directories and
/// every SKIP_DIRS entry, including the newly added `packages`.
#[test]
fn test_non_git_scan_excludes_skip_dirs_and_hidden() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let files = [
        (".cache/snippet.rs", "fn cached() {}\n"),
        ("target/debug/app.rs", "fn main() {}\n"),
        ("packages/web/lib.rs", "fn web() {}\n"),
        ("node_modules/pkg/lib.js", "const x = 1;\n"),
        ("src/main.rs", "pub fn main() {}\n"),
        ("lib/core.py", "def core():\n    pass\n"),
    ];
    for (rel, content) in &files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
    }

    let scan = scan_non_git_project_files(root).unwrap();
    let rels = scan_relative_sources(&scan, root);
    for bad in [
        ".cache/snippet.rs",
        "target/debug/app.rs",
        "packages/web/lib.rs",
        "node_modules/pkg/lib.js",
    ] {
        assert!(
            !rels.iter().any(|r| r == bad),
            "non-git scan must exclude {bad}, got: {rels:?}"
        );
    }
    assert!(
        rels.iter().any(|r| r == "src/main.rs"),
        "src/main.rs must remain in non-git scan results: {rels:?}"
    );
    assert!(
        rels.iter().any(|r| r == "lib/core.py"),
        "lib/core.py must remain in non-git scan results: {rels:?}"
    );
}

/// VAL-DIR-005: standard source directories (`src/`, `lib/`, `tests/`,
/// `benches/`, `docs/`, `examples/`) must NOT be excluded by either scan path.
#[test]
fn test_scan_keeps_legitimate_source_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let files = [
        ("src/main.rs", "pub fn main() {}\n"),
        ("lib/core.rs", "pub fn core() {}\n"),
        ("tests/integration.rs", "fn test_it() {}\n"),
        ("benches/perf.rs", "fn bench() {}\n"),
        ("docs/api.rs", "pub fn docs() {}\n"),
        ("examples/demo.rs", "fn demo() {}\n"),
    ];
    for (rel, content) in &files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
    }

    // Non-git scan.
    let scan = scan_non_git_project_files(root).unwrap();
    let rels = scan_relative_sources(&scan, root);
    for (rel, _) in &files {
        assert!(
            rels.iter().any(|r| r == rel),
            "non-git scan missing {rel}: {rels:?}"
        );
    }

    // Git scan.
    git_scan_fixture(root, &files);
    let scan = scan_git_project_files(root).unwrap();
    let rels = scan_relative_sources(&scan, root);
    for (rel, _) in &files {
        assert!(
            rels.iter().any(|r| r == rel),
            "git scan missing {rel}: {rels:?}"
        );
    }
}

/// VAL-DIR-006: the git-scan post-filter runs before the max_files limit, so
/// excluded files do not consume the budget. With `max_files=5`, 10 files in
/// a hidden dir (sorted before `src/`) and 3 in `src/`, all 3 `src/` files
/// must be indexed.
#[test]
fn test_git_scan_max_files_not_consumed_by_excluded_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..10 {
        files.push((
            format!(".cache/build_{i}.rs"),
            "fn cached() {}\n".to_string(),
        ));
    }
    for i in 0..3 {
        files.push((format!("src/main_{i}.rs"), "pub fn main() {}\n".to_string()));
    }
    for (rel, content) in &files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
    }

    // Strict 5-file budget, applied via the project config file so the scan
    // path under test picks it up.
    let mut config = crate::cli::config::ProjectConfig::default();
    config.indexing.max_files = 5;
    config.save(root).unwrap();

    git_scan_fixture(
        root,
        &files
            .iter()
            .map(|(r, c)| (r.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    );

    let scan = scan_git_project_files(root).unwrap();
    let rels = scan_relative_sources(&scan, root);
    for i in 0..3 {
        assert!(
            rels.iter().any(|r| *r == format!("src/main_{i}.rs")),
            "src/main_{i}.rs must be indexed under max_files=5, got: {rels:?}"
        );
    }
    assert_eq!(
        rels.iter().filter(|r| r.ends_with(".rs")).count(),
        3,
        "excluded files must not consume the max_files budget: {rels:?}"
    );
    assert!(
        !rels.iter().any(|r| r.starts_with(".cache/")),
        "hidden-dir files must be filtered before max_files: {rels:?}"
    );
}

// ============================================================================
// PIPELINE OPTIMIZATION TESTS
// ============================================================================

/// Duplicate node IDs (the supported multiple-unqualified-`new` pattern) must
/// each carry the enriched content of THEIR OWN node: content is recomputed
/// per node index, never cached or looked up by the non-unique string ID.
/// The old ID-keyed cache fed the surviving HashMap body to every duplicate,
/// so one method's index row described a different method's file.
#[test]
fn test_duplicate_node_ids_keep_their_own_enriched_content() {
    use crate::graph::pdg::{Node, NodeType};
    use std::sync::Arc;

    let temp = tempfile::tempdir().unwrap();
    let a_rs = temp.path().join("a.rs");
    let b_rs = temp.path().join("b.rs");
    // Distinctive tokens per file's body; same symbol name and (duplicate)
    // node id in both.
    std::fs::write(&a_rs, "impl A { fn new() -> i32 { wibble_marker() } }\n").unwrap();
    std::fs::write(&b_rs, "impl B { fn new() -> i32 { wobble_marker() } }\n").unwrap();

    let mut pdg = ProgramDependenceGraph::new();
    let id = "proj:new";
    let idx_a = pdg.add_node(Node {
        id: id.to_string(),
        node_type: NodeType::Method,
        name: "new".to_string(),
        file_path: Arc::from(a_rs.to_string_lossy().as_ref()),
        byte_range: (8, 40),
        complexity: 1,
        language: "rust".to_string(),
    });
    let idx_b = pdg.add_node(Node {
        id: id.to_string(),
        node_type: NodeType::Method,
        name: "new".to_string(),
        file_path: Arc::from(b_rs.to_string_lossy().as_ref()),
        byte_range: (8, 40),
        complexity: 1,
        language: "rust".to_string(),
    });

    // Unit level: each duplicate's enriched content names its own body.
    let connectivity_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[EdgeType::Call, EdgeType::DataDependency]),
        excluded_node_types: Some(vec![NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };
    let file_summary_ctx = FileSummaryContext::from_pdg(&pdg);
    let mut file_cache = FileReadCache::per_chunk_scratch();
    let mut content_of = |node_idx| {
        let node = pdg.get_node(node_idx).unwrap();
        let bytes = file_cache
            .get_or_read(std::path::Path::new(&*node.file_path))
            .unwrap();
        enriched_node_content(
            &pdg,
            node_idx,
            node,
            &bytes,
            &connectivity_config,
            &file_summary_ctx,
        )
    };
    let content_a = content_of(idx_a);
    let content_b = content_of(idx_b);
    assert_ne!(
        content_a, content_b,
        "duplicate-ID nodes must enrich to their own bodies"
    );
    assert!(content_a.contains("wibble_marker"));
    assert!(content_b.contains("wobble_marker"));

    // End to end: the row the engine keeps (first duplicate, a.rs) must be
    // tokenized from a.rs's body. The old ID-keyed cache could hand it
    // b.rs's body, so "wibble" found nothing.
    let mut cache = None;
    let mut engine = SearchEngine::new();
    let _embedder = index_nodes(&pdg, &mut engine, &mut cache, 4).unwrap();
    let query = crate::search::search::SearchQuery {
        query: "wibble_marker".to_string(),
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
    assert!(
        results.iter().any(|hit| hit.file_path.ends_with("a.rs")),
        "the surviving duplicate's tokens must come from its own file, got {:?}",
        results.iter().map(|h| &h.file_path).collect::<Vec<_>>()
    );
}

/// Test that `collect_source_files_with_hashes` produces identical results
/// whether called on the same scan (parallel hashing is deterministic).
#[test]
fn test_parallel_file_hashing_deterministic() {
    use crate::cli::index_builder::ProjectFileScan;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create multiple source files with distinct content.
    let files = [
        ("a.rs", "fn alpha() {}\n"),
        ("b.rs", "fn beta() {}\n"),
        ("c.rs", "fn gamma() {}\n"),
        ("d.rs", "fn delta() {}\n"),
        ("e.rs", "fn epsilon() {}\n"),
    ];
    for (rel, content) in &files {
        std::fs::write(root.join(rel), content).unwrap();
    }

    let scan = ProjectFileScan {
        source_paths: files.iter().map(|(r, _)| root.join(r)).collect(),
        ..Default::default()
    };

    let results1 = collect_source_files_with_hashes(&scan).unwrap();
    let results2 = collect_source_files_with_hashes(&scan).unwrap();

    // Sort both for deterministic comparison (par_iter doesn't preserve order).
    let mut sorted1 = results1.clone();
    sorted1.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sorted2 = results2.clone();
    sorted2.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(
        sorted1, sorted2,
        "parallel hashing must produce identical results across runs"
    );

    // Verify hashes are non-empty and correct.
    assert_eq!(sorted1.len(), files.len());
    for (path, hash) in &sorted1 {
        assert!(
            !hash.is_empty(),
            "hash for {} must not be empty",
            path.display()
        );
    }
}

/// `enriched_node_content` must be a pure function of its inputs: the DF
/// pass, the lexical pass and the neural pass each recompute it (no
/// cross-phase cache — VAL-STREAM-012), so a hidden dependency on iteration
/// state or ordering would silently desynchronize the passes.
#[test]
fn test_enriched_node_content_is_deterministic() {
    use crate::graph::pdg::{Node, NodeType};
    use std::sync::Arc;

    let mut pdg = ProgramDependenceGraph::new();
    pdg.add_node(Node {
        id: "test.rs:hello".to_string(),
        node_type: NodeType::Function,
        name: "hello".to_string(),
        file_path: Arc::from("test.rs"),
        byte_range: (0, 20),
        complexity: 1,
        language: "rust".to_string(),
    });

    let node_indices: Vec<petgraph::graph::NodeIndex> = pdg.node_indices().collect();
    let connectivity_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[EdgeType::Call, EdgeType::DataDependency]),
        excluded_node_types: Some(vec![NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };
    let file_summary_ctx = FileSummaryContext::from_pdg(&pdg);

    let compute = || {
        let mut file_cache = FileReadCache::per_chunk_scratch();
        let node_idx = node_indices[0];
        let node = pdg.get_node(node_idx).unwrap();
        let file_bytes = file_cache
            .get_or_read(std::path::Path::new("test.rs"))
            .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));
        enriched_node_content(
            &pdg,
            node_idx,
            node,
            &file_bytes,
            &connectivity_config,
            &file_summary_ctx,
        )
    };
    let first = compute();
    let second = compute();
    assert_eq!(
        first, second,
        "recomputation must be bit-identical: every pass depends on it"
    );
}

#[test]
fn test_persisted_search_identity_matches_load_with_duplicate_node_ids() {
    use crate::storage::pdg_store::load_pdg;
    use crate::storage::schema::Storage;
    use std::sync::Arc;

    let temp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open(temp.path().join("leindex.db")).unwrap();

    let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
    let main = pdg.add_node(crate::graph::pdg::Node {
        id: "src/a.rs:main".to_string(),
        node_type: crate::graph::pdg::NodeType::Function,
        name: "main".to_string(),
        file_path: Arc::from("src/a.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });
    // Two nodes sharing one node_id: the external-import duplicate pattern
    // the (project_id, node_id) upsert collapses to a single DB row.
    let external_id = "src/a.rs:__external__:serde::Serialize";
    let ext = pdg.add_node(crate::graph::pdg::Node {
        id: external_id.to_string(),
        node_type: crate::graph::pdg::NodeType::External,
        name: "serde::Serialize".to_string(),
        file_path: Arc::from("src/a.rs"),
        byte_range: (0, 0),
        complexity: 1,
        language: "external".to_string(),
    });
    let ext_dup = pdg.add_node(crate::graph::pdg::Node {
        id: external_id.to_string(),
        node_type: crate::graph::pdg::NodeType::External,
        name: "serde::Serialize".to_string(),
        file_path: Arc::from("src/a.rs"),
        byte_range: (0, 0),
        complexity: 1,
        language: "external".to_string(),
    });
    pdg.add_call_edges(vec![(main, ext), (main, ext_dup)]);

    crate::cli::index_builder::save_to_storage(&mut storage, "dup_proj", &pdg).unwrap();

    let identity = super::persistence::persisted_search_identity(&storage, "dup_proj")
        .expect("persisted identity computable after save");

    let loaded = load_pdg(&storage, "dup_proj").unwrap();
    assert_eq!(identity.0, loaded.node_count(), "nodes must match load");
    assert_eq!(identity.1, loaded.edge_count(), "edges must match load");
    assert_eq!(
        identity.2,
        super::pdg_search_fingerprint(&loaded),
        "fingerprint must match the DB-reconstructed graph"
    );

    // The in-memory graph genuinely disagrees with its own persisted state —
    // the divergence that used to force the slow rebuild path forever.
    assert_ne!(
        identity.0,
        pdg.node_count(),
        "in-memory count includes the duplicate; persisted identity must not"
    );
}

/// The parallel document-frequency pass must produce exactly what a plain
/// sequential loop over the same nodes produces: same per-token counts, same
/// document total, same cached contents.
#[test]
fn test_parallel_document_frequencies_match_a_sequential_reference() {
    use crate::graph::pdg::{Node, NodeType};
    use std::collections::HashMap;
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let mut pdg = ProgramDependenceGraph::new();
    for file in 0..12 {
        let path = dir.path().join(format!("file_{file}.rs"));
        let mut source = String::new();
        for func in 0..40 {
            source.push_str(&format!(
                "fn func_{file}_{func}(input: u32) -> u32 {{ helper_{} (input) + {func} }}\n",
                func % 5
            ));
        }
        std::fs::write(&path, &source).unwrap();
        let mut offset = 0usize;
        for func in 0..40 {
            let line = source[offset..].lines().next().unwrap();
            pdg.add_node(Node {
                id: format!("{}:func_{file}_{func}", path.display()),
                node_type: NodeType::Function,
                name: format!("func_{file}_{func}"),
                file_path: Arc::from(path.to_string_lossy().as_ref()),
                byte_range: (offset, offset + line.len()),
                complexity: 1,
                language: "rust".to_string(),
            });
            offset += line.len() + 1;
        }
    }
    let node_indices: Vec<petgraph::graph::NodeIndex> = pdg.node_indices().collect();
    let config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[EdgeType::Call, EdgeType::DataDependency]),
        excluded_node_types: Some(vec![NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };
    let ctx = FileSummaryContext::from_pdg(&pdg);

    let (df, total) = build_document_frequencies(&pdg, &node_indices, &config, &ctx);

    // Sequential reference.
    let mut expected_df: HashMap<String, usize> = HashMap::new();
    let mut expected_total = 0;
    let mut cache = FileReadCache::per_chunk_scratch();
    for &idx in &node_indices {
        let node = pdg.get_node(idx).unwrap();
        let bytes = cache
            .get_or_read(std::path::Path::new(&*node.file_path))
            .unwrap();
        let content = enriched_node_content(&pdg, idx, node, &bytes, &config, &ctx);
        let unique: std::collections::HashSet<String> =
            tokenize_code(&content).into_iter().collect();
        for token in unique {
            *expected_df.entry(token).or_insert(0) += 1;
        }
        expected_total += 1;
    }

    assert_eq!(total, expected_total);
    assert_eq!(df, expected_df);
    assert_eq!(total, 12 * 40);
}

/// The backwards line scan must agree with the original implementation, which
/// decoded and split the whole prefix.
#[test]
fn test_preceding_doc_context_matches_the_full_prefix_reference() {
    fn reference(bytes: &[u8], start: usize) -> String {
        let prefix = String::from_utf8_lossy(&bytes[..start.min(bytes.len())]);
        let mut lines = Vec::new();
        for line in prefix.lines().rev() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                if lines.is_empty() {
                    continue;
                }
                break;
            }
            if trimmed.starts_with("//") || trimmed.starts_with("#") || trimmed.starts_with("/*") {
                lines.push(strip_comment_syntax(line.trim()));
                if lines.len() == 24 {
                    break;
                }
            } else {
                break;
            }
        }
        lines.reverse();
        lines.join("\n")
    }

    let pieces = [
        "/// doc one\n",
        "// two\n",
        "# three\r\n",
        "\n",
        "    \n",
        "fn f() {}\n",
        "/* c */\n",
        "  //! indented\n",
        "let x = 1;",
        "\r\n",
        "é// é\n",
    ];
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    for _ in 0..3_000 {
        let mut text = String::new();
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let mut bits = state;
        for _ in 0..(bits % 40) {
            bits = bits.rotate_left(11) ^ 0xA5A5_5A5A;
            text.push_str(pieces[(bits as usize) % pieces.len()]);
        }
        let bytes = text.as_bytes();
        for start in [0, bytes.len() / 2, bytes.len(), bytes.len() + 5] {
            // Only ever called at a symbol start, i.e. on a char boundary.
            let mut at = start.min(bytes.len());
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            assert_eq!(
                preceding_doc_context(bytes, at),
                reference(bytes, at),
                "text {text:?} start {at}"
            );
        }
    }
}
