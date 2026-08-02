//! Search snapshot serialization + hydration.
//!
//! Extracted from `mod.rs` (CCN-19 + file-split item from the Codex review) to
//! keep the core search module under 2000 lines and to lower the cyclomatic
//! complexity of `restore_from_search_snapshot` below the lizard threshold
//! (15) via helper extraction. Behavior is byte-identical to the pre-split
//! implementation — validation, node hydration, and the neural/fragment mmap
//! index installation are each factored into a single-purpose helper.
//!
//! Storage-gated: the snapshot persistence + hydration APIs are consumed by
//! `src/cli/` (`persist_search_snapshot` / `try_hydrate_from_snapshot`) and the
//! structs are gated behind the `storage` feature (cli implies storage, so the
//! strict feature-DAG rule — no `cli` symbols in `search` — is preserved).
//! Under `--features onnx` without `storage` the module is not compiled.

use std::sync::Arc;

use super::*;
use crate::search::vector::MmapEmbeddingIndex;

/// Cli-only snapshot format version (persistence lives in `src/cli/`).
const SEARCH_SNAPSHOT_VERSION: u32 = 1;

/// Output dimension of the bundled Qwen3 embedding model (neural + fragment
/// mmap hydration validation).
pub(super) const NEURAL_EMBEDDING_DIMENSION: usize = 1024;

impl SearchEngine {
    /// Create a compact persisted metadata snapshot for fast cold-start load.
    ///
    /// Storage-gated: consumed by `index_builder::persist_search_snapshot`
    /// (cli implies storage; gating on `storage` keeps cli symbols out of
    /// `search` per the strict feature-DAG rule).
    pub(crate) fn search_snapshot(
        &self,
        pdg_nodes: usize,
        pdg_edges: usize,
        pdg_fingerprint: String,
    ) -> SearchSnapshot {
        let nodes = self
            .nodes
            .iter()
            .map(|node| {
                let mut tokens: Vec<String> = self
                    .node_tokens
                    .get(&node.node_id)
                    .map(|set| set.iter().cloned().collect())
                    .unwrap_or_default();
                tokens.sort();

                SearchSnapshotNode {
                    node_id: node.node_id.clone(),
                    file_path: node.file_path.clone(),
                    symbol_name: node.symbol_name.clone(),
                    language: node.language.clone(),
                    byte_range: node.byte_range,
                    complexity: node.complexity,
                    signature: node.signature.clone(),
                    tokens,
                }
            })
            .collect();

        SearchSnapshot {
            version: SEARCH_SNAPSHOT_VERSION,
            pdg_nodes,
            pdg_edges,
            pdg_fingerprint,
            indexed_nodes: self.nodes.len(),
            nodes,
            // Fragment layer (Task 5): rows come from the hydrated fragment
            // index; the root hash is filled by the cli-side
            // `persist_search_snapshot` (the search crate cannot read the
            // fragment_root.bin artifact — dependency direction is cli -> search).
            fragment_root_hash: None,
            fragment_rows: self
                .fragment_vector_index
                .as_ref()
                .map(|idx| idx.len() as u32)
                .unwrap_or(0),
        }
    }

