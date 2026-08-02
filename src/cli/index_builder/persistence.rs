//! Mmap + snapshot persistence for the index builder (extracted from `mod.rs`
//! to keep the parent module under the large-file gate).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::graph::pdg::ProgramDependenceGraph;
use crate::search::search::{SearchEngine, SearchSnapshot};

use super::fragment;

// ============================================================================
// MMAP EMBEDDING PERSISTENCE (R10)
// ============================================================================

/// Persist all embeddings from the search engine to an mmap-backed binary file.
///
/// After indexing completes, call this to write a `.leindex/embeddings.bin`
/// file that can be memory-mapped for fast read-only access without loading
/// the full embedding matrix into heap memory.
pub(crate) fn persist_embeddings_to_mmap(
    search_engine: &SearchEngine,
    project_path: &Path,
) -> Result<()> {
    let path = crate::search::vector::mmap_embeddings_path(project_path);
    if search_engine.is_mmap_backed() && path.is_file() {
        return Ok(());
    }
    let embeddings = search_engine.collect_embeddings();
    if embeddings.is_empty() {
        return Ok(());
    }
    crate::search::vector::write_mmap_embeddings(&path, &embeddings)
        .map_err(|e| anyhow::anyhow!("Failed to write mmap embeddings: {e}"))?;
    info!(
        count = embeddings.len(),
        path = %path.display(),
        "Persisted embeddings to mmap file"
    );
    Ok(())
}

fn search_snapshot_path(project_path: &Path) -> PathBuf {
    project_path.join(".leindex").join("search_snapshot.bin")
}

/// Persist search metadata required for fast load_from_storage hydration.
pub(crate) fn persist_search_snapshot(
    search_engine: &SearchEngine,
    project_path: &Path,
    pdg_nodes: usize,
    pdg_edges: usize,
    pdg_fingerprint: String,
) -> Result<()> {
    let mut snapshot = search_engine.search_snapshot(pdg_nodes, pdg_edges, pdg_fingerprint);
    if snapshot.indexed_nodes == 0 {
        return Ok(());
    }

    // Fragment layer (Task 5): persist the fragment embedding matrix + root
    // hash alongside the snapshot so cold-start hydration can rebuild the
    // fragment vector index and validate invariant 8 (root-hash + row count).
    // Task 7: the mmap is written FIRST and the root LAST (the root is the
    // commit marker — a crash mid-persist leaves a mismatched/older root that
    // hydration rejects via `fragment_layer_generation_is_consistent`, so a
    // half-synced fragment tree never serves). The root generation is taken
    // from the sync manifest so `fragment_root.bin` and
    // `fragment_sync_manifest.bin` stay generation-aligned.
    let fragment_embeddings = search_engine.collect_fragment_embeddings();
    persist_fragment_embeddings_to_mmap(project_path, &fragment_embeddings)?;
    if !fragment_embeddings.is_empty() {
        let fragment_ids: Vec<String> = fragment_embeddings
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        snapshot.fragment_root_hash = Some(fragment::sync::compute_fragment_root_hash_from_ids(
            &fragment_ids,
        ));
        // CRITICAL: the manifest lives in `.leindex/`, so the lookup must use
        // the storage dir (like `load_fragment_root`) — using `project_path`
        // directly reads the wrong path, falls back to generation 0, and would
        // clobber the engine's gen-N root, permanently disabling the layer.
        let generation =
            fragment::sync::load_fragment_sync_manifest(&project_path.join(".leindex"))
                .ok()
                .flatten()
                .map(|manifest| manifest.generation)
                .unwrap_or(0);
        fragment::sync::persist_fragment_root_from_ids(project_path, &fragment_ids, generation)?;
    }

    let path = search_snapshot_path(project_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create search snapshot directory: {}",
                parent.display()
            )
        })?;
    }

    let bytes = bincode::serialize(&snapshot).context("Failed to serialize search snapshot")?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("Failed to write search snapshot: {}", path.display()))?;
    info!(
        count = snapshot.indexed_nodes,
        path = %path.display(),
        "Persisted search snapshot"
    );
    Ok(())
}

/// Try to load search metadata from an explicit storage directory.
pub(crate) fn try_load_search_snapshot_from_storage(storage_path: &Path) -> Option<SearchSnapshot> {
    let path = storage_path.join("search_snapshot.bin");
    if !path.exists() {
        return None;
    }

    match std::fs::read(&path)
        .with_context(|| format!("Failed to read search snapshot: {}", path.display()))
        .and_then(|bytes| {
            bincode::deserialize::<SearchSnapshot>(&bytes)
                .context("Failed to deserialize search snapshot")
        }) {
        Ok(snapshot) => {
            info!(
                count = snapshot.indexed_nodes,
                path = %path.display(),
                "Loaded search snapshot"
            );
            Some(snapshot)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to load search snapshot"
            );
            None
        }
    }
}

