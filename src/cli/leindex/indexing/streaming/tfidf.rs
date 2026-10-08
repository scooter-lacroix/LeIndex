//! Streaming two-pass TF-IDF stage (WS6-9 Task 4).
//!
//! Pass 1 streams admitted docs to update document frequencies; then
//! vocab/IDF are frozen. Pass 2 streams docs again, tokenizes, and writes
//! each row directly to CAS-staged vector storage. No `Vec<Vec<f32>>` of the
//! entire corpus is ever materialized on the heap (spec §6.4, VAL-STREAM-005).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::index_builder;

/// A TF-IDF row: one document's TF-IDF vector ready for CAS staging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TfidfRow {
    /// Node/document ID this row corresponds to.
    pub doc_id: String,
    /// TF-IDF vector (sparse or dense, depending on downstream consumer).
    pub values: Vec<f32>,
}

/// Statistics from a two-pass TF-IDF build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TfidfStats {
    /// Number of documents (rows) processed.
    pub doc_count: usize,
    /// Number of unique tokens in the frozen vocabulary.
    pub vocab_size: usize,
    /// Total bytes of vector data written.
    pub bytes_written: usize,
}

/// Trait for consuming TF-IDF rows (CAS staging, Vec collection, etc.).
pub trait TfidfRowWriter {
    /// Write a single TF-IDF row directly to staged storage.
    fn write_row(&mut self, row: &TfidfRow) -> Result<()>;
}

/// Vec-backed TF-IDF row writer for testing.
#[derive(Debug, Default)]
pub struct VecTfidfRowWriter {
    /// Collected rows.
    pub rows: Vec<TfidfRow>,
}

impl VecTfidfRowWriter {
    /// Create an empty Vec TF-IDF writer.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TfidfRowWriter for VecTfidfRowWriter {
    fn write_row(&mut self, row: &TfidfRow) -> Result<()> {
        self.rows.push(row.clone());
        Ok(())
    }
}

/// A document input for TF-IDF: (doc_id, tokenized_content).
#[derive(Debug, Clone)]
pub struct TfidfDoc {
    /// Document/node ID.
    pub doc_id: String,
    /// Tokenized content (pre-split tokens).
    pub tokens: Vec<String>,
}

/// Tokenize code using the production index-builder tokenizer. Keeping this
/// public streaming-stage helper as a thin reuse avoids a second camel/acronym/
/// digit boundary implementation drifting from the legacy route.
pub fn tokenize_code(text: &str) -> Vec<String> {
    index_builder::tokenize_code(text)
}

/// Build document frequencies from a pass over documents.
///
/// This is the first pass: iterate over all admitted docs, counting the
/// document frequency of each token. No per-document vectors are retained.
pub fn build_document_frequencies(docs: &[TfidfDoc]) -> (HashMap<String, usize>, usize) {
    let mut df: HashMap<String, usize> = HashMap::new();
    let n = docs.len();
    for doc in docs {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for token in &doc.tokens {
            if seen.insert(token.as_str()) {
                *df.entry(token.clone()).or_insert(0) += 1;
            }
        }
    }
    (df, n)
}

/// Freeze vocabulary + IDF from document frequencies.
///
/// Selects the top-K tokens by IDF score using stratified sampling
/// (simplified version of the existing TfIdfEmbedder::build_from_tokens).
pub fn freeze_vocab_idf(
    df: &HashMap<String, usize>,
    n_docs: usize,
    target_dim: usize,
) -> (Vec<String>, Vec<f32>) {
    if n_docs == 0 || df.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let n_f = n_docs as f32;
    let min_df = if n_docs < 50 {
        1
    } else {
        (n_docs / 1000).max(3)
    };
    let max_df = if n_docs < 50 {
        n_docs
    } else {
        (n_docs / 4).max(min_df + 1)
    };

    let mut idf_scores: Vec<(String, f32)> = df
        .iter()
        .filter(|&(_, &df_count)| df_count >= min_df && df_count <= max_df)
        .map(|(tok, &df_count)| {
            let idf = (n_f / df_count as f32).ln();
            (tok.clone(), idf)
        })
        .collect();

    idf_scores.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let final_scores: Vec<(String, f32)> = if idf_scores.len() <= target_dim {
        idf_scores
    } else {
        let total = idf_scores.len();
        let stride = total as f64 / target_dim as f64;
        (0..target_dim)
            .map(|i| {
                let idx = ((i as f64 * stride) as usize).min(total - 1);
                idf_scores[idx].clone()
            })
            .collect()
    };

    let vocab: Vec<String> = final_scores.iter().map(|(t, _)| t.clone()).collect();
    let idf: Vec<f32> = final_scores.iter().map(|(_, s)| *s).collect();
    (vocab, idf)
}

