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
    // Skip only when the on-disk mmap already describes the engine's current
    // row population. The old check (mmap-backed + file exists) skipped
    // FOREVER after the first write: an incremental reindex on a
    // mmap-hydrated engine grew the snapshot but never the mmap file, so
    // every later hydration failed the "mmap row count != snapshot
    // indexed_nodes" validation and took the full rebuild path.
    if search_engine.is_mmap_backed() && path.is_file() {
        let engine_rows = search_engine.node_count();
        let file_rows = crate::search::vector::MmapEmbeddingIndex::open(&path)
            .map(|index| index.len())
            .unwrap_or(usize::MAX);
        if file_rows == engine_rows {
            return Ok(());
        }
        info!(
            file_rows,
            engine_rows, "Rewriting mmap embeddings after row-count drift"
        );
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

/// The (nodes, edges, fingerprint) identity of a PDG as it will be
/// RECONSTRUCTED from the published Pdg layer.
///
/// Snapshot and embedder freshness metadata must describe the persisted
/// graph, not the raw in-memory one: the in-memory graph may legitimately
/// hold several nodes sharing one `node_id` (external/import duplicates) and
/// parallel same-type edges, which the layer format collapses (last-wins,
/// matching the legacy SQL upserts). Identity recorded from the uncollapsed
/// graph can therefore never match a layer-hydrated graph — every cold
/// hydration would take the full TF-IDF rebuild path forever. Deriving the
/// identity through the same collapse the layer round-trip applies makes
/// save/load agreement structural instead of coincidental.
pub(crate) fn persisted_search_identity_from_graph(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
) -> (usize, usize, String) {
    let mut collapsed = crate::storage::generation::graph_codec::collapse_for_layer(pdg);
    // Mirror hydration exactly: `load_from_storage_inner_at` normalizes
    // external nodes BEFORE fingerprinting, so the identity must describe
    // the same post-normalization graph or the freshness check fails.
    super::normalize_external_nodes(&mut collapsed);
    let fingerprint = pdg_search_fingerprint(&collapsed);
    (collapsed.node_count(), collapsed.edge_count(), fingerprint)
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
    // Identity sidecar. `SearchSnapshot` carries no timestamps — it is a pure
    // function of (PDG content, indexed node set, token dictionary, fragment
    // layer). The identity is a blake3 of the serialized snapshot itself, so
    // EVERY content-bearing field is covered: the earlier multi-field
    // identity omitted the per-node `token_ids` and the token dictionary,
    // and a comment-only edit inside a function body left every one of those
    // fields unchanged (same ids, byte ranges, counts, fingerprint), so the
    // rewrite was skipped and cold hydration kept serving the pre-edit
    // tokens until an unrelated structural change bumped the fingerprint.
    // When the identity matches what is already on disk, the ~11MB rewrite
    // is byte-equivalent and can be skipped. Serializing in memory to hash
    // is far cheaper than the write + fsync it avoids, and the serialized
    // bytes are reused for the write itself.
    let sidecar_path = project_path
        .join(".leindex")
        .join("search_snapshot.identity");
    let bytes = bincode::serialize(&snapshot).context("Failed to serialize search snapshot")?;
    let identity = format!("v2:{}\n", blake3::hash(&bytes));
    if path.is_file()
        && std::fs::read_to_string(&sidecar_path).ok().as_deref() == Some(identity.as_str())
    {
        tracing::debug!(
            path = %path.display(),
            "Search snapshot unchanged; skipping rewrite"
        );
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create search snapshot directory: {}",
                parent.display()
            )
        })?;
    }

    std::fs::write(&path, &bytes)
        .with_context(|| format!("Failed to write search snapshot: {}", path.display()))?;
    std::fs::write(&sidecar_path, &identity).with_context(|| {
        format!(
            "Failed to write search snapshot identity sidecar: {}",
            sidecar_path.display()
        )
    })?;
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

    // Each record is hashed independently and the digests are sorted, so the
    // result is order-independent by construction — which makes the per-record
    // work (167k BLAKE3 hashes on a mid-size project, ~110 ms serial) safe to
    // spread across cores.
    use rayon::prelude::*;
    let node_ids: Vec<_> = pdg.node_indices().collect();
    let mut nodes: Vec<[u8; 32]> = node_ids
        .par_iter()
        .filter_map(|&node_idx| {
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
    nodes.par_sort_unstable();
    for node in nodes {
        hasher.update(&node);
    }

    let edge_ids: Vec<_> = pdg.edge_indices().collect();
    let mut edges: Vec<[u8; 32]> = edge_ids
        .par_iter()
        .filter_map(|&edge_idx| {
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
    edges.par_sort_unstable();
    for edge in edges {
        hasher.update(&edge);
    }

    hasher.finalize().to_hex().to_string()
}

/// Persist neural embeddings to mmap files for fast load_from_storage.
///
/// This stores the neural embeddings separately from the TF-IDF embeddings
/// so they can be restored without re-computing inference for all nodes.
///
/// Rows are partitioned by the Matryoshka policy: full-dimension rows go to
/// `neural_embeddings.bin` (the canonical file every reader understands),
/// sliced tiers go to per-dimension sidecar files that MRL-aware readers
/// union at query time.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn persist_neural_embeddings_to_mmap(
    search_engine: &SearchEngine,
    project_path: &Path,
    identity: Option<&str>,
    matryoshka: crate::config::MatryoshkaSetting,
) -> Result<()> {
    let embeddings = search_engine.collect_neural_embeddings();
    let path = neural_mmap_embeddings_path(project_path);
    let storage_path = project_path.join(".leindex");
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
        remove_neural_tier_mmaps(project_path, &[])?;
        // Nothing is published, so nothing may claim to be the committed
        // generation either.
        remove_neural_generation_manifest(&storage_path)?;
        return Ok(());
    }
    let spans = search_engine.collect_node_span_bytes();
    let buckets =
        crate::search::mrl::partition_rows_by_slice_dimension(&embeddings, &spans, matryoshka);
    // Off also serves providers with native dimensions other than 768. Use
    // their recorded dimension so actual-width buckets retain the canonical
    // path; shorter restored rows stay in sidecars instead of mixing widths.
    let native_dim = if matryoshka == crate::config::MatryoshkaSetting::Off {
        identity
            .and_then(|value| value.rsplit('|').next()?.parse::<usize>().ok())
            .filter(|dim| *dim > 0)
            .unwrap_or_else(|| {
                embeddings
                    .iter()
                    .map(|(_, row)| row.len())
                    .max()
                    .unwrap_or(0)
                    .max(crate::search::mrl::MRL_TIERS[3])
            })
    } else {
        crate::search::mrl::MRL_TIERS[3]
    };

    // Publish as ONE generation. The three phases below are what make a crash
    // anywhere leave a self-consistent generation on disk rather than a mix of
    // new and previous files that nothing on disk can distinguish (Codex P2).
    //
    // Stage: every tier is written to a private staging path and validated by
    // REOPENING it. Existence proves more than nothing, but weight is the real
    // check — opening the mmap validates the payload's extent and checksum, so a
    // truncated write fails here while the previous generation is still whole.
    let mut staged: Vec<StagedNeuralTier> = Vec::with_capacity(buckets.len());
    for (dim, rows) in &buckets {
        let final_path = if *dim == native_dim {
            path.clone()
        } else {
            neural_tier_mmap_embeddings_path(project_path, *dim)
        };
        let stage_path = neural_staging_path(&final_path);
        crate::search::vector::write_mmap_embeddings_with_identity(&stage_path, rows, identity)
            .map_err(|e| anyhow::anyhow!("Failed to stage neural mmap embeddings: {e}"))?;
        match crate::search::vector::MmapEmbeddingIndex::open(&stage_path) {
            Ok(written) if written.len() == rows.len() => {}
            Ok(written) => {
                return Err(anyhow::anyhow!(
                    "Staged neural mmap {} reopened with {} rows, expected {}",
                    stage_path.display(),
                    written.len(),
                    rows.len()
                ));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to reopen staged neural mmap {}: {e}",
                    stage_path.display()
                ));
            }
        }
        staged.push(StagedNeuralTier {
            dim: *dim,
            rows: rows.len(),
            final_path,
            stage_path,
        });
    }

    // Under a fixed sub-native policy (Dim512/Dim256/Dim128) every row is sliced,
    // so no bucket is native. Two things must be true afterwards:
    //   1. the stale canonical file from a previous 768-dim run must not be
    //      served as if it described this generation, and
    //   2. the generation's identity and native dimension must survive.
    // Deleting the canonical file satisfies (1) but breaks (2): it is the only
    // artifact `query_remote_embedder` raw-reads for provider identity and
    // dimension, so a tier-only generation would look absent and query
    // embedding would fall back to a different vector space than the sidecars
    // hold (Codex P1). Overwrite it with a zero-row header carrying the native
    // dimension and the identity instead: readers restore nothing from it
    // (`len() == 0`) but still resolve identity and dimension.
    //
    // Staged like the tiers so the commit phase below is the only thing that
    // touches canonical storage paths.
    let header_is_tier = staged.iter().any(|tier| tier.dim == native_dim);
    if !header_is_tier {
        let stage_path = neural_staging_path(&path);
        crate::search::vector::write_empty_mmap_embeddings_with_identity(
            &stage_path,
            native_dim as u32,
            identity,
        )
        .map_err(|e| {
            anyhow::anyhow!("Failed to stage tier-only canonical neural mmap header: {e}")
        })?;
        info!(
            path = %stage_path.display(),
            dim = native_dim,
            "Staged tier-only canonical neural mmap header (identity + native dimension, zero rows)"
        );
    }

    // Commit. Rename every staged tier onto its canonical path, then write the
    // manifest. `rename` within one filesystem is atomic and replaces the
    // destination, so each tier flips from wholly-previous to wholly-new; and
    // because the manifest lands LAST, it only ever names a generation whose
    // files are already in place. A crash during this phase cannot produce a
    // mixed generation that a reader would honour.
    for tier in &staged {
        publish_staged_tier(tier)?;
        info!(
            count = tier.rows,
            dim = tier.dim,
            path = %tier.final_path.display(),
            "Published neural embeddings mmap tier"
        );
    }
    if !header_is_tier {
        publish_staged_tier(&StagedNeuralTier {
            dim: native_dim,
            rows: 0,
            final_path: path.clone(),
            stage_path: neural_staging_path(&path),
        })?;
    }
    let committed: Vec<(usize, usize)> = staged.iter().map(|tier| (tier.dim, tier.rows)).collect();
    persist_neural_generation_manifest(&storage_path, identity, matryoshka, &committed)?;
    // Only now is the previous generation definitively superseded, so only now
    // is it safe to drop its orphans.
    let keep: Vec<usize> = staged.iter().map(|tier| tier.dim).collect();
    remove_neural_tier_mmaps(project_path, &keep)?;
    Ok(())
}