struct Blake3FormatWriter<'a>(&'a mut blake3::Hasher);

impl std::fmt::Write for Blake3FormatWriter<'_> {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.0.update(value.as_bytes());
        Ok(())
    }
}

/// Stable fingerprint of the PDG state that materially affects search
/// hydration. This prevents a snapshot produced for one PDG from being reused
/// after storage changes that happen to preserve node/edge counts.
pub(crate) fn pdg_search_fingerprint(pdg: &ProgramDependenceGraph) -> String {
    use std::fmt::Write as _;

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leindex-pdg-search-v2");

    let mut nodes: Vec<[u8; 32]> = pdg
        .node_indices()
        .filter_map(|node_idx| {
            pdg.get_node(node_idx).map(|node| {
                let mut record = blake3::Hasher::new();
                write!(
                    Blake3FormatWriter(&mut record),
                    "{}\0{:?}\0{}\0{}\0{}\0{}\0{}\0{}",
                    node.id,
                    node.node_type,
                    node.name,
                    node.file_path,
                    node.byte_range.0,
                    node.byte_range.1,
                    node.complexity,
                    node.language
                )
                .expect("hash writer is infallible");
                *record.finalize().as_bytes()
            })
        })
        .collect();
    nodes.sort_unstable();
    for node in nodes {
        hasher.update(&node);
    }

    let mut edges: Vec<[u8; 32]> = pdg
        .edge_indices()
        .filter_map(|edge_idx| {
            let edge = pdg.get_edge(edge_idx)?;
            let (from, to) = pdg.edge_endpoints(edge_idx)?;
            let from = pdg.get_node(from)?;
            let to = pdg.get_node(to)?;
            let mut record = blake3::Hasher::new();
            write!(
                Blake3FormatWriter(&mut record),
                "{}\0{}\0{:?}\0{:?}\0{:?}\0{:?}",
                from.id,
                to.id,
                edge.edge_type,
                edge.metadata.call_count,
                edge.metadata.variable_name,
                edge.metadata.confidence.map(f32::to_bits)
            )
            .expect("hash writer is infallible");
            Some(*record.finalize().as_bytes())
        })
        .collect();
    edges.sort_unstable();
    for edge in edges {
        hasher.update(&edge);
    }

    hasher.finalize().to_hex().to_string()
}

/// Persist neural embeddings to a separate mmap file for fast load_from_storage.
///
/// This stores the ONNX neural embeddings (1024-dim) separately from the
/// TF-IDF embeddings (768-dim) so they can be restored without re-computing
/// ONNX inference for all nodes.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn persist_neural_embeddings_to_mmap(
    search_engine: &SearchEngine,
    project_path: &Path,
) -> Result<()> {
    let embeddings = search_engine.collect_neural_embeddings();
    let path = neural_mmap_embeddings_path(project_path);
    if embeddings.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                anyhow::anyhow!("Failed to remove stale neural mmap embeddings: {e}")
            })?;
            info!(
                path = %path.display(),
                "Removed stale neural embeddings mmap file"
            );
        }
        return Ok(());
    }
    crate::search::vector::write_mmap_embeddings(&path, &embeddings)
        .map_err(|e| anyhow::anyhow!("Failed to write neural mmap embeddings: {e}"))?;
    info!(
        count = embeddings.len(),
        path = %path.display(),
        "Persisted neural embeddings to mmap file"
    );
    Ok(())
}

/// Path for the neural embeddings mmap file.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn neural_mmap_embeddings_path(project_path: &Path) -> PathBuf {
    project_path.join(".leindex").join("neural_embeddings.bin")
}

/// Try to load previously persisted neural embeddings from mmap file.
///
/// Returns `None` if the file does not exist or is corrupt.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn try_load_neural_mmap_embeddings(
    project_path: &Path,
) -> Option<crate::search::vector::MmapEmbeddingIndex> {
    try_load_neural_mmap_embeddings_from_storage(&project_path.join(".leindex"))
}

