//! Storage-load hydration path: hydrate the search engine from a persisted
//! snapshot, or rebuild it from the PDG, then finalize neural/persist state.
//! Extracted from the main indexing module to keep it under the line-count gate.

use crate::cli::index_builder;
use crate::cli::leindex::LeIndex;
use anyhow::{Context, Result};
use tracing::{info, warn};

/// What a search snapshot yields before the graph is known.
struct PreparedEngine {
    persisted_embedder: Option<index_builder::TfIdfEmbedder>,
    restored: Option<RestoredEngine>,
}

/// A search engine restored from a snapshot, with the identity of the graph it
/// was built for.
struct RestoredEngine {
    engine: crate::search::search::SearchEngine,
    pdg_nodes: usize,
    pdg_edges: usize,
    pdg_fingerprint: String,
    indexed_count: usize,
}

impl LeIndex {
    pub(crate) fn load_from_storage_inner_at(
        &mut self,
        pdg_only: bool,
        storage_override: Option<&crate::storage::schema::Storage>,
        artifact_path: std::path::PathBuf,
        prebuilt_pdg: Option<crate::graph::pdg::ProgramDependenceGraph>,
    ) -> Result<()> {
        crate::cli::mcp::request_meta::PROJECT_HYDRATIONS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        info!(
            "Loading project from storage: {} (pdg_only={})",
            self.project_id, pdg_only
        );

        // The search snapshot does not depend on the graph until it is
        // *validated* against it, so decode and restore it on another thread
        // while this one reads the graph out of SQLite. The two halves cost
        // about the same, so a cold search pays for one of them, not both.
        let (pdg_loaded, prepared) = std::thread::scope(|scope| {
            let restoring = (!pdg_only).then(|| {
                let path = artifact_path.clone();
                scope.spawn(move || Self::load_snapshot_engine(&path))
            });
            // A prebuilt graph (decoded from the generation's Pdg layer, D4)
            // replaces the SQL catalog read entirely.
            let pdg_loaded = match prebuilt_pdg {
                Some(pdg) => Ok(pdg),
                None => match storage_override {
                    Some(storage) => crate::storage::pdg_store::load_pdg(storage, &self.project_id),
                    None => crate::storage::pdg_store::load_pdg(&self.storage, &self.project_id),
                },
            };
            let prepared = restoring.and_then(|handle| handle.join().ok());
            (pdg_loaded, prepared)
        });
        let mut pdg = pdg_loaded.context("Failed to load PDG from storage")?;
        #[cfg(feature = "community")]
        if let Err(error) = crate::storage::community_store::load_community_memberships(
            storage_override.unwrap_or(&self.storage),
            &self.project_id,
            crate::graph::community::COMMUNITY_ALGORITHM,
            crate::graph::community::COMMUNITY_QUALITY,
            crate::graph::community::COMMUNITY_RESOLUTION,
            &mut pdg,
        ) {
            warn!(%error, "Failed to hydrate persisted community memberships");
        }
        info!(
            "Loaded PDG with {} nodes and {} edges",
            pdg.node_count(),
            pdg.edge_count()
        );

        index_builder::normalize_external_nodes(&mut pdg);

        if pdg_only {
            let pdg_node_count = pdg.node_count();
            let pdg_edge_count = pdg.edge_count();
            // Skip search engine population — caller will call index_nodes() later.
            self.embedder = None;
            self.stats.pdg_nodes = pdg_node_count;
            self.stats.pdg_edges = pdg_edge_count;
            self.pdg = Some(std::sync::Arc::new(pdg));
            return Ok(());
        }

        let prepared = prepared.unwrap_or_else(|| Self::load_snapshot_engine(&artifact_path));
        self.hydrate_search_after_pdg_with(pdg, artifact_path, prepared)
    }

    /// Hydrate the search engine on top of a PDG that is *already resident*.
    ///
    /// A graph-only load followed by a search used to fall back to a complete
    /// `load_from_storage`, reading and rebuilding the whole PDG a second time
    /// just to get the engine. This runs only the engine half. If the graph is
    /// not loaded yet, it loads everything.
    pub(crate) fn hydrate_search_engine_from_loaded_pdg(&mut self) -> Result<()> {
        let Some(pdg) = self.take_owned_pdg() else {
            return self.load_from_storage();
        };
        let artifact_path = self.active_storage_path();
        let prepared = Self::load_snapshot_engine(&artifact_path);
        self.hydrate_search_after_pdg_with(pdg, artifact_path, prepared)
    }