/// One tier written to a staging path, awaiting the commit rename.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
struct StagedNeuralTier {
    dim: usize,
    rows: usize,
    final_path: PathBuf,
    stage_path: PathBuf,
}

/// Sibling staging path for a tier file. Sibling (not `temp_dir`) so the commit
/// `rename` stays within one filesystem and is therefore atomic.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn neural_staging_path(final_path: &Path) -> PathBuf {
    let name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("neural");
    final_path.with_file_name(format!(".{name}.staged"))
}

/// The commit step: atomically replace the canonical tier with its staged form.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn publish_staged_tier(tier: &StagedNeuralTier) -> Result<()> {
    if let Some(parent) = tier.final_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create neural mmap directory: {}",
                parent.display()
            )
        })?;
    }
    std::fs::rename(&tier.stage_path, &tier.final_path).with_context(|| {
        format!(
            "Failed to publish staged neural mmap {} -> {}",
            tier.stage_path.display(),
            tier.final_path.display()
        )
    })?;
    Ok(())
}

/// Path for a per-dimension MRL sidecar neural embeddings mmap file.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn neural_tier_mmap_embeddings_path(project_path: &Path, dim: usize) -> PathBuf {
    project_path
        .join(".leindex")
        .join(format!("neural_embeddings_{dim}.bin"))
}

