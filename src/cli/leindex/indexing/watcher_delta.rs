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
        // graph `take_owned_pdg` yields, and `save_pdg` persists exactly the
        // supplied graph — for a never-hydrated resident that would be a
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
        self.persist_and_publish_watcher_delta(
            pdg,
            had_resident_pdg,
            embedder,
            source_files_with_hashes,
            start_time,
        )
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

    /// Persist the watcher-reindex delta (PDG, embeddings, snapshot, neural) and
    /// publish the new generation with fresh health. Owns all post-merge I/O so
    /// the reindex orchestrator stays a thin pipeline.
    ///
    /// `had_resident_pdg` says whether the caller actually took a graph out
    /// of `self.pdg` (versus fabricating an empty default for a
    /// never-hydrated project): error paths restore the graph only in the
    /// former case, because installing an empty or delta-only graph would
    /// defeat every `pdg.is_none()` load gate.
    pub(super) fn persist_and_publish_watcher_delta(
        &mut self,
        mut pdg: crate::graph::pdg::ProgramDependenceGraph,
        had_resident_pdg: bool,
        embedder: index_builder::HybridEmbedder,
        source_files_with_hashes: Vec<(PathBuf, String)>,
        start_time: std::time::Instant,
    ) -> Result<super::super::IndexStats> {
        // Precision does NOT run on the watcher/edit-apply path: a full SCIP
        // indexer pass (minutes with rust-analyzer) would block the project
        // write lock every time a file is saved. Markers for changed files
        // drop until the next explicit index, which re-merges precision.
        // Persist the updated PDG to storage so changes survive restart. On
        // failure the graph is returned to `self.pdg` (it was taken out by
        // the caller) so the engine keeps serving graph reads from the
        // in-memory state it had.
        if let Err(error) =
            index_builder::save_to_storage(&mut self.storage, &self.project_id, &pdg)
        {
            if had_resident_pdg {
                self.pdg = Some(std::sync::Arc::new(pdg));
            }
            return Err(error);
        }
        self.compute_and_persist_communities(&mut pdg);

        // Snapshot/embedder freshness must describe the graph as the DB
        // reconstructs it (duplicate node_ids collapse on save); otherwise
        // every later cold hydration takes the full TF-IDF rebuild path.
        let persisted_identity =
            index_builder::persisted_search_identity(&self.storage, &self.project_id);
        self.pdg = Some(std::sync::Arc::new(pdg));
        self.embedder = Some(embedder);
        if let Some(embedder) = &self.embedder {
            embedder.persist_to_storage(
                &self.project_path,
                self.pdg.as_ref().unwrap(),
                persisted_identity.clone(),
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
        let (pdg_node_count, pdg_edge_count, pdg_fingerprint) = persisted_identity
            .or_else(|| {
                self.pdg.as_ref().map(|pdg| {
                    (
                        pdg.node_count(),
                        pdg.edge_count(),
                        index_builder::pdg_search_fingerprint(pdg),
                    )
                })
            })
            .unwrap_or((self.stats.pdg_nodes, self.stats.pdg_edges, String::new()));
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
    /// every cross-file call/flow relationship involving the edited file until
    /// the next full reindex. Cross-file edges incident to the file are
    /// therefore snapshotted before removal (keyed by stable node-id strings)
    /// and re-attached after the merge (see [`preserve_cross_file_edges`]).
    pub(super) fn apply_incremental_pdg_changes(
        &mut self,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
        deleted_files: &[String],
        parsing_results: Vec<crate::parse::parallel::ParsingResult>,
        source_file_hashes: &HashMap<String, String>,
    ) -> Result<Vec<String>> {
        let mut removed_node_ids = Vec::new();
        for path in deleted_files {
            removed_node_ids.extend(
                pdg.node_indices()
                    .filter_map(|node_idx| pdg.get_node(node_idx))
                    .filter(|node| node.file_path.as_ref() == path.as_str())
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
            // incident edge, including the cross-file ones this snapshot keeps.
            let preserved = preserve_cross_file_edges(pdg, &file_path);
            index_builder::remove_file_from_pdg(pdg, &file_path)?;
            let file_pdg = crate::graph::extract_pdg_from_signatures(
                result.signatures,
                source_bytes,
                &file_path,
                language,
            );
            index_builder::merge_pdgs(pdg, file_pdg);
            restore_cross_file_edges(pdg, preserved);
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

/// A cross-file edge incident to a re-parsed file's nodes, snapshotted before
/// the file's old nodes (and every incident edge) are removed.
struct PreservedCrossFileEdge {
    /// `node.id` string of the endpoint inside the re-parsed file. Ids are
    /// deterministic per file (`file_path:qualified_name`), so the replacement
    /// node for a surviving symbol carries the same string after the merge.
    file_side_id: String,
    /// `node.id` string of the endpoint in another file — a real definition
    /// (untouched by this delta) or an external placeholder.
    other_side_id: String,
    /// `true` when the edge runs `other_side -> file_side`.
    incoming: bool,
    /// The original edge weight (type, call count, confidence, …).
    edge: crate::graph::pdg::Edge,
}

/// Snapshot every edge crossing the file boundary of `file_path`.
///
/// Edges with both endpoints inside the file are skipped (the re-extraction
/// recreates them) as are edges touching neither the file nor its nodes.
/// Both directions are captured: an unchanged caller's edge into the edited
/// file dies with the edited file's old node, and so does the edited file's
/// own resolved edge into another file's definition.
fn preserve_cross_file_edges(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    file_path: &str,
) -> Vec<PreservedCrossFileEdge> {
    let file_node_ids: HashSet<String> = pdg
        .node_indices()
        .filter_map(|idx| pdg.get_node(idx))
        .filter(|node| node.file_path.as_ref() == file_path)
        .map(|node| node.id.to_string())
        .collect();
    let mut preserved = Vec::new();
    for edge_id in pdg.edge_indices() {
        let Some((from, to)) = pdg.edge_endpoints(edge_id) else {
            continue;
        };
        let (Some(from_node), Some(to_node)) = (pdg.get_node(from), pdg.get_node(to)) else {
            continue;
        };
        let from_in_file = file_node_ids.contains(from_node.id.as_str());
        let to_in_file = file_node_ids.contains(to_node.id.as_str());
        if from_in_file == to_in_file {
            continue;
        }
        let Some(edge) = pdg.get_edge(edge_id) else {
            continue;
        };
        preserved.push(if from_in_file {
            PreservedCrossFileEdge {
                file_side_id: from_node.id.to_string(),
                other_side_id: to_node.id.to_string(),
                incoming: false,
                edge: edge.clone(),
            }
        } else {
            PreservedCrossFileEdge {
                file_side_id: to_node.id.to_string(),
                other_side_id: from_node.id.to_string(),
                incoming: true,
                edge: edge.clone(),
            }
        });
    }
    preserved
}

/// Re-attach the snapshotted cross-file edges onto the re-merged graph.
///
/// An edge is restored only when BOTH endpoints resolve by id string: the
/// file side must have survived the edit (a renamed/removed symbol's edges
/// are correctly dropped), and the other side must still exist. An edge the
/// fresh per-file extraction already recreated (e.g. the file's own call to
/// an external placeholder) is not duplicated. Returns the restored count.
fn restore_cross_file_edges(
    pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    preserved: Vec<PreservedCrossFileEdge>,
) -> usize {
    use petgraph::visit::EdgeRef;
    let mut restored = 0usize;
    for edge in preserved {
        let (Some(file_side), Some(other_side)) = (
            pdg.find_by_id(&edge.file_side_id),
            pdg.find_by_id(&edge.other_side_id),
        ) else {
            continue;
        };
        let (from, to) = if edge.incoming {
            (other_side, file_side)
        } else {
            (file_side, other_side)
        };
        let already_present = pdg.graph.edges(from).any(|reference| {
            reference.target() == to && reference.weight().edge_type == edge.edge.edge_type
        });
        if already_present {
            continue;
        }
        pdg.add_edge(from, to, edge.edge);
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

    /// The watcher-delta contract (Codex P1, round 8): removing a re-parsed
    /// file's nodes deletes every incident edge, so the cross-file edges must
    /// be snapshotted before removal and re-attached after the merge — in
    /// BOTH directions (the file's own resolved calls out, other files' calls
    /// in) — or impact/dependency queries lose the edited file's
    /// relationships until a full reindex.
    #[test]
    fn test_cross_file_edges_survive_a_watcher_remerge() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let caller = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let callee = pdg.add_node(symbol("b.rs:helper", "b.rs", "helper", NodeType::Function));
        let external = pdg.add_node(Node {
            language: "external".to_string(),
            node_type: NodeType::External,
            ..symbol("external::log", "<external>", "log", NodeType::External)
        });
        pdg.add_edge(caller, callee, call_edge());
        pdg.add_edge(caller, external, call_edge());
        // An intra-file edge: recreated by re-extraction, never snapshotted.
        let helper2 = pdg.add_node(symbol("a.rs:main2", "a.rs", "main2", NodeType::Function));
        pdg.add_edge(caller, helper2, call_edge());

        let preserved = preserve_cross_file_edges(&pdg, "a.rs");
        assert_eq!(preserved.len(), 2, "only cross-file edges are captured");
        assert!(
            preserved
                .iter()
                .all(|edge| !edge.incoming && edge.file_side_id == "a.rs:main")
        );

        // Simulate the re-merge: drop the file's old nodes (edges die with
        // them), re-add the surviving symbol under the same deterministic id.
        pdg.remove_file("a.rs");
        assert!(!has_edge(&pdg, callee, caller, &EdgeType::Call));
        let caller_new = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));

        let restored = restore_cross_file_edges(&mut pdg, preserved);
        assert_eq!(restored, 2);
        assert!(
            has_edge(&pdg, caller_new, callee, &EdgeType::Call),
            "the file's resolved call into another file must be restored"
        );
        assert!(
            has_edge(&pdg, caller_new, external, &EdgeType::Call),
            "the call to the external placeholder must be restored"
        );
    }

    /// An edge whose far end disappeared with the edit (the symbol was
    /// renamed or removed in the other file) is dropped, not re-attached to a
    /// stale node.
    #[test]
    fn test_restore_drops_edges_whose_other_side_vanished() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let caller = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let callee = pdg.add_node(symbol("b.rs:helper", "b.rs", "helper", NodeType::Function));
        pdg.add_edge(caller, callee, call_edge());

        let preserved = preserve_cross_file_edges(&pdg, "b.rs");
        assert_eq!(preserved.len(), 1);
        assert!(preserved[0].incoming, "b.rs's node is the edge target");

        pdg.remove_file("b.rs");
        // The replacement b.rs no longer defines `helper` — only a renamed one.
        let _replacement = pdg.add_node(symbol(
            "b.rs:renamed",
            "b.rs",
            "renamed",
            NodeType::Function,
        ));
        assert_eq!(restore_cross_file_edges(&mut pdg, preserved), 0);
    }

    /// An edge the fresh per-file extraction already recreated (here: the
    /// file's own call to an external placeholder) must not be duplicated.
    #[test]
    fn test_restore_does_not_duplicate_recreated_edges() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let caller = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        let external = pdg.add_node(Node {
            language: "external".to_string(),
            ..symbol("external::log", "<external>", "log", NodeType::External)
        });
        pdg.add_edge(caller, external, call_edge());

        let preserved = preserve_cross_file_edges(&pdg, "a.rs");
        pdg.remove_file("a.rs");
        let caller_new = pdg.add_node(symbol("a.rs:main", "a.rs", "main", NodeType::Function));
        // The re-extraction's placeholder edge (merge remaps it onto the
        // shared external node).
        pdg.add_edge(caller_new, external, call_edge());

        assert_eq!(restore_cross_file_edges(&mut pdg, preserved), 0);
        assert_eq!(
            pdg.graph.edges(caller_new).count(),
            1,
            "exactly one caller->external edge remains"
        );
    }
}
