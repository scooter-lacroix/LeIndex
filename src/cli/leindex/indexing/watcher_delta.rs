use super::*;

/// Raised by [`LeIndex::incremental_reindex_from_watcher`] when no resident
/// PDG is loaded. The caller MUST escalate to a full index — a delta-only
/// save would truncate the stored graph — and both call sites (the watcher
/// loop and post-edit refreshes) match on this type to run
/// [`LeIndex::index_project`] instead of retrying the incremental path
/// forever.
#[derive(Debug, thiserror::Error)]
#[error(
    "incremental reindex requires a resident PDG; none is loaded (project not \
     hydrated) — a full index is needed"
)]
pub struct NotHydratedError;

impl LeIndex {
    /// Persist a tiny phase marker so diagnostics and owned MCP jobs can show
    /// useful progress without touching the resident PDG/search state. The
    /// marker is advisory until the final atomic snapshot is published.
    pub(super) fn mark_index_phase(
        &self,
        phase: super::super::IndexPhase,
        status: super::super::ComponentStatus,
    ) {
        let Some(previous) = crate::cli::index_freshness::load_health(self.storage_path()) else {
            let health = super::super::IndexHealth {
                phase,
                status,
                ..super::super::IndexHealth::default()
            };
            let _ = crate::cli::index_freshness::save_health(self.storage_path(), &health);
            return;
        };
        let health = super::super::IndexHealth {
            generation: previous.generation,
            phase,
            status,
            head_oid: previous.head_oid,
            tree_oid: previous.tree_oid,
            indexed_file_count: previous.indexed_file_count,
            dirty_file_count: previous.dirty_file_count,
            changed_unindexed_count: previous.changed_unindexed_count,
            indexed_at_unix_ms: previous.indexed_at_unix_ms,
            last_failure_phase: previous.last_failure_phase,
            last_failure: previous.last_failure,
        };
        let _ = crate::cli::index_freshness::save_health(self.storage_path(), &health);
    }

