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
        remove_neural_generation(&path, &storage_path)?;
        return Ok(());
    }
    let spans = search_engine.collect_node_span_bytes();
    let buckets =
        crate::search::mrl::partition_rows_by_slice_dimension(&embeddings, &spans, matryoshka);
    let native_dim = canonical_native_dim(&embeddings, project_path, identity);
    // Stage every tier, then commit them, then commit the manifest. See
    // `stage_neural_tiers` for why the order matters. The canonical file keeps
    // its FIXED path (readers raw-read it for identity/dimension); the SIDECAR
    // tiers are generation-scoped per #103.
    let generation = next_neural_generation(&storage_path);
    let staged = stage_neural_tiers(
        &storage_path,
        &path,
        identity,
        native_dim,
        generation,
        &buckets,
    )?;
    commit_neural_generation(
        &storage_path,
        &path,
        identity,
        matryoshka,
        native_dim,
        generation,
        staged,
    )
}

/// The dimension that owns the canonical `neural_embeddings.bin`.
///
/// Normally the width the provider returned, read off the rows. That is the
/// correct answer whenever the engine holds unsliced vectors, and it is the only
/// answer that survives a provider change: deriving the width from the identity
/// trailer is wrong (the trailer can be absent, stale, or name another model) and
/// hardcoding 768 is worse, since a sub-768 provider was sliced to its real width
/// and then handed a zero-row header claiming 768.
///
/// The generation already on disk outranks the rows ONLY when it is the SAME
/// generation. Fallback hydration (`restore_persisted_neural_embeddings`)
/// restores just the sliced sidecar rows under a fixed sub-native policy and then
/// re-persists them, passing the identity it restored from; the widest of those
/// rows is the SLICE width, so honouring it would narrow the canonical header
/// while the identity still declared 768 and disable neural search on the next
/// restart. Persisted width is authoritative there because the rows are slices of
/// it.
///
/// A DIFFERENT identity means the provider or model changed — genuinely fresh
/// output, whose width the old files know nothing about. Widen to it and a
/// project switching to a narrower endpoint (say 384) would file its new rows
/// under the stale 768 header: `partition_rows_by_slice_dimension` leaves rows
/// already narrower than the requested slice at their own width, so those 384
/// rows land in `neural_embeddings_384.bin` — a dimension `MRL_TIERS` never
/// enumerates — behind a zero-row 768 canonical, and the entire fresh generation
/// vanishes on the next load. So identity is the discriminator, not magnitude.
fn canonical_native_dim(
    embeddings: &[(String, Vec<f32>)],
    project_path: &Path,
    identity: Option<&str>,
) -> usize {
    let widest_row = embeddings
        .iter()
        .map(|(_, row)| row.len())
        .max()
        .unwrap_or(0);
    let persisted = try_load_neural_mmap_embeddings(project_path)
        // Same generation only. `None` on either side means "cannot prove it is
        // the same generation", which is exactly when the rows should win.
        .filter(|mmap| match (identity, mmap.identity()) {
            (Some(incoming), Some(on_disk)) => incoming == on_disk,
            _ => false,
        })
        .map(|mmap| {
            // Rows wider than the persisted width cannot be slices of it, so the
            // match is not a slice-restore even if the identity agreed.
            if mmap.dimension() as usize >= widest_row {
                mmap.dimension() as usize
            } else {
                widest_row
            }
        })
        .unwrap_or(0);
    widest_row.max(persisted)
}

/// Drop every neural artifact. Nothing is published, so nothing may claim to be
/// the committed generation either — otherwise a restart would honour a
/// manifest whose files no longer exist.
fn remove_neural_generation(path: &Path, storage_path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|e| anyhow::anyhow!("Failed to remove stale neural mmap embeddings: {e}"))?;
        info!(
            path = %path.display(),
            "Removed stale neural embeddings mmap file"
        );
    }
    remove_neural_generation_tier_files(storage_path)?;
    remove_neural_generation_manifest(storage_path)?;
    Ok(())
}