/// Compute a TF-IDF row for a single document against the frozen vocab/IDF.
///
/// Returns a `target_dim`-length L2-normalized vector. This is the per-doc
/// computation for pass 2 — only one row is on the heap at a time.
pub fn compute_tfidf_row(doc: &TfidfDoc, vocab: &[String], idf: &[f32], dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    if vocab.is_empty() {
        return vec;
    }

    let total = doc.tokens.len() as f32;
    if total == 0.0 {
        return vec;
    }

    let mut tf_map: HashMap<&str, f32> = HashMap::new();
    for tok in &doc.tokens {
        *tf_map.entry(tok.as_str()).or_insert(0.0) += 1.0;
    }

    for (slot, (word, idf_val)) in vec.iter_mut().zip(vocab.iter().zip(idf.iter())) {
        if let Some(&count) = tf_map.get(word.as_str()) {
            *slot = (count / total) * idf_val;
        }
    }

    // L2 normalize
    let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if magnitude > 1e-9 {
        for v in &mut vec {
            *v /= magnitude;
        }
    }

    vec
}

/// Two-pass streaming TF-IDF build.
///
/// Pass 1: stream all docs, build document frequencies, freeze vocab/IDF.
/// Pass 2: stream all docs again, compute each row, write to `writer`.
///
/// At no point is a `Vec<Vec<f32>>` of the entire corpus materialized. Only
/// one row is on the heap at a time (the one being written). VAL-STREAM-005.
pub fn streaming_tfidf<W: TfidfRowWriter>(
    docs: &[TfidfDoc],
    writer: &mut W,
    target_dim: usize,
) -> Result<TfidfStats> {
    // Pass 1: build DF
    let (df, n_docs) = build_document_frequencies(docs);
    // Freeze vocab/IDF
    let (vocab, idf) = freeze_vocab_idf(&df, n_docs, target_dim);

    let mut stats = TfidfStats {
        doc_count: 0,
        vocab_size: vocab.len(),
        bytes_written: 0,
    };

    // Pass 2: stream docs, compute each row, write directly
    for doc in docs {
        let values = compute_tfidf_row(doc, &vocab, &idf, target_dim);
        let row = TfidfRow {
            doc_id: doc.doc_id.clone(),
            values,
        };
        stats.bytes_written += row.values.len() * std::mem::size_of::<f32>();
        writer.write_row(&row)?;
        stats.doc_count += 1;
    }

    Ok(stats)
}

/// Serialize a set of TF-IDF rows for CAS staging.
pub fn serialize_tfidf_rows(rows: &[TfidfRow]) -> Result<Vec<u8>> {
    Ok(bincode::serialize(rows)?)
}

/// Deserialize TF-IDF rows from a CAS blob.
pub fn deserialize_tfidf_rows(data: &[u8]) -> Result<Vec<TfidfRow>> {
    Ok(bincode::deserialize(data)?)
}

