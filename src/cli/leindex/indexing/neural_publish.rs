use super::*;

impl LeIndex {
    #[cfg(feature = "onnx")]
    pub(super) fn configured_neural_embedder(
        &self,
    ) -> Result<Option<index_builder::HybridEmbedder>> {
        let mut embedder = index_builder::HybridEmbedder::hybrid_local(
            self.embedder
                .as_ref()
                .context("core embedder is set before neural enrichment")?
                .tfidf()
                .clone(),
            Some(crate::config::LeIndexConfig::load_cached().neural_weight_f32()),
        )
        .ok();
        if let Some(reason) = embedder
            .as_ref()
            .and_then(index_builder::HybridEmbedder::cpu_fallback_reason)
        {
            tracing::warn!("{}", reason);
            embedder = None;
        }
        if crate::config::LeIndexConfig::load_cached()
            .search
            .search_mode
            == "text"
        {
            embedder = None;
        }
        // Feature-flag rollout gate: a deployment can kill neural indexing via
        // LEINDEX_FEATURE_NEURAL_SEARCH=false even when config enables it.
        // `is_neural_enabled` ANDs the runtime flag (default-on for this GA
        // feature) with the config knob, so a disabled config stays disabled.
        if !crate::feature_flags::is_neural_enabled(
            crate::config::LeIndexConfig::load_cached().neural.enabled,
        ) {
            embedder = None;
        }
        Ok(embedder)
    }

    #[cfg(not(feature = "onnx"))]
    pub(super) fn configured_neural_embedder(
        &self,
    ) -> Result<Option<index_builder::HybridEmbedder>> {
        Ok(None)
    }

    pub(super) fn restore_neural_checkpoint(
        &mut self,
        checkpoint: Option<&NeuralCheckpoint>,
        lexical_hash: &str,
        current_model: &str,
    ) -> (usize, bool) {
        let requested = checkpoint.is_some_and(|checkpoint| {
            checkpoint.lexical_hash == lexical_hash
                && checkpoint.model == current_model
                && (checkpoint.rows == 0 || checkpoint.mmap_path.is_file())
        });
        let rows = 0;
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let mut rows = rows;
        // loaded stays false until the mmap restore actually yields rows; a
        // checkpoint with rows==0 means nothing was loaded, not a successful restore.
        let loaded = false;
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let mut loaded = loaded;
        if requested {
            #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
            if let Some(neural_mmap) =
                index_builder::try_load_neural_mmap_embeddings(&self.project_path)
            {
                rows = self.search_engine.restore_neural_embeddings(&neural_mmap);
                loaded = rows > 0;
            }
        }
        (rows, loaded)
    }

    pub(super) fn persist_neural_snapshot(
        &mut self,
        state: &IndexPipelineState,
        rows: usize,
        embedder: Option<index_builder::HybridEmbedder>,
    ) -> Result<()> {
        if rows == 0 {
            return Ok(());
        }
        if embedder.is_some() {
            self.embedder = embedder;
        }
        // Fragment layer (Task 7): incremental sync before the snapshot
        // persist. On failure the layer is CLEARED (not just logged) — see
        // `sync_fragment_layer_or_clear` (Codex wave-4 P2).
        self.sync_fragment_layer_or_clear();
        // Neural rows attach to an already-persisted graph; re-derive the
        // identity from storage so hydration's fingerprint check agrees even
        // when the in-memory graph still holds duplicate node_ids.
        let (identity_nodes, identity_edges, identity_fingerprint) =
            index_builder::persisted_search_identity(&self.storage, &self.project_id)
                .unwrap_or_else(|| (state.pdg_node_count, state.pdg_edge_count, String::new()));
        index_builder::persist_search_snapshot(
            &self.search_engine,
            &self.project_path,
            identity_nodes,
            identity_edges,
            identity_fingerprint,
        )
    }