    /// Acknowledge a scan that proved the indexed content still matches the
    /// working tree ("no changes detected").
    ///
    /// The fast freshness check (`is_stale_fast`) compares the current git tree
    /// and the mtimes of source directories against what was recorded when the
    /// index was last written. A no-op scan writes nothing, so a commit that
    /// only touched non-indexed files, or a new non-indexed file in a source
    /// directory, would leave both comparisons failing permanently and make
    /// every request re-launch the scan. This records the present tree/HEAD in
    /// the health snapshot and advances the `leindex.db` reference time to when
    /// the scan began (changes made after that still count as stale).
    pub(super) fn record_clean_scan(&self, scanned_at: std::time::SystemTime) {
        if let Some(mut health) = crate::cli::index_freshness::load_health(self.storage_path()) {
            if let Some(tree_oid) = git_tree_oid(&self.project_path) {
                if health.tree_oid.as_deref() != Some(tree_oid.as_str()) {
                    health.tree_oid = Some(tree_oid);
                    if let Some(head_oid) = crate::cli::git::status(&self.project_path)
                        .ok()
                        .and_then(|status| status.head_oid)
                    {
                        health.head_oid = Some(head_oid);
                    }
                    let _ = crate::cli::index_freshness::save_health(self.storage_path(), &health);
                }
            }
        }
        let db = self.storage_path().join("leindex.db");
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&db) {
            let _ = file.set_modified(scanned_at);
        }
    }

    pub(crate) fn incremental_reindex_from_watcher(&mut self) -> Result<super::super::IndexStats> {
        // NOTE: the cross-process write lock is acquired by the WATCHER
        // (non-blocking, skip-on-busy) before calling this fn — see
        // `try_acquire_write_lock` in mod.rs and watcher.rs. Do not add a
        // blocking flock here: spawn_blocking cannot be cancelled, so a
        // blocking acquire held by another process would stall the watcher.
        //
        // A resident PDG is REQUIRED: the delta is built by merging into the
        // graph `take_owned_pdg` yields, and the generation publish persists
        // exactly the supplied graph — for a never-hydrated resident that would be a
        // delta-only graph holding just the changed files, and persisting it
        // would delete every unchanged file's nodes from the database (and
        // then install the truncated graph as resident, defeating every
        // load gate). Fail with the typed [`NotHydratedError`] so callers
        // escalate to a full index once instead of retrying this path on
        // every tick.
        if self.pdg.is_none() {
            return Err(NotHydratedError.into());
        }
        let start_time = std::time::Instant::now();
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .context("Failed to load indexed files from storage")?;

        // Hash source files without caching bodies (VAL-STREAM-012: no
        // cross-phase source-body retention). Changed-file nodes re-read
        // their file per chunk in build_changed_node_infos.
        let source_files_with_hashes = self.collect_source_files_with_hashes(true)?;
        let source_file_hashes: std::collections::HashMap<String, String> =
            source_files_with_hashes
                .iter()
                .map(|(path, hash)| (path.display().to_string(), hash.clone()))
                .collect();
        let current_file_paths: HashSet<String> = source_files_with_hashes
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect();

        let changed_files: Vec<_> = source_files_with_hashes
            .iter()
            .filter_map(|(path, hash)| {
                let path_str = path.display().to_string();
                if indexed_files.get(&path_str) != Some(hash) {
                    Some(path.clone())
                } else {
                    None
                }
            })
            .collect();
        let deleted_files: Vec<String> = indexed_files
            .keys()
            .filter(|p| !current_file_paths.contains(*p))
            .cloned()
            .collect();

        if changed_files.is_empty() && deleted_files.is_empty() {
            return Ok(self.stats.clone());
        }

        let parser = crate::parse::parallel::ParallelParser::new();
        let parsing_results = if changed_files.is_empty() {
            Vec::new()
        } else {
            parser.parse_files(changed_files)
        };
        // The graph now exists only in this local. The fallible steps below
        // must return it to `self.pdg` on error — a transient failure
        // (SQLite error, disk full) otherwise leaves the engine with no
        // resident PDG, degrading every graph-dependent read tool until a
        // full reload. The restore happens ONLY when a graph was resident:
        // `unwrap_or_default()` fabricates an empty graph for a
        // never-hydrated project, and installing THAT would defeat every
        // `pdg.is_none()` load gate (`ensure_pdg_loaded`,
        // `reload_pdg_from_cache`, `warm_caches`) — the delta-only merge is
        // not a valid graph either way.
        let had_resident_pdg = self.pdg.is_some();
        let mut pdg = self.take_owned_pdg().unwrap_or_default();
        let applied = self.apply_incremental_pdg_changes(
            &mut pdg,
            &deleted_files,
            parsing_results,
            &source_file_hashes,
        );
        let removed_node_ids = match applied {
            Ok(ids) => ids,
            Err(error) => {
                if had_resident_pdg {
                    // The partially-mutated graph, restored as-is: the
                    // per-file deletes already committed inside this call
                    // are then reflected in memory too.
                    self.pdg = Some(std::sync::Arc::new(pdg));
                }
                return Err(error);
            }
        };

        // Resume-proof FileSummary pass: covers ALL files (the incremental merge
        // loop only touched changed files; existing files keep/refresh summaries).
        pdg.ensure_file_summary_nodes();
        // Match the full-index pipeline: external/import nodes from the
        // re-parsed files must be normalized to NodeType::External BEFORE
        // save, or the DB stores a graph whose fingerprint changes again
        // during hydration's post-load normalization — permanently defeating
        // the snapshot fast path for watcher-published generations.
        index_builder::normalize_external_nodes(&mut pdg);

        // Build the set of changed file paths so we only include nodes from
        // those files in the incremental delta.
        let changed_file_set: HashSet<String> = source_file_hashes
            .keys()
            .filter(|p| {
                indexed_files.get(*p).map(|s| s.as_str())
                    != source_file_hashes.get(*p).map(|s| s.as_str())
            })
            .cloned()
            .collect();

        // Load the persisted embedder (built during the last full index) so we
        // can embed changed-file nodes with the same TF-IDF vocabulary.  Do NOT
        // call index_nodes_with_embedder() here — that processes ALL nodes and
        // populates the search engine from scratch (i.e. a full rebuild).
        let tfidf_embedder = index_builder::TfIdfEmbedder::load_from_storage(&self.project_path)
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                // No persisted embedder — build a minimal one from the
                // changed-file node tokens so we can still produce embeddings.
                tracing::warn!(
                    "Failed to load persisted TF-IDF embedder for incremental reindex. \
                    This will result in degraded search quality (zero-vector embeddings) \
                    for new/modified nodes until a full reindex is performed. \
                    Consider running a full reindex to restore search quality."
                );
                index_builder::TfIdfEmbedder::build_from_tokens(&[])
            });

        let embedder = index_builder::HybridEmbedder::tfidf_only(tfidf_embedder);

        let updated_nodes = Self::build_changed_node_infos(&pdg, &changed_file_set, &embedder);

        self.search_engine
            .incremental_reindex(crate::search::search::TextIndexDelta {
                removed_node_ids,
                updated_nodes,
            });
        self.persist_and_publish_watcher_delta(pdg, embedder, source_files_with_hashes, start_time)
    }
    /// Compute Leiden communities over the completed PDG and persist them in
    /// one batched transaction (roadmap Part IV). Placement: after PDG edge
    /// construction and cross-file relinking, before embeddings — communities
    /// depend on edges, not vectors. Feature-flagged; failures degrade to
    /// "no communities" and never fail the index.
    #[cfg(feature = "community")]
    pub(super) fn compute_and_persist_communities(
        &mut self,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    ) {
        if !crate::feature_flags::FeatureFlag::CommunityDetection.is_enabled() {
            return;
        }
        match crate::storage::community_store::compute_and_persist(
            &mut self.storage,
            &self.project_id,
            pdg,
        ) {
            Ok(stats) => tracing::info!(
                communities = stats.community_count,
                quality = stats.quality,
                recompute_ms = stats.recompute_ms,
                "community detection complete"
            ),
            Err(error) => {
                tracing::warn!(%error, "community persistence failed; community metadata unavailable")
            }
        }
    }

    #[cfg(not(feature = "community"))]
    pub(super) fn compute_and_persist_communities(
        &mut self,
        _pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    ) {
    }

    #[cfg(feature = "precision")]
    pub(super) fn run_precision_ingest(&self, pdg: &mut crate::graph::pdg::ProgramDependenceGraph) {
        if !crate::feature_flags::FeatureFlag::PrecisionIngest.is_enabled() {
            return;
        }
        let report = crate::intel::run_precision_ingest(pdg, &self.project_path);
        if report.definitions_seen > 0 || report.relationships_seen > 0 {
            tracing::info!(
                definitions_seen = report.definitions_seen,
                definitions_matched = report.definitions_matched,
                relationships_upgraded = report.relationships_upgraded,
                relationships_added = report.relationships_added,
                "SCIP precision ingest complete"
            );
        }
    }

    #[cfg(not(feature = "precision"))]
    pub(super) fn run_precision_ingest(
        &self,
        _pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    ) {
    }

    /// Persist the watcher-reindex delta (embeddings, snapshot, neural) and
    /// publish the new generation with fresh health. Owns all post-merge I/O so
    /// the reindex orchestrator stays a thin pipeline.
    ///
    /// The caller's graph installs into `self.pdg` before the first fallible
    /// step here (persist_to_storage below), so no error-restore parameter is
    /// needed: a failure past that point leaves the resident graph in place.
    pub(super) fn persist_and_publish_watcher_delta(
        &mut self,
        mut pdg: crate::graph::pdg::ProgramDependenceGraph,
        embedder: index_builder::HybridEmbedder,
        source_files_with_hashes: Vec<(PathBuf, String)>,
        start_time: std::time::Instant,
    ) -> Result<super::super::IndexStats> {
        // D5: the graph persists only as the Pdg layer of the generation
        // published at the end of this run — no SQL row writes.
        self.compute_and_persist_communities(&mut pdg);

        // Snapshot/embedder freshness must describe the graph as the
        // published layer reconstructs it (duplicate node_ids and parallel
        // edges collapse there).
        let persisted_identity = index_builder::persisted_search_identity_from_graph(&pdg);
        self.pdg = Some(std::sync::Arc::new(pdg));
        self.embedder = Some(embedder);
        if let Some(embedder) = &self.embedder {
            embedder.persist_to_storage(
                &self.project_path,
                self.pdg.as_ref().unwrap(),
                Some(persisted_identity.clone()),
            )?;
        }
        self.build_file_stats_cache();
        self.stats.indexing_time_ms = start_time.elapsed().as_millis() as u64;

        // R10: Persist embeddings to mmap file after watcher incremental reindex
        index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)?;
        // Fragment layer (Task 7): incremental sync before the snapshot
        // persist. On failure the layer is CLEARED (not just logged) so this
        // generation can never pair new nodes with stale pre-change fragment
        // text/byte ranges (Codex wave-4 P2). Node-level ranking stays
        // authoritative; the fragment layer is simply off for this generation.
        self.sync_fragment_layer_or_clear();
        let (pdg_node_count, pdg_edge_count, pdg_fingerprint) = persisted_identity;
        index_builder::persist_search_snapshot(
            &self.search_engine,
            &self.project_path,
            pdg_node_count,
            pdg_edge_count,
            pdg_fingerprint,
        )?;
        // Persist neural embeddings separately for fast load_from_storage
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
            )?;
        }

        // Publish the watcher delta before returning. Neural rows are kept
        // from the previous snapshot when available; full owned index jobs
        // perform any missing neural enrichment without blocking file-save
        // latency here.
        self.update_last_indexed_timestamp()?;
        self.save_stats_to_storage()?;
        let generation = self.checkpoint_generation();
        let git_status = crate::cli::git::status(&self.project_path).ok();
        let indexed_paths: std::collections::HashSet<PathBuf> = source_files_with_hashes
            .iter()
            .map(|(path, _)| path.clone())
            .collect();
        let dirty_source_paths = self.dirty_source_paths(git_status.as_ref());
        let changed_unindexed_count = dirty_source_paths
            .iter()
            .filter(|path| !indexed_paths.contains(*path))
            .count();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let mut health = super::super::IndexHealth {
            generation,
            phase: super::super::IndexPhase::Complete,
            status: super::super::ComponentStatus::Fresh,
            head_oid: git_status
                .as_ref()
                .and_then(|status| status.head_oid.clone()),
            tree_oid: git_tree_oid(&self.project_path),
            indexed_file_count: source_files_with_hashes.len(),
            dirty_file_count: dirty_source_paths.len(),
            changed_unindexed_count,
            indexed_at_unix_ms: Some(now_ms),
            last_failure_phase: None,
            last_failure: None,
        };
        self.publish_generation_snapshot(generation, &mut health, true)?;
        crate::cli::index_freshness::save_health(self.storage_path(), &health)?;

        // Clear search query and analysis caches so stale results are not
        // served after an incremental reindex (VAL-INDEX-005).
        index_builder::clear_query_caches(&mut self.cache.cache_spiller, &self.project_id);

        info!(
            "Watcher incremental reindex completed in {}ms",
            self.stats.indexing_time_ms
        );
        Ok(self.stats.clone())
    }

    /// Apply deleted-file removals and changed-file re-parsing to the PDG and
    /// storage during an incremental watcher reindex. Returns the IDs of nodes
    /// removed from deleted files (for search-engine delta eviction).
    ///
    /// A re-parsed file's old nodes are removed together with every incident
    /// edge, while the freshly extracted per-file PDG only carries unresolved
    /// external placeholders — persisting that merge as-is would silently drop
    /// every cross-file relationship involving the edited file until the next
    /// full reindex. Cross-file edges are therefore handled in two halves
    /// (see [`preserve_cross_file_edges`]): edges from the still-unedited
    /// callers into the file are re-attached after the merge, and the file's
    /// own outgoing edges to real definitions are re-attached only when the
    /// re-extracted source still makes that call — the fresh per-file graph
    /// is authoritative, so a removed call is not resurrected.
    pub(super) fn apply_incremental_pdg_changes(
        &mut self,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
        deleted_files: &[String],
        parsing_results: Vec<crate::parse::parallel::ParsingResult>,
        source_file_hashes: &HashMap<String, String>,
    ) -> Result<Vec<String>> {
        let mut removed_node_ids = Vec::new();
        for path in deleted_files {
            // O(nodes in file) via the file index, not a full graph scan.
            removed_node_ids.extend(
                pdg.nodes_in_file(path)
                    .iter()
                    .filter_map(|node_idx| pdg.get_node(*node_idx))
                    .map(|node| node.id.clone()),
            );
            index_builder::remove_file_from_pdg(pdg, path)?;
            if let Err(e) = crate::storage::pdg_store::delete_file_data(
                &mut self.storage,
                &self.project_id,
                path,
            ) {
                warn!(
                    "Failed to delete file data from storage for '{}' during incremental reindex: {}",
                    path, e
                );
            }
        }

        for result in parsing_results {
            if !result.is_success() {
                continue;
            }
            let file_path = result.file_path.display().to_string();
            let language = result.language.as_deref().unwrap_or("unknown");
            let source_bytes = result.source_bytes.as_deref().unwrap_or(&[]);
            // Snapshot BEFORE removal: removing the file's nodes deletes every
            // incident edge. Only the INCOMING half is snapshotted — an
            // unedited caller's edge into the re-parsed file dies with the
            // old node and nothing else can restore it. Outgoing
            // relationships are regenerated after the merge, by the same
            // cross-file resolution a full index runs (see below).
            let preserved = preserve_cross_file_edges(pdg, &file_path);
            index_builder::remove_file_from_pdg(pdg, &file_path)?;
            // The scoped resolution below needs the call/flow facts of the
            // re-extracted source; extraction consumes the signatures.
            let file_signatures = result.signatures.clone();
            let file_pdg = crate::graph::extract_pdg_from_signatures(
                result.signatures,
                source_bytes,
                &file_path,
                language,
            );
            index_builder::merge_pdgs(pdg, file_pdg);
            restore_cross_file_edges(pdg, preserved);
            resolve_file_cross_file_edges(pdg, &file_path, file_signatures);
            if let Some(hash) = source_file_hashes.get(&file_path) {
                if let Err(e) = crate::storage::pdg_store::update_indexed_file(
                    &mut self.storage,
                    &self.project_id,
                    &file_path,
                    hash,
                ) {
                    warn!(
                        "Failed to update indexed file record for '{}' during incremental reindex: {}",
                        file_path, e
                    );
                }
            }
        }
        Ok(removed_node_ids)
    }
    /// Build NodeInfo entries for nodes in changed files, applying the same
    /// pruning gate and TF-IDF embedding as a full index (restricted to the delta).
    pub(super) fn build_changed_node_infos(
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        changed_file_set: &HashSet<String>,
        embedder: &index_builder::HybridEmbedder,
    ) -> Vec<crate::search::search::NodeInfo> {
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
        let pruner = crate::search::search::ContentPruner::new();
        let mut updated_nodes: Vec<crate::search::search::NodeInfo> = Vec::new();
        let file_summary_ctx = &index_builder::FileSummaryContext::from_pdg(pdg);
        // Per-chunk scratch: only one file body resident at a time.
        let mut file_cache = index_builder::FileReadCache::per_chunk_scratch();

        for node_idx in pdg.node_indices() {
            let Some(node) = pdg.get_node(node_idx) else {
                continue;
            };
            let file_path_str = node.file_path.as_ref();
            if !changed_file_set.contains(file_path_str) {
                continue;
            }
            let file_bytes = file_cache
                .get_or_read(std::path::Path::new(file_path_str))
                .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));
            let node_content = index_builder::enriched_node_content(
                pdg,
                node_idx,
                node,
                file_bytes.as_ref(),
                &connectivity_config,
                file_summary_ctx,
            );
            let pruning_decision = pruner.evaluate(&node.file_path, &node_content, &node.name);
            if pruning_decision != crate::search::search::PruningDecision::Keep {
                continue;
            }
            let tokens = index_builder::tokenize_code(&node_content);
            let signature =
                crate::search::search::SearchEngine::extract_signature_from_content(&node_content);
            let tfidf_embedding = embedder.embed_tfidf(&tokens);
            updated_nodes.push(crate::search::search::NodeInfo {
                node_id: node.id.clone(),
                file_path: node.file_path.to_string(),
                symbol_name: node.name.clone(),
                language: node.language.clone(),
                content: node_content,
                byte_range: node.byte_range,
                tfidf_embedding,
                neural_embedding: None,
                complexity: node.complexity,
                signature,
                pre_tokenized: Some(tokens),
            });
        }
        updated_nodes
    }

    /// Collect source-extension dirty paths (modified/staged/untracked/deleted)
    /// from git status, made absolute and filtered to known source extensions.
    pub(super) fn dirty_source_paths(
        &self,
        git_status: Option<&crate::cli::git::GitStatus>,
    ) -> std::collections::HashSet<PathBuf> {
        let Some(status) = git_status else {
            return std::collections::HashSet::new();
        };
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
            .collect()
    }
}

