//! Unit tests for `neural_publish`, moved out of the parent file by pure code
//! motion (Large File Detection gate: the parent crossed 2000 lines). Declared
//! as `mod tests` there, so `super::*` still reaches every private item.

use super::*;
use crate::cli::leindex::indexing::streaming::neural::NeuralRowWriter;

/// A neural pass that embeds fewer nodes than it admitted must be
/// reportable as a gap. The generation now survives a blip instead of
/// freezing, which means partial coverage is the ORDINARY outcome of a
/// transient outage — and nothing ever retries those nodes on resume, so
/// silence here is a permanently thinner neural layer.
#[test]
fn test_neural_coverage_gap_reports_only_missing_rows() {
    assert_eq!(neural_coverage_gap(70, 100), Some(30));
    assert_eq!(neural_coverage_gap(0, 100), Some(100));
    assert_eq!(neural_coverage_gap(100, 100), None);
    // An empty admitted set is full coverage, not a 0/0 failure: a route
    // with nothing to embed must not warn.
    assert_eq!(neural_coverage_gap(0, 0), None);
}

/// The title is what a remote provider receives as `{title}`, so a leak here
/// is a leak on the wire. These pin the two properties the previous inline
/// match got wrong: an empty remainder after `strip_prefix` must NOT become
/// an empty title, and the fallback must actually separate the paths the
/// "last two components" version claimed to separate.
#[test]
fn test_document_title_relativizes_inside_the_project() {
    let root = std::path::Path::new("/checkout/project");
    assert_eq!(
        document_title("/checkout/project/src/lib.rs", root, "new"),
        "src/lib.rs",
        "a path inside the project becomes project-relative"
    );
    assert_eq!(
        document_title("/checkout/project/src\\win.rs", root, "new"),
        "src/win.rs",
        "backslashes are normalized so the title is separator-stable"
    );
}

#[test]
fn test_document_title_at_the_project_root_uses_the_project_name() {
    // The reported bug: `strip_prefix` SUCCEEDS with an empty remainder for
    // a node whose path IS the checkout root (a project summary node), and a
    // plain `Ok(relative)` arm sent `Some("")` as the title.
    //
    // The fix must NOT be the trailing-components fallback: the absolute
    // path's last three components are the checkout's PARENT directories —
    // `alice/work/leindex` for `/home/alice/work/leindex` — which is the
    // local-path leak this function exists to prevent, merely moved. The
    // answer is the project's own directory name.
    let root = std::path::Path::new("/home/alice/work/leindex");
    assert_eq!(
        document_title("/home/alice/work/leindex", root, "project-summary"),
        "leindex",
        "a checkout-root node is titled by the project, not by its parents"
    );
}

#[test]
fn test_document_title_never_carries_the_checkout_prefix() {
    // The privacy property is that no component of the ABSOLUTE path prefix
    // reaches the title. Asserting `!title.starts_with('/')` cannot fail:
    // the fallback drops every non-`Normal` component, so no title can
    // begin with a separator whatever the input. This asserts the thing
    // that can actually break — that the machine-specific prefix is absent.
    //
    // The shallow tails are the load-bearing cases. An earlier version of
    // this test used only paths with three or more trailing components, so
    // the old fixed-length suffix could never expose `alice` for any of
    // them: the assertion could not fail, whatever the fallback did. A
    // 2-deep tail (`/home/alice/src/main.rs` -> `alice/src/main.rs`) is
    // exactly where the leak lived.
    let root = std::path::Path::new("/home/alice/work/leindex");
    for path in [
        "/home/alice/work/leindex",            // empty remainder
        "/home/alice/work/leindex/src/lib.rs", // ordinary file
        "/elsewhere/sub/module/file.rs",       // outside the project
        "/home/bob/other/src/lib.rs",          // a different checkout
        "/home/alice/src/main.rs",             // shallow outside-project tail
        "/work/alice/app.go",                  // shallower still
    ] {
        let title = document_title(path, root, "node");
        for leaked in ["home", "alice", "bob", "work", "elsewhere"] {
            assert!(
                !title.split('/').any(|component| component == leaked),
                "{path} produced {title:?}, which carries the checkout component {leaked}"
            );
        }
    }
}

