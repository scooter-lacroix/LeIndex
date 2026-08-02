//! Storage-load hydration path: hydrate the search engine from a persisted
//! snapshot, or rebuild it from the PDG, then finalize neural/persist state.
//! Extracted from the main indexing module to keep it under the line-count gate.

use crate::cli::index_builder;
use crate::cli::leindex::LeIndex;
use anyhow::{Context, Result};
use tracing::{info, warn};

impl LeIndex {
    pub(crate) fn load_from_storage_inner_at(
        &mut self,
        pdg_only: bool,
        storage_override: Option<&crate::storage::schema::Storage>,
        artifact_path: std::path::PathBuf,
    ) -> Result<()> {
        crate::cli::mcp::request_meta::PROJECT_HYDRATIONS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        info!(
            "Loading project from storage: {} (pdg_only={})",
            self.project_id, pdg_only
        );

        let mut pdg = match storage_override {
            Some(storage) => crate::storage::pdg_store::load_pdg(storage, &self.project_id),
            None => crate::storage::pdg_store::load_pdg(&self.storage, &self.project_id),
        }
        .context("Failed to load PDG from storage")?;
        let persist_artifacts = artifact_path == self.storage_path;

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!(
            "Loaded PDG with {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );

        index_builder::normalize_external_nodes(&mut pdg);

        if pdg_only {
            // Skip search engine population — caller will call index_nodes() later.
            self.embedder = None;
            self.stats.pdg_nodes = pdg_node_count;
            self.stats.pdg_edges = pdg_edge_count;
            self.pdg = Some(pdg);
            return Ok(());
        }

        let persisted_embedder =
            index_builder::TfIdfEmbedder::load_from_artifact_path(&artifact_path)
                .ok()
                .flatten();
        let current_pdg_fingerprint = index_builder::pdg_search_fingerprint(&pdg);

        // Fast path: hydrate the search engine from a persisted snapshot when it
        // matches the current PDG and a fresh TF-IDF mmap/embedder are available.
        if self.try_hydrate_from_snapshot(
            &artifact_path,
            pdg_node_count,
            pdg_edge_count,
            &current_pdg_fingerprint,
            persisted_embedder.as_ref(),
        ) {
            self.pdg = Some(pdg);
            return Ok(());
        }

        // Slow path: rebuild the TF-IDF index from the PDG, then finalize.
        let embedder = self.rebuild_search_index_from_pdg(
            &pdg,
            persisted_embedder,
            pdg_node_count,
            pdg_edge_count,
        )?;
        self.finalize_hydration(
            &artifact_path,
            pdg,
            embedder,
            pdg_node_count,
            pdg_edge_count,
            current_pdg_fingerprint,
            persist_artifacts,
        )
    }

    /// Try to hydrate the search engine from a persisted snapshot. Returns true
    /// when hydration succeeded (embedder/stats set; caller assigns `self.pdg`
    /// and returns), false when the snapshot is absent/stale/mismatched and the
    /// caller must rebuild from the PDG.
    fn try_hydrate_from_snapshot(
        &mut self,
        artifact_path: &std::path::Path,
        pdg_node_count: usize,
        pdg_edge_count: usize,
        current_pdg_fingerprint: &str,
        persisted_embedder: Option<&index_builder::TfIdfEmbedder>,
    ) -> bool {
        let Some(snapshot) = index_builder::try_load_search_snapshot_from_storage(artifact_path)
        else {
            return false;
        };
        let Some(tfidf_mmap) = index_builder::try_load_mmap_embeddings_from_storage(artifact_path)
        else {
            return false;
        };
        let Some(tfidf_embedder) = persisted_embedder.cloned() else {
            return false;
        };
        if !(snapshot.pdg_nodes == pdg_node_count
            && snapshot.pdg_edges == pdg_edge_count
            && snapshot.pdg_fingerprint == current_pdg_fingerprint
            && tfidf_embedder.is_fresh(pdg_node_count, pdg_edge_count, current_pdg_fingerprint))
        {
            info!("Search snapshot/embedder stale for current PDG; rebuilding search index");
            return false;
        }

        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let neural_mmap =
            index_builder::try_load_neural_mmap_embeddings_from_storage(artifact_path)
                .map(std::sync::Arc::new);
        #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
        let neural_mmap: Option<std::sync::Arc<crate::search::vector::MmapEmbeddingIndex>> = None;

        // Fragment layer (Task 5, invariant 8): thread the fragment mmap + id
        // list into the restore call. The rich fragment store (owner/file/byte
        // metadata) is wired in Task 7; until fragment_store.bin exists the id
        // list is empty and the fragment layer stays off (feature-off
        // compatible). `fragment_layer_is_valid` rejects stale roots, so a
        // mismatched artifact can never poison the node-level path. Shared with
        // the slow TF-IDF-fallback rebuild path (Codex wave-6 item 1).
        let (fragment_mmap, fragment_ids, fragment_store) = Self::load_validated_fragment_layer(
            artifact_path,
            snapshot.fragment_root_hash.as_deref(),
        );

        match self.search_engine.restore_from_search_snapshot(
            snapshot,
            std::sync::Arc::new(tfidf_mmap),
            neural_mmap,
            fragment_mmap,
            fragment_ids.as_deref(),
        ) {
            Ok(indexed_count) => {
                #[cfg(feature = "onnx")]
                {
                    match index_builder::HybridEmbedder::hybrid_local(
                        tfidf_embedder,
                        Some(crate::config::LeIndexConfig::load_cached().neural_weight_f32()),
                    ) {
                        Ok(hybrid) => self.embedder = Some(hybrid),
                        Err(e) => {
                            warn!(
                                "Failed to create hybrid_local embedder for query embedding: {}",
                                e
                            );
                            self.embedder = persisted_embedder
                                .cloned()
                                .map(index_builder::HybridEmbedder::tfidf_only);
                        }
                    }
                }
                #[cfg(not(feature = "onnx"))]
                {
                    self.embedder = Some(index_builder::HybridEmbedder::tfidf_only(tfidf_embedder));
                }

                // Fragment owner mapping (Task 6, invariant 6): content hash →
                // ALL (owner node id, byte range) refs from the store, used at
                // query time to map fragment hits back to their Tier-1 owners.
                // A Vec per hash because identical content can legitimately
                // live under N owners — dedup must not collapse multi-owner
                // fragments to the first (Codex wave-2 item 5).
                if let Ok(Some(store)) = &fragment_store {
                    self.search_engine
                        .set_fragment_refs(fragment_owner_refs(store));
                }

                if let Err(err) = self.load_stats_from_path(artifact_path) {
                    warn!("Failed to load persisted index stats: {err:#}");
                }
                self.stats.pdg_nodes = pdg_node_count;
                self.stats.pdg_edges = pdg_edge_count;
                self.stats.indexed_nodes = indexed_count;
                self.build_file_stats_cache();
                info!(
                    "Hydrated search index from snapshot with {} nodes",
                    indexed_count
                );
                true
            }
            Err(err) => {
                warn!(
                    "Failed to hydrate search index from snapshot; rebuilding from PDG: {}",
                    err
                );
                false
            }
        }
    }

    /// Rebuild the TF-IDF search index from the PDG. Reuses a persisted embedder
    /// when fresh, rebuilds the vocabulary when stale/absent. Returns the embedder
    /// to use for query-time embedding. Neural embeddings are restored separately.
    fn rebuild_search_index_from_pdg(
        &mut self,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        persisted_embedder: Option<index_builder::TfIdfEmbedder>,
        pdg_node_count: usize,
        pdg_edge_count: usize,
    ) -> Result<index_builder::HybridEmbedder> {
        let batch_size = self.indexing_batch_size();
        Ok(if let Some(embedder) = persisted_embedder {
            let fp = crate::cli::index_builder::pdg_search_fingerprint(pdg);
            if embedder.is_fresh(pdg_node_count, pdg_edge_count, &fp) {
                info!("Loaded persisted embedder from storage");
                // Use tfidf_only during load_from_storage to avoid expensive
                // batch neural embedding; neural embeddings are restored below.
                let tfidf_embedder = index_builder::HybridEmbedder::tfidf_only(embedder);
                index_builder::index_nodes_tfidf_only(
                    pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    Some(tfidf_embedder),
                    None,
                )?
            } else {
                info!("Persisted embedder is stale; rebuilding TF-IDF index");
                index_builder::index_nodes_tfidf_only(
                    pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    None,
                    None,
                )?
            }
        } else {
            // No persisted embedder; build a fresh vocab.
            index_builder::index_nodes_tfidf_only(
                pdg,
                &mut self.search_engine,
                &mut self.cache.file_stats_cache,
                batch_size,
                None,
                None,
            )?
        })
    }

    /// Finalize hydration after a TF-IDF rebuild: restore neural embeddings,
    /// upgrade the embedder to hybrid_local for query-time neural, persist
    /// artifacts, and set final stats.
    fn finalize_hydration(
        &mut self,
        artifact_path: &std::path::Path,
        pdg: crate::graph::pdg::ProgramDependenceGraph,
        embedder: index_builder::HybridEmbedder,
        pdg_node_count: usize,
        pdg_edge_count: usize,
        current_pdg_fingerprint: String,
        persist_artifacts: bool,
    ) -> Result<()> {
        // Restore neural embeddings from the persisted neural mmap file (if any).
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            if let Some(neural_mmap) =
                index_builder::try_load_neural_mmap_embeddings_from_storage(artifact_path)
            {
                let restored = self.search_engine.restore_neural_embeddings(&neural_mmap);
                if restored > 0 {
                    info!(
                        "Restored {} neural embeddings from persisted neural mmap file",
                        restored
                    );
                } else {
                    info!(
                        "Neural mmap file loaded but no matching node IDs found; neural scores will be 0"
                    );
                }
            } else {
                info!(
                    "No persisted neural embeddings found; run 'leindex index --force' with onnx feature to generate them"
                );
            }
        }

        // Fragment layer (Codex wave-6 item 1): the slow TF-IDF-fallback
        // rebuild path must restore + validate the persisted fragment layer
        // too, not just the fast snapshot-hydrate path. Extracted to a method
        // to keep this function under the CCN-15 gate.
        self.restore_fragment_layer_from_snapshot(artifact_path);

        // VAL-ONNX-001: upgrade to hybrid_local for query-time neural embedding.
        #[cfg(feature = "onnx")]
        {
            if let Some(tfidf) =
                index_builder::TfIdfEmbedder::load_from_artifact_path(artifact_path)
                    .ok()
                    .flatten()
            {
                match index_builder::HybridEmbedder::hybrid_local(
                    tfidf,
                    Some(crate::config::LeIndexConfig::load_cached().neural_weight_f32()),
                ) {
                    Ok(hybrid) => self.embedder = Some(hybrid),
                    Err(e) => {
                        warn!(
                            "Failed to create hybrid_local embedder for query embedding: {}",
                            e
                        );
                        self.embedder = Some(embedder);
                    }
                }
            } else {
                self.embedder = Some(embedder);
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            self.embedder = Some(embedder);
        }

        if persist_artifacts {
            if let Some(embedder) = &self.embedder {
                embedder
                    .persist_to_storage(&self.project_path, &pdg)
                    .context("Failed to persist TF-IDF embedder during hydration")?;
            }
        }
        let indexed_count = self.search_engine.node_count();

        info!("Rebuilt search index with {} nodes", indexed_count);

        // Load persisted stats first, then overwrite live PDG/search counts.
        if let Err(err) = self.load_stats_from_path(artifact_path) {
            warn!("Failed to load persisted index stats: {err:#}");
        }
        self.stats.pdg_nodes = pdg_node_count;
        self.stats.pdg_edges = pdg_edge_count;
        self.stats.indexed_nodes = indexed_count;

        self.pdg = Some(pdg);
        self.build_file_stats_cache();

        // R10: Persist embeddings to mmap file for fast read-only access.
        if persist_artifacts {
            index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)
                .context("Failed to persist TF-IDF mmap embeddings during hydration")?;
            index_builder::persist_search_snapshot(
                &self.search_engine,
                &self.project_path,
                pdg_node_count,
                pdg_edge_count,
                current_pdg_fingerprint,
            )
            .context("Failed to persist search snapshot during hydration")?;
        }
        // Persist neural embeddings separately for fast load_from_storage.
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            if !persist_artifacts {
                return Ok(());
            }
            index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
            )
            .context("Failed to persist neural mmap embeddings during hydration")?;
        }

        Ok(())
    }
}