/// Committed-generation manifest for the neural tier set.
///
/// Tier files live at FIXED paths per dimension, so filenames carry no
/// generation. Publishing them one at a time means a crash can leave a mix of
/// new and previous-generation files behind, and nothing on disk says which is
/// which. This manifest is the commit point: it is written last and atomically,
/// and it names exactly the dimensions — with the identity and row count of each
/// — that the publish actually completed (Codex P2: publish all MRL tier files
/// as one generation).
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct NeuralGenerationManifest {
    schema_version: u32,
    /// Provider/model/template identity shared by every tier in the generation.
    identity: Option<String>,
    /// Matryoshka policy the generation was partitioned under.
    matryoshka: String,
    /// `(dimension, rows)` for each published tier, ascending by dimension.
    tiers: Vec<(usize, usize)>,
}

#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
const NEURAL_GENERATION_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn neural_generation_manifest_path(storage_path: &Path) -> PathBuf {
    storage_path.join("neural_generation.bin")
}

/// Atomically commit the manifest. Written LAST in the publish sequence, so a
/// crash before this call leaves the previous manifest in force and readers fall
/// back to the previous generation instead of serving a short new one.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn persist_neural_generation_manifest(
    storage_path: &Path,
    identity: Option<&str>,
    matryoshka: crate::config::MatryoshkaSetting,
    tiers: &[(usize, usize)],
) -> Result<()> {
    let mut tiers = tiers.to_vec();
    tiers.sort_unstable();
    let manifest = NeuralGenerationManifest {
        schema_version: NEURAL_GENERATION_MANIFEST_SCHEMA_VERSION,
        identity: identity.map(str::to_owned),
        matryoshka: format!("{matryoshka:?}"),
        tiers,
    };
    let path = neural_generation_manifest_path(storage_path);
    let payload = bincode::serialize(&manifest).context("Failed to serialize neural manifest")?;
    fragment::atomic_write(&path, &payload).with_context(|| {
        format!(
            "Failed to persist neural generation manifest: {}",
            path.display()
        )
    })?;
    info!(
        tiers = manifest.tiers.len(),
        path = %path.display(),
        "Committed neural generation manifest"
    );
    Ok(())
}