    /// Restore the search engine for `pdg` from a persisted snapshot when it is
    /// current, otherwise rebuild it, then finalize (neural, stats, `self.pdg`).
    fn hydrate_search_after_pdg_with(
        &mut self,
        pdg: crate::graph::pdg::ProgramDependenceGraph,
        artifact_path: std::path::PathBuf,
        prepared: PreparedEngine,
    ) -> Result<()> {
        let persist_artifacts = artifact_path == self.storage_path;
        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();
        let current_pdg_fingerprint = index_builder::pdg_search_fingerprint(&pdg);
        let PreparedEngine {
            persisted_embedder,
            restored,
        } = prepared;

        // Fast path: adopt the restored engine when it matches the current PDG.
        if let Some(restored) = restored {
            if self.install_restored_engine(
                restored,
                &artifact_path,
                pdg_node_count,
                pdg_edge_count,
                &current_pdg_fingerprint,
                persisted_embedder.as_ref(),
            ) {
                self.pdg = Some(std::sync::Arc::new(pdg));
                return Ok(());
            }
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

    /// Read and restore everything a snapshot holds that does not need the
    /// graph: the TF-IDF embedder, the snapshot, the embedding mmaps and the
    /// search engine built from them. Pure with respect to `self`, so it can
    /// run beside the graph load; [`Self::install_restored_engine`] validates
    /// the result against the graph afterwards.
    fn load_snapshot_engine(artifact_path: &std::path::Path) -> PreparedEngine {
        let persisted_embedder =
            index_builder::TfIdfEmbedder::load_from_artifact_path(artifact_path)
                .ok()
                .flatten();
        // Bind the snapshot so the restore below reads one consistent config
        // even if a reload lands mid-operation.
        let config = crate::config::LeIndexConfig::load_cached();
        let restored =
            Self::restore_engine_from_snapshot(artifact_path, persisted_embedder.as_ref(), &config);
        PreparedEngine {
            persisted_embedder,
            restored,
        }
    }

    fn restore_engine_from_snapshot(
        artifact_path: &std::path::Path,
        persisted_embedder: Option<&index_builder::TfIdfEmbedder>,
        config: &crate::config::LeIndexConfig,
    ) -> Option<RestoredEngine> {
        let snapshot = index_builder::try_load_search_snapshot_from_storage(artifact_path)?;
        let tfidf_mmap = index_builder::try_load_mmap_embeddings_from_storage(artifact_path)?;
        persisted_embedder?;

        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let neural_mmap =
            index_builder::try_load_neural_mmap_embeddings_from_storage(artifact_path)
                .map(std::sync::Arc::new);
        #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
        let neural_mmap: Option<std::sync::Arc<crate::search::vector::MmapEmbeddingIndex>> = None;

        // MRL sidecar tiers are independent stores, NOT a projection of the
        // canonical file: under a fixed sub-native Matryoshka policy every row is
        // sliced, so no canonical store is written at all and gating tier loading
        // on `neural_mmap.is_some()` dropped every tier at restart — a fully
        // persistent index silently lost all neural retrieval until a complete
        // re-embed (Codex P1 / CodeRabbit Major). Load whatever sidecars exist;
        // each one validates its own header, and snapshot install filters by node
        // id below.
        //
        // Sidecars ARE gated on the effective Matryoshka setting: a generation
        // persisted while the rollout flag was armed must not keep serving sliced
        // vectors after the flag is switched off, or the deployment has no way to
        // roll the capability back short of a full re-index (Codex P1). When the
        // flag is off we simply do not install them, so search falls back to
        // whatever canonical rows exist.
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let neural_tier_mmaps: Vec<
            std::sync::Arc<crate::search::vector::MmapEmbeddingIndex>,
        > = if config.indexing.embeddings.effective_matryoshka()
            == crate::config::MatryoshkaSetting::Off
        {
            Vec::new()
        } else {
            // Sidecar paths are stable per dimension, so a failed republish can
            // leave a previous generation's sliced vectors beside a canonical
            // file describing the current one. The canonical trailer is the
            // generation marker; a sidecar that disagrees with it is disabled.
            let expected_identity = neural_mmap.as_ref().and_then(|mmap| mmap.identity());
            index_builder::load_neural_tier_mmaps_from_storage(artifact_path, expected_identity)
                .into_iter()
                .map(std::sync::Arc::new)
                .collect()
        };
        #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
        let neural_tier_mmaps: Vec<
            std::sync::Arc<crate::search::vector::MmapEmbeddingIndex>,
        > = Vec::new();

        // Fragment layer (Task 5, invariant 8): thread the fragment mmap + id
        // list into the restore call. `fragment_layer_is_valid` rejects stale
        // roots, so a mismatched artifact can never poison the node-level path.
        let (fragment_mmap, fragment_ids, fragment_store) = Self::load_validated_fragment_layer(
            artifact_path,
            snapshot.fragment_root_hash.as_deref(),
        );

        let (pdg_nodes, pdg_edges, pdg_fingerprint) = (
            snapshot.pdg_nodes,
            snapshot.pdg_edges,
            snapshot.pdg_fingerprint.clone(),
        );
        let mut engine = Self::configured_search_engine();
        // B1.6: node embeddings carry whatever dimension the generation that
        // wrote them used (remote override/measured, or the local model's
        // 1024 after a pre-success local fallback). Validate against the
        // PERSISTED neural mmap header so a generation is never rejected for
        // disagreeing with today's config; config only applies when nothing
        // is persisted.
        #[cfg(feature = "remote-embeddings")]
        let configured_neural_dim = config
            .indexing
            .embeddings
            .resolved_remote()
            .and_then(|remote| remote.remote_dimension);
        #[cfg(not(feature = "remote-embeddings"))]
        let configured_neural_dim = None;
        let expected_neural_dim = neural_mmap
            .as_ref()
            .map(|mmap| mmap.dimension() as usize)
            .or(configured_neural_dim);
        match engine.restore_from_search_snapshot(
            snapshot,
            std::sync::Arc::new(tfidf_mmap),
            neural_mmap,
            neural_tier_mmaps,
            fragment_mmap,
            fragment_ids.as_deref(),
            expected_neural_dim,
        ) {
            Ok(indexed_count) => {
                // Fragment owner mapping (Task 6, invariant 6): content hash →
                // ALL (owner node id, byte range) refs from the store.
                if let Ok(Some(store)) = &fragment_store {
                    engine.set_fragment_refs(fragment_owner_refs(store));
                }
                Some(RestoredEngine {
                    engine,
                    pdg_nodes,
                    pdg_edges,
                    pdg_fingerprint,
                    indexed_count,
                })
            }
            Err(err) => {
                warn!(
                    "Failed to hydrate search index from snapshot; rebuilding from PDG: {}",
                    err
                );
                None
            }
        }
    }

    /// Adopt a restored engine if it describes exactly this graph. Returns
    /// false (leaving `self` untouched) when the snapshot or embedder is stale.
    fn install_restored_engine(
        &mut self,
        restored: RestoredEngine,
        artifact_path: &std::path::Path,
        pdg_node_count: usize,
        pdg_edge_count: usize,
        current_pdg_fingerprint: &str,
        persisted_embedder: Option<&index_builder::TfIdfEmbedder>,
    ) -> bool {
        let Some(tfidf_embedder) = persisted_embedder.cloned() else {
            return false;
        };
        if !(restored.pdg_nodes == pdg_node_count
            && restored.pdg_edges == pdg_edge_count
            && restored.pdg_fingerprint == current_pdg_fingerprint
            && tfidf_embedder.is_fresh(pdg_node_count, pdg_edge_count, current_pdg_fingerprint))
        {
            info!("Search snapshot/embedder stale for current PDG; rebuilding search index");
            return false;
        }
        self.search_engine = restored.engine;

        #[cfg(feature = "onnx")]
        {
            // B1: the query-time embedder follows the same ladder as index
            // construction and enrichment — remote endpoint first (helper
            // logs its own degradation), then local ONNX, never hard-failing.
            // `self.search_engine` is already the restored engine (assigned
            // just above), so passing it lets the gate count the sidecar rows
            // this graph's node IDs can actually reach.
            #[cfg(feature = "remote-embeddings")]
            let decision = query_remote_embedder(
                &tfidf_embedder,
                artifact_path,
                &crate::config::LeIndexConfig::load_cached()
                    .indexing
                    .embeddings,
                &self.search_engine,
            );
            #[cfg(not(feature = "remote-embeddings"))]
            let decision = QueryNeuralDecision::FollowLocal;

            let fallback = index_builder::HybridEmbedder::tfidf_only(tfidf_embedder.clone());
            self.embedder = Some(resolve_query_embedder_from_decision(
                decision,
                tfidf_embedder,
                fallback,
            ));
        }
        #[cfg(not(feature = "onnx"))]
        {
            self.embedder = Some(index_builder::HybridEmbedder::tfidf_only(tfidf_embedder));
        }

        if let Err(err) = self.load_stats_from_path(artifact_path) {
            warn!("Failed to load persisted index stats: {err:#}");
        }
        self.stats.pdg_nodes = pdg_node_count;
        self.stats.pdg_edges = pdg_edge_count;
        self.stats.indexed_nodes = restored.indexed_count;
        self.build_file_stats_cache();
        info!(
            "Hydrated search index from snapshot with {} nodes",
            restored.indexed_count
        );
        true
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
                )?
            } else {
                info!("Persisted embedder is stale; rebuilding TF-IDF index");
                index_builder::index_nodes_tfidf_only(
                    pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
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
        self.restore_persisted_neural_embeddings(artifact_path);

        // Fragment layer (Codex wave-6 item 1): the slow TF-IDF-fallback
        // rebuild path must restore + validate the persisted fragment layer
        // too, not just the fast snapshot-hydrate path. Extracted to a method
        // to keep this function under the CCN-15 gate.
        self.restore_fragment_layer_from_snapshot(artifact_path);

        // VAL-ONNX-001: upgrade to a neural-capable embedder for query time.
        // B1: ladder mirrors construction/enrichment — remote endpoint first,
        // then local ONNX, never hard-failing.
        #[cfg(feature = "onnx")]
        {
            if let Some(tfidf) =
                index_builder::TfIdfEmbedder::load_from_artifact_path(artifact_path)
                    .ok()
                    .flatten()
            {
                // `restore_persisted_neural_embeddings` above has already run,
                // so `self.search_engine` holds this generation's nodes and the
                // gate can count which sidecar rows they actually reach.
                #[cfg(feature = "remote-embeddings")]
                let decision = query_remote_embedder(
                    &tfidf,
                    artifact_path,
                    &crate::config::LeIndexConfig::load_cached()
                        .indexing
                        .embeddings,
                    &self.search_engine,
                );
                #[cfg(not(feature = "remote-embeddings"))]
                let decision = QueryNeuralDecision::FollowLocal;

                self.embedder = Some(resolve_query_embedder_from_decision(
                    decision, tfidf, embedder,
                ));
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
                // The rebuild path only runs when the snapshot identity
                // mismatched; persist the corrected identity (the graph as
                // the layer reconstructs it) so the NEXT hydration takes the
                // fast path instead of looping on the same mismatch.
                let persisted_identity =
                    Some(index_builder::persisted_search_identity_from_graph(&pdg));
                embedder
                    .persist_to_storage(&self.project_path, &pdg, persisted_identity)
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

        self.pdg = Some(std::sync::Arc::new(pdg));
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
        // Record which generation this in-memory state was loaded from so
        // the registry can detect external rebuilds (N-13).
        if let Some(generation) =
            crate::storage::generation::lease::read_current_generation(&self.storage_path)
        {
            self.hydrated_generation
                .store(generation, std::sync::atomic::Ordering::Release);
        }

        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        self.persist_neural_mmap_preserving_identity(artifact_path, persist_artifacts)?;

        Ok(())
    }

    /// Restore neural embeddings from the persisted neural mmap file (if any),
    /// logging the outcome either way. Extracted from finalize_hydration to
    /// keep its branch count under the CCN-15 gate.
    ///
    /// This must restore the MRL sidecars too, not just the canonical file:
    /// `finalize_hydration` re-persists immediately afterwards, and
    /// `persist_neural_embeddings_to_mmap` removes every sidecar whose dimension
    /// is absent from the buckets it just wrote. Restoring only
    /// `neural_embeddings.bin` therefore *deleted* the generation's own sidecars
    /// — a fixed 128/256/512 generation lost every neural row, and Dynamic lost
    /// its 256/512 tiers (Codex P1). Gated on the effective Matryoshka setting so
    /// this path agrees with `restore_engine_from_snapshot`.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    fn restore_persisted_neural_embeddings(&mut self, artifact_path: &std::path::Path) {
        let canonical = index_builder::try_load_neural_mmap_embeddings_from_storage(artifact_path);
        let mut restored = canonical
            .as_ref()
            .map(|neural_mmap| self.search_engine.restore_neural_embeddings(neural_mmap))
            .unwrap_or(0);
        let mut any_store = canonical.is_some();
        if crate::config::LeIndexConfig::load_cached()
            .indexing
            .embeddings
            .effective_matryoshka()
            != crate::config::MatryoshkaSetting::Off
        {
            // Same generation gate as `restore_engine_from_snapshot`: the
            // canonical trailer identifies the generation these sidecars must
            // belong to before their rows are merged into one index.
            let expected_identity = canonical.as_ref().and_then(|mmap| mmap.identity());
            for mmap in
                index_builder::load_neural_tier_mmaps_from_storage(artifact_path, expected_identity)
            {
                any_store = true;
                restored += self.search_engine.restore_neural_embeddings(&mmap);
            }
        }
        if restored > 0 {
            info!(
                "Restored {} neural embeddings from persisted neural mmap file(s)",
                restored
            );
        } else if any_store {
            info!(
                "Neural mmap file loaded but no matching node IDs found; neural scores will be 0"
            );
        } else {
            info!(
                "No persisted neural embeddings found; run 'leindex index --force' with onnx feature to generate them"
            );
        }
    }

    /// Re-persist the resident engine's neural rows under the identity of the
    /// generation they were restored from, skipped when this hydration must
    /// not write artifacts. Extracted from finalize_hydration to keep its
    /// branch count under the CCN-15 gate.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    fn persist_neural_mmap_preserving_identity(
        &self,
        artifact_path: &std::path::Path,
        persist_artifacts: bool,
    ) -> Result<()> {
        if !persist_artifacts {
            return Ok(());
        }
        let neural_identity =
            index_builder::try_load_neural_mmap_embeddings_from_storage(artifact_path)
                .and_then(|mmap| mmap.identity().map(str::to_owned));
        index_builder::persist_neural_embeddings_to_mmap(
            &self.search_engine,
            &self.project_path,
            neural_identity.as_deref(),
            crate::config::LeIndexConfig::load_cached()
                .indexing
                .embeddings
                .effective_matryoshka(),
        )
        .context("Failed to persist neural mmap embeddings during hydration")?;
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

/// Query-time neural embedder decision for a hydrated generation (Codex review round-3 P1).
#[cfg(feature = "onnx")]
#[derive(Debug)]
pub(crate) enum QueryNeuralDecision {
    #[cfg(feature = "remote-embeddings")]
    Remote(Box<index_builder::HybridEmbedder>),
    FollowLocal,
    DisableNeural,
}

#[cfg(feature = "onnx")]
fn resolve_query_embedder_from_decision(
    decision: QueryNeuralDecision,
    tfidf: index_builder::TfIdfEmbedder,
    fallback: index_builder::HybridEmbedder,
) -> index_builder::HybridEmbedder {
    match decision {
        #[cfg(feature = "remote-embeddings")]
        QueryNeuralDecision::Remote(hybrid) => *hybrid,
        QueryNeuralDecision::FollowLocal => {
            let neural_weight = crate::config::LeIndexConfig::load_cached().neural_weight_f32();
            index_builder::HybridEmbedder::hybrid_local(tfidf, Some(neural_weight)).unwrap_or_else(
                |_| {
                    warn!(
                        "Failed to create query-time neural embedder; falling back to tfidf_only"
                    );
                    fallback
                },
            )
        }
        QueryNeuralDecision::DisableNeural => {
            // Deliberate, not a failure: identity or dimension mismatch, or an
            // identity-only header with slicing disabled. The cause is logged
            // where the decision is made; this arm only carries it out.
            warn!("Neural scoring disabled for this generation; falling back to tfidf_only");
            fallback
        }
    }
}

#[cfg(all(feature = "onnx", feature = "remote-embeddings"))]
fn build_remote_decision(
    tfidf: &index_builder::TfIdfEmbedder,
    embeddings_cfg: &crate::config::EmbeddingConfig,
    persisted_dim: Option<usize>,
) -> QueryNeuralDecision {
    let Some(embedder) = index_builder::try_remote_neural_embedder(tfidf.clone(), embeddings_cfg)
    else {
        return if persisted_dim.is_some() {
            QueryNeuralDecision::DisableNeural
        } else {
            QueryNeuralDecision::FollowLocal
        };
    };
    let is_unpinned = embeddings_cfg
        .resolved_remote()
        .and_then(|r| r.remote_dimension)
        .is_none();
    if is_unpinned {
        if let Some(dim) = persisted_dim {
            embedder.seed_measured_dimension(dim);
        }
    }
    QueryNeuralDecision::Remote(Box::new(embedder))
}

/// How many persisted neural rows would actually reach a node under
/// `embeddings_cfg`, counted with the SAME predicate `SearchEngine::
/// restore_neural_embeddings` applies (an entry that exists and is non-empty).
///
/// Counting the store's RAW rows overstates this: a sidecar is keyed by node ID
/// and its path is fixed per dimension, so a generation written for a graph the
/// current index no longer describes can be full of rows that map to nothing.
/// Neural scoring would then be enabled with every cosine zero — the same
/// distortion the zero-row gate exists to prevent (CodeRabbit).
///
/// Returns 0 when the effective policy is `Off` (sidecars are not restored
/// then), when no node is indexed yet, and when every stored row belongs to a
/// node ID this index does not have.
#[cfg(all(feature = "onnx", feature = "remote-embeddings"))]
fn admitted_neural_tier_rows(
    artifact_path: &std::path::Path,
    expected_identity: Option<&str>,
    embeddings_cfg: &crate::config::EmbeddingConfig,
    nodes: &crate::search::search::SearchEngine,
) -> usize {
    if embeddings_cfg.effective_matryoshka() == crate::config::MatryoshkaSetting::Off {
        return 0;
    }
    index_builder::load_neural_tier_mmaps_from_storage(artifact_path, expected_identity)
        .iter()
        .map(|index| rows_reachable_by(index, nodes))
        .sum()
}

/// Rows in `index` that a node of `nodes` can actually be matched to.
///
/// The predicate is deliberately identical to `restore_neural_embeddings`'
/// (`get_embedding` is `Some` and not empty), so the number this reports is the
/// number the restore would install — the two cannot drift.
#[cfg(all(feature = "onnx", feature = "remote-embeddings"))]
fn rows_reachable_by(
    index: &crate::search::vector::MmapEmbeddingIndex,
    nodes: &crate::search::search::SearchEngine,
) -> usize {
    nodes
        .node_ids()
        .filter(|node_id| index.get_embedding(node_id).is_some_and(|e| !e.is_empty()))
        .count()
}

/// Query-time remote embedder for a hydrated generation. The generation's
/// persisted neural rows are authoritative: when their dimension disagrees
/// with the dimension config pins for the remote endpoint, the rows came from
/// another provider (e.g. a pre-success local fallback), so remote must not
/// embed queries against them and neural search is disabled. With no
/// pinned dimension (custom endpoint, measured at first embed) the persisted
/// rows cannot be cross-checked here and remote is kept, seeded with the
/// persisted dimension (Codex review round-3 P1 + P2).
///
/// `nodes` is the hydrated engine, used to count the persisted rows that can
/// actually be matched to one of its node IDs — see `admitted_neural_tier_rows`.
#[cfg(all(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn query_remote_embedder(
    tfidf: &index_builder::TfIdfEmbedder,
    artifact_path: &std::path::Path,
    embeddings_cfg: &crate::config::EmbeddingConfig,
    nodes: &crate::search::search::SearchEngine,
) -> QueryNeuralDecision {
    let Some(mmap) = index_builder::try_load_neural_mmap_embeddings_from_storage(artifact_path)
    else {
        // Nothing persisted at all. That is indistinguishable from a first
        // run, so config alone decides here; the identity-only gate below
        // applies only when a canonical header exists.
        return build_remote_decision(tfidf, embeddings_cfg, None);
    };

    let persisted_identity = mmap.identity();
    let persisted_dim = mmap.dimension() as usize;

    // A tier-only generation persists a ZERO-ROW canonical header that carries
    // only the identity and native dimension. Restoring it is deliberate: the
    // identity lets a later generation resume without re-embedding. But it is a
    // usable NEURAL generation only when the sliced sidecars are restored
    // alongside it — and that restore is gated on the Matryoshka rollout flag
    // (`effective_matryoshka`, the same predicate
    // `restore_persisted_neural_embeddings` applies to the sidecars). After
    // LEINDEX_FEATURE_MATRYOSHKA is disabled, the sidecars are deliberately
    // omitted, so this header is the only persisted neural state: treating its
    // identity as a live generation would seed a query vector whose cosine
    // against every node is zero, and the scorer would apply the configured
    // neural weight to all-zero scores — distorting every non-exact ranking
    // instead of providing the rollback (Codex P1). Disable neural scoring
    // instead. The file is left untouched, so the identity metadata survives
    // and re-enabling the flag resumes the generation.
    //
    // The same decision applies with Matryoshka ENABLED when the sidecars are
    // present but rejected — a policy mismatch in
    // `load_neural_tier_mmaps_from_storage`, a generation-identity mismatch, a
    // row-count disagreement with the committed manifest, or an unreadable tier
    // file all yield an empty loader result. Gating on the flag alone left that
    // shape enabled with zero admitted rows, which reorders results the same way
    // (Codex P2). So the gate is "no rows were admitted, from either store",
    // which is what actually decides whether a neural score can exist.
    //
    // An absent canonical file is NOT this case: it is indistinguishable from a
    // first run, so config decides there (the early return above). And a
    // zero-row header is never accompanied by a NATIVE-dimension tier — the
    // canonical bucket always wins that dimension on persist — so on a native
    // generation with no sidecar this check stays quiet and the identity gets
    // its chance below.
    //
    // Both branches count rows by NODE ID, not by store length. The stores are
    // keyed by node ID and live at fixed paths, so either one can be full of
    // rows belonging to a graph this index no longer describes — `restore_
    // neural_embeddings` skips those, and counting them here would enable
    // neural scoring whose every cosine is zero.
    let admitted_rows = if mmap.is_empty() {
        admitted_neural_tier_rows(artifact_path, persisted_identity, embeddings_cfg, nodes)
    } else {
        rows_reachable_by(&mmap, nodes)
    };
    if admitted_rows == 0 {
        info!(
            dim = persisted_dim,
            "Persisted neural canonical file advertises a generation but admits no rows \
             (zero-row header and no loadable tier for the effective policy); treating it \
             as no neural generation so all-zero scores cannot distort ranking"
        );
        return QueryNeuralDecision::DisableNeural;
    }

    if let Some(identity) = persisted_identity {
        if identity.starts_with("local|") {
            info!(
                identity = ?identity,
                "Persisted neural rows were produced locally; query embedder follows the recorded generation"
            );
            return QueryNeuralDecision::FollowLocal;
        }
        if identity.starts_with("remote|") {
            let configured_identity = configured_remote_identity(embeddings_cfg, persisted_dim);
            if configured_identity.as_deref() == Some(identity) {
                return build_remote_decision(tfidf, embeddings_cfg, Some(persisted_dim));
            }
            info!(
                configured = ?configured_identity,
                persisted = %identity,
                "Persisted neural rows were produced by a different remote model or remote is unconfigured; \
                 disabling neural search"
            );
            return QueryNeuralDecision::DisableNeural;
        }
    }

    let configured_pinned_dim = embeddings_cfg
        .resolved_remote()
        .and_then(|r| r.remote_dimension);
    if let Some(configured_dim) = configured_pinned_dim {
        if configured_dim != persisted_dim {
            info!(
                configured_dim,
                persisted_dim,
                "Persisted neural rows were not produced by the configured remote; \
                 disabling neural search"
            );
            return QueryNeuralDecision::DisableNeural;
        }
    }

    build_remote_decision(tfidf, embeddings_cfg, Some(persisted_dim))
}

/// Identity the configured remote would stamp on its rows, built without
/// network: the provider is constructed from config (no request is made) and
/// its identity is read from the struct. `api_key` is not part of identity.
#[cfg(all(feature = "onnx", feature = "remote-embeddings"))]
fn configured_remote_identity(
    embeddings_cfg: &crate::config::EmbeddingConfig,
    dimension: usize,
) -> Option<String> {
    let resolved = embeddings_cfg.resolved_remote()?;
    let remote_cfg = crate::search::onnx::RemoteEmbeddingConfig {
        provider: crate::search::onnx::RemoteProvider::Custom {
            endpoint: resolved.endpoint,
        },
        model: resolved.model,
        remote_dimension: resolved.remote_dimension,
        doc_template: resolved.doc_template,
        query_template: resolved.query_template,
        base_url: None,
        ..crate::search::onnx::RemoteEmbeddingConfig::default()
    };
    crate::search::onnx::GenericRemoteProvider::from_config(remote_cfg)
        .ok()
        .map(|provider| provider.identity_with_dimension(dimension))
}

#[cfg(all(test, feature = "remote-embeddings"))]
mod tests {
    use super::*;

    /// The one node every fixture generation is built from. `neural_dim` sets
    /// the width of its neural row, which is what `persist_neural_embeddings_
    /// to_mmap` writes the store with.
    fn fixture_node(neural_dim: usize) -> crate::search::search::NodeInfo {
        crate::search::search::NodeInfo {
            node_id: "sample".into(),
            file_path: "sample.rs".into(),
            symbol_name: "sample".into(),
            language: "rust".into(),
            content: "fn sample() {}".into(),
            byte_range: (0, 14),
            tfidf_embedding: vec![0.5; 768],
            neural_embedding: Some(vec![0.25; neural_dim]),
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }
    }

    /// An engine holding the fixture node, so the row-reachability gate sees the
    /// node the fixtures' rows belong to. The identity/dimension tests below are
    /// about later branches; without this they would trip the reachability gate
    /// first and could not prove anything about the branch they name. Only the
    /// node ID is consulted — the embedding width is arbitrary.
    fn fixture_engine() -> crate::search::search::SearchEngine {
        let mut engine = crate::search::search::SearchEngine::new();
        engine.index_nodes(vec![fixture_node(768)]);
        engine
    }

    /// Persist a generation whose neural rows are `neural_dim`-wide.
    fn persist_fixture(project: &std::path::Path, neural_dim: usize) {
        persist_fixture_with_identity(project, neural_dim, None);
    }

    fn persist_fixture_with_identity(
        project: &std::path::Path,
        neural_dim: usize,
        identity: Option<&str>,
    ) {
        let mut engine = crate::search::search::SearchEngine::new();
        engine.index_nodes(vec![fixture_node(neural_dim)]);
        index_builder::persist_embeddings_to_mmap(&engine, project).expect("lexical mmap");
        index_builder::persist_neural_embeddings_to_mmap(
            &engine,
            project,
            identity,
            crate::config::MatryoshkaSetting::Off,
        )
        .expect("neural mmap");
        index_builder::persist_search_snapshot(&engine, project, 1, 0, "fixture".into())
            .expect("snapshot");
    }

    #[test]
    fn test_persisted_local_1024_generation_hydrates_under_preset_768_config() {
        let project = tempfile::tempdir().expect("snapshot fixture");
        persist_fixture(project.path(), 1024);
        // Default config = embeddinggemma-2 preset pinned at 768.
        let config = crate::config::LeIndexConfig::default();
        assert_eq!(
            config
                .indexing
                .embeddings
                .resolved_remote()
                .and_then(|r| r.remote_dimension),
            Some(768)
        );
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let restored = LeIndex::restore_engine_from_snapshot(
            &project.path().join(".leindex"),
            Some(&tfidf),
            &config,
        )
        .expect("persisted 1024-dim generation must hydrate, not loop on a TF-IDF rebuild");
        let neural = restored.engine.collect_neural_embeddings();
        assert_eq!(neural.len(), 1);
        assert_eq!(neural[0].1.len(), 1024);
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_follows_recorded_generation_space() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let cfg = crate::config::EmbeddingConfig::default(); // preset, 768
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;

        let local = tempfile::tempdir().expect("local generation");
        persist_fixture_with_identity(local.path(), 768, Some("local|onnx/model|768"));
        assert!(
            matches!(
                query_remote_embedder(
                    &tfidf,
                    &local.path().join(".leindex"),
                    &cfg,
                    &fixture_engine()
                ),
                QueryNeuralDecision::FollowLocal
            ),
            "local identity must follow local pipeline"
        );

        let remote = tempfile::tempdir().expect("remote generation");
        persist_fixture_with_identity(
            remote.path(),
            768,
            Some(&configured_remote_identity(&cfg, 768).expect("configured identity")),
        );
        assert!(
            matches!(
                query_remote_embedder(
                    &tfidf,
                    &remote.path().join(".leindex"),
                    &cfg,
                    &fixture_engine()
                ),
                QueryNeuralDecision::Remote(_)
            ),
            "a matching remote generation keeps the remote query embedder"
        );

        let legacy = tempfile::tempdir().expect("legacy generation without identity");
        persist_fixture(legacy.path(), 768);
        assert!(
            matches!(
                query_remote_embedder(
                    &tfidf,
                    &legacy.path().join(".leindex"),
                    &cfg,
                    &fixture_engine()
                ),
                QueryNeuralDecision::Remote(_)
            ),
            "absent identity keeps the dimension-only legacy behavior"
        );

        let empty = tempfile::tempdir().expect("no persisted neural rows");
        assert!(
            matches!(
                query_remote_embedder(&tfidf, empty.path(), &cfg, &fixture_engine()),
                QueryNeuralDecision::Remote(_)
            ),
            "nothing persisted: config decides"
        );
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_identity_only_header_disables_neural_when_mrl_off() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        // The generation was written under a configured fixed-slice policy;
        // only the rollout flag differs between the two halves of this test,
        // so the kill switch alone flips the decision. The config must stay on
        // the preset: `effective_matryoshka` requires the embeddinggemma-2
        // preset to be active, which also PINS the remote dimension — so the
        // flag-on arm cannot additionally assert a seeded query dimension
        // (seeding only happens for unpinned custom endpoints, where MRL is
        // inert by construction). The discriminating assertion is the flag-off
        // arm below; this arm pins the gate's scoping: flag on, the header
        // keeps the remote query embedder instead of following local.
        let cfg = crate::config::EmbeddingConfig {
            matryoshka: crate::config::MatryoshkaSetting::Dim128,
            ..crate::config::EmbeddingConfig::default()
        }; // preset, pinned 768, Dim128 policy

        // A tier-only generation persists a ZERO-ROW canonical header carrying
        // the generation identity. Restarting with LEINDEX_FEATURE_MATRYOSHKA
        // disabled must treat that header as NO neural generation: the sidecar
        // restore is gated off, so seeding a query vector from the identity
        // would score every node 0.0 while the scorer still applies the
        // configured neural weight (Codex P1).
        let project = tempfile::tempdir().expect("identity-only generation");
        let storage = project.path().join(".leindex");
        std::fs::create_dir_all(&storage).expect("storage dir");
        let identity = configured_remote_identity(&cfg, 768).expect("configured identity");
        crate::search::vector::write_empty_mmap_embeddings_with_identity(
            &storage.join("neural_embeddings.bin"),
            768,
            Some(&identity),
        )
        .expect("write identity-only header");

        crate::feature_flags::with_flag_override(
            crate::feature_flags::FeatureFlag::Matryoshka,
            false,
            || {
                assert!(
                    matches!(
                        query_remote_embedder(&tfidf, &storage, &cfg, &fixture_engine()),
                        QueryNeuralDecision::DisableNeural
                    ),
                    "identity-only header with Matryoshka disabled must disable neural scoring"
                );
            },
        );

        // The gate is about ADMITTED ROWS, not about emptiness or the flag. With
        // the flag on but no sidecar on disk there is still nothing to score, so
        // flag-on does not by itself make the header a live generation — it
        // returns `DisableNeural` for the same all-zero-score reason the flag-off
        // arm does. What flag-on must NOT do is overfire once rows exist, which
        // the sidecar added here proves: one admissible tier and the header keeps
        // its remote query embedder.
        crate::search::vector::write_mmap_embeddings_with_identity(
            &storage.join("neural_embeddings_128.bin"),
            &[("sample".to_string(), vec![0.5; 128])],
            Some(&identity),
        )
        .expect("write admissible tier");
        crate::feature_flags::with_flag_override(
            crate::feature_flags::FeatureFlag::Matryoshka,
            true,
            || {
                // Variant-level assert, deliberately: with a pinned preset,
                // Remote-from-header and Remote-from-nothing are observably
                // identical at this seam (no seeding happens), so the stronger
                // discriminator is not expressible here. Flag-on still pins
                // real behavior — pre-fix this branch returned FollowLocal,
                // so Remote(_) proves the gate did not overfire AND that the
                // identity branch was reached rather than skipped.
                assert!(
                    matches!(
                        query_remote_embedder(&tfidf, &storage, &cfg, &fixture_engine()),
                        QueryNeuralDecision::Remote(_)
                    ),
                    "an identity-only header with an ADMISSIBLE tier is a live generation"
                );
            },
        );
    }

    /// Matryoshka ENABLED, header present, sidecar present but INADMISSIBLE: no
    /// row reaches the query, so neural scoring must be disabled or the scorer
    /// applies the configured neural weight to all-zero cosines — the same
    /// distortion the flag-off gate prevents. Gating on the rollback flag alone
    /// let this shape through (Codex P2).
    ///
    /// The sidecar is rejected on its generation identity, which is the property
    /// this test is about: a foreign-generation tier contributes no rows and must
    /// not be mistaken for a live generation just because a header names one.
    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_disables_neural_when_tiers_are_inadmissible() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let cfg = crate::config::EmbeddingConfig {
            matryoshka: crate::config::MatryoshkaSetting::Dim128,
            ..crate::config::EmbeddingConfig::default()
        };

        let project = tempfile::tempdir().expect("tier-only generation");
        let storage = project.path().join(".leindex");
        std::fs::create_dir_all(&storage).expect("storage dir");
        let identity = configured_remote_identity(&cfg, 768).expect("configured identity");
        // Zero-row header: the generation identity and native dimension survive,
        // but no rows do.
        crate::search::vector::write_empty_mmap_embeddings_with_identity(
            &storage.join("neural_embeddings.bin"),
            768,
            Some(&identity),
        )
        .expect("write identity-only header");
        // A real MRL tier from a DIFFERENT generation: the loader's identity gate
        // rejects it, so the admitted row count is zero while the header still
        // advertises a generation. (A native-dimension sidecar would NOT do here:
        // the duplicate check keys off a non-empty canonical, so a zero-row header
        // leaves it admitted.)
        crate::search::vector::write_mmap_embeddings_with_identity(
            &storage.join("neural_embeddings_128.bin"),
            &[("sample".to_string(), vec![0.5; 128])],
            Some("remote|other-provider|other-model|128"),
        )
        .expect("write foreign-generation tier");

        crate::feature_flags::with_flag_override(
            crate::feature_flags::FeatureFlag::Matryoshka,
            true,
            || {
                assert_eq!(
                    admitted_neural_tier_rows(&storage, Some(&identity), &cfg, &fixture_engine()),
                    0,
                    "the foreign-generation tier must be rejected, or this test proves nothing"
                );
                assert!(
                    matches!(
                        query_remote_embedder(&tfidf, &storage, &cfg, &fixture_engine()),
                        QueryNeuralDecision::DisableNeural
                    ),
                    "a generation that admits no neural rows must not enable neural scoring"
                );
            },
        );
    }

    /// A persisted store can be full of rows and still reach NOTHING: the stores
    /// are keyed by node ID, and `restore_neural_embeddings` only updates nodes it
    /// can match. A generation written for a graph this index no longer describes
    /// therefore restores zero embeddings while the store looks healthy, and
    /// gating on the store's length instead of the reachable rows enabled neural
    /// scoring whose every cosine is zero — the same distortion the zero-row gate
    /// exists to prevent, reached through the other door (CodeRabbit).
    ///
    /// Both stores are covered, because the canonical branch had the identical
    /// bug: fixing only the sidecar branch would have left the twin.
    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_disables_neural_when_rows_reach_no_indexed_node() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let cfg = crate::config::EmbeddingConfig::default(); // preset, 768
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;
        let identity = configured_remote_identity(&cfg, 768).expect("configured identity");

        // An engine holding a node the stores' rows DO belong to. This is the
        // positive control: it proves the rows load and the generation is
        // otherwise live, so the assertions below fail for the reason they name
        // rather than because the fixture is unreadable.
        let mut matching = crate::search::search::SearchEngine::new();
        let mut orphaned = fixture_node(768);
        orphaned.node_id = "ghost".into();
        matching.index_nodes(vec![orphaned]);

        let canonical = tempfile::tempdir().expect("orphaned canonical store");
        let canonical_storage = canonical.path().join(".leindex");
        std::fs::create_dir_all(&canonical_storage).expect("storage dir");
        crate::search::vector::write_mmap_embeddings_with_identity(
            &canonical_storage.join("neural_embeddings.bin"),
            &[("ghost".to_string(), vec![0.5; 768])],
            Some(&identity),
        )
        .expect("write orphaned canonical");
        assert!(
            matches!(
                query_remote_embedder(&tfidf, &canonical_storage, &cfg, &matching),
                QueryNeuralDecision::Remote(_)
            ),
            "control: a row the index HAS must keep the remote query embedder"
        );
        assert!(
            matches!(
                query_remote_embedder(&tfidf, &canonical_storage, &cfg, &fixture_engine()),
                QueryNeuralDecision::DisableNeural
            ),
            "a canonical store whose rows match no indexed node restores nothing and must \
             not enable neural scoring"
        );

        // Same shape through a tier sidecar: zero-row canonical header (so the
        // sidecar branch is the one that decides) plus a sidecar whose row
        // belongs to a node this index does not have.
        let tiered = tempfile::tempdir().expect("orphaned tier store");
        let tiered_storage = tiered.path().join(".leindex");
        std::fs::create_dir_all(&tiered_storage).expect("storage dir");
        crate::search::vector::write_empty_mmap_embeddings_with_identity(
            &tiered_storage.join("neural_embeddings.bin"),
            768,
            Some(&identity),
        )
        .expect("write identity-only header");
        crate::search::vector::write_mmap_embeddings_with_identity(
            &tiered_storage.join("neural_embeddings_128.bin"),
            &[("ghost".to_string(), vec![0.5; 128])],
            Some(&identity),
        )
        .expect("write orphaned tier");
        let tiered_cfg = crate::config::EmbeddingConfig {
            matryoshka: crate::config::MatryoshkaSetting::Dim128,
            ..crate::config::EmbeddingConfig::default()
        };
        // `effective_matryoshka` also requires the rollout flag, so without this
        // the policy reads `Off` and `admitted_neural_tier_rows` returns 0 for
        // BOTH engines — the control below would fail, and neither assertion
        // would be about reachability.
        // Set through the lock-free setter, not `with_flag_override`: this test
        // already holds FLAG_TEST_LOCK for its whole duration, and
        // `with_flag_override` takes that same non-reentrant mutex — calling it
        // here self-deadlocks the test and every sibling queued behind it.
        // `_reset` above clears the override on drop.
        crate::feature_flags::set_flag_override_for_test(
            crate::feature_flags::FeatureFlag::Matryoshka,
            true,
        );
        assert_eq!(
            admitted_neural_tier_rows(&tiered_storage, Some(&identity), &tiered_cfg, &matching),
            1,
            "control: an admissible tier whose node IS indexed must be counted"
        );
        assert_eq!(
            admitted_neural_tier_rows(
                &tiered_storage,
                Some(&identity),
                &tiered_cfg,
                &fixture_engine()
            ),
            0,
            "a tier whose rows match no indexed node admits nothing"
        );
        assert!(
            matches!(
                query_remote_embedder(&tfidf, &tiered_storage, &tiered_cfg, &fixture_engine()),
                QueryNeuralDecision::DisableNeural
            ),
            "a tier-only generation whose rows match no indexed node must not enable \
             neural scoring"
        );
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_rejects_same_dimension_remote_model_swap() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let cfg = crate::config::EmbeddingConfig::default(); // preset, 768
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;

        let swapped = tempfile::tempdir().expect("generation written by another model");
        let mut other_model = cfg.clone();
        other_model.remote_model = Some("other-model-768".to_string());
        let other_identity = configured_remote_identity(&other_model, 768).expect("other identity");
        persist_fixture_with_identity(swapped.path(), 768, Some(&other_identity));
        assert!(
            matches!(
                query_remote_embedder(
                    &tfidf,
                    &swapped.path().join(".leindex"),
                    &cfg,
                    &fixture_engine()
                ),
                QueryNeuralDecision::DisableNeural
            ),
            "same-dimension remote generation from another model must disable neural"
        );
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_keeps_unpinned_custom_endpoint_at_persisted_dimension() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let cfg = crate::config::EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "custom".to_string(),
            remote_endpoint: Some("http://127.0.0.1:1/v1".to_string()),
            remote_model: Some("measured-model".to_string()),
            remote_dimension: None,
            remote_doc_template: None,
            remote_query_template: None,
            ..Default::default()
        };
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;

        let project = tempfile::tempdir().expect("unpinned custom generation");
        let persisted_dimension = 1024;
        let persisted_identity =
            configured_remote_identity(&cfg, persisted_dimension).expect("configured identity");
        persist_fixture_with_identity(
            project.path(),
            persisted_dimension,
            Some(&persisted_identity),
        );

        assert!(
            matches!(
                query_remote_embedder(
                    &tfidf,
                    &project.path().join(".leindex"),
                    &cfg,
                    &fixture_engine()
                ),
                QueryNeuralDecision::Remote(_)
            ),
            "unpinned custom endpoint must compare identity using the persisted dimension"
        );
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_remote_identity_with_remote_config_removed_disables_neural() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let cfg = crate::config::EmbeddingConfig::default(); // preset, 768
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;

        let remote = tempfile::tempdir().expect("remote generation");
        let remote_identity = configured_remote_identity(&cfg, 768).expect("configured identity");
        persist_fixture_with_identity(remote.path(), 768, Some(&remote_identity));

        let mut unconfigured = cfg.clone();
        unconfigured.enable_hybrid = false;
        assert!(
            matches!(
                query_remote_embedder(
                    &tfidf,
                    &remote.path().join(".leindex"),
                    &unconfigured,
                    &fixture_engine()
                ),
                QueryNeuralDecision::DisableNeural
            ),
            "remote generation when remote config is removed must disable neural"
        );
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_query_embedder_seeds_unpinned_provider_with_persisted_dimension() {
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let cfg = crate::config::EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "custom".to_string(),
            remote_endpoint: Some("http://127.0.0.1:1/v1".to_string()),
            remote_model: Some("seeded-model".to_string()),
            remote_dimension: None,
            remote_doc_template: None,
            remote_query_template: None,
            ..Default::default()
        };
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;

        let project = tempfile::tempdir().expect("unpinned custom generation 512");
        let persisted_dimension = 512;
        let persisted_identity =
            configured_remote_identity(&cfg, persisted_dimension).expect("configured identity");
        persist_fixture_with_identity(
            project.path(),
            persisted_dimension,
            Some(&persisted_identity),
        );

        let decision = query_remote_embedder(
            &tfidf,
            &project.path().join(".leindex"),
            &cfg,
            &fixture_engine(),
        );
        let QueryNeuralDecision::Remote(embedder) = decision else {
            panic!("expected Remote decision, got {:?}", decision);
        };
        assert_eq!(
            embedder.observed_neural_dimension(),
            Some(512),
            "unpinned provider must be seeded with persisted dimension 512"
        );
    }