/// Publish phase 1: write every tier to a private staging path and validate it
/// by REOPENING it.
///
/// Existence proves more than nothing, but weight is the real check — opening
/// the mmap validates the payload's extent and checksum, so a truncated write
/// fails here, while the previous generation is still whole on disk.
///
/// The canonical path is a tier too: it owns `native_dim`, which is now
/// guaranteed to be a dimension present in `buckets`. When no bucket is native
/// (a fixed sub-native policy such as Dim512/Dim256/Dim128, where every row is
/// sliced), it instead receives a zero-row header carrying the identity and
/// native dimension: readers restore nothing from it (`len() == 0`) but still
/// resolve identity and dimension, which `query_remote_embedder` raw-reads.
fn stage_neural_tiers(
    storage_path: &Path,
    canonical_path: &Path,
    identity: Option<&str>,
    native_dim: usize,
    generation: u64,
    buckets: &[(usize, Vec<(String, Vec<f32>)>)],
) -> Result<Vec<StagedNeuralTier>> {
    let mut staged: Vec<StagedNeuralTier> = Vec::with_capacity(buckets.len());
    for (dim, rows) in buckets {
        let final_path = if *dim == native_dim {
            canonical_path.to_path_buf()
        } else {
            neural_tier_mmap_embeddings_path(storage_path, *dim, generation)
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

    if !staged.iter().any(|tier| tier.dim == native_dim) {
        let stage_path = neural_staging_path(canonical_path);
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
        staged.push(StagedNeuralTier {
            dim: native_dim,
            rows: 0,
            final_path: canonical_path.to_path_buf(),
            stage_path,
        });
    }
    Ok(staged)
}

/// Publish phase 2 and 3: rename staged tiers into place, commit the manifest,
/// then sweep the superseded generation's files.
///
/// Sidecar tiers land at GENERATION-SCOPED paths, so the renames create new
/// files and never disturb the previous generation's. The manifest lands LAST
/// and atomically: until it does, the previous manifest stays in force and
/// names files that still exist, whole and untouched — a crash anywhere before
/// the swap leaves the reader serving the previous generation, never a mix of
/// two. Only after the swap are the superseded files deleted (#103).
///
/// The canonical file keeps its FIXED path (`neural_embeddings.bin`): readers
/// raw-read it for the generation's identity and native dimension, and the
/// mmap writer's atomic replace keeps that swap safe on platforms where a
/// plain rename cannot replace a mapped file. It is renamed FIRST so the
/// identity a reader raw-reads never describes files the committed manifest
/// does not name.
fn commit_neural_generation(
    storage_path: &Path,
    canonical_path: &Path,
    identity: Option<&str>,
    matryoshka: crate::config::MatryoshkaSetting,
    native_dim: usize,
    generation: u64,
    staged: Vec<StagedNeuralTier>,
) -> Result<()> {
    let empty_header = staged
        .iter()
        .any(|tier| tier.dim == native_dim && tier.rows == 0);
    for tier in &staged {
        publish_staged_tier(tier)?;
        info!(
            count = tier.rows,
            dim = tier.dim,
            path = %tier.final_path.display(),
            "Published neural embeddings mmap tier"
        );
    }
    // The manifest describes EVERY file the publish wrote — including the
    // zero-row identity header of a tier-only generation, so a reader can tell
    // the intended state from a stale artifact of a superseded publish.
    let committed: Vec<NeuralTierEntry> = staged
        .iter()
        .map(|tier| NeuralTierEntry {
            dim: tier.dim,
            rows: tier.rows,
            file: tier
                .final_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect();
    persist_neural_generation_manifest(
        storage_path,
        identity,
        matryoshka,
        native_dim,
        generation,
        &committed,
    )?;
    // Only now is the previous generation definitively superseded, so only now
    // is it safe to drop its files. Best-effort by design: the committed
    // manifest is already authoritative and names exactly the files that may
    // serve, so a file this sweep fails to delete is inert — and failing the
    // publish over housekeeping would discard a generation that just committed
    // cleanly. (Windows can keep an old tier mapped by a serving process; the
    // file simply outlives this call until unmapped.)
    sweep_superseded_neural_files(storage_path, &committed);
    if empty_header {
        info!(
            path = %canonical_path.display(),
            dim = native_dim,
            "Canonical neural mmap carries identity only (all rows are sliced)"
        );
    }
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
    // Same swap the mmap writer uses: a plain `rename` cannot replace a file a
    // serving process still has mapped on Windows, and these tiers are exactly
    // the files `MmapEmbeddingIndex` keeps mapped.
    crate::search::vector::replace_file_atomically(&tier.stage_path, &tier.final_path)
        .with_context(|| {
            format!(
                "Failed to publish staged neural mmap {} -> {}",
                tier.stage_path.display(),
                tier.final_path.display()
            )
        })?;
    Ok(())
}

/// Path for a per-dimension MRL sidecar neural embeddings mmap file,
/// generation-scoped so two publishes never share a filename (#103).
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn neural_tier_mmap_embeddings_path(storage_path: &Path, dim: usize, generation: u64) -> PathBuf {
    storage_path.join(neural_tier_filename(dim, generation))
}

/// Committed-generation manifest for the neural tier set.
///
/// Tier files are GENERATION-SCOPED (`neural_embeddings_{dim}.{generation}.bin`),
/// and this manifest is the commit point: it is written last and atomically,
/// and it names the exact file of every tier the publish completed. Renames
/// create NEW files — they never replace the previous generation's — so a
/// crash before the manifest swap leaves the previous manifest in force
/// naming files that still exist, whole and untouched. A reader can never see
/// a mixed generation. Files of superseded generations are deleted only
/// AFTER the swap (#103).
///
/// Schema 1 named only `(dimension, rows)` pairs against fixed per-dimension
/// paths; a torn publish could serve same-row-count tiers from two source
/// generations under the old manifest. Schema 2 adds the generation counter,
/// the native dimension, and per-tier filenames. Schema-1 manifests are
/// ignored by this reader (callers fall back to the legacy fixed paths, which
/// remain on disk until the next publish sweeps them).
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct NeuralGenerationManifest {
    schema_version: u32,
    /// Monotonic publish counter; every tier filename embeds it.
    generation: u64,
    /// The dimension that owns the canonical (identity-carrying) file. Its
    /// entry is the one tier loaders exclude — callers merge the canonical
    /// index themselves.
    native_dim: usize,
    /// Provider/model/template identity shared by every tier in the generation.
    identity: Option<String>,
    /// Matryoshka policy the generation was partitioned under.
    matryoshka: String,
    /// Every file the publish wrote, ascending by dimension. A row count of 0
    /// names the canonical file's zero-row identity header (written when every
    /// row was sliced), so the manifest describes the whole generation rather
    /// than only its row-bearing part.
    tiers: Vec<NeuralTierEntry>,
}

/// One committed tier file: its slice dimension, its row count, and the exact
/// generation-scoped filename that holds it (storage-relative).
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct NeuralTierEntry {
    dim: usize,
    rows: usize,
    file: String,
}

#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
const NEURAL_GENERATION_MANIFEST_SCHEMA_VERSION: u32 = 2;

/// Generation-scoped tier filename (#103): `neural_embeddings_{dim}.{gen}.bin`.
/// Two publishes never share a filename, so a rename cannot destroy the
/// previous generation and a torn publish cannot mix two generations' rows
/// behind one manifest.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn neural_tier_filename(dim: usize, generation: u64) -> String {
    format!("neural_embeddings_{dim}.{generation}.bin")
}

/// The generation number the next publish commits: one past the committed
/// manifest's, or 1 for a fresh (or legacy schema-1) store.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn next_neural_generation(storage_path: &Path) -> u64 {
    try_load_neural_generation_manifest(storage_path).map_or(1, |manifest| manifest.generation + 1)
}

#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn neural_generation_manifest_path(storage_path: &Path) -> PathBuf {
    storage_path.join("neural_generation.bin")
}

/// Atomically commit the manifest — the publish's commit point. It is written
/// LAST in the sequence, after every tier file it names is already in place,
/// so a crash before this call leaves the PREVIOUS manifest in force naming
/// files that still exist, whole and untouched. `tiers` is sorted here so the
/// serialized form is canonical (ascending by dimension) regardless of the
/// bucket order the partitioner produced.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn persist_neural_generation_manifest(
    storage_path: &Path,
    identity: Option<&str>,
    matryoshka: crate::config::MatryoshkaSetting,
    native_dim: usize,
    generation: u64,
    tiers: &[NeuralTierEntry],
) -> Result<()> {
    let mut tiers = tiers.to_vec();
    tiers.sort_unstable_by_key(|entry| entry.dim);
    let manifest = NeuralGenerationManifest {
        schema_version: NEURAL_GENERATION_MANIFEST_SCHEMA_VERSION,
        generation,
        native_dim,
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

/// Every storage entry whose name looks like a neural tier artifact — the
/// generation-scoped `neural_embeddings_{dim}.{gen}.bin` this build writes and
/// the fixed-path `neural_embeddings_{dim}.bin` of pre-#103 builds. Staging
/// (`.…staged`) leftovers are removed by the mmap writer's staging sweep and
/// are deliberately not touched here.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn neural_tier_artifacts(storage_path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(storage_path) else {
        return Vec::new();
    };
    let mut artifacts = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_neural_tier_artifact_name(&name) {
            artifacts.push(entry.path());
        }
    }
    artifacts
}

