//! Built-in labeled corpus data covering all spec section 9.2 categories.
//!
//! These cases are designed around the LeIndex codebase itself (using its
//! actual symbols and file layout) so they exercise real retrieval patterns.
//! Each category has at least 2 cases, split across train/eval.

use super::{Corpus, CorpusCase, EvalCategory, Language, Split};

/// Build the default LeIndex evaluation corpus.
///
/// The corpus covers all 14 spec section 9.2 retrieval categories.
/// Each category has cases spread across train and eval splits.
/// Splits are fixed: case metadata is the label source-of-truth.
#[allow(clippy::too_many_lines)]
pub fn build_default_corpus() -> Corpus {
    let mut corpus = Corpus::new();

    // ── 1. NL-to-symbol ─────────────────────────────────────────────────
    add(
        &mut corpus,
        "nl_sym_1",
        "where does LeIndex parse Rust source files into syntax trees?",
        EvalCategory::NlToSymbol,
        Split::Train,
        vec!["leindex::parse::rust::parse_rust", "parse_rust"],
        vec!["src/parse/rust.rs"],
        vec![Language::Rust],
        "NL query targeting a specific parser entry point",
    );
    add(
        &mut corpus,
        "nl_sym_2",
        "function that computes the cosine similarity between two embedding vectors",
        EvalCategory::NlToSymbol,
        Split::Eval,
        vec![
            "leindex::search::vector::cosine_similarity",
            "cosine_similarity",
        ],
        vec!["src/search/vector.rs"],
        vec![Language::Rust],
        "NL describing mathematical operation to a symbol",
    );
    add(
        &mut corpus,
        "nl_sym_3",
        "the handler that processes the MCP initialize handshake",
        EvalCategory::NlToSymbol,
        Split::Eval,
        vec![
            "leindex::cli::mcp::server::handle_initialize",
            "handle_initialize",
        ],
        vec!["src/cli/mcp/server.rs"],
        vec![Language::Rust],
        "NL pointing to an MCP protocol handler",
    );

    // ── 2. Exact/partial identifier ─────────────────────────────────────
    add(
        &mut corpus,
        "id_exact_1",
        "CasStore",
        EvalCategory::ExactPartialIdentifier,
        Split::Train,
        vec!["leindex::storage::cas::CasStore", "CasStore"],
        vec!["src/storage/cas/mod.rs"],
        vec![Language::Rust],
        "Exact type name lookup",
    );
    add(
        &mut corpus,
        "id_partial_1",
        "Generation",
        EvalCategory::ExactPartialIdentifier,
        Split::Eval,
        vec![
            "leindex::storage::generation::manifest::Manifest",
            "GenerationLease",
            "GenerationWriter",
        ],
        vec![
            "src/storage/generation/manifest.rs",
            "src/storage/generation/lease.rs",
            "src/storage/generation/writer.rs",
        ],
        vec![Language::Rust],
        "Partial identifier matching multiple generation-related symbols",
    );
    add(
        &mut corpus,
        "id_partial_2",
        "EmbeddingCache",
        EvalCategory::ExactPartialIdentifier,
        Split::Eval,
        vec!["GlobalEmbeddingCache", "CacheKey"],
        vec!["src/embed/cache.rs"],
        vec![Language::Rust],
        "Partial identifier for embedding cache types",
    );

    // ── 3. Concept to implementation ────────────────────────────────────
    add(
        &mut corpus,
        "concept_1",
        "content-addressed storage system",
        EvalCategory::ConceptToImpl,
        Split::Train,
        vec!["CasStore", "blob_hash", "encode_blob", "validate_blob"],
        vec!["src/storage/cas/"],
        vec![Language::Rust],
        "Concept maps to a subsystem, not a single symbol",
    );
    add(
        &mut corpus,
        "concept_2",
        "how does the scheduler decide what work to do next when there are multiple requests",
        EvalCategory::ConceptToImpl,
        Split::Eval,
        vec![
            "leindex::scheduler::queue::DrrQueue",
            "leindex::scheduler::admission::AdmissionController",
            "Scheduler",
        ],
        vec!["src/scheduler/queue.rs", "src/scheduler/admission.rs"],
        vec![Language::Rust],
        "Concept requesting the scheduling subsystem",
    );
    add(
        &mut corpus,
        "concept_3",
        "how does LeIndex keep its index fresh when source files change",
        EvalCategory::ConceptToImpl,
        Split::Eval,
        vec![
            "leindex::cli::index_freshness::check_freshness",
            "index_freshness",
        ],
        vec!["src/cli/index_freshness.rs"],
        vec![Language::Rust],
        "Concept asking about incremental indexing and freshness",
    );

    // ── 4. Error/log → origin ───────────────────────────────────────────
    add(
        &mut corpus,
        "error_1",
        "panic: refcount underflow: cannot decrement below zero",
        EvalCategory::ErrorLogToOrigin,
        Split::Train,
        vec!["leindex::storage::cas::refs::decr", "decr"],
        vec!["src/storage/cas/refs.rs"],
        vec![Language::Rust],
        "Error message originating from refcount decrement logic",
    );
    add(
        &mut corpus,
        "error_2",
        "ERROR leindex: blob validation failed: hash mismatch",
        EvalCategory::ErrorLogToOrigin,
        Split::Eval,
        vec!["validate_blob", "blob_hash"],
        vec!["src/storage/cas/blob.rs"],
        vec![Language::Rust],
        "Log line originating from blob validation",
    );
    add(
        &mut corpus,
        "error_3",
        "error: BadBlob: corrupt magic header expected LIDX-BLB1",
        EvalCategory::ErrorLogToOrigin,
        Split::Eval,
        vec!["validate_blob", "BadBlob"],
        vec!["src/storage/cas/blob.rs"],
        vec![Language::Rust],
        "Error originating from CAS blob validation",
    );

    // ── 5. Caller/callee/data-flow ──────────────────────────────────────
    add(
        &mut corpus,
        "caller_1",
        "who calls blob_hash when writing a new CAS blob",
        EvalCategory::CallerCalleeDataFlow,
        Split::Train,
        vec!["CasStore::put", "encode_blob", "GenerationWriter::stage"],
        vec![
            "src/storage/cas/mod.rs",
            "src/storage/cas/blob.rs",
            "src/storage/generation/writer.rs",
        ],
        vec![Language::Rust],
        "Data-flow query: callers of blob_hash",
    );
    add(
        &mut corpus,
        "caller_2",
        "what functions are called when a search query runs through the fused pipeline",
        EvalCategory::CallerCalleeDataFlow,
        Split::Eval,
        vec!["fused_search", "tfidf_search", "dense_search", "rerank"],
        vec!["src/search/search/mod.rs"],
        vec![Language::Rust],
        "Data-flow query: callees of fused search",
    );
    add(
        &mut corpus,
        "caller_3",
        "trace the embedding generation path from text input to stored vector",
        EvalCategory::CallerCalleeDataFlow,
        Split::Eval,
        vec!["HybridEmbedder", "EmbeddingClient", "batch_embed"],
        vec![
            "src/cli/index_builder/hybrid.rs",
            "src/search/onnx/client.rs",
        ],
        vec![Language::Rust],
        "Data-flow trace across embedding subsystem",
    );

    // ── 6. Interface → implementation ───────────────────────────────────
    add(
        &mut corpus,
        "iface_1",
        "implementations of the BoundedJob trait",
        EvalCategory::InterfaceToImpl,
        Split::Train,
        vec!["IndexJob", "BoundedJob"],
        vec!["src/scheduler/jobs/"],
        vec![Language::Rust],
        "Trait definition to implementations",
    );
    add(
        &mut corpus,
        "iface_2",
        "implementations of RefcountStore trait",
        EvalCategory::InterfaceToImpl,
        Split::Eval,
        vec!["JsonSidecarStore", "SqliteRefStore", "RefcountStore"],
        vec!["src/storage/cas/refs.rs"],
        vec![Language::Rust],
        "Trait to its concrete implementations (sidecar + SQLite)",
    );
    add(
        &mut corpus,
        "iface_3",
        "types implementing the NeuralRowWriter trait",
        EvalCategory::InterfaceToImpl,
        Split::Eval,
        vec!["NeuralRowWriter", "StagedNeuralWriter"],
        vec!["src/search/"],
        vec![Language::Rust],
        "Neural row writer trait and its implementation",
    );

    // ── 7. Config/doc → code ────────────────────────────────────────────
    add(
        &mut corpus,
        "config_1",
        "LEINDEX_TOKIO_WORKERS environment variable configuration",
        EvalCategory::ConfigDocToCode,
        Split::Train,
        vec!["configured_worker_count"],
        vec!["src/bin/leindex.rs"],
        vec![Language::Rust],
        "Env var referenced in docs/config to code that reads it",
    );
    add(
        &mut corpus,
        "config_2",
        "the .env.example documentation for MALLOC_ARENA_MAX",
        EvalCategory::ConfigDocToCode,
        Split::Eval,
        vec!["configured_worker_count"],
        vec![".env.example", "src/bin/leindex.rs"],
        vec![Language::Rust],
        "Config file documentation to code that uses the env var",
    );
    add(
        &mut corpus,
        "config_3",
        "AGENTS.md says run cargo clippy --workspace --all-targets",
        EvalCategory::ConfigDocToCode,
        Split::Eval,
        vec!["AGENTS.md"],
        vec!["AGENTS.md"],
        vec![Language::Rust],
        "Documentation reference to a validation command",
    );

    // ── 8. Similar algorithm, different name ────────────────────────────
    add_with_negs(
        &mut corpus,
        "sim_algo_1",
        "fuzzy string matching for symbol names",
        EvalCategory::SimilarAlgoDifferentName,
        Split::Train,
        vec!["fuzzy_match", "levenshtein"],
        vec!["src/search/"],
        vec![Language::Rust],
        "Query using 'fuzzy' term; implementation may use a different algorithm name",
        vec!["regex_match", "exact_match"],
    );
    add_with_negs(
        &mut corpus,
        "sim_algo_2",
        "nearest neighbor search using approximate indexing",
        EvalCategory::SimilarAlgoDifferentName,
        Split::Eval,
        vec!["hnsw_search", "HnswIndex"],
        vec!["src/search/vector.rs"],
        vec![Language::Rust],
        "Query says 'approximate indexing'; impl uses HNSW",
        vec!["linear_search", "brute_force_dot"],
    );
    add_with_negs(
        &mut corpus,
        "sim_algo_3",
        "vector quantization and dimensionality reduction",
        EvalCategory::SimilarAlgoDifferentName,
        Split::Eval,
        vec!["quantize_int8", "NeuralReader"],
        vec!["src/storage/generation/reader.rs"],
        vec![Language::Rust],
        "Query uses generic ML terms; impl uses INT8 SIMD",
        vec!["pca", "svd_decompose"],
    );

    // ── 9. Same name, different behavior ────────────────────────────────
    add_with_negs(
        &mut corpus,
        "same_name_1",
        "search function",
        EvalCategory::SameNameDifferentBehavior,
        Split::Train,
        vec!["SearchEngine::search", "fused_search"],
        vec!["src/search/search/mod.rs"],
        vec![Language::Rust],
        "'search' is overloaded: SearchEngine::search vs fused_search vs tfidf_search",
        vec!["search_binary_tree"],
    );
    add_with_negs(
        &mut corpus,
        "same_name_2",
        "get method",
        EvalCategory::SameNameDifferentBehavior,
        Split::Eval,
        vec!["CasStore::get", "GlobalEmbeddingCache::get"],
        vec!["src/storage/cas/mod.rs", "src/embed/cache.rs"],
        vec![Language::Rust],
        "'get' exists on multiple types with different behavior",
        vec!["HashMap::get"],
    );
    add_with_negs(
        &mut corpus,
        "same_name_3",
        "gc function",
        EvalCategory::SameNameDifferentBehavior,
        Split::Eval,
        vec!["CasStore::gc"],
        vec!["src/storage/cas/refs.rs"],
        vec![Language::Rust],
        "'gc' means CAS refcount-based GC, not general garbage collection",
        vec!["std::mem::drop"],
    );

    // ── 10. Changed/deleted/freshness ──────────────────────────────────
    add(
        &mut corpus,
        "fresh_1",
        "files that changed since the last index",
        EvalCategory::ChangedDeletedFreshness,
        Split::Train,
        vec!["check_freshness", "compute_changes"],
        vec!["src/cli/index_freshness.rs"],
        vec![Language::Rust],
        "Freshness-sensitive query: recent changes",
    );
    add(
        &mut corpus,
        "fresh_2",
        "how does LeIndex detect that a source file was deleted",
        EvalCategory::ChangedDeletedFreshness,
        Split::Eval,
        vec!["detect_deletions", "compute_changes"],
        vec!["src/cli/index_freshness.rs"],
        vec![Language::Rust],
        "Freshness-sensitive: file deletion detection",
    );
    add(
        &mut corpus,
        "fresh_3",
        "what happens when the git HEAD changes relative to the indexed tree",
        EvalCategory::ChangedDeletedFreshness,
        Split::Eval,
        vec!["corpus_tree_oid", "git_revision"],
        vec!["tools/memcheck/src/env_capture.rs"],
        vec![Language::Rust],
        "Freshness-sensitive: code revision changes",
    );

    // ── 11. Large/generated distractors ─────────────────────────────────
    add_with_negs(
        &mut corpus,
        "large_1",
        "the main MCP tool handler registration",
        EvalCategory::LargeGeneratedDistractors,
        Split::Train,
        vec!["all_tool_handlers"],
        vec!["src/cli/mcp/server.rs"],
        vec![Language::Rust],
        "Correct answer is in a large file; distractors are generated-looking",
        vec!["auto_generated_handler", "gen_stub"],
    );
    add(
        &mut corpus,
        "large_2",
        "canonical phase definitions used in memcheck",
        EvalCategory::LargeGeneratedDistractors,
        Split::Eval,
        vec!["CANONICAL_PHASES"],
        vec!["tools/memcheck/src/workload.rs"],
        vec![Language::Rust],
        "Large source file with many phases; must find the right constant",
    );
    add_with_negs(
        &mut corpus,
        "large_3",
        "the Cargo.toml feature flag definitions",
        EvalCategory::LargeGeneratedDistractors,
        Split::Eval,
        vec!["features"],
        vec!["Cargo.toml"],
        vec![Language::Rust],
        "The Cargo.toml has a large features section; distractors are other config sections",
        vec!["dependencies_list", "profile_settings"],
    );

    // ── 12. Multi-language (Rust/TS/Python/Go/Java/C/C++) ──────────────
    add(
        &mut corpus,
        "multi_rust_1",
        "Rust struct definition for parsed source metadata",
        EvalCategory::MultiLanguage,
        Split::Train,
        vec!["ParseResult", "leindex::parse::ParseResult"],
        vec!["src/parse/mod.rs"],
        vec![Language::Rust],
        "Rust language target",
    );
    add(
        &mut corpus,
        "multi_ts_1",
        "TypeScript interface defining the MCP tool call shape",
        EvalCategory::MultiLanguage,
        Split::Eval,
        vec!["ToolCall", "MCPRequest"],
        vec!["packages/npm-leindex-mcp/src/types.ts"],
        vec![Language::TypeScript],
        "TypeScript language target",
    );
    add(
        &mut corpus,
        "multi_py_1",
        "Python class wrapping the LeIndex CLI",
        EvalCategory::MultiLanguage,
        Split::Eval,
        vec!["LeIndexClient"],
        vec!["packages/pypi-leindex/leindex/client.py"],
        vec![Language::Python],
        "Python language target",
    );
    add(
        &mut corpus,
        "multi_go_1",
        "Go struct for the search request protobuf",
        EvalCategory::MultiLanguage,
        Split::Eval,
        vec!["SearchRequest"],
        vec!["proto/leindex.pb.go"],
        vec![Language::Go],
        "Go language target (hypothetical proto bindings)",
    );
    add(
        &mut corpus,
        "multi_java_1",
        "Java class for the MCP JSON-RPC response envelope",
        EvalCategory::MultiLanguage,
        Split::Eval,
        vec!["JsonRpcResponse"],
        vec!["packages/java/src/main/java/leindex/RpcResponse.java"],
        vec![Language::Java],
        "Java language target",
    );
    add(
        &mut corpus,
        "multi_c_cpp_1",
        "C header declaring the embedding worker IPC protocol",
        EvalCategory::MultiLanguage,
        Split::Eval,
        vec!["leindex_embed_protocol", "EmbedRequest"],
        vec!["include/leindex/embed_protocol.h"],
        vec![Language::C, Language::Cpp],
        "C/C++ language target",
    );

    // ── 13. Cross-language ──────────────────────────────────────────────
    add(
        &mut corpus,
        "xlang_1",
        "how is embedding batch request serialized to the worker in both Rust and the Python client",
        EvalCategory::CrossLanguage,
        Split::Train,
        vec!["EmbedRequest", "batch_embed"],
        vec!["src/embed/", "packages/pypi-leindex/leindex/client.py"],
        vec![Language::Rust, Language::Python],
        "Cross-language query: same concept in Rust server + Python client",
    );
    add(
        &mut corpus,
        "xlang_2",
        "the JSON-RPC notification shape across TypeScript and Rust implementations",
        EvalCategory::CrossLanguage,
        Split::Eval,
        vec!["notify", "McpNotification"],
        vec![
            "packages/npm-leindex-mcp/src/notify.ts",
            "src/cli/mcp/server.rs",
        ],
        vec![Language::TypeScript, Language::Rust],
        "Cross-language: notification format in TS and Rust",
    );
    add(
        &mut corpus,
        "xlang_3",
        "how does the Go SDK call into the Rust search API via HTTP",
        EvalCategory::CrossLanguage,
        Split::Eval,
        vec!["SearchRequest", "leindex_search"],
        vec!["packages/go/leindex/search.go", "src/server/"],
        vec![Language::Go, Language::Rust],
        "Cross-language: Go client to Rust HTTP server",
    );

    // ── 14. Hard negatives ──────────────────────────────────────────────
    add_with_negs(
        &mut corpus,
        "hard_neg_1",
        "the blob hash function used in CAS content addressing",
        EvalCategory::HardNegatives,
        Split::Train,
        vec!["blob_hash"],
        vec!["src/storage/cas/blob.rs"],
        vec![Language::Rust],
        "Correct: CAS blob_hash. Distractors: other hashing code that is syntactically similar",
        vec![
            "sha256_verify", // Similar name (hash) but different purpose (download verification)
            "fingerprint_compute", // Graph fingerprint, different hash
            "tree_oid",      // Git object ID, different hash
        ],
    );
    add_with_negs(
        &mut corpus,
        "hard_neg_2",
        "function that decrements the CAS reference count",
        EvalCategory::HardNegatives,
        Split::Eval,
        vec!["decr"],
        vec!["src/storage/cas/refs.rs"],
        vec![Language::Rust],
        "Correct: CAS refcount decr. Distractors: semantic near-misses",
        vec![
            "incr",            // Mirror operation name, but wrong direction
            "gc",              // Related to refcount (calls decr) but wrong level
            "drop_generation", // Drop also reduces references, wrong context
        ],
    );
    add_with_negs(
        &mut corpus,
        "hard_neg_3",
        "the atomic rename used when publishing a generation manifest",
        EvalCategory::HardNegatives,
        Split::Eval,
        vec!["publish", "GenerationWriter::publish"],
        vec!["src/storage/generation/writer.rs"],
        vec![Language::Rust],
        "Correct: generation publish: rename manifest.partial -> manifest. Distractors: other renames",
        vec![
            "blob_rename",  // CAS blob staging also renames
            "swap_current", // CURRENT file write, related but different
        ],
    );

    corpus
}