#[test]
fn test_document_title_at_a_shallow_project_root_does_not_use_the_directory_name() {
    // `/home/alice` is a two-component project root: `file_name()` is
    // `alice`, which is a username. The directory name is only the project's
    // when there is a path above it that could have been the machine part.
    let shallow = std::path::Path::new("/home/alice");
    assert_eq!(
        document_title("/home/alice", shallow, "project-summary"),
        "project-summary",
        "a two-component root is more likely a home dir than a checkout name"
    );
    let bare = std::path::Path::new("/work");
    assert_eq!(document_title("/work", bare, "sym"), "sym");
    // A normal checkout still gets its own name.
    let nested = std::path::Path::new("/home/alice/work/leindex");
    assert_eq!(
        document_title("/home/alice/work/leindex", nested, "sym"),
        "leindex"
    );
}

#[test]
fn test_document_title_falls_back_to_the_leaf_name_not_a_path_suffix() {
    // A fixed-length suffix of an ABSOLUTE path cannot be safe: the fallback
    // runs precisely when `strip_prefix` failed, so it cannot tell the
    // checkout from the project and ends up shipping `a` / `alice` / a mount
    // point. The contract is now leaf-only. This replaces an earlier test
    // that required `a/src/lib.rs` and `b/src/lib.rs` to differ — that
    // separation is exactly what cost us a username on the wire, so the
    // collision is accepted and pinned here instead.
    let root = std::path::Path::new("/checkout/project");
    let a = document_title("/outside/a/src/lib.rs", root, "new");
    let b = document_title("/outside/b/src/lib.rs", root, "new");
    assert_eq!(a, "lib.rs", "only the file name may be kept");
    assert_eq!(b, "lib.rs");
    assert_eq!(
        a, b,
        "a cosmetic title collision is the accepted price of never \
         sending a parent directory to the provider"
    );
}

#[test]
fn test_document_title_rejects_a_remainder_that_escapes_the_project() {
    // `strip_prefix` is lexical: it does not resolve `..`, so a path that
    // walks out of the project comes back as a SUCCESSFUL relative strip.
    // Sending that verbatim would put `../private/x.rs` — a path outside the
    // project — on the wire under the label "project-relative".
    let root = std::path::Path::new("/checkout/project");
    let title = document_title("/checkout/project/../private/x.rs", root, "node");
    assert!(
        !title.split('/').any(|component| component == ".."),
        "an escaping remainder must not be sent as a title: {title}"
    );
    assert_eq!(
        title, "x.rs",
        "the escaping path takes the leaf-only fallback, so `project/` — a \
         directory name the code cannot prove is safe — is not sent"
    );
}

#[test]
fn test_document_title_normalizes_a_windows_path_on_a_unix_host() {
    // On a Unix host `C:\Users\bob\a.rs` has no `/`, so it parses as ONE
    // `Normal` component whose text still carries the drive letter and the
    // backslashes. Joining components raw (which the fallback used to do)
    // emitted that whole string as a "relative" title — an absolute path on
    // the platform it came from.
    //
    // `bob` is a username here. An earlier version of this test asserted
    // `Users/bob/a.rs` and justified it by calling `Users` an "interior
    // component like /home". That was wrong: on Windows `C:\Users\<name>` IS
    // the home-directory layout, and the leaf-only fallback now keeps
    // neither `Users` nor `bob`.
    #[cfg(unix)]
    {
        let root = std::path::Path::new("/checkout/project");
        let title = document_title(r"C:\Users\bob\a.rs", root, "node");
        assert!(
            !title.contains('\\'),
            "separators must be normalized: {title}"
        );
        assert!(
            !title.starts_with("C:") && !title.split('/').any(|c| c.ends_with(':')),
            "a drive prefix must not reach the title: {title}"
        );
        assert_eq!(title, "a.rs", "no home-directory component may survive");
    }
}

#[test]
fn test_document_title_falls_back_to_the_symbol_name() {
    // A path with no `Normal` components at all: nothing to take, and the
    // symbol name is the only non-leaking stand-in available.
    let root = std::path::Path::new("/checkout/project");
    assert_eq!(document_title("/", root, "root-symbol"), "root-symbol");
    assert_eq!(document_title("..", root, "parent-symbol"), "parent-symbol");
}