    /// Persist the neural mmap only when embeddings were freshly produced
    /// (not resumed) AND there are rows to write — never persist an empty mmap.
    /// Extracted from run_neural to keep that function's branch count bounded.
    pub(super) fn persist_neural_mmap(
        &self,
        _neural_resume_loaded: bool,
        _neural_rows: usize,
    ) -> Result<()> {
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        if !_neural_resume_loaded && _neural_rows > 0 {
            index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
            )?;
        }
        Ok(())
    }

    /// Incremental fragment sync (Task 7): diff the current source files
    /// against the persisted manifest, re-chunk ONLY changed files via the
    /// PDG (Tier-2 sub-symbol + Tier-3 orphans), embed ONLY content hashes
    /// missing from the store (batch-256 IPC), then update the store + root
    /// under a bumped generation and populate the engine's fragment vector
    /// index so `persist_search_snapshot` writes real fragment twins.
    ///
    /// Feature-off compatible: a no-op when `[search] fragment_index_enabled`
    /// is false, no PDG is resident, or no neural embedder is configured. A
    /// mid-build crash is handled by the generation guard in
    /// `fragment_layer_is_valid` — hydration serves the last complete root
    /// (i.e. keeps the fragment layer off) rather than a half-synced tree.
    /// Run the fragment-layer incremental sync, clearing the in-memory
    /// fragment rows on failure (Codex wave-4 P2).
    ///
    /// Every snapshot persist is preceded by a fragment sync; on sync failure
    /// the engine would otherwise keep the PREVIOUS generation's fragment
    /// index + owner refs, so a fresh node snapshot could rank/surface a
    /// changed symbol against deleted (pre-change) fragment content. Clearing
    /// makes the pre-existing "fragment layer disabled for this generation"
    /// warning honest and keeps the persisted snapshot fragment-free.
    /// `set_fragment_embeddings` with empty input drops the index, the owner
    /// refs, and the result cache in one call.
    pub(super) fn sync_fragment_layer_or_clear(&mut self) {
        if let Err(e) = self.sync_fragment_layer() {
            warn!(
                "Fragment layer sync failed (fragment layer disabled for this generation): {e:#}"
            );
            self.search_engine.set_fragment_embeddings(Vec::new());
        }
    }

    pub(super) fn sync_fragment_layer(&mut self) -> Result<()> {
        let cfg = crate::config::LeIndexConfig::load_cached();
        if !cfg.search.fragment_index_enabled || self.pdg.is_none() {
            return Ok(());
        }
        // The fragment layer only produces rows when a neural embedder is
        // configured; without one (e.g. text-only builds) it is a no-op.
        let embedder = self.configured_neural_embedder()?;
        if embedder.is_none() {
            return Ok(());
        }
        let files = self.collect_source_files_with_hashes(false)?;
        if files.is_empty() {
            return Ok(());
        }

        let mut store =
            index_builder::fragment::FragmentStore::load_from_storage(&self.project_path)?
                .unwrap_or_default();
        let max_bytes = cfg.search.fragment_max_bytes as usize;
        let orphan_enabled = cfg.search.fragment_orphan_enabled;
        let naive_fallback = cfg.search.fragment_naive_fallback;
        // Codex P1: persist the model + fragment-knob identity so a model or
        // knob change while sources are byte-identical forces a fragment
        // re-sync (mirrors the node-level `NeuralCheckpoint.model` discipline;
        // without it the source-hash skip would silently serve stale rows).
        let extraction_identity = index_builder::fragment::sync::FragmentExtractionIdentity::new(
            &cfg.neural.model_name,
            max_bytes,
            orphan_enabled,
            naive_fallback,
        );

        // P2-4 (Codex review): detect a missing/corrupt fragment embeddings mmap
        // BEFORE the sync so unchanged files are NOT skipped. With the mmap gone
        // but the store+manifest intact, a normal run would embed nothing, install
        // an empty fragment index, and the snapshot path would remove the mmap
        // again — permanently disabling fragment retrieval. Recover by forcing a
        // full re-embed of every content hash.
        let pre_sync_mmap_rows: std::collections::HashMap<String, Vec<f32>> =
            index_builder::try_load_fragment_mmap_embeddings_from_storage(
                &self.project_path.join(".leindex"),
            )
            .map(|mmap| mmap.entries().unwrap_or_default().into_iter().collect())
            .unwrap_or_default();
        let force_reembed = !store.is_empty() && pre_sync_mmap_rows.is_empty();

        // Scoped so the chunk closure (which borrows `self.pdg`) is dropped
        // before we mutate `self.search_engine` below.
        let (summary, new_embeddings) = {
            let pdg = self.pdg.as_ref().expect("checked above");
            let mut chunk_fn = |path: &std::path::Path, bytes: &[u8]| {
                index_builder::fragment::extract::extract_file_fragments(
                    pdg,
                    path,
                    bytes,
                    max_bytes,
                    orphan_enabled,
                    naive_fallback,
                )
            };
            let mut embed_fn = |texts: &[String]| -> Vec<Option<Vec<f32>>> {
                #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
                {
                    match &embedder {
                        Some(embedder) => embedder.embed_neural_batch_blocking(texts),
                        None => vec![None; texts.len()],
                    }
                }
                #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
                {
                    let _ = (&embedder, texts);
                    vec![None; texts.len()]
                }
            };
            index_builder::fragment::sync::incremental_sync_fragments(
                &self.project_path,
                &mut store,
                &files,
                &mut chunk_fn,
                &mut embed_fn,
                force_reembed,
                &extraction_identity,
            )?
        };

        info!(
            files_scanned = summary.files_scanned,
            files_changed = summary.files_changed,
            fragments_total = summary.fragments_total,
            embedded = summary.embedded,
            reused = summary.reused,
            generation = summary.generation,
            "Fragment incremental sync complete"
        ); // Merge freshly embedded rows with reused rows, then populate the
        // engine's fragment index so the snapshot persist twins write the
        // complete matrix. EVERY content hash in the store needs an embedding:
        // prefer this pass's fresh rows, fall back to the previous fragment
        // mmap (reused hashes are not re-embedded). A hash with neither is
        // skipped — mirroring the engine's skip-on-None discipline so store
        // row-count ≡ engine row-count (invariant 8) is preserved.
        let fresh_rows: std::collections::HashMap<String, Vec<f32>> =
            new_embeddings.into_iter().collect();
        let old_rows: std::collections::HashMap<String, Vec<f32>> =
            index_builder::try_load_fragment_mmap_embeddings_from_storage(
                &self.project_path.join(".leindex"),
            )
            .map(|mmap| mmap.entries().unwrap_or_default().into_iter().collect())
            .unwrap_or_default();
        let mut rows: Vec<(String, Vec<f32>)> = Vec::with_capacity(store.len());
        for hash in store.content_hashes() {
            if let Some(embedding) = fresh_rows.get(hash).or_else(|| old_rows.get(hash)) {
                rows.push((hash.to_string(), embedding.clone()));
            }
        }
        self.search_engine.set_fragment_embeddings(rows);

        // Owner refs (invariant 6): content hash → ALL (owner node id, byte
        // range) refs. A Vec per hash because identical content can live under
        // N owners — dedup must not collapse multi-owner fragments to the
        // first (Codex wave-2 item 5).
        let refs: std::collections::HashMap<String, Vec<(String, (usize, usize))>> = store
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
            .collect();
        self.search_engine.set_fragment_refs(refs);
        Ok(())
    }

    pub(crate) fn run_neural(
        &mut self,
        _job: &JobPaths,
        lexical: &LexicalCheckpoint,
    ) -> Result<NeuralCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("neural phase started without pipeline state")?;
        let lexical_hash = state
            .lexical_hash
            .clone()
            .unwrap_or_else(|| lexical.pdg_hash.clone());
        let neural_embedder = self.configured_neural_embedder()?;
        // Cache-key fix: a model swap must NOT silently resume the previous
        // model's embeddings. The checkpoint stores the embedder model_name that
        // produced its rows; a mismatch forces a full re-embed.
        let current_embed_model = crate::config::LeIndexConfig::load_cached()
            .neural
            .model_name
            .clone();
        let (mut neural_rows, neural_resume_loaded) = self.restore_neural_checkpoint(
            state.resumed_neural.as_ref(),
            &lexical_hash,
            &current_embed_model,
        );
        if neural_rows == 0 && !neural_resume_loaded {
            if let Some(neural_embedder) = neural_embedder.as_ref() {
                let pdg = self
                    .pdg
                    .as_ref()
                    .context("PDG is resident before neural enrichment")?;
                progress_stderr(&format!(
                    "Indexing: neural embedding {} admitted nodes...",
                    state.admitted_node_ids.len()
                ));
                let (cache_hits_before, _) = neural_cache_counters();
                let rows = index_builder::enrich_neural_embeddings(
                    pdg,
                    neural_embedder,
                    &state.admitted_node_ids,
                );
                let (cache_hits_after, cache_misses) = neural_cache_counters();
                progress_stderr(&format!(
                    "Indexing: neural done — {} rows ({} from embed cache, {} embedded)...",
                    rows.len(),
                    cache_hits_after - cache_hits_before,
                    cache_misses
                ));
                neural_rows = self.search_engine.update_neural_embeddings(rows);
            }
        }
        self.persist_neural_mmap(neural_resume_loaded, neural_rows)?;
        self.persist_neural_snapshot(&state, neural_rows, neural_embedder)?;
        progress_stderr("Indexing: publishing enhanced generation...");
        let neural_checkpoint = NeuralCheckpoint {
            lexical_hash,
            mmap_path: self.project_path.join(".leindex/neural_embeddings.bin"),
            rows: neural_rows,
            provider: if neural_rows == 0 {
                "unavailable".to_string()
            } else {
                std::env::var("LEINDEX_NEURAL_PROVIDER").unwrap_or_else(|_| "onnx".to_string())
            },
            model: crate::config::LeIndexConfig::load_cached()
                .neural
                .model_name
                .clone(),
        };
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("neural phase missing checkpoint store")?;
        let neural_hash = checkpoint_store.write_neural(&neural_checkpoint)?;
        self.checkpoint_state(checkpoint_store, "neural", neural_hash);
        injected_phase_failure("neural")?;
        state.neural_rows = neural_rows;
        state.neural_resume_loaded = neural_resume_loaded;
        state.neural_checkpoint = Some(neural_checkpoint.clone());
        self.pipeline = Some(state);
        Ok(neural_checkpoint)
    }

    pub(crate) fn publish_generation(
        &mut self,
        _job: &JobPaths,
        neural: Option<&NeuralCheckpoint>,
    ) -> Result<PublishedGeneration> {
        let mut state = self
            .pipeline
            .take()
            .context("publication started without pipeline state")?;

        // Compute core health if it hasn't been set yet (consolidated single
        // publish path — the intermediate lexical-only publish was removed).
        if state.core_health.is_none() {
            self.update_last_indexed_timestamp()?;
            self.save_stats_to_storage()?;
            let generation = self.checkpoint_generation();
            let git_status = crate::cli::git::status(&self.project_path).ok();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            let indexed_paths: std::collections::HashSet<PathBuf> = state
                .source_files_with_hashes
                .iter()
                .map(|(path, _)| path.clone())
                .collect();
            let dirty_source_paths =
                git_status
                    .as_ref()
                    .map_or_else(std::collections::HashSet::new, |status| {
                        status
                            .modified
                            .iter()
                            .chain(status.staged.iter())
                            .chain(status.untracked.iter())
                            .chain(status.deleted.iter())
                            .map(|path| {
                                if path.is_absolute() {
                                    path.clone()
                                } else {
                                    self.project_path.join(path)
                                }
                            })
                            .filter(|path| {
                                path.extension()
                                    .and_then(|extension| extension.to_str())
                                    .is_some_and(|extension| {
                                        super::super::SOURCE_FILE_EXTENSIONS
                                            .iter()
                                            .any(|known| known.eq_ignore_ascii_case(extension))
                                    })
                            })
                            .collect::<std::collections::HashSet<_>>()
                    });
            let changed_unindexed_count = dirty_source_paths
                .iter()
                .filter(|path| !indexed_paths.contains(*path))
                .count();
            let mut health = super::super::IndexHealth {
                generation,
                phase: super::super::IndexPhase::Complete,
                status: super::super::ComponentStatus::Fresh,
                head_oid: git_status
                    .as_ref()
                    .and_then(|status| status.head_oid.clone()),
                tree_oid: git_tree_oid(&self.project_path),
                indexed_file_count: state.source_files_with_hashes.len(),
                dirty_file_count: dirty_source_paths.len(),
                changed_unindexed_count,
                indexed_at_unix_ms: Some(now_ms),
                last_failure_phase: None,
                last_failure: None,
            };
            let core_published =
                self.publish_generation_snapshot(generation, &mut health, false)?;
            crate::cli::index_freshness::save_health(self.storage_path(), &health)?;
            state.core_health = Some(health);

            // If no neural checkpoint, return the core generation immediately.
            if neural.is_none() {
                injected_phase_failure("lexical")?;
                self.pipeline = Some(state);
                return Ok(core_published);
            }
        }

        let core_health = state
            .core_health
            .clone()
            .context("neural publication missing core health")?;
        let published = if neural.is_some_and(|checkpoint| checkpoint.rows > 0) {
            let mut health = super::super::IndexHealth {
                generation: core_health.generation.saturating_add(1),
                ..core_health.clone()
            };
            let published =
                self.publish_generation_snapshot(health.generation, &mut health, true)?;
            crate::cli::index_freshness::save_health(self.storage_path(), &health)?;
            published
        } else {
            crate::cli::index_freshness::save_health(self.storage_path(), &core_health)?;
            PublishedGeneration {
                generation: core_health.generation,
                storage_path: self
                    .storage_path()
                    .join("generations")
                    .join(core_health.generation.to_string()),
                health: core_health,
            }
        };
        self.pipeline = Some(state);
        Ok(published)
    }
}