/// Streaming pass 1 over the PDG for the production lexical route.
///
/// Builds document frequencies in `chunk_size`-node chunks with the
/// production tokenizer and the exact `enriched_node_content` corpus the
/// legacy route tokenizes. File bodies are re-read per chunk rather than
/// retained across the pass. IO failures abort rather than publish partial
/// document frequencies.
pub fn build_document_frequencies_chunked(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    chunk_size: usize,
) -> Result<(HashMap<String, usize>, usize)> {
    use rayon::prelude::*;

    let connectivity_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[
            crate::graph::pdg::EdgeType::Call,
            crate::graph::pdg::EdgeType::DataDependency,
        ]),
        excluded_node_types: Some(vec![crate::graph::pdg::NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };
    let file_summary_ctx = index_builder::FileSummaryContext::from_pdg(pdg);
    let node_indices: Vec<petgraph::graph::NodeIndex> = pdg.node_indices().collect();
    let mut df: HashMap<String, usize> = HashMap::new();
    let mut total_docs = 0usize;

    for nodes in node_indices.chunks(chunk_size.max(1)) {
        let documents: Vec<Result<HashMap<String, usize>>> = nodes
            .par_iter()
            .filter_map(|&node_idx| {
                let node = pdg.get_node(node_idx)?;
                if node.node_type == crate::graph::pdg::NodeType::External {
                    return None;
                }
                Some((|| {
                    let file_bytes = std::fs::read(Path::new(&*node.file_path))
                        .with_context(|| format!("read source for TF-IDF node {}", node.id))?;
                    let node_content = index_builder::enriched_node_content(
                        pdg,
                        node_idx,
                        node,
                        &file_bytes,
                        &connectivity_config,
                        &file_summary_ctx,
                    );
                    let tokens = index_builder::tokenize_code(&node_content);
                    let mut seen = std::collections::HashSet::new();
                    let mut doc_df = HashMap::new();
                    for token in tokens {
                        if seen.insert(token.clone()) {
                            *doc_df.entry(token).or_insert(0) += 1;
                        }
                    }
                    Ok(doc_df)
                })())
            })
            .collect();

        total_docs += documents.len();
        for document in documents {
            for (token, count) in document? {
                *df.entry(token).or_insert(0) += count;
            }
        }
    }

    Ok((df, total_docs))
}

/// Build the production TF-IDF embedder from chunked streaming document
/// frequencies. The vocabulary policy intentionally matches the legacy
/// builder: moderate-frequency filter, sorted `(idf, token)`, and stratified
/// sampling to the same fixed dimension.
pub fn build_streaming_embedder(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    chunk_size: usize,
) -> Result<index_builder::TfIdfEmbedder> {
    let (df, total_docs) = build_document_frequencies_chunked(pdg, chunk_size)?;
    if total_docs == 0 {
        return Ok(index_builder::TfIdfEmbedder::build_from_tokens(&[]));
    }
    let (vocab, idf) = freeze_vocab_idf(
        &df,
        total_docs,
        crate::search::search::DEFAULT_EMBEDDING_DIMENSION,
    );
    Ok(index_builder::TfIdfEmbedder {
        vocab,
        idf,
        dimension: crate::search::search::DEFAULT_EMBEDDING_DIMENSION,
        pdg_nodes: pdg.node_count(),
        pdg_edges: pdg.edge_count(),
        pdg_fingerprint: index_builder::pdg_search_fingerprint(pdg),
        slots: Default::default(),
    })
}

#[cfg(all(test, feature = "full"))]
mod test {
    use super::*;

    fn make_docs() -> Vec<TfidfDoc> {
        vec![
            TfidfDoc {
                doc_id: "doc1".into(),
                tokens: tokenize_code("fn hello_world() { let x = 1; }"),
            },
            TfidfDoc {
                doc_id: "doc2".into(),
                tokens: tokenize_code("fn goodbye_world() { let y = 2; }"),
            },
            TfidfDoc {
                doc_id: "doc3".into(),
                tokens: tokenize_code("struct MyStruct { field1: i32, field2: String }"),
            },
        ]
    }

    /// VAL-STREAM-005: Two-pass TF-IDF with no corpus materialization.
    #[test]
    fn test_streaming_tfidf_no_corpus_vec() {
        let docs = make_docs();
        let mut writer = VecTfidfRowWriter::new();
        let stats = streaming_tfidf(&docs, &mut writer, 32).unwrap();

        // All rows written individually
        assert_eq!(writer.rows.len(), 3);
        assert_eq!(stats.doc_count, 3);
        assert!(stats.vocab_size > 0);

        // Each row has the correct dimension
        for row in &writer.rows {
            assert_eq!(row.values.len(), 32);
        }
    }