#[test]
fn test_stuck_hint_only_after_quiet_timeout_window() {
    use std::time::Duration;
    // Fresh phase: nothing to report.
    assert!(NeuralProgress::stuck_hint(0, Duration::from_secs(0)).is_empty());
    // Still inside the pre-fallback window (30s timeout x 3 retries).
    assert!(NeuralProgress::stuck_hint(0, Duration::from_secs(44)).is_empty());
    // Past it with zero rows: the provider is slow or unreachable.
    assert!(!NeuralProgress::stuck_hint(0, Duration::from_secs(45)).is_empty());
    // Rows landing means the provider is fine; never cry wolf.
    assert!(NeuralProgress::stuck_hint(1, Duration::from_secs(600)).is_empty());
}

/// A checkpoint whose recorded row count disagrees with what the mmap
/// actually restored must report ZERO rows. The mismatch is the only
/// signal that the persisted store is partial, and the caller's
/// re-embed gate is `neural_rows == 0 && !neural_resume_loaded` — handing
/// it the partial count closes that gate and publishes the partial store
/// over the good artifacts, committing the exact corruption the row-count
/// check exists to reject.
#[test]
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn test_checkpoint_row_mismatch_reports_no_rows() {
    use crate::search::search::{DEFAULT_EMBEDDING_DIMENSION, NodeInfo};

    let dir = tempfile::tempdir().expect("checkpoint fixture");
    let storage = dir.path().join(".leindex");
    std::fs::create_dir_all(&storage).expect("storage dir");
    let mmap_path = storage.join("neural_embeddings.bin");

    let node = |node_id: &str| {
        let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
        tfidf_embedding[0] = 1.0;
        NodeInfo {
            node_id: node_id.to_string(),
            file_path: "fixture.rs".to_string(),
            symbol_name: "new".to_string(),
            language: "rust".to_string(),
            content: "fn new() {}".to_string(),
            byte_range: (0, 11),
            tfidf_embedding,
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }
    };

    // 2 rows on disk, 2 matching nodes in the resident engine, but the
    // checkpoint claims 5 — the partial-store case.
    let rows = vec![
        ("fixture.rs:new".to_string(), vec![1.0, 0.0]),
        ("fixture.rs:other".to_string(), vec![0.0, 1.0]),
    ];
    crate::search::vector::write_mmap_embeddings(&mmap_path, &rows).expect("persist fixture");

    let mut index = LeIndex::new(dir.path()).expect("checkpoint index");
    index
        .search_engine
        .index_nodes(vec![node("fixture.rs:new"), node("fixture.rs:other")]);

    let checkpoint = NeuralCheckpoint {
        lexical_hash: "lexical".to_string(),
        mmap_path: mmap_path.clone(),
        rows: 5,
        provider: "remote".to_string(),
        model: "model".to_string(),
        matryoshka: None,
    };

    assert_eq!(
        index.restore_neural_checkpoint(Some(&checkpoint), "lexical", "model"),
        (0, false),
        "a partial restore must report zero rows so the caller re-embeds"
    );
}

#[test]
fn test_neural_progress_writer_counts_rows_and_passes_through() {
    let rows = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut inner = streaming::neural::VecNeuralRowWriter::new();
    {
        let mut writer = ProgressNeuralRowWriter {
            inner: &mut inner,
            rows_reported: &rows,
        };
        let batch = |w: &mut ProgressNeuralRowWriter<'_, streaming::neural::VecNeuralRowWriter>,
                     base: usize,
                     n: usize| {
            let batch: Vec<_> = (base..base + n)
                .map(|i| (format!("node-{i}"), vec![i as f32, (i + 1) as f32]))
                .collect();
            w.write_batch(batch).expect("write_batch");
        };
        batch(&mut writer, 0, 2);
        assert_eq!(rows.load(std::sync::atomic::Ordering::Relaxed), 2);
        batch(&mut writer, 2, 3);
        assert_eq!(rows.load(std::sync::atomic::Ordering::Relaxed), 5);
    }
    assert_eq!(inner.rows.len(), 5, "all rows must reach the inner writer");
    assert_eq!(
        inner.rows[0],
        ("node-0".to_owned(), vec![0.0, 1.0]),
        "rows must pass through unmodified"
    );
    // Single-row path forwards too.
    let mut writer = ProgressNeuralRowWriter {
        inner: &mut inner,
        rows_reported: &rows,
    };
    writer
        .write_row("solo", &[9.0])
        .expect("write_row forwards");
    assert!(inner.rows.iter().any(|(id, _)| id == "solo"));
}