/// Drop the manifest (used when a publish empties the neural store entirely).
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn remove_neural_generation_manifest(storage_path: &Path) -> Result<()> {
    let path = neural_generation_manifest_path(storage_path);
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| {
            format!(
                "Failed to remove stale neural generation manifest: {}",
                path.display()
            )
        })?;
    }
    Ok(())
}

/// Load the committed-generation manifest (None when absent or schema-mismatched,
/// in which case callers fall back to per-file identity gating).
#[cfg(any(test, feature = "onnx", feature = "remote-embeddings"))]
fn try_load_neural_generation_manifest(storage_path: &Path) -> Option<NeuralGenerationManifest> {
    let path = neural_generation_manifest_path(storage_path);
    let bytes = std::fs::read(&path).ok()?;
    match bincode::deserialize::<NeuralGenerationManifest>(&bytes) {
        Ok(manifest) => {
            if manifest.schema_version != NEURAL_GENERATION_MANIFEST_SCHEMA_VERSION {
                tracing::warn!(
                    path = %path.display(),
                    found = manifest.schema_version,
                    expected = NEURAL_GENERATION_MANIFEST_SCHEMA_VERSION,
                    "Neural generation manifest schema mismatch; ignoring it and \
                     falling back to per-file identity gating"
                );
                return None;
            }
            Some(manifest)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to deserialize neural generation manifest; ignoring it"
            );
            None
        }
    }
}

/// Remove sidecar tier mmaps whose dimension is not in `keep`. `keep` may
/// contain the native dimension; its canonical file is never touched here.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn remove_neural_tier_mmaps(project_path: &Path, keep: &[usize]) -> Result<()> {
    for dim in crate::search::mrl::MRL_TIERS {
        if keep.contains(&dim) {
            continue;
        }
        let path = neural_tier_mmap_embeddings_path(project_path, dim);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| anyhow::anyhow!("Failed to remove stale neural tier mmap: {e}"))?;
            info!(
                path = %path.display(),
                "Removed stale neural tier mmap file"
            );
        }
    }
    Ok(())
}