// ── Helpers ─────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn add(
    corpus: &mut Corpus,
    id: &str,
    query: &str,
    category: EvalCategory,
    split: Split,
    symbols: Vec<&str>,
    files: Vec<&str>,
    languages: Vec<Language>,
    notes: &str,
) {
    corpus.add_case(CorpusCase {
        id: id.to_string(),
        query: query.to_string(),
        category,
        split,
        relevant_symbols: symbols.into_iter().map(String::from).collect(),
        relevant_files: files.into_iter().map(String::from).collect(),
        languages,
        expected_position: None,
        notes: Some(notes.to_string()),
        hard_negatives: Vec::new(),
    });
}

#[allow(clippy::too_many_arguments)]
fn add_with_negs(
    corpus: &mut Corpus,
    id: &str,
    query: &str,
    category: EvalCategory,
    split: Split,
    symbols: Vec<&str>,
    files: Vec<&str>,
    languages: Vec<Language>,
    notes: &str,
    hard_negatives: Vec<&str>,
) {
    corpus.add_case(CorpusCase {
        id: id.to_string(),
        query: query.to_string(),
        category,
        split,
        relevant_symbols: symbols.into_iter().map(String::from).collect(),
        relevant_files: files.into_iter().map(String::from).collect(),
        languages,
        expected_position: None,
        notes: Some(notes.to_string()),
        hard_negatives: hard_negatives.into_iter().map(String::from).collect(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_corpus_built_and_nonempty() {
        let corpus = build_default_corpus();
        assert!(!corpus.is_empty());
        assert!(corpus.len() >= 28);
    }

    #[test]
    fn test_all_categories_represented() {
        let corpus = build_default_corpus();
        for cat in EvalCategory::all() {
            let count = corpus.cases_for_category(cat).count();
            assert!(
                count >= 2,
                "Category '{}' has only {} cases",
                cat.as_str(),
                count
            );
        }
    }

    #[test]
    fn test_all_categories_have_eval_split() {
        let corpus = build_default_corpus();
        for cat in EvalCategory::all() {
            let eval_count = corpus
                .cases_for_category(cat)
                .filter(|c| c.split == Split::Eval)
                .count();
            assert!(
                eval_count >= 1,
                "Category '{}' has {} eval split cases",
                cat.as_str(),
                eval_count
            );
        }
    }

    #[test]
    fn test_all_cases_have_labels() {
        let corpus = build_default_corpus();
        for case in corpus.all() {
            assert!(
                !case.relevant_symbols.is_empty() || !case.relevant_files.is_empty(),
                "Case '{}' has no labels",
                case.id
            );
        }
    }

    #[test]
    fn test_no_duplicate_case_ids() {
        let corpus = build_default_corpus();
        let mut seen = std::collections::HashSet::new();
        for case in corpus.all() {
            assert!(seen.insert(&case.id), "Duplicate case ID: {}", case.id);
        }
    }

    #[test]
    fn test_hard_negative_cases_have_negatives() {
        let corpus = build_default_corpus();
        for case in corpus.cases_for_category(EvalCategory::HardNegatives) {
            assert!(
                !case.hard_negatives.is_empty(),
                "Hard negative case '{}' missing hard_negatives",
                case.id
            );
        }
    }

    #[test]
    fn test_similar_algo_cases_have_negatives() {
        let corpus = build_default_corpus();
        for case in corpus.cases_for_category(EvalCategory::SimilarAlgoDifferentName) {
            // Similar algo cases should have hard negatives
            assert!(
                !case.hard_negatives.is_empty(),
                "Similar algo case '{}' missing hard_negatives",
                case.id
            );
        }
    }

    #[test]
    fn test_corpus_verification_passes() {
        let corpus = build_default_corpus();
        corpus
            .verify_category_coverage()
            .expect("category coverage");
        corpus.verify_split_integrity().expect("split integrity");
    }

    #[test]
    fn test_multi_language_variety() {
        let corpus = build_default_corpus();
        let multi: Vec<_> = corpus
            .cases_for_category(EvalCategory::MultiLanguage)
            .collect();
        let langs: std::collections::HashSet<_> = multi
            .iter()
            .flat_map(|c| c.languages.iter().copied())
            .collect();
        assert!(langs.len() >= 3, "Need >= 3 languages in multi-language");
    }
}