/// `neural_embeddings_<dim>.bin` (legacy fixed path) or
/// `neural_embeddings_<dim>.<gen>.bin` (generation-scoped). Dimension and
/// generation are digits-only; anything else is not ours.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn is_neural_tier_artifact_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("neural_embeddings_") else {
        return false;
    };
    let stem = rest.strip_suffix(".bin").unwrap_or(rest);
    let mut parts = stem.split('.');
    let dim = parts.next().unwrap_or_default();
    let generation = parts.next().unwrap_or("");
    dim.bytes().all(|b| b.is_ascii_digit())
        && !dim.is_empty()
        && (generation.is_empty() || generation.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}

/// Delete every tier artifact the committed manifest does not name — the
/// previous generation's generation-scoped files and any legacy fixed-path
/// sidecar. Runs only AFTER the manifest swap, so the previous generation is
/// no longer reachable by any reader. Best-effort: a file that cannot be
/// deleted (Windows: still mapped by a serving process) is inert, because
/// nothing loads a tier file the manifest does not name.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn sweep_superseded_neural_files(storage_path: &Path, committed: &[NeuralTierEntry]) {
    let keep: Vec<String> = committed.iter().map(|entry| entry.file.clone()).collect();
    for path in neural_tier_artifacts(storage_path) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if keep.iter().any(|file| file == name) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                info!(
                    path = %path.display(),
                    "Swept superseded neural tier mmap file"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to sweep superseded neural tier mmap; it is inert \
                     (the committed manifest does not name it)"
                );
            }
        }
    }
}

/// Remove every neural tier artifact (used when the neural store is emptied —
/// including a fixed-path canonical `neural_embeddings.bin` dropped by the
/// caller before this runs). Unlike the post-swap sweep this is fatal on
/// failure: the store is being fully cleared, so a survivor would be a
/// stale-but-loadable file with no manifest to disqualify it.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn remove_neural_generation_tier_files(storage_path: &Path) -> Result<()> {
    for path in neural_tier_artifacts(storage_path) {
        std::fs::remove_file(&path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to remove stale neural tier mmap {}: {e}",
                path.display()
            )
        })?;
        info!(
            path = %path.display(),
            "Removed stale neural tier mmap file"
        );
    }
    Ok(())
}