    #[test]
    fn test_tfidf_rows_normalized() {
        let docs = make_docs();
        let mut writer = VecTfidfRowWriter::new();
        let _stats = streaming_tfidf(&docs, &mut writer, 16).unwrap();

        for row in &writer.rows {
            let magnitude: f32 = row.values.iter().map(|v| v * v).sum::<f32>().sqrt();
            // L2 normalized: magnitude is 0 (all-zero) or ~1.0
            assert!(
                magnitude < 1e-6 || (magnitude - 1.0).abs() < 1e-4,
                "Row not normalized: magnitude = {magnitude}"
            );
        }
    }

    #[test]
    fn test_freeze_vocab_empty() {
        let (vocab, idf) = freeze_vocab_idf(&HashMap::new(), 0, 32);
        assert!(vocab.is_empty());
        assert!(idf.is_empty());
    }

    #[test]
    fn test_compute_single_row_zero_tokens() {
        let doc = TfidfDoc {
            doc_id: "x".into(),
            tokens: vec![],
        };
        let row = compute_tfidf_row(&doc, &["a".into()], &[1.0], 1);
        assert_eq!(row, vec![0.0]);
    }

    #[test]
    fn test_streaming_tfidf_empty_docs() {
        let mut writer = VecTfidfRowWriter::new();
        let stats = streaming_tfidf(&[], &mut writer, 32).unwrap();
        assert_eq!(stats.doc_count, 0);
        assert!(writer.rows.is_empty());
    }

    #[test]
    fn test_tfidf_rows_roundtrip() {
        let rows = vec![
            TfidfRow {
                doc_id: "n1".into(),
                values: vec![1.0, 0.5, 0.0],
            },
            TfidfRow {
                doc_id: "n2".into(),
                values: vec![0.0, 0.3, 0.9],
            },
        ];
        let bytes = serialize_tfidf_rows(&rows).unwrap();
        let back = deserialize_tfidf_rows(&bytes).unwrap();
        assert_eq!(back, rows);
    }

    #[test]
    fn test_tokenize_code_matches_production_acronym_and_digit_boundaries() {
        for text in ["HTTP2Connection", "getUserName", "parseJSONResponse"] {
            assert_eq!(
                tokenize_code(text),
                index_builder::tokenize_code(text),
                "streaming TF-IDF tokenizer must match production for {text}"
            );
        }
    }

    #[test]
    fn test_streaming_vocab_and_rows_match_production_embedder() {
        let texts = [
            "HTTP2Connection parses UserName and JSONResponse values",
            "getUserName formats HTTPConnection errors",
            "JSON2Parser handles UserName and requestID tokens",
        ];
        let docs: Vec<TfidfDoc> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| TfidfDoc {
                doc_id: format!("doc{i}"),
                tokens: index_builder::tokenize_code(text),
            })
            .collect();
        let (df, n_docs) = build_document_frequencies(&docs);
        let (stream_vocab, stream_idf) = freeze_vocab_idf(
            &df,
            n_docs,
            crate::search::search::DEFAULT_EMBEDDING_DIMENSION,
        );
        let production_docs: Vec<(String, Vec<String>)> = docs
            .iter()
            .map(|doc| (doc.doc_id.clone(), doc.tokens.clone()))
            .collect();
        let production = index_builder::TfIdfEmbedder::build_from_tokens(&production_docs);

        assert_eq!(stream_vocab, production.vocab);
        assert_eq!(stream_idf, production.idf);
        for doc in &docs {
            assert_eq!(
                compute_tfidf_row(
                    doc,
                    &stream_vocab,
                    &stream_idf,
                    crate::search::search::DEFAULT_EMBEDDING_DIMENSION,
                ),
                production.embed_tokens(&doc.tokens),
                "streaming TF-IDF row must be bit-identical for {}",
                doc.doc_id
            );
        }
    }

    #[test]
    fn test_tokenize_code_basic() {
        let tokens = tokenize_code("fn myFunction()");
        assert!(tokens.contains(&"fn".into()));
        assert!(tokens.contains(&"my".into()));
        assert!(tokens.contains(&"function".into()));
    }
}