/// Load every MRL sidecar neural mmap present in storage, ascending by
/// dimension. Missing or corrupt sidecars are skipped (non-fatal): the tier
/// simply does not contribute at query time.
///
/// # Generation filtering
///
/// Sidecars live at a FIXED path per dimension (`neural_embeddings_{dim}.bin`),
/// so the filenames carry no generation. On its own that is not enough to
/// re-persist cleanly: when a publication fails partway — or the process dies
/// between writing a tier and rewriting the canonical trailer — the previous
/// generation's sidecar survives next to a canonical file that already
/// describes the NEW one. Sliced vectors from one provider, model, dimension
/// or template are not comparable with the current query embedding, yet they
/// would be cosined against it, producing confident nonsense.
///
/// The canonical mmap trailer records the exact identity the current
/// generation was persisted under, so a sidecar is admitted only when it
/// agrees. Two cases stay permissive:
///  - `expected_identity` is `None`: the caller has no canonical generation to
///    compare against (tier-only stores whose header is absent or corrupt);
///    refusing then would silently drop every tier, so we load as before.
///  - the sidecar has no identity trailer: pre-trailer artifacts. Their rows
///    are still the best available, and refusing them would regress existing
///    installations for no provable gain.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn load_neural_tier_mmaps_from_storage(
    storage_path: &Path,
    expected_identity: Option<&str>,
) -> Vec<crate::search::vector::MmapEmbeddingIndex> {
    // The committed-generation manifest, when present, is authoritative: it names
    // exactly the tiers the last completed publish wrote, with the identity and
    // row count of each. That makes a short-but-identity-matching generation
    // (crash midway through a publish under an UNCHANGED provider identity)
    // visible, which per-file identity comparison alone cannot see.
    let manifest = try_load_neural_generation_manifest(storage_path);
    let mut loaded: Vec<crate::search::vector::MmapEmbeddingIndex> = crate::search::mrl::MRL_TIERS
        .iter()
        .filter_map(|dim| {
            let path = storage_path.join(format!("neural_embeddings_{dim}.bin"));
            if !path.exists() {
                return None;
            }
            // The committed generation, when present, decides membership AND
            // weight: a sidecar it does not name belongs to a superseded or
            // partial publish, and one whose row count disagrees is not the file
            // the manifest describes. Per-file identity gating alone cannot see
            // either case — a short regeneration under an UNCHANGED provider
            // identity is indistinguishable from a complete one.
            let committed_rows = manifest.as_ref().and_then(|manifest| {
                manifest
                    .tiers
                    .iter()
                    .find(|(tier, _)| tier == dim)
                    .map(|(_, rows)| *rows)
            });
            if manifest.is_some() && committed_rows.is_none() {
                tracing::warn!(
                    path = %path.display(),
                    dim = *dim,
                    "Neural tier mmap is not part of the committed generation; tier disabled"
                );
                return None;
            }
            let expected_for_file = manifest
                .as_ref()
                .and_then(|manifest| manifest.identity.as_deref())
                .or(expected_identity);
            match crate::search::vector::MmapEmbeddingIndex::open(&path) {
                Ok(index) => {
                    if let Some(expected_rows) = committed_rows {
                        if index.len() != expected_rows {
                            tracing::warn!(
                                path = %path.display(),
                                dim = *dim,
                                found = index.len(),
                                expected = expected_rows,
                                "Neural tier mmap row count disagrees with the committed \
                                 generation; tier disabled"
                            );
                            return None;
                        }
                    }
                    if let (Some(expected), Some(found)) = (expected_for_file, index.identity()) {
                        if found != expected {
                            tracing::warn!(
                                path = %path.display(),
                                dim = *dim,
                                found = %found,
                                expected = %expected,
                                "Neural tier mmap is from a different generation \
                                 (provider/model/dimension/template); tier disabled"
                            );
                            return None;
                        }
                    }
                    Some(index)
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to load neural tier mmap; tier disabled"
                    );
                    None
                }
            }
        })
        .collect();
    loaded.sort_by_key(|index| index.dimension());

    // The canonical file is a tier too when the provider's native width is not
    // one of the MRL tier dimensions (a 1024-dim model, say): publish routes the
    // actual-width bucket there, and skipping it here would silently drop the
    // widest — and for an `Off` store, only — rows. It is read only when no
    // sidecar already carries that dimension, so the committed 768 bucket keeps
    // the canonical file to itself.
    let storage = storage_path.to_path_buf();
    if let Some(canonical) = try_load_neural_mmap_embeddings_from_storage(&storage) {
        let width = canonical.dimension();
        let already_covered = loaded.iter().any(|index| index.dimension() == width);
        let committed = manifest.as_ref().is_none_or(|manifest| {
            manifest
                .tiers
                .iter()
                .any(|(dim, _)| u32::try_from(*dim).ok() == Some(width))
        });
        let rows_agree = manifest
            .as_ref()
            .and_then(|manifest| {
                manifest
                    .tiers
                    .iter()
                    .find(|(dim, _)| u32::try_from(*dim).ok() == Some(width))
                    .map(|(_, rows)| *rows)
            })
            .is_none_or(|rows| rows == canonical.len());
        if !already_covered && committed && rows_agree {
            loaded.push(canonical);
        } else if !already_covered && !committed {
            tracing::warn!(
                dim = width,
                "Canonical neural mmap is not part of the committed generation; tier disabled"
            );
        }
    }
    loaded
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