/// A cross-file edge pointing INTO a re-parsed file, snapshotted before the
/// file's old nodes (and every incident edge) are removed.
struct PreservedCrossFileEdge {
    /// `node.id` string of the endpoint inside the re-parsed file. Ids are
    /// deterministic per file (`file_path:qualified_name`), so the replacement
    /// node for a surviving symbol carries the same string after the merge.
    file_side_id: String,
    /// The endpoint in another file. Carried as a `NodeId` because the
    /// untouched side survives this delta — but validated against
    /// `other_side_id` at re-attachment time, since a petgraph
    /// `StableGraph` recycles vacated slots when a later file in the same
    /// batch is re-merged, and a recycled slot must not alias the edge onto
    /// an unrelated node.
    other: crate::graph::pdg::NodeId,
    /// `node.id` string of `other` (the recycled-slot guard above).
    other_side_id: String,
    /// The original edge weight (type, call count, confidence, …).
    edge: crate::graph::pdg::Edge,
}

/// Snapshot the cross-file edges pointing INTO `file_path` (an unedited
/// caller's edge into the re-parsed file).
///
/// Outgoing relationships are deliberately NOT snapshotted: they are
/// regenerated after the merge by [`resolve_file_cross_file_edges`] — the
/// same cross-file resolution a full index runs — which restores exactly the
/// calls and flows the edited source still makes and none of the removed
/// ones. Snapshotted-and-gated restoration cannot do that: the per-file
/// extraction produces no evidence for project callees (its unresolved
/// non-std calls vanish silently), so no gate on the fresh graph can
/// distinguish a surviving call from a removed one.
///
/// Edges with both endpoints inside the file are skipped (the re-extraction
/// recreates them) as are edges touching neither the file nor its nodes.
/// Node membership comes from the file index (O(nodes in file)) rather than
/// a full graph scan — this runs under the project write lock on the
/// file-save path.
fn preserve_cross_file_edges(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    file_path: &str,
) -> Vec<PreservedCrossFileEdge> {
    use std::collections::HashSet;
    let file_nodes: HashSet<crate::graph::pdg::NodeId> =
        pdg.nodes_in_file(file_path).into_iter().collect();
    let mut preserved = Vec::new();
    for edge_id in pdg.edge_indices() {
        let Some((from, to)) = pdg.edge_endpoints(edge_id) else {
            continue;
        };
        let from_in_file = file_nodes.contains(&from);
        let to_in_file = file_nodes.contains(&to);
        if from_in_file == to_in_file || from_in_file {
            // Both endpoints inside the file (re-extracted), or the edge
            // runs OUT of the re-parsed file (regenerated by resolution).
            continue;
        }
        let Some(node) = pdg.get_node(from) else {
            continue;
        };
        let Some(file_side_id) = pdg.get_node(to).map(|n| n.id.to_string()) else {
            continue;
        };
        let Some(edge) = pdg.get_edge(edge_id) else {
            continue;
        };
        preserved.push(PreservedCrossFileEdge {
            file_side_id,
            other: from,
            other_side_id: node.id.to_string(),
            edge: edge.clone(),
        });
    }
    preserved
}