    /// Hydrate the search engine from persisted metadata plus mmap embeddings.
    ///
    /// This preserves the same resident structures built by `append_nodes`
    /// without rereading source files or recomputing TF-IDF/neural embeddings.
    ///
    /// Storage-gated: consumed by `LeIndex::try_hydrate_from_snapshot` (cli
    /// implies storage; gating on `storage` keeps cli symbols out of `search`).
    pub(crate) fn restore_from_search_snapshot(
        &mut self,
        snapshot: SearchSnapshot,
        tfidf_mmap: Arc<MmapEmbeddingIndex>,
        neural_mmap: Option<Arc<MmapEmbeddingIndex>>,
        fragment_mmap: Option<Arc<MmapEmbeddingIndex>>,
        fragment_ids: Option<&[String]>,
    ) -> Result<usize, String> {
        validate_snapshot_shape(&snapshot, &tfidf_mmap, neural_mmap.as_ref())?;
        let nodes = hydrate_snapshot_nodes(&snapshot, &tfidf_mmap)?;

        let preserved_neural_weight = self.neural_weight;
        let preserved_fragment_enabled = self.fragment_index_enabled;
        let preserved_fragment_weight = self.fragment_weight;
        let preserved_fragment_refs = self.fragment_refs.clone();
        let mut staged = SearchEngine::new();
        staged.append_nodes(nodes);
        let node_ids = staged
            .nodes
            .iter()
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>();
        staged.vector_index = VectorIndexImpl::Mmap(
            MmapVectorIndex::from_snapshot(tfidf_mmap, &node_ids)
                .map_err(|error| format!("failed to build mmap vector index: {error}"))?,
        );
        if let Some(mmap) = neural_mmap.as_ref() {
            Self::install_neural_vector_index(&mut staged, mmap, &node_ids);
        }
        Self::install_fragment_vector_index(
            &mut staged,
            fragment_mmap.as_ref(),
            fragment_ids,
            snapshot.fragment_rows,
        );

        if staged.nodes.len() != snapshot.indexed_nodes {
            return Err(format!(
                "hydrated node count {} != snapshot indexed_nodes {}",
                staged.nodes.len(),
                snapshot.indexed_nodes
            ));
        }
        if staged.vector_index.len() != snapshot.indexed_nodes {
            return Err(format!(
                "hydrated vector count {} != snapshot indexed_nodes {}",
                staged.vector_index.len(),
                snapshot.indexed_nodes
            ));
        }

        staged.neural_weight = preserved_neural_weight;
        staged.fragment_index_enabled = preserved_fragment_enabled;
        staged.fragment_weight = preserved_fragment_weight;
        staged.fragment_refs = preserved_fragment_refs;
        *self = staged;
        Ok(self.nodes.len())
    }

    /// Build the lazy-paged neural ANN — reuses MmapVectorIndex, the SAME
    /// pattern as the tfidf index above. This is the semantic RETRIEVAL
    /// signal: its top-K hits are unioned into the candidate pool at query
    /// time (replaces the brute-force neural scan, which duplicated this
    /// `.search()` logic). Failure is non-fatal: semantic retrieval just won't
    /// contribute.
    fn install_neural_vector_index(
        staged: &mut SearchEngine,
        mmap: &Arc<MmapEmbeddingIndex>,
        node_ids: &[String],
    ) {
        match MmapVectorIndex::from_snapshot(std::sync::Arc::clone(mmap), node_ids) {
            Ok(idx) => staged.neural_vector_index = Some(VectorIndexImpl::Mmap(idx)),
            Err(error) => tracing::warn!(
                error = %error,
                "failed to build neural mmap vector index; semantic retrieval disabled"
            ),
        }
    }

    /// Install the fragment vector index (Task 5, invariant 8): row count must
    /// match the snapshot's recorded fragment_rows. Failure is NON-fatal — the
    /// fragment layer must never block the node-level path (invariant 3); a
    /// mismatch simply disables fragment retrieval, leaving the node index
    /// fully hydrated.
    fn install_fragment_vector_index(
        staged: &mut SearchEngine,
        fragment_mmap: Option<&Arc<MmapEmbeddingIndex>>,
        fragment_ids: Option<&[String]>,
        snapshot_rows: u32,
    ) {
        let (Some(mmap), Some(ids)) = (fragment_mmap, fragment_ids) else {
            return;
        };
        if mmap.len() as u32 != snapshot_rows {
            tracing::warn!(
                rows = mmap.len(),
                snapshot_rows,
                "fragment mmap row count != snapshot; fragment retrieval disabled"
            );
            return;
        }
        if ids.len() as u32 != snapshot_rows {
            tracing::warn!(
                ids = ids.len(),
                snapshot_rows,
                "fragment id list count != snapshot; fragment retrieval disabled"
            );
            return;
        }
        if mmap.dimension() as usize != NEURAL_EMBEDDING_DIMENSION {
            tracing::warn!(
                dim = mmap.dimension(),
                expected = NEURAL_EMBEDDING_DIMENSION,
                "fragment mmap dimension mismatch; fragment retrieval disabled"
            );
            return;
        }
        match MmapVectorIndex::from_snapshot(std::sync::Arc::clone(mmap), ids) {
            Ok(idx) => staged.fragment_vector_index = Some(VectorIndexImpl::Mmap(idx)),
            Err(error) => tracing::warn!(
                error = %error,
                "failed to build fragment mmap vector index; fragment retrieval disabled"
            ),
        }
    }
}