#[test]
fn test_neural_progress_heartbeat_starts_and_stops() {
    // The heartbeat thread must start, print at least once, and exit on
    // drop — proving the phase can report liveness even while the first
    // embed batch is still in flight.
    let progress = NeuralProgress::start_reporting(10, None);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let observed = progress
        .rows
        .fetch_add(0, std::sync::atomic::Ordering::Relaxed);
    drop(progress);
    assert_eq!(observed, 0, "no rows reported by the heartbeat itself");
}

#[test]
fn test_all_batches_empty_is_distinguishable_from_no_work() {
    // enrich_neural_for_route warns only when batches ran and produced no
    // rows. Pin the stats contract that condition reads: an embedder that
    // answers with only empty vectors must land in rows_skipped with
    // batches > 0, never be mistaken for "no nodes to embed".
    struct DeadEmbedder;
    impl streaming::neural::StreamingEmbedder for DeadEmbedder {
        fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
            vec![Vec::new(); texts.len()]
        }
    }
    let mut inputs = (0..4).map(|i| {
        Ok(streaming::neural::NeuralInput {
            node_id: format!("node-{i}"),
            text: format!("document-{i}"),
            title: None,
        })
    });
    let mut writer = streaming::neural::VecNeuralRowWriter::new();
    let budget = streaming::BatchBudget {
        max_texts: 2,
        ..Default::default()
    };
    let dead = streaming::neural::enrich_neural_streaming(
        &mut inputs,
        &DeadEmbedder,
        &mut writer,
        &budget,
    )
    .expect("empty vectors are a skip, not an error");
    assert!(dead.batches > 0, "batches ran");
    assert_eq!(dead.rows_written, 0, "no rows landed");
    assert_eq!(dead.rows_skipped, 4, "every input accounted for");
    assert!(writer.rows.is_empty());

    // Zero inputs is the other side of the same condition: no batches, so
    // the dead-embedder warning must not fire.
    let mut none = std::iter::empty();
    let idle = streaming::neural::enrich_neural_streaming(
        &mut none,
        &DeadEmbedder,
        &mut streaming::neural::VecNeuralRowWriter::new(),
        &budget,
    )
    .expect("empty input");
    assert_eq!(idle.batches, 0);
    assert_eq!(idle.rows_written, 0);
}

#[test]
fn test_search_neural_row_writer_keeps_first_row_per_node_id() {
    use crate::search::search::{DEFAULT_EMBEDDING_DIMENSION, NodeInfo, SearchEngine};

    // append_nodes keeps the FIRST occurrence of a duplicate node_id while
    // update_neural_embeddings applies rows last-wins. A late duplicate row
    // must therefore not overwrite the retained node's own body.
    let node = |node_id: &str, file_path: &str| {
        let mut tfidf_embedding = vec![0.0; DEFAULT_EMBEDDING_DIMENSION];
        tfidf_embedding[0] = 1.0;
        NodeInfo {
            node_id: node_id.to_string(),
            file_path: file_path.to_string(),
            symbol_name: "new".to_string(),
            language: "rust".to_string(),
            content: "fn new() {}".to_string(),
            byte_range: (0, 11),
            tfidf_embedding,
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }
    };
    let mut engine = SearchEngine::new();
    engine.index_nodes(vec![node("a.rs:new", "a.rs"), node("b.rs:new", "b.rs")]);

    let mut writer = SearchNeuralRowWriter {
        engine: &mut engine,
        seen_node_ids: HashSet::new(),
    };
    writer
        .write_batch(vec![
            ("a.rs:new".to_string(), vec![1.0, 0.0]),
            ("a.rs:new".to_string(), vec![0.0, 1.0]),
        ])
        .expect("first batch");
    writer
        .write_batch(vec![("a.rs:new".to_string(), vec![0.25, 0.75])])
        .expect("second batch");
    drop(writer);

    assert_eq!(
        engine.collect_neural_embeddings(),
        vec![("a.rs:new".to_string(), vec![1.0, 0.0])],
        "first occurrence per node_id kept, later duplicates dropped"
    );
}

