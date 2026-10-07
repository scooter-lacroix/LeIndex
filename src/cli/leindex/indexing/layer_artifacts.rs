//! Step 6 D2: restore the search artifacts of a leased generation from its CAS
//! layers instead of from a per-generation file mirror.
//!
//! The legacy decoders (`load_snapshot_engine` and the `try_load_*` family)
//! read fixed file names from a directory. Rather than fork them, the layers
//! are materialized under those names into the snapshot's own temp directory,
//! so the generation read path runs the *same* decode and freshness checks as
//! the mutable-root path:
//!
//! - `Search` / `Embedder` layers are the verbatim `search_snapshot.bin` /
//!   `tfidf_embedder.bin` bytes.
//! - `Tfidf` is sparse `(doc, term, value)` keyed by the Pdg layer's node
//!   order; it is densified back into the `LIEE` matrix, zero-filling the
//!   all-zero documents the encoder omitted (lossless: only `0.0` is dropped).
//! - `Neural` carries a per-row node-id table; rows are re-keyed by node id.
//! - `Fragments` is the `LIDX-FRG1` bundle of the four fragment artifacts.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use tracing::info;

use crate::search::search::SearchSnapshot;
use crate::storage::generation::migrate::{FRAGMENT_BUNDLE_FILES, decode_fragment_bundle};
use crate::storage::generation::{GenerationSnapshot, LayerKind, PdgReader};

/// Written last: its presence means the materialization completed.
const COMPLETION_MARKER: &str = "search_snapshot.bin";

/// Materialize the search layers of `snapshot` into
/// [`GenerationSnapshot::artifact_dir`]. Idempotent. A generation that lacks
/// the `Search` or `Embedder` layer (docs-only or pre-layer store) writes
/// nothing, which leaves hydration on its rebuild-from-graph path.
pub(super) fn materialize_search_artifacts(snapshot: &GenerationSnapshot) -> Result<()> {
    let dir = snapshot.artifact_dir();
    if dir.join(COMPLETION_MARKER).is_file() {
        return Ok(());
    }
    let (Some(search), Some(embedder)) = (
        snapshot.layer_bytes(LayerKind::Search)?,
        snapshot.layer_bytes(LayerKind::Embedder)?,
    ) else {
        return Ok(());
    };
    let parsed: SearchSnapshot =
        bincode::deserialize(&search).context("decode Search layer snapshot")?;
    let pdg = snapshot
        .pdg()
        .context("generation has no Pdg layer to key the vector layers by")?;
    let node_ids = pdg_node_ids(pdg)?;

    write_tfidf_matrix(snapshot, &parsed, &node_ids, dir)?;
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    write_neural_matrix(snapshot, &node_ids, dir)?;
    write_fragment_files(snapshot, dir)?;
    std::fs::write(dir.join("tfidf_embedder.bin"), embedder).context("write Embedder layer")?;
    std::fs::write(dir.join(COMPLETION_MARKER), search).context("write Search layer")?;
    info!(
        generation = snapshot.generation(),
        nodes = parsed.indexed_nodes,
        "Materialized search artifacts from generation layers"
    );
    Ok(())
}

/// Node id strings in layer order: the vector layers address documents by the
/// position of the node in the (id-sorted) Pdg layer.
fn pdg_node_ids(pdg: &PdgReader) -> Result<Vec<String>> {
    (0..pdg.num_nodes())
        .map(|index| {
            let node = pdg.node_full(index)?;
            pdg.resolve_string(node.node_id)
                .map(str::to_owned)
                .with_context(|| format!("Pdg layer node {index} has no interned id"))
        })
        .collect()
}

/// Rebuild `embeddings.bin` (one dense row per snapshot node, in snapshot
/// order) from the sparse Tfidf layer.
fn write_tfidf_matrix(
    snapshot: &GenerationSnapshot,
    parsed: &SearchSnapshot,
    node_ids: &[String],
    dir: &Path,
) -> Result<()> {
    let tfidf = snapshot.tfidf().context("generation has no Tfidf layer")?;
    let dimension = tfidf.num_terms();
    if dimension == 0 {
        bail!("Tfidf layer is empty; nothing to densify");
    }
    let doc_of: HashMap<&str, u32> = node_ids
        .iter()
        .enumerate()
        .map(|(doc, id)| (id.as_str(), doc as u32))
        .collect();

    let mut dense: HashMap<u32, Vec<f32>> = HashMap::new();
    for entry in tfidf.entries() {
        let term = entry.term_id as usize;
        if term >= dimension {
            bail!("Tfidf term {term} outside dimension {dimension}");
        }
        dense
            .entry(entry.doc_id)
            .or_insert_with(|| vec![0.0; dimension])[term] = entry.value;
    }

    let mut rows = Vec::with_capacity(parsed.nodes.len());
    for node in &parsed.nodes {
        let doc = doc_of
            .get(node.node_id.as_str())
            .with_context(|| format!("snapshot node {} is not in the Pdg layer", node.node_id))?;
        let row = dense.remove(doc).unwrap_or_else(|| vec![0.0; dimension]);
        rows.push((node.node_id.clone(), row));
    }
    crate::search::vector::write_mmap_embeddings(&dir.join("embeddings.bin"), &rows)
        .map_err(|error| anyhow::anyhow!("write Tfidf matrix: {error}"))
}

/// Rebuild `neural_embeddings.bin` from the Neural layer. A layer with no
/// vectors (no neural model ran) writes nothing; an INT8 or id-less (v1)
/// layer is skipped too — the engine then serves TF-IDF only, as it does when
/// the neural file is absent.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn write_neural_matrix(
    snapshot: &GenerationSnapshot,
    node_ids: &[String],
    dir: &Path,
) -> Result<()> {
    use crate::storage::generation::{NeuralDtype, manifest_has_neural_vectors};

    if !manifest_has_neural_vectors(snapshot.manifest()) {
        return Ok(());
    }
    let Some(neural) = snapshot.neural() else {
        return Ok(());
    };
    if neural.dtype() != NeuralDtype::F32 {
        tracing::warn!("Neural layer is not F32; semantic retrieval disabled for this hydration");
        return Ok(());
    }
    let mut rows = Vec::with_capacity(neural.count());
    for row in 0..neural.count() {
        let Some(node) = neural.node_id(row) else {
            tracing::warn!("Neural layer has no node-id table; semantic retrieval disabled");
            return Ok(());
        };
        let id = node_ids
            .get(node as usize)
            .with_context(|| format!("Neural row {row} references unknown node {node}"))?;
        rows.push((id.clone(), neural.vector(row).as_f32_slice().to_vec()));
    }
    crate::search::vector::write_mmap_embeddings(&dir.join("neural_embeddings.bin"), &rows)
        .map_err(|error| anyhow::anyhow!("write Neural matrix: {error}"))
}

/// Unpack the Fragments bundle. Entry names are checked against the closed
/// set the encoder writes, so a corrupt bundle cannot write outside `dir`.
fn write_fragment_files(snapshot: &GenerationSnapshot, dir: &Path) -> Result<()> {
    let Some(bundle) = snapshot.layer_bytes(LayerKind::Fragments)? else {
        return Ok(());
    };
    for (name, bytes) in decode_fragment_bundle(&bundle)? {
        if !FRAGMENT_BUNDLE_FILES.contains(&name.as_str()) {
            bail!("unexpected fragment bundle entry {name:?}");
        }
        std::fs::write(dir.join(&name), bytes)
            .with_context(|| format!("write fragment artifact {name}"))?;
    }
    Ok(())
}