/// Load every MRL sidecar neural mmap present in storage, ascending by
/// dimension. Missing or corrupt sidecars are skipped (non-fatal): the tier
/// simply does not contribute at query time.
///
/// Two arms, two eras (#103):
///
/// - A schema-2 committed manifest names the EXACT generation-scoped file of
///   every tier the last completed publish wrote, so that arm loads precisely
///   those files and rejects any whose rows, dimension, or identity disagree
///   with the commit. The manifest is the publish's commit point, so what it
///   names is one generation by construction — a torn publish cannot be
///   observed, because until the manifest swaps, the PREVIOUS manifest (and
///   its whole, untouched files) is still what this reads.
/// - No manifest at all, or a schema-1 manifest (v2.1.1 fixed-path layout):
///   fall back to the legacy arm, scanning the fixed per-dimension paths with
///   per-file identity gating. Those installations must keep working
///   unchanged; the first publish by this build swaps them onto schema 2.
///
/// In both arms a sidecar at the canonical's own dimension is never returned
/// as a tier: callers merge the canonical file themselves, and a duplicate
/// would double-count its rows against the checkpoint.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn load_neural_tier_mmaps_from_storage(
    storage_path: &Path,
    expected_identity: Option<&str>,
) -> Vec<crate::search::vector::MmapEmbeddingIndex> {
    let manifest = try_load_neural_generation_manifest(storage_path);
    // A generation is partitioned under one policy, so a generation committed
    // under a DIFFERENT policy is not the store the current setting describes.
    // Identity and row counts cannot see this: a `Dynamic` generation (tiers
    // 256/512/768) narrowed to `Dim512` has the same identity and the same rows,
    // so it would be served forever and the policy change would never take
    // effect. Comparing the recorded policy makes the narrowing real. The kill
    // switch is separate (`Off` makes callers skip this loader entirely).
    if let Some(manifest) = manifest.as_ref() {
        let effective = format!(
            "{:?}",
            crate::config::LeIndexConfig::load_cached()
                .indexing
                .embeddings
                .effective_matryoshka()
        );
        if manifest.matryoshka != effective {
            tracing::warn!(
                recorded = manifest.matryoshka.as_str(),
                "Neural generation was partitioned under a different Matryoshka policy; \
                 tier disabled until re-indexed"
            );
            return Vec::new();
        }
    }
    match manifest {
        Some(manifest) => load_committed_neural_tiers(storage_path, &manifest),
        None => load_legacy_neural_tiers(storage_path, expected_identity),
    }
}

/// The schema-2 arm: load exactly the files the committed manifest names.
/// Anything that disagrees with the commit — row count, dimension, identity —
/// or fails to open is skipped with a warning, because the manifest is the
/// only authority on what the completed generation contains.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn load_committed_neural_tiers(
    storage_path: &Path,
    manifest: &NeuralGenerationManifest,
) -> Vec<crate::search::vector::MmapEmbeddingIndex> {
    let mut loaded: Vec<crate::search::vector::MmapEmbeddingIndex> = Vec::new();
    for entry in &manifest.tiers {
        // The native dimension's entry names the canonical file (its rows, or
        // the zero-row identity header of a tier-only generation). Callers
        // merge the canonical index themselves; returning it here would count
        // its rows twice against the checkpoint.
        if entry.dim == manifest.native_dim {
            continue;
        }
        // A zero-row entry carries no rows to restore (and the canonical
        // header entry is already excluded above); opening it buys nothing.
        if entry.rows == 0 {
            continue;
        }
        let path = storage_path.join(&entry.file);
        if !path.is_file() {
            tracing::warn!(
                path = %path.display(),
                dim = entry.dim,
                generation = manifest.generation,
                "Committed neural tier file is missing; tier disabled"
            );
            continue;
        }
        match crate::search::vector::MmapEmbeddingIndex::open(&path) {
            Ok(index) => {
                if let Some(reject) = committed_tier_rejection(&index, entry, manifest) {
                    tracing::warn!(
                        path = %path.display(),
                        dim = entry.dim,
                        reason = reject,
                        "Neural tier mmap disagrees with the committed generation; tier disabled"
                    );
                    continue;
                }
                loaded.push(index);
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to load neural tier mmap; tier disabled"
                );
            }
        }
    }
    loaded.sort_by_key(|index| index.dimension());
    loaded
}

/// Why a committed tier file does not match its manifest entry, or `None` when
/// it does. Split out of the load loop to keep that loop's branch count under
/// the CCN-15 gate.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn committed_tier_rejection(
    index: &crate::search::vector::MmapEmbeddingIndex,
    entry: &NeuralTierEntry,
    manifest: &NeuralGenerationManifest,
) -> Option<&'static str> {
    if index.len() != entry.rows {
        return Some("row count disagrees with the commit");
    }
    if index.dimension() as usize != entry.dim {
        return Some("dimension disagrees with the commit");
    }
    if let (Some(expected), Some(found)) = (manifest.identity.as_deref(), index.identity())
        && found != expected
    {
        return Some("identity disagrees with the commit");
    }
    None
}