/// Try to load neural embeddings from an explicit storage directory.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn try_load_neural_mmap_embeddings_from_storage(
    storage_path: &Path,
) -> Option<crate::search::vector::MmapEmbeddingIndex> {
    let path = storage_path.join("neural_embeddings.bin");
    if !path.exists() {
        return None;
    }
    match crate::search::vector::MmapEmbeddingIndex::open(&path) {
        Ok(index) => {
            info!(
                nodes = index.len(),
                dim = index.dimension(),
                path = %path.display(),
                "Loaded neural mmap embedding index"
            );
            Some(index)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to load neural mmap embedding index"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Fragment embeddings mmap twins (fragment-embeddings 1.11.0 Task 4)
// ---------------------------------------------------------------------------
//
// Structural twins of the neural mmap path (`neural_embeddings.bin` →
// `MmapEmbeddingIndex`). Fragment embeddings are content-hash-addressed: the
// IDs written are blake3 content hashes of the enriched fragment text, so a
// row in `fragments_embeddings.bin` maps 1:1 to a `FragmentStore` row key.
// Additive-only (invariant 4): never mutates `embeddings.bin`/
// `neural_embeddings.bin`.
//
// Fragment persistence twins (Task 4) — now live: `persist_search_snapshot`
// persists the fragment mmap + root (Task 5) and `indexing/load.rs` loads the
// mmap for hydration.

/// Path for the fragment embeddings mmap file.
pub(crate) fn fragment_mmap_embeddings_path(project_path: &Path) -> PathBuf {
    project_path
        .join(".leindex")
        .join("fragments_embeddings.bin")
}

/// Persist fragment embeddings to a separate mmap file for fast hydration.
///
/// Mirrors `persist_neural_embeddings_to_mmap`: an empty input removes a stale
/// file (feature-off leaves no orphan artifact); otherwise writes the matrix
/// via `write_mmap_embeddings`. The `embeddings` slice is `(content_hash,
/// Vec<f32>)` pairs collected by Task 5's `SearchEngine::collect_fragment_embeddings`
/// (or its fragment-store fallback); passing them in keeps this task free of a
/// `SearchEngine` dependency that does not exist until Task 5 adds
/// `fragment_vector_index`.
pub(crate) fn persist_fragment_embeddings_to_mmap(
    project_path: &Path,
    embeddings: &[(String, Vec<f32>)],
) -> Result<()> {
    let path = fragment_mmap_embeddings_path(project_path);
    if embeddings.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                anyhow::anyhow!("Failed to remove stale fragment mmap embeddings: {e}")
            })?;
            info!(
                path = %path.display(),
                "Removed stale fragment embeddings mmap file"
            );
        }
        return Ok(());
    }
    crate::search::vector::write_mmap_embeddings(&path, embeddings)
        .map_err(|e| anyhow::anyhow!("Failed to write fragment mmap embeddings: {e}"))?;
    info!(
        count = embeddings.len(),
        path = %path.display(),
        "Persisted fragment embeddings to mmap file"
    );
    Ok(())
}

/// Try to load previously persisted fragment embeddings from an explicit
/// storage directory (`.leindex/fragments_embeddings.bin`).
///
/// Returns `None` when the file does not exist or is corrupt (mirrors the
/// neural loader's warn-and-continue behavior).
pub(crate) fn try_load_fragment_mmap_embeddings_from_storage(
    storage_path: &Path,
) -> Option<crate::search::vector::MmapEmbeddingIndex> {
    let path = storage_path.join("fragments_embeddings.bin");
    if !path.exists() {
        return None;
    }
    match crate::search::vector::MmapEmbeddingIndex::open(&path) {
        Ok(index) => {
            info!(
                nodes = index.len(),
                dim = index.dimension(),
                path = %path.display(),
                "Loaded fragment mmap embedding index"
            );
            Some(index)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to load fragment mmap embedding index"
            );
            None
        }
    }
}

/// Validate the persisted fragment layer against the snapshot (invariant 8).
///
/// The snapshot records the fragment root hash (filled at persist time by
/// `persist_search_snapshot`); `fragment_root.bin` must carry the SAME hash
/// and a row count matching the fragment mmap. Any mismatch means the fragment
/// artifacts are stale relative to the snapshot — callers must disable the
/// fragment layer (never block the node-level path, invariant 3).
pub(crate) fn fragment_layer_is_valid(
    snapshot_root: Option<&str>,
    fragment_mmap: Option<&crate::search::vector::MmapEmbeddingIndex>,
    storage_path: &Path,
) -> bool {
    let (Some(root), Some(mmap)) = (snapshot_root, fragment_mmap) else {
        return false;
    };
    match fragment::sync::load_fragment_root(storage_path) {
        Ok(Some(state)) => {
            // Task 7: refuse a HALF-SYNCED fragment tree. The manifest + store +
            // root are written together under one bumped generation; a
            // mid-build crash leaves an older root (generation mismatch) which
            // must not serve — the caller falls back to the last complete root
            // (i.e. the fragment layer stays off). Legacy Task 5/6 artifacts
            // with no manifest are accepted.
            state.root_hash == root
                && state.fragment_rows == mmap.len() as u64
                && fragment::sync::fragment_layer_generation_is_consistent(storage_path, &state)
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to load fragment root for validation; fragment layer disabled"
            );
            false
        }
    }
}