/// Validate snapshot version, node counts, and mmap shape before hydration.
/// Returns the first inconsistency as an error string.
fn validate_snapshot_shape(
    snapshot: &SearchSnapshot,
    tfidf_mmap: &MmapEmbeddingIndex,
    neural_mmap: Option<&Arc<MmapEmbeddingIndex>>,
) -> Result<(), String> {
    if snapshot.version != SEARCH_SNAPSHOT_VERSION {
        return Err(format!(
            "unsupported search snapshot version {}",
            snapshot.version
        ));
    }
    if snapshot.indexed_nodes != snapshot.nodes.len() {
        return Err(format!(
            "snapshot indexed_nodes {} != node metadata count {}",
            snapshot.indexed_nodes,
            snapshot.nodes.len()
        ));
    }
    if tfidf_mmap.len() != snapshot.indexed_nodes {
        return Err(format!(
            "TF-IDF mmap row count {} != snapshot indexed_nodes {}",
            tfidf_mmap.len(),
            snapshot.indexed_nodes
        ));
    }
    if tfidf_mmap.dimension() as usize != DEFAULT_EMBEDDING_DIMENSION {
        return Err(format!(
            "TF-IDF mmap dimension {} != expected {}",
            tfidf_mmap.dimension(),
            DEFAULT_EMBEDDING_DIMENSION
        ));
    }
    if let Some(mmap) = neural_mmap {
        if mmap.dimension() as usize != NEURAL_EMBEDDING_DIMENSION {
            return Err(format!(
                "neural mmap dimension {} != expected {}",
                mmap.dimension(),
                NEURAL_EMBEDDING_DIMENSION
            ));
        }
    }
    Ok(())
}

/// Build hydrated `NodeInfo` rows from the snapshot, skipping any node that
/// lacks a TF-IDF mmap row. Errors if any node is missing its embedding
/// (a structural inconsistency — the snapshot would be unusable).
fn hydrate_snapshot_nodes(
    snapshot: &SearchSnapshot,
    tfidf_mmap: &MmapEmbeddingIndex,
) -> Result<Vec<NodeInfo>, String> {
    let mut nodes = Vec::with_capacity(snapshot.nodes.len());
    let mut missing_tfidf = 0usize;
    for snap in &snapshot.nodes {
        if tfidf_mmap.find_node_row(&snap.node_id).is_none() {
            missing_tfidf += 1;
            continue;
        }

        nodes.push(NodeInfo {
            node_id: snap.node_id.clone(),
            file_path: snap.file_path.clone(),
            symbol_name: snap.symbol_name.clone(),
            language: snap.language.clone(),
            content: String::new(),
            byte_range: snap.byte_range,
            // Base vectors remain in the mmap-backed vector index below;
            // keeping empty per-node vectors avoids a second heap mirror.
            tfidf_embedding: Vec::new(),
            neural_embedding: None,
            complexity: snap.complexity,
            signature: snap.signature.clone(),
            pre_tokenized: Some(snap.tokens.clone()),
        });
    }
    if missing_tfidf > 0 {
        return Err(format!(
            "snapshot missing {} TF-IDF embedding record(s)",
            missing_tfidf
        ));
    }
    Ok(nodes)
}