/// Resolve the re-parsed file's outgoing cross-file edges against the merged
/// graph — the watcher-delta equivalent of the post-merge
/// `resolve_cross_file_call_edges_for_files` /
/// `resolve_cross_file_flow_edges_for_files` passes a full index runs over
/// every signature at once, scoped to this file because every other file's
/// relationships are already durable. The per-file extraction resolves calls
/// against LOCAL symbols only, so without this pass a call into another file
/// would surface no edge until the next full reindex.
fn resolve_file_cross_file_edges(
    pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    file_path: &str,
    signatures: Vec<crate::parse::traits::SignatureInfo>,
) {
    let scoped: Vec<(String, crate::parse::traits::SignatureInfo)> = signatures
        .into_iter()
        .map(|signature| (file_path.to_string(), signature))
        .collect();
    crate::graph::resolve_cross_file_call_edges_for_files(pdg, &scoped);
    crate::graph::resolve_cross_file_flow_edges_for_files(pdg, &scoped);
}

/// Re-attach the snapshotted incoming edges onto the re-merged graph.
///
/// The file side must have survived the edit (resolved by its deterministic
/// id string); the caller side is used through its carried `NodeId` — the
/// untouched endpoint survives this delta — but only while the node it now
/// points at still carries the snapshotted id, guarding against petgraph
/// slot recycling when later files in the same batch re-merge. Returns the
/// restored count.
fn restore_cross_file_edges(
    pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    preserved: Vec<PreservedCrossFileEdge>,
) -> usize {
    use petgraph::visit::EdgeRef;
    let mut restored = 0usize;
    for edge in preserved {
        let Some(file_side) = pdg.find_by_id(&edge.file_side_id) else {
            continue;
        };
        // Slot-recycling guard: if `other` was vacated and reused by a
        // different node, fall back to the id string (and drop the edge when
        // even that does not resolve — the far end is gone).
        let other_side = match pdg.get_node(edge.other) {
            Some(node) if node.id == edge.other_side_id => Some(edge.other),
            _ => pdg.find_by_id(&edge.other_side_id),
        };
        let Some(other_side) = other_side else {
            continue;
        };
        let already_present = pdg.graph.edges(other_side).any(|reference| {
            reference.target() == file_side && reference.weight().edge_type == edge.edge.edge_type
        });
        if already_present {
            continue;
        }
        pdg.add_edge(other_side, file_side, edge.edge);
        restored += 1;
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::pdg::{Edge, EdgeMetadata, EdgeType, Node, NodeType};

    fn symbol(id: &str, file: &str, name: &str, node_type: NodeType) -> Node {
        Node {
            id: id.to_string(),
            node_type,
            name: name.to_string(),
            file_path: std::sync::Arc::from(file),
            byte_range: (0, 0),
            complexity: 0,
            language: "rust".to_string(),
        }
    }

    fn call_edge() -> Edge {
        Edge {
            edge_type: EdgeType::Call,
            metadata: EdgeMetadata::empty(),
        }
    }

    fn has_edge(
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        from: crate::graph::pdg::NodeId,
        to: crate::graph::pdg::NodeId,
        edge_type: &EdgeType,
    ) -> bool {
        use petgraph::visit::EdgeRef;
        pdg.graph
            .edges(from)
            .any(|reference| reference.target() == to && reference.weight().edge_type == *edge_type)
    }

    /// Minimal real signature for `extract_pdg_from_signatures`: the node id
    /// it produces is `format!("{file}:{qualified_name}")`, matching the
    /// deterministic ids the delta contract relies on.
    fn signature(name: &str, calls: Vec<&str>) -> crate::parse::traits::SignatureInfo {
        crate::parse::traits::SignatureInfo {
            name: name.to_string(),
            qualified_name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            visibility: crate::parse::traits::Visibility::Public,
            is_async: false,
            is_method: false,
            docstring: None,
            calls: calls.into_iter().map(str::to_string).collect(),
            imports: Vec::new(),
            byte_range: (0, 0),
            cyclomatic_complexity: 0,
            flow_facts: Vec::new(),
        }
    }

    /// The watcher-delta contract, incoming half (round-8 Codex P1, reworked
    /// in round 9): removing a re-parsed file's nodes deletes every incident
    /// edge, so an unedited caller's edge INTO the file must be snapshotted
    /// before removal and re-attached after the merge — the caller was not
    /// re-parsed, so nothing else can restore it. The premise is asserted in
    /// the edge's own direction, not the reverse one.
    #[test]
    fn test_incoming_cross_file_edges_survive_a_watcher_remerge() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let caller = pdg.add_node(symbol("b.rs:caller", "b.rs", "caller", NodeType::Function));
        let handler = pdg.add_node(symbol(
            "a.rs:handler",
            "a.rs",
            "handler",
            NodeType::Function,
        ));
        pdg.add_edge(caller, handler, call_edge());

        let preserved = preserve_cross_file_edges(&pdg, "a.rs");
        assert_eq!(preserved.len(), 1);
        assert_eq!(preserved[0].other_side_id, "b.rs:caller");

        pdg.remove_file("a.rs");
        assert!(
            !has_edge(&pdg, caller, handler, &EdgeType::Call),
            "premise: removal kills the caller's edge into the edited file"
        );
        let handler_new = pdg.add_node(symbol(
            "a.rs:handler",
            "a.rs",
            "handler",
            NodeType::Function,
        ));

        assert_eq!(restore_cross_file_edges(&mut pdg, preserved), 1);
        assert!(
            has_edge(&pdg, caller, handler_new, &EdgeType::Call),
            "the unedited caller's edge must be restored onto the replacement node"
        );
    }

    /// The outgoing half (round-10 Kilo CRITICAL): the edited file's cross-
    /// file call into another file's definition must SURVIVE the delta when
    /// the re-extracted source still makes the call. The round-9 gate could
    /// never fire — per-file extraction resolves against LOCAL symbols only,
    /// so a project callee produces no placeholder evidence — and dropped
    /// every such edge. The fix regenerates them by running the same
    /// cross-file resolution a full index runs, scoped to this file's real
    /// signatures, against the merged graph. This test uses the real
    /// extractor; no hand-built placeholder fixtures.
    #[test]
    fn test_outgoing_cross_file_call_edges_are_regenerated_after_a_remerge() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let main = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let helper = pdg.add_node(symbol("b.rs:helper", "b.rs", "helper", NodeType::Function));
        pdg.add_edge(main, helper, call_edge());

        // Outgoing edges are not snapshotted: regeneration is resolution's job.
        let preserved = preserve_cross_file_edges(&pdg, "a.rs");
        assert!(preserved.is_empty(), "only incoming edges are snapshotted");
        pdg.remove_file("a.rs");
        assert!(
            !has_edge(&pdg, main, helper, &EdgeType::Call),
            "premise: removal kills the edited file's outgoing edge"
        );

        // The REAL per-file extraction of the edited source, which still
        // calls helper: per-file extraction resolves against LOCAL symbols
        // only, so the fresh graph holds no edge to b.rs:helper — the exact
        // evidence the round-9 gate demanded and could never get.
        let signatures = vec![signature("main", vec!["helper"])];
        let file_pdg =
            crate::graph::extract_pdg_from_signatures(signatures.clone(), &[], "a.rs", "rust");
        let fresh_main = file_pdg.find_by_id("a.rs:main");
        assert!(fresh_main.is_some(), "premise: the caller was re-extracted");
        assert!(
            file_pdg.find_by_id("b.rs:helper").is_none(),
            "premise: per-file extraction holds no node for another file's definition"
        );

        index_builder::merge_pdgs(&mut pdg, file_pdg);
        restore_cross_file_edges(&mut pdg, preserved);
        resolve_file_cross_file_edges(&mut pdg, "a.rs", signatures);

        let main_new = pdg
            .find_by_id("a.rs:main")
            .expect("the merged graph keeps the caller");
        assert!(
            has_edge(&pdg, main_new, helper, &EdgeType::Call),
            "the still-existing call must be resolved against the resident definition"
        );
    }

    /// The edit removed the call: resolution runs against the fresh source's
    /// signatures, so the stale edge must NOT come back (the old graph's
    /// truth is not the edited source's).
    #[test]
    fn test_removed_calls_are_not_resurrected_by_the_resolution() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let main = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let helper = pdg.add_node(symbol("b.rs:helper", "b.rs", "helper", NodeType::Function));
        pdg.add_edge(main, helper, call_edge());
        pdg.remove_file("a.rs");

        // The edited source no longer calls helper — it calls something else.
        let signatures = vec![signature("main", vec!["unrelated_local"])];
        let file_pdg =
            crate::graph::extract_pdg_from_signatures(signatures.clone(), &[], "a.rs", "rust");
        index_builder::merge_pdgs(&mut pdg, file_pdg);
        resolve_file_cross_file_edges(&mut pdg, "a.rs", signatures);

        let main_new = pdg.find_by_id("a.rs:main").unwrap();
        assert!(
            !has_edge(&pdg, main_new, helper, &EdgeType::Call),
            "a removed call must not be resurrected"
        );
    }

    /// Outgoing edges to external placeholders are the fresh extraction's
    /// job (one per still-existing std/prelude call); the scoped resolution
    /// must not add a second one.
    #[test]
    fn test_placeholder_edges_are_left_to_the_fresh_extraction() {
        use petgraph::visit::EdgeRef;
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let main = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let external = pdg.add_node(Node {
            language: "external".to_string(),
            ..symbol(
                "external::std.fs.read",
                "<external>",
                "std.fs.read",
                NodeType::External,
            )
        });
        pdg.add_edge(main, external, call_edge());
        pdg.remove_file("a.rs");

        // Real extraction: `std::fs::read` is an external call target, so the
        // per-file graph regenerates the placeholder edge itself (the target
        // is dot-normalized: `external::std.fs.read`).
        let signatures = vec![signature("main", vec!["std::fs::read"])];
        let file_pdg =
            crate::graph::extract_pdg_from_signatures(signatures.clone(), &[], "a.rs", "rust");
        assert!(
            file_pdg.find_by_id("external::std.fs.read").is_some(),
            "premise: per-file extraction emits the normalized std placeholder"
        );
        index_builder::merge_pdgs(&mut pdg, file_pdg);
        resolve_file_cross_file_edges(&mut pdg, "a.rs", signatures);

        let main_new = pdg.find_by_id("a.rs:main").unwrap();
        let placeholder = pdg.find_by_id("external::std.fs.read").unwrap();
        let call_edges = pdg
            .graph
            .edges(main_new)
            .filter(|reference| {
                reference.target() == placeholder && reference.weight().edge_type == EdgeType::Call
            })
            .count();
        assert_eq!(
            call_edges, 1,
            "exactly one placeholder edge: the extraction's, not a resolved duplicate"
        );
    }

    /// The slot-recycling guard has real coverage (round-10 Kilo): `other`
    /// is carried as a NodeId because the caller's file is untouched by THIS
    /// delta — but a later file in the same batch can vacate that slot and
    /// re-add a different node into it. The guard must reject the recycled
    /// alias, and the id-string fallback must then find nothing (the caller
    /// really is gone).
    #[test]
    fn test_restore_rejects_a_recycled_other_side_slot() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let caller = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let callee = pdg.add_node(symbol("b.rs:helper", "b.rs", "helper", NodeType::Function));
        pdg.add_edge(caller, callee, call_edge());

        let preserved = preserve_cross_file_edges(&pdg, "b.rs");
        assert_eq!(preserved.len(), 1);

        // A later batch file re-merges a.rs: the caller's slot is vacated
        // and recycled by an unrelated node.
        pdg.remove_file("a.rs");
        let recycled = pdg.add_node(symbol(
            "a.rs:unrelated",
            "a.rs",
            "unrelated",
            NodeType::Function,
        ));
        assert_eq!(
            preserved[0].other, recycled,
            "premise: petgraph recycled the vacated slot for the new node"
        );

        assert_eq!(
            restore_cross_file_edges(&mut pdg, preserved),
            0,
            "the carried NodeId now aliases an unrelated node — the edge must be dropped"
        );
    }

    /// An incoming edge whose caller vanished without slot recycling
    /// (symbol removed in the other file, nothing re-added) is dropped via
    /// the id-string fallback, not re-attached to a stale node.
    #[test]
    fn test_restore_drops_edges_whose_other_side_vanished() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let caller = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let callee = pdg.add_node(symbol("b.rs:helper", "b.rs", "helper", NodeType::Function));
        pdg.add_edge(caller, callee, call_edge());

        let preserved = preserve_cross_file_edges(&pdg, "b.rs");
        assert_eq!(preserved.len(), 1);

        pdg.remove_file("a.rs");
        assert_eq!(restore_cross_file_edges(&mut pdg, preserved), 0);
    }
}