impl LeIndex {
    /// Load + validate the persisted fragment-layer artifacts (fragment mmap,
    /// store, and content-hash id list) against the snapshot's recorded
    /// fragment root (invariant 8).
    ///
    /// Shared by the fast snapshot-hydrate path and the slow TF-IDF-fallback
    /// rebuild path (Codex wave-6 item 1) so BOTH restore an otherwise-valid
    /// fragment layer after a cold start. Returns `(fragment_mmap,
    /// fragment_ids, fragment_store)`; the two Options are `None` when the
    /// layer is absent or fails validation (callers keep the fragment layer
    /// off — feature-off compatible).
    pub(super) fn load_validated_fragment_layer(
        artifact_path: &std::path::Path,
        snapshot_fragment_root: Option<&str>,
    ) -> (
        Option<std::sync::Arc<crate::search::vector::MmapEmbeddingIndex>>,
        Option<Vec<String>>,
        Result<Option<index_builder::fragment::FragmentStore>, anyhow::Error>,
    ) {
        let fragment_mmap_raw =
            index_builder::try_load_fragment_mmap_embeddings_from_storage(artifact_path);
        let fragment_store =
            index_builder::fragment::FragmentStore::load_from_artifact_path(artifact_path);
        let fragment_ids: Option<Vec<String>> = match &fragment_store {
            Ok(Some(store)) => Some(store.content_hashes().map(str::to_string).collect()),
            Ok(None) => None,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to load fragment store; fragment layer disabled"
                );
                None
            }
        };
        let fragment_layer_ok = fragment_ids.is_some()
            && index_builder::fragment_layer_is_valid(
                snapshot_fragment_root,
                fragment_mmap_raw.as_ref(),
                artifact_path,
            );
        let fragment_mmap = match (fragment_layer_ok, fragment_mmap_raw) {
            (true, Some(mmap)) => Some(std::sync::Arc::new(mmap)),
            _ => None,
        };
        let fragment_ids = if fragment_layer_ok {
            fragment_ids
        } else {
            None
        };
        (fragment_mmap, fragment_ids, fragment_store)
    }

    /// Restore the persisted fragment layer after a TF-IDF-fallback rebuild
    /// (Codex wave-6 item 1).
    ///
    /// The slow path must restore + validate the persisted fragment layer
    /// too, not just the fast snapshot-hydrate path. An otherwise-valid
    /// fragment layer was silently dropped for the process lifetime after a
    /// cold start whose TF-IDF mmap was missing/corrupt — and on the
    /// mutable-storage path, finalization could then persist a
    /// fragment-free engine and remove the valid fragment mmap. The persisted
    /// snapshot (even when stale for the PDG) still records the fragment root
    /// the artifacts were synced against, so it is the correct validation
    /// reference here. Row-count/dimension validation is delegated to
    /// `SearchEngine::restore_fragment_index`; failure is non-fatal and
    /// leaves the fragment layer off (invariant 3).
    fn restore_fragment_layer_from_snapshot(&mut self, artifact_path: &std::path::Path) {
        let Some(snapshot) = index_builder::try_load_search_snapshot_from_storage(artifact_path)
        else {
            return;
        };
        let (fragment_mmap, fragment_ids, fragment_store) = Self::load_validated_fragment_layer(
            artifact_path,
            snapshot.fragment_root_hash.as_deref(),
        );
        let (Some(mmap), Some(ids)) = (fragment_mmap, fragment_ids) else {
            return;
        };
        self.search_engine
            .restore_fragment_index(Some(mmap), Some(&ids), snapshot.fragment_rows);
        if let Ok(Some(store)) = &fragment_store {
            self.search_engine
                .set_fragment_refs(fragment_owner_refs(store));
        }
        info!(
            rows = ids.len(),
            "Restored fragment layer after TF-IDF-fallback rebuild"
        );
    }
}

/// Build the fragment owner-ref map (content hash → ALL (owner node id, byte
/// range) refs) from the store (invariant 6). A Vec per hash because identical
/// content can live under N owners — dedup must not collapse multi-owner
/// fragments to the first (Codex wave-2 item 5).
fn fragment_owner_refs(
    store: &index_builder::fragment::FragmentStore,
) -> std::collections::HashMap<String, Vec<(String, (usize, usize))>> {
    store
        .content_hashes()
        .filter_map(|hash| {
            let owners: Vec<(String, (usize, usize))> = store
                .get(hash)
                .into_iter()
                .flatten()
                .filter_map(|meta| {
                    meta.owner
                        .as_ref()
                        .map(|owner| (owner.clone(), meta.byte_range))
                })
                .collect();
            (!owners.is_empty()).then(|| (hash.to_string(), owners))
        })
        .collect()
}