/// The legacy arm: no usable manifest, so tier files sit at the fixed
/// per-dimension paths of pre-#103 builds (`neural_embeddings_{dim}.bin`).
/// Sliced vectors from one provider, model, dimension, or template are not
/// comparable with the current query embedding, so a sidecar is admitted only
/// when its identity trailer agrees with the canonical generation. Two cases
/// stay permissive:
///  - `expected_identity` is `None`: the caller has no canonical generation to
///    compare against (tier-only stores whose header is absent or corrupt);
///    refusing then would silently drop every tier, so we load as before.
///  - the sidecar has no identity trailer: pre-trailer artifacts. Their rows
///    are still the best available, and refusing them would regress existing
///    installations for no provable gain.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn load_legacy_neural_tiers(
    storage_path: &Path,
    expected_identity: Option<&str>,
) -> Vec<crate::search::vector::MmapEmbeddingIndex> {
    // A sidecar at the canonical's own dimension is always a leftover, never a
    // tier: the publish sends the native bucket to the canonical path, so the
    // only way both files carry that dimension is a superseded generation
    // surviving a torn commit. Returning it would hand the caller the same
    // node rows twice — callers sum the per-file restore counts against
    // `checkpoint.rows`, so the sum overshoots and a clean checkpoint resume
    // is discarded. The canonical is read here for its dimension only.
    let canonical_dim = try_load_neural_mmap_embeddings_from_storage(storage_path)
        .filter(|index| !index.is_empty())
        .map(|index| index.dimension() as usize);
    let mut loaded: Vec<crate::search::vector::MmapEmbeddingIndex> = crate::search::mrl::MRL_TIERS
        .iter()
        .filter_map(|dim| {
            let path = storage_path.join(format!("neural_embeddings_{dim}.bin"));
            if !path.exists() {
                return None;
            }
            if canonical_dim == Some(*dim) {
                tracing::warn!(
                    path = %path.display(),
                    dim = *dim,
                    "Stale neural tier mmap duplicates the canonical generation; \
                     tier disabled"
                );
                return None;
            }
            match crate::search::vector::MmapEmbeddingIndex::open(&path) {
                Ok(index) => {
                    if let (Some(expected), Some(found)) = (expected_identity, index.identity())
                        && found != expected
                    {
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

    /// Commit a schema-2 manifest naming `entries` for `dir`, the way a
    /// finished publish would. Each entry's tier file is WRITTEN at its
    /// generation-scoped path with ONE physical row; the entry's `rows` says
    /// what the manifest CLAIMS, which is how the disagreement tests model a
    /// file that is not the one the commit describes.
    fn commit_manifest(
        dir: &tempfile::TempDir,
        identity: Option<&str>,
        native_dim: usize,
        entries: &[(usize, usize)],
    ) {
        // Commit under the policy that is EFFECTIVE here, the way a real publish
        // does: the loader refuses a generation recorded under a policy other
        // than the current one, so a fixture that pinned `Dynamic` would model a
        // store this configuration would never write.
        let generation = 1_u64;
        let mut tiers = Vec::new();
        for &(dim, rows) in entries {
            let file = neural_tier_filename(dim, generation);
            let path = dir.path().join(&file);
            let physical = vec![("node-1".to_string(), vec![0.25_f32; dim])];
            crate::search::vector::write_mmap_embeddings_with_identity(&path, &physical, identity)
                .unwrap();
            tiers.push(NeuralTierEntry { dim, rows, file });
        }
        persist_neural_generation_manifest(
            dir.path(),
            identity,
            crate::config::LeIndexConfig::load_cached()
                .indexing
                .embeddings
                .effective_matryoshka(),
            native_dim,
            generation,
            &tiers,
        )
        .unwrap();
    }

    #[test]
    fn test_tier_outside_the_committed_generation_is_not_loaded() {
        // The exact gap per-file identity gating cannot see: the stray file
        // carries the SAME provider identity as the committed generation, so
        // nothing about the file itself marks it stale. Only the manifest says
        // this file was never part of the generation that completed — the
        // committed arm loads exactly what the manifest names, and the stray
        // (a superseded generation's file, or a pre-#103 fixed-path sidecar)
        // contributes nothing.
        let identity = "remote|openai|same||768";
        let dir = tier_store(Some(identity)); // fixed-path 256 sidecar, no manifest
        let stray = crate::search::vector::write_mmap_embeddings_with_identity(
            &dir.path().join(neural_tier_filename(256, 2)),
            &[("node-1".to_string(), vec![0.25_f32; 256])],
            Some(identity),
        );
        assert!(stray.is_ok(), "fixture: stray generation-2 tier written");
        commit_manifest(&dir, Some(identity), 768, &[(128, 1)]);
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some(identity));
        assert_eq!(
            loaded.len(),
            1,
            "only the tier the committed generation names may contribute rows"
        );
        assert_eq!(
            loaded[0].dimension() as usize,
            128,
            "the stray same-identity files (gen-2 and fixed-path 256) must not load"
        );
    }

    #[test]
    fn test_tier_row_count_disagreeing_with_the_committed_generation_is_not_loaded() {
        // A publish that replaced the file but not the row count, or a truncated
        // artifact behind a stale trailer, would otherwise be cosined silently.
        // The fixture writes ONE physical row but commits an entry claiming 99.
        let identity = "remote|openai|same||768";
        let dir = tier_store(Some(identity));
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some(identity), 768, &[(dim, 99)]);
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some(identity));
        assert!(
            loaded.is_empty(),
            "a sidecar whose weight disagrees with the commit marker must not load"
        );
    }

    #[test]
    fn test_tier_named_by_the_committed_generation_is_loaded() {
        let identity = "remote|openai|same||768";
        let dir = tier_store(Some(identity));
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some(identity), 768, &[(dim, 1)]);
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some(identity));
        assert_eq!(loaded.len(), 1, "the committed generation's tier loads");
        assert_eq!(loaded[0].len(), 1, "and its rows are reachable");
    }

    #[test]
    fn test_manifest_identity_overrides_the_canonical_one() {
        // The manifest is the authority on the generation: when it disagrees with
        // the canonical trailer handed in by the caller, the manifest wins,
        // because the canonical file may itself be the half-written artifact.
        let identity = "remote|openai|manifest-model||768";
        let dir = tier_store(Some(identity));
        let dim = crate::search::mrl::MRL_TIERS[1];
        commit_manifest(&dir, Some(identity), 768, &[(dim, 1)]);
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
        // End-to-end over the real publish sequence. Three things must hold:
        // the staging files are gone (renamed, not left behind), the manifest
        // names each tier's exact generation-scoped file with its weight, and
        // a restart loads exactly what it names.
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        let tier_path = storage.join(neural_tier_filename(
            crate::search::mrl::MRL_TIERS[1],
            1, // the generation a fresh store publishes
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
        assert!(tier_path.is_file(), "the tier landed at its final path");
        assert!(
            !stage_path.exists(),
            "rename must not leave the staging file behind"
        );

        persist_neural_generation_manifest(
            &storage,
            Some("id"),
            crate::config::LeIndexConfig::load_cached()
                .indexing
                .embeddings
                .effective_matryoshka(),
            768, // native: the tier under test is a non-native sidecar
            1,
            &[NeuralTierEntry {
                dim: tier.dim,
                rows: tier.rows,
                file: neural_tier_filename(tier.dim, 1),
            }],
        )
        .unwrap();
        let manifest = try_load_neural_generation_manifest(&storage).expect("manifest committed");
        assert_eq!(manifest.generation, 1);
        assert_eq!(manifest.native_dim, 768);
        assert_eq!(
            manifest.tiers,
            vec![NeuralTierEntry {
                dim: tier.dim,
                rows: tier.rows,
                file: neural_tier_filename(tier.dim, 1),
            }]
        );
        let loaded = load_neural_tier_mmaps_from_storage(&storage, Some("id"));
        assert_eq!(loaded.len(), 1, "a just-committed generation loads");
    }

    #[test]
    fn test_failed_publish_keeps_the_previous_generation_readable() {
        // The property generation-scoped files exist for: if the publish dies
        // before the manifest swap, the previous generation must still be what
        // a reader sees — never a half-published mix. The newer generation's
        // file is fully staged and even renamed into place, but the manifest
        // still names generation 1's file, so generation 1 is all a reader can
        // reach; the uncommitted gen-2 file is inert until the next publish
        // sweeps it.
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        let dim = crate::search::mrl::MRL_TIERS[1];
        let gen1 = storage.join(neural_tier_filename(dim, 1));
        let previous = vec![("node-1".to_string(), vec![0.25_f32; dim])];
        crate::search::vector::write_mmap_embeddings_with_identity(&gen1, &previous, Some("id"))
            .unwrap();
        persist_neural_generation_manifest(
            &storage,
            Some("id"),
            crate::config::LeIndexConfig::load_cached()
                .indexing
                .embeddings
                .effective_matryoshka(),
            768,
            1,
            &[NeuralTierEntry {
                dim,
                rows: previous.len(),
                file: neural_tier_filename(dim, 1),
            }],
        )
        .unwrap();

        // A NEWER generation is fully written and even renamed into place —
        // but the manifest swap never happens (the crash point this models).
        let newer = vec![
            ("node-1".to_string(), vec![0.75_f32; dim]),
            ("node-2".to_string(), vec![0.75_f32; dim]),
        ];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &storage.join(neural_tier_filename(dim, 2)),
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
    fn test_generation_partitioned_under_another_policy_is_not_loaded() {
        // A `Dynamic` generation narrowed to `Dim512` keeps the same identity and
        // the same row counts, so nothing about the files marks them stale — only
        // the recorded policy does. Without this check the narrowing would never
        // take effect and differently-partitioned vectors would keep serving.
        let identity = "remote|openai|same||768";
        let dir = tier_store(Some(identity));
        let dim = crate::search::mrl::MRL_TIERS[1];
        persist_neural_generation_manifest(
            dir.path(),
            Some(identity),
            crate::config::MatryoshkaSetting::Dynamic,
            768,
            1,
            &[NeuralTierEntry {
                dim,
                rows: 1,
                file: neural_tier_filename(dim, 1),
            }],
        )
        .unwrap();
        let effective = crate::config::LeIndexConfig::load_cached()
            .indexing
            .embeddings
            .effective_matryoshka();
        if effective == crate::config::MatryoshkaSetting::Dynamic {
            // Nothing to prove when the policy under test is the one recorded.
            return;
        }
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some(identity));
        assert!(
            loaded.is_empty(),
            "a generation committed under a different policy must not serve"
        );
    }

    #[test]
    fn test_canonical_file_is_never_returned_as_a_tier() {
        // All three callers load the canonical file themselves and then merge
        // this function's result into the same index. A manifest entry at the
        // NATIVE dimension names the canonical file (its rows, or a tier-only
        // generation's zero-row header); returning it here as well would count
        // those rows twice: `neural_publish` compares the merged total against
        // the checkpoint, sees a mismatch, and discards the whole restored set
        // to re-embed — defeating checkpoint resume in exactly the
        // configuration it exists for.
        let identity = "remote|openai|same||768";
        let dir = tier_store(Some(identity));
        let canonical_rows = vec![("node-1".to_string(), vec![0.5_f32; 768])];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &dir.path().join("neural_embeddings.bin"),
            &canonical_rows,
            Some(identity),
        )
        .unwrap();
        let dim = crate::search::mrl::MRL_TIERS[1];
        // The manifest names BOTH the native canonical file (768) and a
        // sidecar tier (256) — the loader must return only the sidecar.
        commit_manifest(&dir, Some(identity), 768, &[(256, 1), (768, 1)]);
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some(identity));
        assert_eq!(loaded.len(), 1, "one sidecar, one entry");
        assert_eq!(
            loaded[0].dimension() as usize,
            dim,
            "the canonical file is never returned as a tier"
        );
    }

    #[test]
    fn test_stale_sidecar_duplicating_the_canonical_dimension_is_not_returned_as_a_tier() {
        // A torn commit can leave a sidecar at the canonical's own dimension: the
        // native bucket is published to `neural_embeddings.bin`, but the cleanup
        // that removes the superseded sidecar runs after the manifest swap, so a
        // crash in between leaves both files holding that dimension. Serving the
        // sidecar hands the caller the same rows twice — every caller merges this
        // function's result into an index it already seeded from the canonical,
        // and `neural_publish` sums the per-file restore counts against
        // `checkpoint.rows`. The overshoot trips the mismatch branch, discards a
        // restore that was in fact clean, and re-embeds the whole set.
        // The fixture sidecar carries the canonical identity so the ONLY
        // variable under test is the duplicate dimension: a no-trailer sidecar
        // would be admitted by the legacy arm's separate pre-trailer rule
        // (pinned by test_tier_mmap_without_an_identity_trailer_is_loaded).
        let dir = tier_store(Some("remote|openai|same||768"));
        let native = *crate::search::mrl::MRL_TIERS.last().unwrap(); // 768
        let canonical_rows = vec![("node-1".to_string(), vec![0.5_f32; native])];
        crate::search::vector::write_mmap_embeddings_with_identity(
            &dir.path().join("neural_embeddings.bin"),
            &canonical_rows,
            Some("remote|openai|same||768"),
        )
        .unwrap();
        crate::search::vector::write_mmap_embeddings_with_identity(
            &dir.path().join(format!("neural_embeddings_{native}.bin")),
            &canonical_rows,
            Some("remote|openai|same||768"),
        )
        .unwrap();
        // No manifest is committed: this is the v2.1.1 fixed-path layout, and
        // the LEGACY arm's duplicate-dimension check is what must refuse the
        // sidecar (a v2 publish would never name it, but this store predates
        // that and has no manifest to lean on).
        let loaded =
            load_neural_tier_mmaps_from_storage(dir.path(), Some("remote|openai|same||768"));
        assert_eq!(
            loaded.len(),
            1,
            "the legitimate 256 tier loads; only the duplicate is refused"
        );
        assert_eq!(
            loaded[0].dimension() as usize,
            crate::search::mrl::MRL_TIERS[1],
            "a sidecar at the canonical dimension duplicates the generation and must not load"
        );
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

    #[test]
    fn test_generation_scoped_tier_filenames_never_collide_across_publishes() {
        // #103's storage invariant, stated as a filename property: two
        // publishes never share a tier filename, so the commit rename
        // creates a new file rather than replacing the previous
        // generation's, and no torn publish can mix two generations' rows
        // behind one manifest.
        assert_eq!(neural_tier_filename(256, 1), "neural_embeddings_256.1.bin");
        assert_eq!(neural_tier_filename(256, 2), "neural_embeddings_256.2.bin");
        assert_ne!(
            neural_tier_filename(256, 1),
            neural_tier_filename(256, 2),
            "same dimension, different generation: never the same file"
        );
        assert_ne!(
            neural_tier_filename(128, 1),
            neural_tier_filename(256, 1),
            "same generation, different dimension: never the same file"
        );
        assert!(is_neural_tier_artifact_name("neural_embeddings_256.1.bin"));
        assert!(is_neural_tier_artifact_name("neural_embeddings_256.bin"));
        assert!(
            !is_neural_tier_artifact_name("neural_embeddings.bin"),
            "the canonical file is not a tier artifact"
        );
        assert!(
            !is_neural_tier_artifact_name(".neural_embeddings_256.bin.staged"),
            "staging files are not tier artifacts"
        );
        assert!(
            !is_neural_tier_artifact_name("neural_embeddings_256.1.2.bin"),
            "two dots is not a filename this build writes"
        );
        assert!(!is_neural_tier_artifact_name("neural_embeddings_x.bin"));
        assert!(!is_neural_tier_artifact_name("neural_embeddings_.bin"));
        assert!(
            !is_neural_tier_artifact_name("neural_embeddings_256.txt"),
            "wrong extension is not ours"
        );
    }

    #[test]
    fn test_republish_sweeps_the_superseded_generation() {
        // Two real publishes into one store. The second must (a) bump the
        // generation, (b) leave the committed files of generation 1 on disk
        // until its own manifest swap, and (c) sweep them after the swap, so
        // the store never accumulates superseded tiers and a reader can only
        // ever reach the latest committed generation.
        let project = tempfile::tempdir().unwrap();
        let storage = project.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        let identity = "remote|openai|same||768";

        let publish = || {
            let mut engine = crate::search::search::SearchEngine::new();
            engine.index_nodes(vec![crate::search::search::NodeInfo {
                node_id: "node-1".into(),
                file_path: "sample.rs".into(),
                symbol_name: "sample".into(),
                language: "rust".into(),
                content: "fn sample() {}".into(),
                byte_range: (0, 14),
                tfidf_embedding: vec![0.5; 768],
                neural_embedding: Some(vec![0.25; 768]),
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            }]);
            persist_neural_embeddings_to_mmap(
                &engine,
                project.path(),
                Some(identity),
                crate::config::MatryoshkaSetting::Off,
            )
            .unwrap();
        };

        publish();
        let manifest1 = try_load_neural_generation_manifest(&storage).expect("gen-1 manifest");
        assert_eq!(manifest1.generation, 1);
        assert!(storage.join("neural_embeddings.bin").is_file());

        publish();
        let manifest2 = try_load_neural_generation_manifest(&storage).expect("gen-2 manifest");
        assert_eq!(manifest2.generation, 2, "generation must advance");

        // Native-only publishes write no sidecars, so the sweep's effect is
        // proven on the manifest's file names instead: the committed set
        // names exactly the current generation's files (no fixed-path
        // leftovers, no other-generation names).
        for entry in &manifest2.tiers {
            let path = storage.join(&entry.file);
            assert!(path.is_file(), "committed tier {} exists", entry.file);
            assert!(
                entry
                    .file
                    .ends_with(&format!(".{}.bin", manifest2.generation))
                    || entry.file == "neural_embeddings.bin",
                "every sidecar name carries the committed generation"
            );
        }
        // Off-policy publishes produce a native-only generation; nothing else
        // tier-shaped may survive.
        for path in neural_tier_artifacts(&storage) {
            let name = path.file_name().unwrap().to_str().unwrap();
            let committed = manifest2.tiers.iter().any(|e| e.file == name);
            assert!(committed, "sweep must leave only committed files ({name})");
        }
    }

    #[test]
    fn test_schema1_manifest_store_falls_back_to_fixed_paths() {
        // A v2.1.1 store: schema-1 manifest + fixed-path sidecars on disk.
        // The v2 reader must ignore the old manifest and serve the legacy
        // files exactly as v2.1.1 did — per-file identity gating, no
        // membership check — so an upgrade never loses its tiers.
        let identity = "remote|openai|same||768";
        let dir = tier_store(Some(identity)); // fixed-path 256 sidecar
        // Write a schema-1 manifest the way v2.1.1 serialized it.
        #[derive(serde::Serialize)]
        struct Schema1Manifest {
            schema_version: u32,
            identity: Option<String>,
            matryoshka: String,
            tiers: Vec<(usize, usize)>,
        }
        let manifest = Schema1Manifest {
            schema_version: 1,
            identity: Some(identity.to_string()),
            matryoshka: format!(
                "{:?}",
                crate::config::LeIndexConfig::load_cached()
                    .indexing
                    .embeddings
                    .effective_matryoshka()
            ),
            tiers: vec![(256, 1)],
        };
        std::fs::write(
            neural_generation_manifest_path(dir.path()),
            bincode::serialize(&manifest).unwrap(),
        )
        .unwrap();
        // Sanity: the v2 reader ignores it.
        assert!(try_load_neural_generation_manifest(dir.path()).is_none());
        let loaded = load_neural_tier_mmaps_from_storage(dir.path(), Some(identity));
        assert_eq!(
            loaded.len(),
            1,
            "a schema-1 store's fixed-path tiers keep serving after upgrade"
        );
        assert_eq!(loaded[0].dimension() as usize, 256);
    }

    #[test]
    fn test_canonical_native_dim_is_the_provider_width() {
        // The canonical file carries the generation's native dimension, and
        // `query_remote_embedder` raw-reads it to seed a query embedder. Both a
        // constant (768 regardless of provider) and the identity trailer (absent,
        // stale, or naming another model) can disagree with the rows actually
        // stored — which is how a 256-dim provider ended up with a canonical
        // header claiming 768.
        let rows = |dim: usize, count: usize| -> Vec<(String, Vec<f32>)> {
            (0..count)
                .map(|i| (format!("node-{i}"), vec![0.5_f32; dim]))
                .collect()
        };
        let empty = tempfile::tempdir().unwrap();

        assert_eq!(
            canonical_native_dim(&rows(1024, 2), empty.path(), None),
            1024,
            "a width the ladder never produces still owns the canonical file"
        );
        assert_eq!(
            canonical_native_dim(&rows(256, 3), empty.path(), None),
            256,
            "a sub-768 provider must not be reported as 768"
        );
        assert_eq!(canonical_native_dim(&rows(768, 1), empty.path(), None), 768);
        assert_eq!(
            canonical_native_dim(&[], empty.path(), None),
            0,
            "no rows and nothing persisted means no width to claim"
        );
    }

    #[test]
    fn test_canonical_native_dim_keeps_the_persisted_width_over_restored_slices() {
        // Fallback hydration restores only SLICED sidecar rows under a fixed
        // sub-native policy and then re-persists them, passing the identity it
        // restored from. Deriving the native width from those rows reports the
        // slice width (512) and narrows the canonical header, so the next restart
        // seeds the query embedder with 512-wide vectors while the identity still
        // says 768 — neural search disabled, and the canonical index dimension
        // wrong even if it were not.
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        crate::search::vector::write_empty_mmap_embeddings_with_identity(
            &storage.join("neural_embeddings.bin"),
            768,
            Some("remote|openai|same||768"),
        )
        .unwrap();

        let sliced: Vec<(String, Vec<f32>)> = (0..3)
            .map(|i| (format!("node-{i}"), vec![0.5_f32; 512]))
            .collect();
        assert_eq!(
            canonical_native_dim(&sliced, dir.path(), Some("remote|openai|same||768")),
            768,
            "the persisted provider width outranks the restored slice width"
        );
    }

    #[test]
    fn test_canonical_native_dim_follows_a_narrower_provider_change() {
        // The mirror image, and the trap the magnitude rule fell into: a project
        // re-embedded against a NARROWER endpoint (768 -> 384) while a 768-wide
        // generation is still on disk. These rows are fresh provider output, not
        // slices, so the persisted width must NOT win. If it did,
        // `partition_rows_by_slice_dimension` would leave the 384-wide rows at
        // their own width, they would be written to `neural_embeddings_384.bin` —
        // a dimension `MRL_TIERS` never enumerates — behind a zero-row 768
        // canonical, and the whole fresh generation would vanish on next load.
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        crate::search::vector::write_empty_mmap_embeddings_with_identity(
            &storage.join("neural_embeddings.bin"),
            768,
            Some("remote|openai|old-endpoint||768"),
        )
        .unwrap();

        let fresh: Vec<(String, Vec<f32>)> = (0..3)
            .map(|i| (format!("node-{i}"), vec![0.5_f32; 384]))
            .collect();
        assert_eq!(
            canonical_native_dim(&fresh, dir.path(), Some("remote|openai|new-endpoint||384")),
            384,
            "a different generation's width must not outrank fresh provider rows"
        );
        assert_eq!(
            canonical_native_dim(&fresh, dir.path(), None),
            384,
            "an unknown incoming identity cannot prove a slice restore"
        );
    }
}