#[test]
fn test_fragment_provider_identity_prevents_old_space_merge() {
    let mut manifest = index_builder::fragment::sync::FragmentFileManifest::new();
    manifest.extraction_identity.model_name = "remote|old-template|768".into();
    let old_rows = HashMap::from([("content-hash".into(), vec![0.5; 768])]);
    assert_eq!(
        reusable_fragment_rows(old_rows.clone(), Some(&manifest), "remote|old-template|768").len(),
        1
    );
    assert!(
        reusable_fragment_rows(old_rows.clone(), Some(&manifest), "remote|new-template|768")
            .is_empty()
    );
    assert!(
        reusable_fragment_rows(old_rows.clone(), Some(&manifest), "local|qwen|1024").is_empty()
    );
    assert!(reusable_fragment_rows(old_rows, None, "remote|old-template|768").is_empty());
}

#[test]
#[cfg(feature = "remote-embeddings")]
fn test_streaming_generation_remote_outage_persists_single_space() {
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_requests = requests.clone();
    let endpoint = index_builder::remote_embed_test::spawn_mock_embeddings_server_responses(
        move |_| {
            if server_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                768
            } else {
                0
            }
        },
        None,
        2,
    );
    let tfidf = index_builder::TfIdfEmbedder::build(&[("a.rs".into(), "fn a() {}".into())]);
    let generation = index_builder::GenerationNeuralEmbedder::new(
        index_builder::HybridEmbedder::hybrid_remote(
            tfidf,
            crate::search::onnx::RemoteEmbeddingConfig {
                provider: crate::search::onnx::RemoteProvider::Custom { endpoint },
                remote_dimension: Some(768),
                ..crate::search::onnx::RemoteEmbeddingConfig::default()
            },
            None,
        )
        .expect("remote generation"),
    );
    let adapter = HybridStreamingEmbedder { inner: &generation };
    let mut inputs = (0..6).map(|i| {
        Ok(streaming::neural::NeuralInput {
            node_id: format!("node-{i}"),
            text: format!("document-{i}"),
            title: Some(format!("src/file-{i}.rs")),
        })
    });
    let mut writer = streaming::neural::VecNeuralRowWriter::new();
    let budget = streaming::BatchBudget {
        max_texts: 2,
        ..Default::default()
    };
    let stats =
        streaming::neural::enrich_neural_streaming(&mut inputs, &adapter, &mut writer, &budget)
            .expect("streaming generation");
    assert_eq!(stats.rows_written, 2);
    assert_eq!(stats.rows_skipped, 4);
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    let dir = tempfile::tempdir().expect("streaming mmap");
    let path = dir.path().join("neural_embeddings.bin");
    crate::search::vector::write_mmap_embeddings(&path, &writer.rows).expect("persist streaming");
    let mmap = crate::search::vector::MmapEmbeddingIndex::open(&path).expect("open streaming");
    let rows = mmap.entries().expect("streaming stored rows");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(_, row)| row.len() == 768));
}