#[cfg(all(test, any(feature = "onnx", feature = "remote-embeddings")))]
mod tests {
    use super::*;

    /// Write a sidecar tier mmap carrying `identity`, the way a publication
    /// would. Returns the storage dir it landed in.
    fn tier_store(identity: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let dim = crate::search::mrl::MRL_TIERS[1]; // 256: a non-native tier
        let rows = vec![("node-1".to_string(), vec![0.25_f32; dim])];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &dir.path().join(format!("neural_embeddings_{dim}.bin")),
            &rows,
            identity,
        )
        .unwrap();
        dir
    }

    #[test]
    fn test_tier_mmap_from_a_foreign_generation_is_not_loaded() {
        // Sidecar paths are fixed per dimension, so a republish that failed
        // partway leaves the previous generation's sliced rows beside a
        // canonical file describing the new one. Serving them would cosine
        // vectors from another provider/model/template against the current
        // query embedding.
        let dir = tier_store(Some("remote|openai|old-model||768"));
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|new-model||768"));
        assert!(
            loaded.is_empty(),
            "a sidecar from another generation must not contribute rows"
        );
    }

    #[test]
    fn test_tier_mmap_of_the_current_generation_is_loaded() {
        let dir = tier_store(Some("remote|openai|same-model||768"));
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|same-model||768"));
        assert_eq!(loaded.len(), 1, "the current generation's sidecar loads");
        assert_eq!(loaded[0].len(), 1, "and its rows are reachable");
    }

    #[test]
    fn test_tier_mmap_is_loaded_when_no_generation_is_known() {
        // No canonical file to compare against (tier-only store whose header is
        // absent or corrupt). Refusing here would drop every tier, regressing a
        // deployment that legitimately has no canonical generation.
        let dir = tier_store(Some("remote|openai|whatever||768"));
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), None);
        assert_eq!(
            loaded.len(),
            1,
            "an unknown generation must not disable tiers"
        );
    }

    #[test]
    fn test_tier_mmap_without_an_identity_trailer_is_loaded() {
        // Pre-trailer artifacts carry no generation at all. Their rows are the
        // best available and are still admitted.
        let dir = tier_store(None);
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some("any|identity||768"));
        assert_eq!(
            loaded.len(),
            1,
            "a sidecar with no trailer is not provably foreign"
        );
    }

    /// Commit a manifest naming `tiers` for `dir`, the way a finished publish
    /// would.
    fn commit_manifest(dir: &tempfile::TempDir, identity: Option<&str>, tiers: &[(usize, usize)]) {
        persist_neural_generation_manifest(
            dir.path(),
            identity,
            crate::config::MatryoshkaSetting::Dynamic,
            tiers,
        )
        .unwrap();
    }

    #[test]
    fn test_tier_outside_the_committed_generation_is_not_loaded() {
        // The exact gap per-file identity gating cannot see: the sidecar carries
        // the SAME provider identity as the committed generation, so nothing
        // about the file itself marks it stale. Only the manifest says this
        // dimension was never part of the generation that completed.
        let dir = tier_store(Some("remote|openai|same||768"));
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some("remote|openai|same||768"), &[(128, 1)]);
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|same||768"));
        assert!(
            loaded.is_empty(),
            "a sidecar the committed generation does not name must not contribute rows ({dim})"
        );
    }

    #[test]
    fn test_tier_row_count_disagreeing_with_the_committed_generation_is_not_loaded() {
        // A publish that replaced the file but not the row count, or a truncated
        // artifact behind a stale trailer, would otherwise be cosined silently.
        let dir = tier_store(Some("remote|openai|same||768"));
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some("remote|openai|same||768"), &[(dim, 99)]);
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|same||768"));
        assert!(
            loaded.is_empty(),
            "a sidecar whose weight disagrees with the commit marker must not load"
        );
    }

    #[test]
    fn test_tier_named_by_the_committed_generation_is_loaded() {
        let dir = tier_store(Some("remote|openai|same||768"));
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some("remote|openai|same||768"), &[(dim, 1)]);
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|same||768"));
        assert_eq!(loaded.len(), 1, "the committed generation's tier loads");
        assert_eq!(loaded[0].len(), 1, "and its rows are reachable");
    }

    #[test]
    fn test_manifest_identity_overrides_the_canonical_one() {
        // The manifest is the authority on the generation: when it disagrees with
        // the canonical trailer handed in by the caller, the manifest wins,
        // because the canonical file may itself be the half-written artifact.
        let dir = tier_store(Some("remote|openai|manifest-model||768"));
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some("remote|openai|manifest-model||768"), &[(dim, 1)]);
        let loaded = load_neural_tier_mmaps_from_storage(
            dir.path(),
            Some("remote|openai|canonical-disagrees||768"),
        );
        assert_eq!(
            loaded.len(),
            1,
            "the committed manifest, not the canonical trailer, identifies the generation"
        );
    }

    #[test]
    fn test_absent_manifest_falls_back_to_per_file_identity_gating() {
        // Upgrades from builds that never wrote a manifest must keep working: no
        // manifest means no membership/weight check, and the pre-existing
        // per-file identity rule decides.
        let dir = tier_store(Some("remote|openai|same||768"));
        assert!(
            try_load_neural_generation_manifest(dir.path()).is_none(),
            "precondition: no manifest was written"
        );
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|same||768"));
        assert_eq!(loaded.len(), 1, "legacy artifacts stay readable");
    }

    #[test]
    fn test_publish_leaves_no_staging_file_and_commits_the_manifest() {
        // End-to-end over the real publish sequence. Two things must hold: the
        // staging file is gone (renamed, not left behind), and the manifest names
        // the tier with its weight, so a restart can trust it.
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        let tier_path = storage.join(format!(
            "neural_embeddings_{}.bin",
            crate::search::mrl::MRL_TIERS[1]
        ));
        let stage_path = neural_staging_path(&tier_path);
        let rows = vec![(
            "node-1".to_string(),
            vec![0.5_f32; crate::search::mrl::MRL_TIERS[1]],
        )];
        crate::search::vector::write_mmap_embeddings_with_identity(&stage_path, &rows, Some("id"))
            .unwrap();
        let tier = StagedNeuralTier {
            dim: crate::search::mrl::MRL_TIERS[1],
            rows: rows.len(),
            final_path: tier_path.clone(),
            stage_path: stage_path.clone(),
        };
        publish_staged_tier(&tier).unwrap();
        assert!(tier_path.is_file(), "the tier landed at its canonical path");
        assert!(
            !stage_path.exists(),
            "rename must not leave the staging file behind"
        );

        persist_neural_generation_manifest(
            &storage,
            Some("id"),
            crate::config::MatryoshkaSetting::Dynamic,
            &[(tier.dim, tier.rows)],
        )
        .unwrap();
        let manifest = try_load_neural_generation_manifest(&storage).expect("manifest committed");
        assert_eq!(manifest.tiers, vec![(tier.dim, tier.rows)]);
        let loaded = load_neural_tier_mmaps_from_storage(&storage, Some("id"));
        assert_eq!(loaded.len(), 1, "a just-committed generation loads");
    }

    #[test]
    fn test_failed_publish_keeps_the_previous_generation_readable() {
        // The property the commit marker exists for: if the publish dies before
        // the rename, the previous generation must still be what a reader sees —
        // not a half-published mix.
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        let dim = crate::search::mrl::MRL_TIERS[1];
        let tier_path = storage.join(format!("neural_embeddings_{dim}.bin"));
        let previous = vec![("node-1".to_string(), vec![0.25_f32; dim])];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &tier_path,
            &previous,
            Some("id"),
        )
        .unwrap();
        persist_neural_generation_manifest(
            &storage,
            Some("id"),
            crate::config::MatryoshkaSetting::Dynamic,
            &[(dim, previous.len())],
        )
        .unwrap();

        // Stage a NEWER generation but never publish or commit it.
        let newer = vec![
            ("node-1".to_string(), vec![0.75_f32; dim]),
            ("node-2".to_string(), vec![0.75_f32; dim]),
        ];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &neural_staging_path(&tier_path),
            &newer,
            Some("id"),
        )
        .unwrap();

        let loaded = load_neural_tier_mmaps_from_storage(&storage, Some("id"));
        assert_eq!(loaded.len(), 1, "the previous generation still loads");
        assert_eq!(
            loaded[0].len(),
            previous.len(),
            "and it is the previous generation's rows, not the uncommitted ones"
        );
    }

    #[test]
    fn test_native_width_not_in_the_tier_ladder_loads_from_the_canonical_file() {
        // Publish routes an actual-width bucket (a 1024-dim provider, say) to the
        // canonical file because 1024 is not one of MRL_TIERS. The loader used to
        // filter every MRL_TIERS dimension as a sidecar and never read the
        // canonical path, so those rows were unreachable at restart.
        let dir = tempfile::tempdir().unwrap();
        let width = 1024usize;
        let rows = vec![("node-1".to_string(), vec![0.5_f32; width])];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &dir.path().join("neural_embeddings.bin"),
            &rows,
            Some("remote|openai|m||1024"),
        )
        .unwrap();
        persist_neural_generation_manifest(
            dir.path(),
            Some("remote|openai|m||1024"),
            crate::config::MatryoshkaSetting::Off,
            &[(width, rows.len())],
        )
        .unwrap();
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|m||1024"));
        assert_eq!(
            loaded.len(),
            1,
            "the canonical file is a tier when its width is not on the ladder"
        );
        assert_eq!(loaded[0].dimension(), width as u32);
        assert_eq!(loaded[0].len(), rows.len());
    }

    #[test]
    fn test_canonical_file_is_not_re_read_when_a_sidecar_covers_its_width() {
        // The committed 768 bucket belongs to the canonical file and is reached as
        // a tier; reading it a second time would double its rows.
        let dir = tier_store(Some("remote|openai|same||768"));
        let canonical_rows = vec![("node-1".to_string(), vec![0.5_f32; 768])];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &dir.path().join("neural_embeddings.bin"),
            &canonical_rows,
            Some("remote|openai|same||768"),
        )
        .unwrap();
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some("remote|openai|same||768"), &[(dim, 1)]);
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|same||768"));
        assert_eq!(loaded.len(), 1, "one file, one entry");
    }

    #[test]
    fn test_staging_path_is_a_sibling_of_the_tier_file() {
        // The commit step relies on `rename` being atomic, which only holds
        // within one filesystem — so the staging file must live beside the
        // destination, never in a system temp dir.
        let final_path = Path::new("/tmp/proj/.leindex/neural_embeddings_256.bin");
        let staged = neural_staging_path(final_path);
        assert_eq!(staged.parent(), final_path.parent());
        assert_eq!(
            staged.file_name().and_then(|name| name.to_str()),
            Some(".neural_embeddings_256.bin.staged")
        );
    }
}