    #[test]
    fn test_fresh_preset_restores_768_neural_snapshot_without_dimension_override() {
        let project = tempfile::tempdir().expect("snapshot fixture");
        let config = crate::config::LeIndexConfig::default();
        assert_eq!(config.indexing.embeddings.remote_dimension, None);
        let tfidf =
            index_builder::TfIdfEmbedder::build(&[("sample.rs".into(), "fn sample() {}".into())]);
        let mut engine = crate::search::search::SearchEngine::new();
        engine.index_nodes(vec![crate::search::search::NodeInfo {
            node_id: "sample".into(),
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
        index_builder::persist_embeddings_to_mmap(&engine, project.path()).expect("lexical mmap");
        index_builder::persist_neural_embeddings_to_mmap(
            &engine,
            project.path(),
            None,
            crate::config::MatryoshkaSetting::Off,
        )
        .expect("neural mmap");
        index_builder::persist_search_snapshot(&engine, project.path(), 1, 0, "fixture".into())
            .expect("snapshot");
        let restored = LeIndex::restore_engine_from_snapshot(
            &project.path().join(".leindex"),
            Some(&tfidf),
            &config,
        )
        .expect("preset dimension must hydrate without falling back to local 1024");
        assert_eq!(restored.indexed_count, 1);
        let neural = restored.engine.collect_neural_embeddings();
        assert_eq!(neural.len(), 1);
        assert_eq!(neural[0].1.len(), 768);
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