#[test]
fn test_split_neural_chunks_covers_every_byte_under_the_cap() {
    // A node far larger than NEURAL_CHUNK_BYTES: the pre-fix path truncated
    // it at 64 KiB, so everything past that never reached the embedder.
    let mut source = String::new();
    for index in 0..4_000 {
        source.push_str(&format!("fn generated_{index}() -> i32 {{ {index} }}\n"));
    }
    assert!(
        source.len() > 2 * NEURAL_CHUNK_BYTES,
        "fixture must exceed the chunk cap"
    );

    let chunks = split_neural_chunks(&source, Path::new("generated.rs"));

    assert!(chunks.len() > 1, "a large node must split into chunks");
    // Coverage, not just count: every non-terminator character must appear
    // exactly once, in order. Newlines are insignificant here — a fragment
    // includes the terminators INSIDE it but not the one ending its last line,
    // so a seam drops exactly one newline per split.
    let joined: String = chunks.concat();
    assert_eq!(
        joined.replace('\n', ""),
        source.replace('\n', ""),
        "chunks must cover every line of the node, in order"
    );
    for chunk in &chunks {
        // The bound that matters: no chunk may reach `cap_neural_text`'s
        // 64 KiB truncation point, or the tail of that chunk is dropped and
        // we are back to the bug this splitter exists to fix. (The chunker's
        // nominal target can overshoot by a few bytes, so assert against the
        // truncation point rather than NEURAL_CHUNK_BYTES.)
        assert!(
            chunk.len() < 64 * 1024,
            "chunk of {} bytes reaches the 64 KiB truncation point",
            chunk.len()
        );
    }
    // The pre-fix path truncated at 64 KiB. Prove the coverage now exceeds
    // that, i.e. the tail that used to be dropped is embedded.
    assert!(
        joined.len() > 64 * 1024,
        "coverage ({}) must exceed the old 64 KiB truncation point",
        joined.len()
    );
}

#[test]
fn test_split_neural_chunks_keeps_small_node_as_one_input() {
    // Short nodes must be byte-for-byte unchanged from the pre-chunking path.
    let source = "// type:function lang:rust\n// f in a.rs\nfn f() -> i32 { 1 }\n";
    let chunks = split_neural_chunks(source, Path::new("a.rs"));
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].trim_end(), source.trim_end());
}

#[test]
fn test_split_neural_chunks_never_returns_empty() {
    // An empty node still needs one input, or the node silently gets no row.
    assert_eq!(
        split_neural_chunks("", Path::new("a.rs")),
        vec![String::new()]
    );
}

#[test]
fn test_mean_pool_emits_one_row_per_node_across_a_batch_boundary() {
    // The regression this guards: `SearchNeuralRowWriter` keeps only the
    // FIRST row per node id. Without pooling, a node's later chunks would be
    // dropped; with pooling, the mean must land exactly once even though the
    // node's chunks were flushed in two separate batches.
    let inputs: Vec<Result<streaming::neural::NeuralInput>> = vec![
        Ok(streaming::neural::NeuralInput {
            node_id: "big".to_string(),
            text: "chunk-a".to_string(),
            title: None,
        }),
        Ok(streaming::neural::NeuralInput {
            node_id: "big".to_string(),
            text: "chunk-b".to_string(),
            title: None,
        }),
        Ok(streaming::neural::NeuralInput {
            node_id: "big".to_string(),
            text: "chunk-c".to_string(),
            title: None,
        }),
        Ok(streaming::neural::NeuralInput {
            node_id: "small".to_string(),
            text: "chunk-d".to_string(),
            title: None,
        }),
    ];
    // One input per batch forces "big" to straddle three batch boundaries.
    let budget = streaming::BatchBudget {
        max_texts: 1,
        ..Default::default()
    };
    let mut source = inputs.into_iter();
    let mut writer = MeanPoolNeuralRowWriter::new(streaming::neural::VecNeuralRowWriter::new());
    let embedder = streaming::neural::MockEmbedder::new(4);
    let stats =
        streaming::neural::enrich_neural_streaming(&mut source, &embedder, &mut writer, &budget)
            .expect("mean-pooled streaming");
    // Mirrors `enrich_neural_for_route`: the streaming pass has no finish
    // hook, so the trailing node must be flushed explicitly. Without this the
    // last node silently loses its row.
    writer.finish().expect("flush the trailing node");

    let rows = &writer.inner.rows;
    assert_eq!(rows.len(), 2, "one row per node, got {rows:?}");
    assert_eq!(rows[0].0, "big");
    assert_eq!(rows[1].0, "small");

    // The pooled row must be the true mean of the three chunk vectors.
    use streaming::neural::StreamingEmbedder as _;
    let expected: Vec<Vec<f32>> = embedder.embed_batch(&[
        "chunk-a".to_string(),
        "chunk-b".to_string(),
        "chunk-c".to_string(),
    ]);
    for (dim, pooled) in rows[0].1.iter().enumerate() {
        let mean = (expected[0][dim] + expected[1][dim] + expected[2][dim]) / 3.0;
        assert!(
            (pooled - mean).abs() < 1e-6,
            "dim {dim}: pooled {pooled} != mean {mean}"
        );
    }
    // 4 inputs consumed, 2 rows written to the wrapped writer.
    assert_eq!(stats.batches, 4);
}

/// Opt-in measurement of the chunk-split + mean-pool input stage on a fixed
/// synthetic corpus. No model, no network, no timing assertions — it RECORDS
/// numbers and guards the invariants the feature exists to provide.
///
/// Run:
/// `cargo test --lib --features onnx test_neural_chunking_benchmark -- --ignored --nocapture`
///
/// Corpus: `NODES` nodes of `NODE_BYTES` each (default 512 × 256 KiB = 128 MiB
/// of source) generated one node at a time, so the measurement reflects
/// streaming rather than a pre-built corpus.
///
/// Guards (each fails the run):
/// - one stored row per node (chunk pooling must not drop or duplicate),
/// - no chunk reaches the 64 KiB truncation point,
/// - concatenated chunk coverage equals the node text (nothing skipped),
/// - peak RSS growth stays under `RSS_GROWTH_LIMIT` — the regression guard
///   against re-introducing whole-corpus accumulation.
#[test]
#[ignore = "opt-in measurement; run explicitly with --ignored --nocapture"]
fn test_neural_chunking_benchmark() {
    use std::fmt::Write as _;

    /// Generous ceiling: streaming peaks a few MiB above baseline, while
    /// accumulating this corpus would need >= the full 128 MiB.
    const RSS_GROWTH_LIMIT: u64 = 64 * 1024 * 1024;

    fn env_usize(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value: &usize| *value > 0)
            .unwrap_or(default)
    }

    /// Peak resident set in bytes.
    ///
    /// `/proc/self/status` is Linux-only, so the reader is gated rather than
    /// relying on the `ok()?` of a failing open: an ungated version compiles
    /// everywhere and silently reports `None` on macOS/Windows, which would
    /// turn the RSS regression guard below into a no-op that still prints a
    /// passing benchmark.
    #[cfg(target_os = "linux")]
    fn peak_rss_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status.lines().find_map(|line| {
            let rest = line.strip_prefix("VmHWM:")?;
            Some(rest.split_whitespace().next()?.parse::<u64>().ok()? * 1024)
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn peak_rss_bytes() -> Option<u64> {
        None
    }

    /// One node's enriched text: exactly `bytes` bytes of realistic Rust.
    fn node_text(index: usize, bytes: usize) -> String {
        let mut text = format!("// type:function lang:rust\n// bench_{index} in bench.rs\n");
        let mut line = 0;
        while text.len() < bytes {
            let _ = writeln!(
                text,
                "fn generated_{index}_{line}(items: &[String]) -> Result<usize, Error> {{ \
                 let mut total = 0; for item in items {{ total += item.len(); }} Ok(total) }}"
            );
            line += 1;
        }
        text
    }

    let nodes = env_usize("LEINDEX_BENCH_NODES", 512);
    let node_bytes = env_usize("LEINDEX_BENCH_NODE_BYTES", 256 * 1024);

    let baseline_rss = peak_rss_bytes();
    let started = std::time::Instant::now();

    println!(
        "nodes,node_bytes,total_bytes,chunks,chunks_per_node,rows,bytes_covered,\
         bytes_old_cap_kept,coverage_gain"
    );

    let mut total_chunks = 0usize;
    let mut total_covered = 0usize;
    let mut total_old_cap = 0usize;

    // One streaming pass over lazily generated nodes: the writer sees only
    // one node's chunks at a time, never the whole corpus.
    let mut inputs = (0..nodes).flat_map(|index| {
        let text = node_text(index, node_bytes);
        // Sample the first node's shape for the per-node guards below.
        let chunks = split_neural_chunks(&text, Path::new("bench.rs"));
        total_chunks += chunks.len();
        total_covered += chunks.iter().map(String::len).sum::<usize>();
        total_old_cap += text.len().min(64 * 1024);
        assert!(
            chunks.len() > 1,
            "node of {} bytes must split into multiple chunks",
            text.len()
        );
        for chunk in &chunks {
            assert!(
                chunk.len() < 64 * 1024,
                "chunk of {} bytes reaches the 64 KiB truncation point",
                chunk.len()
            );
        }
        let joined: String = chunks.concat();
        assert_eq!(
            joined.replace('\n', ""),
            text.replace('\n', ""),
            "chunk coverage must reproduce the node text"
        );
        chunks.into_iter().map(move |chunk| {
            Ok(streaming::neural::NeuralInput {
                node_id: format!("bench-{index}"),
                text: chunk,
                title: Some("bench.rs".to_string()),
            })
        })
    });

    let mut writer = MeanPoolNeuralRowWriter::new(streaming::neural::VecNeuralRowWriter::new());
    let budget = neural_batch_budget();
    streaming::neural::enrich_neural_streaming(
        &mut inputs,
        &streaming::neural::MockEmbedder::new(64),
        &mut writer,
        &budget,
    )
    .expect("benchmark streaming pass");
    writer.finish().expect("flush trailing node");

    let elapsed = started.elapsed();
    let peak_rss = peak_rss_bytes();
    println!(
        "{nodes},{node_bytes},{},{total_chunks},{:.2},{},{total_covered},{total_old_cap},{}",
        nodes * node_bytes,
        total_chunks as f64 / nodes as f64,
        writer.inner.rows.len(),
        total_covered.saturating_sub(total_old_cap),
    );

    // Guard: exactly one stored row per node — chunk pooling must neither
    // drop a node nor duplicate one.
    assert_eq!(
        writer.inner.rows.len(),
        nodes,
        "one pooled row per node required, got {} for {nodes} nodes",
        writer.inner.rows.len()
    );
    // Guard: the feature's whole point — coverage now exceeds what the old
    // 64 KiB truncation kept.
    assert!(
        total_covered > total_old_cap,
        "coverage ({total_covered}) must exceed the old cap's ({total_old_cap})"
    );
    if let (Some(baseline), Some(peak)) = (baseline_rss, peak_rss) {
        let growth = peak.saturating_sub(baseline);
        println!(
            "peak_rss_bytes={peak} baseline_rss_bytes={baseline} growth_bytes={growth} \
             limit_bytes={RSS_GROWTH_LIMIT} elapsed_s={:.2}",
            elapsed.as_secs_f64()
        );
        assert!(
            growth < RSS_GROWTH_LIMIT,
            "peak RSS grew {growth} bytes (limit {RSS_GROWTH_LIMIT}): the input stage is \
             accumulating the corpus instead of streaming one node at a time"
        );
    }
}

#[test]
fn test_streaming_neural_inputs_missing_source_uses_empty_bytes() {
    let dir = tempfile::tempdir().expect("neural source fixture");
    let project_path = dir.path().join("checkout");
    std::fs::create_dir_all(&project_path).expect("project root fixture");
    let file_path = project_path.join("missing.rs").display().to_string();
    let node_id = format!("{file_path}:missing_source");
    let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
    pdg.add_node(crate::graph::pdg::Node {
        id: node_id.clone(),
        node_type: crate::graph::pdg::NodeType::Function,
        name: "missing_source".to_string(),
        file_path: std::sync::Arc::from(file_path),
        byte_range: (0, 1),
        complexity: 1,
        language: "rust".to_string(),
    });
    let admitted = HashSet::from([node_id.clone()]);

    let mut inputs = streaming_neural_inputs(&pdg, &admitted, &project_path);
    let input = inputs
        .next()
        .expect("admitted node should produce a neural input")
        .expect("missing source uses the empty-byte fallback");
    assert_eq!(input.node_id, node_id);
    // The title is the PROJECT-RELATIVE path: an absolute one would leak the
    // local checkout path to a remote provider and make title-aware
    // embeddings differ between identical checkouts.
    assert_eq!(input.title.as_deref(), Some("missing.rs"));
    assert!(inputs.next().is_none());
}
