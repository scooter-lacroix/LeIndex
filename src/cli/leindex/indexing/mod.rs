// Indexing pipeline methods for LeIndex: index_project and load_from_storage.

use super::{LeIndex, ProjectFileScan};
use crate::cli::index_builder;
use crate::cli::index_job::{
    CheckpointStore, JobPaths, LexicalCheckpoint, NeuralCheckpoint, ParseCheckpoint,
    ParsedFileCheckpoint, PdgCheckpoint, PublishedGeneration, ScanCheckpoint,
    latest_incomplete_job,
};
use crate::cli::memory_cap::MemoryCapGuard;
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;
use tracing::{info, warn};
mod helpers;
use helpers::*;

mod load;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Streaming index pipeline modules (WS6-9 Tasks 1-7).
///
/// Each stage converts from "materialize the whole corpus" to "stream bounded
/// chunks" with direct CAS staging. Feature-flagged via
/// `LEINDEX_FEATURE_STREAMING_*`, default OFF. The legacy pipeline runs
/// unmodified when flags are disabled.
///
/// `#[allow(dead_code)]` marks modules whose public API will be consumed by
/// the streaming pipeline integration landing in SP4 Task 8-9 (the follow-up
/// feature that wires these stages behind feature flags into the main
/// `index_project_inner` loop). Tests exercise every public item today.
#[allow(dead_code)]
pub(crate) mod streaming;

/// Runtime state carried between the six explicit indexing phases. It is
/// present only while `index_project_inner` is executing and is cleared before
/// the call returns. Durable checkpoints remain the restart contract.
pub(crate) struct IndexPipelineState {
    pub(crate) force: bool,
    pub(crate) start_time: Instant,
    pub(crate) job: JobPaths,
    pub(crate) checkpoint_store: Option<CheckpointStore>,
    pub(crate) indexed_files: HashMap<String, String>,
    pub(crate) old_scan: Option<ProjectFileScan>,
    pub(crate) source_files_with_hashes: Vec<(PathBuf, String)>,
    pub(crate) source_file_hashes: HashMap<String, String>,
    pub(crate) current_file_paths: HashSet<String>,
    pub(crate) files_to_parse: Vec<PathBuf>,
    pub(crate) unchanged_files: HashSet<String>,
    pub(crate) deleted_files: Vec<String>,
    pub(crate) resumed_scan: Option<ScanCheckpoint>,
    pub(crate) resumed_parse: Option<ParseCheckpoint>,
    pub(crate) resumed_pdg: Option<PdgCheckpoint>,
    pub(crate) resumed_lexical: Option<LexicalCheckpoint>,
    pub(crate) resumed_neural: Option<NeuralCheckpoint>,
    pub(crate) parsing_results: Vec<crate::parse::parallel::ParsingResult>,
    pub(crate) parse_checkpoint: Option<ParseCheckpoint>,
    pub(crate) pdg: Option<crate::graph::pdg::ProgramDependenceGraph>,
    pub(crate) pdg_checkpoint: Option<PdgCheckpoint>,
    pub(crate) pdg_node_count: usize,
    pub(crate) pdg_edge_count: usize,
    pub(crate) files_parsed: usize,
    pub(crate) successful: usize,
    pub(crate) failed: usize,
    pub(crate) total_sigs: usize,
    pub(crate) ext_in_lockfile: usize,
    pub(crate) ext_resolved: usize,
    pub(crate) ext_unresolved: usize,
    pub(crate) ext_total: usize,
    pub(crate) ext_builtin: usize,
    pub(crate) lexical_checkpoint: Option<LexicalCheckpoint>,
    pub(crate) lexical_hash: Option<String>,
    pub(crate) core_health: Option<super::IndexHealth>,
    pub(crate) neural_checkpoint: Option<NeuralCheckpoint>,
    pub(crate) neural_rows: usize,
    pub(crate) neural_resume_loaded: bool,
    pub(crate) admitted_node_ids: HashSet<String>,
    /// Cached enriched node content from the DF/indexing pass, reused by the
    /// neural enrichment pass to avoid recomputing `enriched_node_content`.
    pub(crate) enriched_content_cache: HashMap<String, String>,
    pub(crate) skip: bool,
}

fn sorted_admitted_node_ids(admitted_node_ids: &HashSet<String>) -> Vec<String> {
    let mut ids: Vec<String> = admitted_node_ids.iter().cloned().collect();
    ids.sort_unstable();
    ids
}

fn restored_admitted_node_ids(checkpoint: Option<&LexicalCheckpoint>) -> HashSet<String> {
    checkpoint
        .map(|checkpoint| checkpoint.admitted_node_ids.iter().cloned().collect())
        .unwrap_or_default()
}

impl IndexPipelineState {
    fn new(force: bool, start_time: Instant, job: JobPaths) -> Self {
        Self {
            force,
            start_time,
            job,
            checkpoint_store: None,
            indexed_files: HashMap::new(),
            old_scan: None,
            source_files_with_hashes: Vec::new(),
            source_file_hashes: HashMap::new(),
            current_file_paths: HashSet::new(),
            files_to_parse: Vec::new(),
            unchanged_files: HashSet::new(),
            deleted_files: Vec::new(),
            resumed_scan: None,
            resumed_parse: None,
            resumed_pdg: None,
            resumed_lexical: None,
            resumed_neural: None,
            parsing_results: Vec::new(),
            parse_checkpoint: None,
            pdg: None,
            pdg_checkpoint: None,
            pdg_node_count: 0,
            pdg_edge_count: 0,
            files_parsed: 0,
            successful: 0,
            failed: 0,
            total_sigs: 0,
            ext_in_lockfile: 0,
            ext_resolved: 0,
            ext_unresolved: 0,
            ext_total: 0,
            ext_builtin: 0,
            lexical_checkpoint: None,
            lexical_hash: None,
            core_health: None,
            neural_checkpoint: None,
            neural_rows: 0,
            neural_resume_loaded: false,
            admitted_node_ids: HashSet::new(),
            enriched_content_cache: HashMap::new(),
            skip: false,
        }
    }
}

impl LeIndex {
    fn checkpoint_store(&self, generation: u64) -> CheckpointStore {
        CheckpointStore::new(self.storage_path(), generation)
    }

    fn checkpoint_state(&self, store: &CheckpointStore, phase: &str, hash: String) {
        let mut state = store.read_state().ok().flatten().unwrap_or_default();
        if state.job_id.is_empty() {
            state.job_id = format!("index-{}", store.paths.generation);
        }
        state.input_generation = store.paths.generation.saturating_sub(1);
        state.last_reusable_phase = Some(phase.to_string());
        state.artifact_hashes.insert(phase.to_string(), hash);
        state.updated_at_unix_ms = crate::cli::index_job::checkpoint_now_unix_ms();
        let _ = store.write_state(&state);
    }

    fn checkpoint_generation(&self) -> u64 {
        let max_generation = std::fs::read_dir(self.storage_path().join("generations"))
            .ok()
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().to_str()?.parse::<u64>().ok())
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let root_generation = crate::cli::index_freshness::load_health(self.storage_path())
            .map(|health| health.generation)
            .unwrap_or(0);
        max_generation.max(root_generation).saturating_add(1)
    }

    fn prepare_generation_snapshot(
        &self,
        staging: &std::path::Path,
        health: &super::IndexHealth,
        include_neural: bool,
    ) -> Result<()> {
        // WAL is checkpointed before copying the immutable catalog snapshot;
        // query readers never observe a half-written generation.
        self.storage
            .conn()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .context("checkpoint SQLite WAL before generation publication")?;
        let mut copied = HashSet::new();
        let mut artifact_paths = vec![
            self.storage_path().join("leindex.db"),
            self.storage_path().join("index_stats.json"),
            self.project_path.join(".leindex/search_snapshot.bin"),
            self.project_path.join(".leindex/embeddings.bin"),
            self.project_path.join(".leindex/tfidf_embedder.bin"),
        ];
        if include_neural {
            artifact_paths.push(self.project_path.join(".leindex/neural_embeddings.bin"));
        }
        // Fragment layer (Task 6, invariant 8): the four fragment artifacts
        // must be published with each generation or a cold start resolving the
        // immutable generation would lose the fragment store/mmap and the
        // indexed fragment layer would silently vanish. Copied only when the
        // files exist (feature-off leaves nothing extra); validated on load by
        // `fragment_layer_is_valid` (root hash + mmap row count).
        for name in [
            "fragment_store.bin",
            "fragment_root.bin",
            "fragment_sync_manifest.bin",
            "fragments_embeddings.bin",
        ] {
            artifact_paths.push(self.project_path.join(".leindex").join(name));
        }
        for source in artifact_paths {
            if !source.is_file() || !copied.insert(source.clone()) {
                continue;
            }
            let Some(name) = source.file_name() else {
                continue;
            };
            let destination = staging.join(name);
            let next = destination.with_extension("next");
            std::fs::copy(&source, &next).with_context(|| {
                format!(
                    "copy generation artifact {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
            std::fs::rename(next, destination)?;
        }
        crate::cli::index_freshness::save_health(staging, health)?;
        #[cfg(unix)]
        std::fs::File::open(staging)?.sync_all()?;
        Ok(())
    }

    fn promote_generation_snapshot(
        &self,
        staging: &std::path::Path,
        target: &std::path::Path,
        _generations: &std::path::Path,
        generation: u64,
    ) -> Result<()> {
        // Rename the complete directory once. Readers either see no new
        // generation or the fully materialized immutable snapshot.
        std::fs::rename(staging, target).with_context(|| {
            format!(
                "promote staged generation {} -> {}",
                staging.display(),
                target.display()
            )
        })?;
        #[cfg(unix)]
        std::fs::File::open(_generations)?.sync_all()?;
        let current = self.storage_path().join("CURRENT");
        let next = self.storage_path().join("CURRENT.next");
        let mut current_file = std::fs::File::create(&next)?;
        use std::io::Write as _;
        current_file.write_all(format!("{generation}\n").as_bytes())?;
        current_file.sync_all()?;
        drop(current_file);
        std::fs::rename(next, current)?;
        #[cfg(unix)]
        std::fs::File::open(self.storage_path())?.sync_all()?;
        Ok(())
    }

    fn publish_generation_snapshot(
        &self,
        requested_generation: u64,
        health: &mut super::IndexHealth,
        include_neural: bool,
    ) -> Result<PublishedGeneration> {
        let generations = self.storage_path().join("generations");
        std::fs::create_dir_all(&generations)?;

        // Allocate the generation number under contention. The requested
        // number is only a hint computed earlier (max+1 at planning time —
        // a TOCTOU guess): a concurrent writer (a second MCP server, or the
        // edit-triggered incremental refresh racing the watcher refresh)
        // may publish the same number between planning and this call.
        // Previously the loser bailed with "generation N already exists",
        // which failed the entire index job and left the store
        // status=failed with a wrecked signature count (N-10). Snapshots
        // are immutable, but the NUMBER is allocatable: bump past every
        // number that exists on disk — or that wins the rename race — and
        // retry.
        let mut generation = requested_generation;
        loop {
            while generations.join(generation.to_string()).exists() {
                generation += 1;
            }
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let staging = generations.join(format!(
                ".staging-{generation}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&staging).with_context(|| {
                format!("create staging generation directory {}", staging.display())
            })?;
            // The health record embedded in the snapshot (and later
            // persisted by the caller) must carry the number actually
            // published, not the stale planning hint.
            health.generation = generation;
            let target = generations.join(generation.to_string());
            let attempt = self
                .prepare_generation_snapshot(&staging, health, include_neural)
                .and_then(|()| {
                    self.promote_generation_snapshot(&staging, &target, &generations, generation)
                });
            if let Err(error) = attempt {
                let _ = std::fs::remove_dir_all(&staging);
                // Lost the rename race to a concurrent publisher: the
                // target appeared between the exists() check and the
                // atomic rename. Retry on the next free number; anything
                // else is a real failure.
                if target.exists() {
                    generation += 1;
                    continue;
                }
                return Err(error);
            }
            self.hydrated_generation
                .store(generation, std::sync::atomic::Ordering::Release);
            return Ok(PublishedGeneration {
                generation,
                storage_path: target,
                health: health.clone(),
            });
        }
    }

    /// The generation this process's in-memory index state was hydrated (or
    /// last published) from — `None` until the first hydration. Compared by
    /// the registry against the persisted `CURRENT` pointer to detect
    /// external rebuilds (N-13).
    pub fn hydrated_generation(&self) -> Option<u64> {
        let value = self
            .hydrated_generation
            .load(std::sync::atomic::Ordering::Acquire);
        (value > 0).then_some(value)
    }

    /// Persist a tiny phase marker so diagnostics and owned MCP jobs can show
    /// useful progress without touching the resident PDG/search state. The
    /// marker is advisory until the final atomic snapshot is published.
    fn mark_index_phase(&self, phase: super::IndexPhase, status: super::ComponentStatus) {
        let Some(previous) = crate::cli::index_freshness::load_health(self.storage_path()) else {
            let health = super::IndexHealth {
                phase,
                status,
                ..super::IndexHealth::default()
            };
            let _ = crate::cli::index_freshness::save_health(self.storage_path(), &health);
            return;
        };
        let health = super::IndexHealth {
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

    pub(crate) fn incremental_reindex_from_watcher(&mut self) -> Result<super::IndexStats> {
        // NOTE: the cross-process write lock is acquired by the WATCHER
        // (non-blocking, skip-on-busy) before calling this fn — see
        // `try_acquire_write_lock` in mod.rs and watcher.rs. Do not add a
        // blocking flock here: spawn_blocking cannot be cancelled, so a
        // blocking acquire held by another process would stall the watcher.
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
        let mut pdg = self.pdg.take().unwrap_or_default();
        let removed_node_ids = self.apply_incremental_pdg_changes(
            &mut pdg,
            &deleted_files,
            parsing_results,
            &source_file_hashes,
        )?;

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
    fn compute_and_persist_communities(
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
    fn compute_and_persist_communities(
        &mut self,
        _pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    ) {
    }

    #[cfg(feature = "precision")]
    fn run_precision_ingest(&self, pdg: &mut crate::graph::pdg::ProgramDependenceGraph) {
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
    fn run_precision_ingest(&self, _pdg: &mut crate::graph::pdg::ProgramDependenceGraph) {}

    /// Persist the watcher-reindex delta (PDG, embeddings, snapshot, neural) and
    /// publish the new generation with fresh health. Owns all post-merge I/O so
    /// the reindex orchestrator stays a thin pipeline.
    fn persist_and_publish_watcher_delta(
        &mut self,
        mut pdg: crate::graph::pdg::ProgramDependenceGraph,
        embedder: index_builder::HybridEmbedder,
        source_files_with_hashes: Vec<(PathBuf, String)>,
        start_time: std::time::Instant,
    ) -> Result<super::IndexStats> {
        // Precision does NOT run on the watcher/edit-apply path: a full SCIP
        // indexer pass (minutes with rust-analyzer) would block the project
        // write lock every time a file is saved. Markers for changed files
        // drop until the next explicit index, which re-merges precision.
        // Persist the updated PDG to storage so changes survive restart
        index_builder::save_to_storage(&mut self.storage, &self.project_id, &pdg)?;
        self.compute_and_persist_communities(&mut pdg);

        // Snapshot/embedder freshness must describe the graph as the DB
        // reconstructs it (duplicate node_ids collapse on save); otherwise
        // every later cold hydration takes the full TF-IDF rebuild path.
        let persisted_identity =
            index_builder::persisted_search_identity(&self.storage, &self.project_id);
        self.pdg = Some(pdg);
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
        let mut health = super::IndexHealth {
            generation,
            phase: super::IndexPhase::Complete,
            status: super::ComponentStatus::Fresh,
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
    fn apply_incremental_pdg_changes(
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
            index_builder::remove_file_from_pdg(pdg, &file_path)?;
            let file_pdg = crate::graph::extract_pdg_from_signatures(
                result.signatures,
                source_bytes,
                &file_path,
                language,
            );
            index_builder::merge_pdgs(pdg, file_pdg);
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
    fn build_changed_node_infos(
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
    fn dirty_source_paths(
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
                        super::SOURCE_FILE_EXTENSIONS
                            .iter()
                            .any(|known| known.eq_ignore_ascii_case(extension))
                    })
            })
            .collect()
    }

    /// Index the project with an optional memory cap.
    ///
    /// This is the same as `index_project(force)` but additionally monitors RSS
    /// memory usage throughout the indexing pipeline. When `max_memory_bytes` is
    /// `Some(bytes)`, a `MemoryCapGuard` is created that:
    /// - Logs a warning when RSS exceeds 90% of the cap
    /// - Reports `OverCap` (a deferral signal) when RSS exceeds 100% of the cap
    ///
    /// VAL-SCHED-015: the cap is an ADMISSION cap, not a hard error. Over-cap
    /// pressure defers heavy work (and the global admission controller owns the
    /// actual defer/reduce/evict decision) — indexing is never aborted at the
    /// cap. The memory check is performed at key checkpoints during indexing to
    /// avoid excessive overhead while still catching runaway memory usage.
    pub fn index_project_with_memory_cap(
        &mut self,
        force: bool,
        max_memory_bytes: Option<u64>,
    ) -> Result<super::IndexStats> {
        let mut cap_guard = match max_memory_bytes {
            Some(bytes) => {
                let mb = bytes / (1024 * 1024);
                if mb == 0 {
                    bail!("--max-memory must be at least 1 MB");
                }
                info!("Memory cap enabled: {} MB", mb);
                Some(MemoryCapGuard::new(mb))
            }
            None => None,
        };

        self.index_project_inner(force, cap_guard.as_mut())
    }

    /// Index the project
    ///
    /// This executes the full indexing pipeline:
    /// 1. Parse all source files in parallel (incrementally)
    /// 2. Extract PDG from parsed signatures
    /// 3. Index nodes for semantic search
    /// 4. Persist PDG to storage
    ///
    /// # Arguments
    ///
    /// * `force` - If true, re-index all files regardless of changes
    ///
    /// # Returns
    ///
    /// `Result<IndexStats>` - Statistics from the indexing operation
    pub fn index_project(&mut self, force: bool) -> Result<super::IndexStats> {
        self.index_project_inner(force, None)
    }

    fn index_project_inner(
        &mut self,
        force: bool,
        mut cap_guard: Option<&mut MemoryCapGuard>,
    ) -> Result<super::IndexStats> {
        // Serialize concurrent writers across processes (e.g. a second MCP
        // instance, or MCP + CLI) so two processes never write leindex.db at
        // once. Blocks until exclusive; RAII releases on return. Without this,
        // concurrent writers contend on SQLite WAL (one writer max) and can
        // corrupt the DB, bricking the generation. See `ProjectWriteLock`.
        //
        // Scope note: this guards the HEAVY writes (PDG + neural embeddings +
        // publish) — the contention that actually bricked gen-93. The brief
        // startup writes in `LeIndex::new` (schema migration, project-metadata
        // insert) happen before this guard runs; they are idempotent and
        // serialized by SQLite's own busy_timeout, so they are not a bricking
        // risk. If startup-write contention is ever observed, extend the lock
        // to a write-mode `Storage::open` (or make `ProjectWriteLock`
        // re-entrant so `new()` can also acquire it without self-deadlock).
        let _write_lock = self.acquire_write_lock()?;
        let start_time = Instant::now();
        let job = JobPaths::new(self.storage_path(), self.checkpoint_generation());
        self.pipeline = Some(IndexPipelineState::new(force, start_time, job.clone()));
        self.mark_index_phase(
            super::IndexPhase::Scan,
            super::ComponentStatus::Initializing,
        );
        info!(
            "Starting project indexing for: {} (force={})",
            self.project_id, force
        );

        let scan = self.run_scan(&job)?;
        check_memory_cap(&mut cap_guard)?;
        self.mark_index_phase(
            super::IndexPhase::Parse,
            super::ComponentStatus::Initializing,
        );
        let parsed = self.run_parse(&job, &scan)?;
        check_memory_cap(&mut cap_guard)?;
        if self.pipeline.as_ref().is_some_and(|state| state.skip) {
            self.pipeline = None;
            progress_clear();
            return Ok(self.stats.clone());
        }

        let pdg = self.run_pdg(&job, &parsed)?;
        check_memory_cap(&mut cap_guard)?;
        let lexical = self.run_lexical(&job, &pdg)?;

        // Publish the core (lexical-only) generation as a crash-recovery
        // checkpoint.  If the process dies during neural, the resumed run
        // can skip straight to the neural phase using this snapshot.
        let _core = self.publish_generation(&job, None)?;
        self.refresh_text_index();

        let neural = self.run_neural(&job, &lexical)?;
        let _enhanced = self.publish_generation(&job, Some(&neural))?;

        let state = self
            .pipeline
            .take()
            .context("indexing pipeline state missing during finalization")?;
        let store = state
            .checkpoint_store
            .as_ref()
            .context("indexing checkpoint store missing during finalization")?;
        index_builder::clear_query_caches(&mut self.cache.cache_spiller, &self.project_id);
        info!("Indexing completed in {}ms", self.stats.indexing_time_ms);
        crate::cli::memory_report::observe_rss("post_index");
        progress_clear();
        let health =
            crate::cli::index_freshness::load_health(self.storage_path()).unwrap_or_default();
        self.checkpoint_state(
            store,
            "complete",
            blake3::hash(&serde_json::to_vec(&health).unwrap_or_default())
                .to_hex()
                .to_string(),
        );
        Ok(self.stats.clone())
    }

    pub(crate) fn run_scan(&mut self, _job: &JobPaths) -> Result<ScanCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("scan phase started without pipeline state")?;
        progress_stderr("Indexing: scanning files...");
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .context("Failed to load indexed files from storage")?;
        let old_scan = self.get_project_scan(false).ok();
        // Hash source files without caching bodies (VAL-STREAM-012: no
        // cross-phase source-body retention). Each phase re-reads per chunk.
        let source_files_with_hashes = self.collect_source_files_with_hashes(true)?;
        info!("Found {} source files", source_files_with_hashes.len());
        let scan = scan_checkpoint(&source_files_with_hashes);
        let generation = state.job.generation;
        // force_reindex=true must bypass resume entirely. The resume reuses a
        // prior job's parse/PDG artifacts when the source hash matches — correct
        // for non-force incremental runs (and crash recovery), but on a forced
        // rebuild it would re-publish stale artifacts and prevent picking up
        // parser / indexing-logic changes (the whole point of --force). Note
        // `latest_incomplete_job` keys on `last_reusable_phase != "complete"`,
        // and no publication path writes "complete", so a successfully published
        // job remains forever "resumable" — force therefore has to skip the
        // lookup rather than rely on a completeness marker.
        let (checkpoint_store, resumed_scan) = if state.force {
            (self.checkpoint_store(generation), None)
        } else {
            latest_incomplete_job(self.storage_path())
                .and_then(|(paths, _)| {
                    let store = CheckpointStore::from_paths(paths);
                    let saved = store.read_scan().ok().flatten()?;
                    (saved.input_hash == scan.input_hash).then_some((store, saved))
                })
                .map(|(store, saved)| (store, Some(saved)))
                .unwrap_or_else(|| (self.checkpoint_store(generation), None))
        };
        state.job = checkpoint_store.paths.clone();
        if resumed_scan.is_none() {
            let scan_hash = checkpoint_store.write_scan(&scan)?;
            self.checkpoint_state(&checkpoint_store, "scan", scan_hash);
        }
        let checkpoint_state = checkpoint_store.read_state().ok().flatten();
        let resumed_parse = read_verified_artifact(
            checkpoint_state.as_ref(),
            "parse",
            &checkpoint_store.paths.parse(),
            CheckpointStore::read_parse,
            &checkpoint_store,
        )
        .filter(|checkpoint| checkpoint.scan_hash == scan.input_hash);
        let resumed_pdg = load_resumed_pdg(
            &checkpoint_store,
            &scan,
            resumed_scan.is_some(),
            checkpoint_state
                .as_ref()
                .and_then(|checkpoint| checkpoint.artifact_hashes.get("pdg").cloned()),
        );
        let resumed_lexical = read_verified_artifact(
            checkpoint_state.as_ref(),
            "lexical",
            &checkpoint_store.paths.lexical(),
            CheckpointStore::read_lexical,
            &checkpoint_store,
        )
        .filter(valid_lexical_checkpoint);
        let resumed_neural = read_verified_artifact(
            checkpoint_state.as_ref(),
            "neural",
            &checkpoint_store.paths.neural(),
            CheckpointStore::read_neural,
            &checkpoint_store,
        );
        state.indexed_files = indexed_files;
        state.old_scan = old_scan;
        state.source_files_with_hashes = source_files_with_hashes;
        state.resumed_scan = resumed_scan;
        state.resumed_parse = resumed_parse;
        state.resumed_pdg = resumed_pdg
            .as_ref()
            .map(|(checkpoint, _)| checkpoint.clone());
        state.pdg = resumed_pdg.map(|(_, pdg)| pdg);
        state.resumed_lexical = resumed_lexical;
        state.admitted_node_ids = restored_admitted_node_ids(state.resumed_lexical.as_ref());
        state.resumed_neural = resumed_neural;
        state.checkpoint_store = Some(checkpoint_store);
        injected_phase_failure("scan")?;
        let result = scan.clone();
        self.pipeline = Some(state);
        Ok(result)
    }

    fn manifests_changed(&mut self, old_scan: Option<&ProjectFileScan>) -> Result<bool> {
        if self.check_manifest_stale() {
            info!("Manifest files changed — running external dependency annotation");
            return Ok(true);
        }
        let current_scan = self.get_project_scan(false)?;
        let changed_manifests = match old_scan {
            Some(old) => current_scan
                .manifest_paths
                .iter()
                .filter(|manifest| {
                    let key = manifest.display().to_string();
                    current_scan.manifest_hashes.get(&key) != old.manifest_hashes.get(&key)
                        && !key.to_lowercase().contains("node_modules")
                        && !key.to_lowercase().contains("/build/")
                        && !key.to_lowercase().contains("\\build\\")
                        && !key.to_lowercase().contains("/dist/")
                        && !key.to_lowercase().contains("\\dist\\")
                        && !key.to_lowercase().contains("/target/")
                        && !key.to_lowercase().contains(".cache")
                })
                .cloned()
                .collect::<Vec<_>>(),
            None => index_builder::detect_changed_manifests(
                &current_scan,
                &self.project_id,
                &self.cache.cache_spiller,
            ),
        };
        if changed_manifests.is_empty() {
            return Ok(false);
        }
        info!(
            "Manifest content changed ({} files) — re-annotating",
            changed_manifests.len()
        );
        Ok(true)
    }

    fn write_parse_checkpoint(
        &self,
        store: &CheckpointStore,
        scan: &ScanCheckpoint,
        parsing_results: &[crate::parse::parallel::ParsingResult],
        source_file_hashes: &HashMap<String, String>,
    ) -> Result<ParseCheckpoint> {
        let mut artifact_paths = Vec::new();
        let mut artifact_hashes = std::collections::BTreeMap::new();
        let mut parsed_buckets: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, Vec<ParsedFileCheckpoint>>,
        > = std::collections::BTreeMap::new();
        for result in parsing_results.iter().filter(|result| result.is_success()) {
            let path_key = result.file_path.display().to_string();
            if let Some(source_hash) = source_file_hashes.get(&path_key) {
                let checkpoint = ParsedFileCheckpoint {
                    file_path: result.file_path.clone(),
                    language: result.language.as_deref().unwrap_or("unknown").to_string(),
                    signatures: result.signatures.clone(),
                    parse_time_ms: result.parse_time_ms,
                };
                let bucket = source_hash.chars().take(2).collect::<String>();
                parsed_buckets
                    .entry(bucket)
                    .or_default()
                    .entry(source_hash.clone())
                    .or_default()
                    .push(checkpoint);
            }
        }
        for (bucket, files) in parsed_buckets {
            let artifact_hash = store.write_parsed_batch(&bucket, &files)?;
            artifact_paths.push(store.paths.parsed_bucket(&bucket));
            for source_hash in files.keys() {
                artifact_hashes.insert(source_hash.clone(), artifact_hash.clone());
            }
        }
        artifact_paths.sort();
        Ok(ParseCheckpoint {
            scan_hash: scan.input_hash.clone(),
            artifact_paths,
            artifact_hashes,
        })
    }

    pub(crate) fn run_parse(
        &mut self,
        _job: &JobPaths,
        scan: &ScanCheckpoint,
    ) -> Result<ParseCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("parse phase started without pipeline state")?;
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("parse phase missing checkpoint store")?;
        let mut plan = parse_plan(&state);
        let resumed_parse_results = reuse_parse_results(
            state.resumed_scan.is_some(),
            state.resumed_parse.as_ref(),
            checkpoint_store,
            &plan.source_file_hashes,
            &mut plan.files_to_parse,
        )?;
        info!(
            "Incremental analysis: {} to parse, {} unchanged, {} deleted",
            plan.files_to_parse.len(),
            plan.unchanged_files.len(),
            plan.deleted_files.len()
        );
        if plan.files_to_parse.is_empty()
            && plan.deleted_files.is_empty()
            && self.is_indexed()
            && state.resumed_lexical.is_none()
            && !self.manifests_changed(state.old_scan.as_ref())?
        {
            info!("No changes detected, skipping indexing");
            self.mark_index_phase(super::IndexPhase::Complete, super::ComponentStatus::Fresh);
            state.skip = true;
            let result = ParseCheckpoint {
                scan_hash: scan.input_hash.clone(),
                artifact_paths: Vec::new(),
                artifact_hashes: std::collections::BTreeMap::new(),
            };
            self.pipeline = Some(state);
            return Ok(result);
        }
        progress_stderr(&format!(
            "Indexing: parsing {} files...",
            plan.files_to_parse.len()
        ));
        let parser = crate::parse::parallel::ParallelParser::new();
        let mut parsing_results = if plan.files_to_parse.is_empty() {
            Vec::new()
        } else {
            parser.parse_files(std::mem::take(&mut plan.files_to_parse))
        };
        parsing_results.extend(resumed_parse_results);
        let parse_checkpoint = self.write_parse_checkpoint(
            checkpoint_store,
            scan,
            &parsing_results,
            &plan.source_file_hashes,
        )?;
        let parse_hash = checkpoint_store.write_parse(&parse_checkpoint)?;
        self.checkpoint_state(checkpoint_store, "parse", parse_hash);
        injected_phase_failure("parse")?;
        state.source_file_hashes = plan.source_file_hashes;
        state.current_file_paths = plan.current_file_paths;
        state.files_to_parse = plan.files_to_parse;
        state.unchanged_files = plan.unchanged_files;
        state.deleted_files = plan.deleted_files;
        state.parsing_results = parsing_results;
        state.parse_checkpoint = Some(parse_checkpoint.clone());
        self.pipeline = Some(state);
        Ok(parse_checkpoint)
    }

    fn apply_pdg_file_changes(
        &mut self,
        state: &IndexPipelineState,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
        parsing_results: Vec<crate::parse::parallel::ParsingResult>,
        use_streaming: bool,
    ) -> Result<()> {
        for path in &state.deleted_files {
            index_builder::remove_file_from_pdg(pdg, path)?;
            if let Err(error) = crate::storage::pdg_store::delete_file_data(
                &mut self.storage,
                &self.project_id,
                path,
            ) {
                warn!(
                    "Failed to delete file data from storage for '{}' during indexing: {}",
                    path, error
                );
            }
        }
        let newly_parsed: Vec<crate::parse::parallel::ParsingResult> = parsing_results
            .into_iter()
            .filter(|result| result.is_success())
            .collect();
        // Drop stale nodes for freshly parsed files *before* the merge, so node
        // id namespaces (file_path:qualified_name) stay disjoint when the new
        // graphs are added.
        let mut changed_file_count = 0usize;
        for result in &newly_parsed {
            let file_path = result.file_path.display().to_string();
            index_builder::remove_file_from_pdg(pdg, &file_path)?;
            if let Some(hash) = state.source_file_hashes.get(&file_path) {
                if let Err(error) = crate::storage::pdg_store::update_indexed_file(
                    &mut self.storage,
                    &self.project_id,
                    &file_path,
                    hash,
                ) {
                    warn!(
                        "Failed to update indexed file record for '{}' during indexing: {}",
                        file_path, error
                    );
                }
            }
            changed_file_count += 1;
        }
        if !newly_parsed.is_empty() {
            let (new_pdg, route) = build_changed_file_pdg(newly_parsed, use_streaming);
            index_builder::merge_pdgs(pdg, new_pdg);
            info!(
                "PDG: rebuilt {} changed file(s) via {:?} ({} nodes, {} edges)",
                changed_file_count,
                route,
                pdg.node_count(),
                pdg.edge_count()
            );
        }
        Ok(())
    }

    fn annotate_external_dependencies(
        &self,
        state: &mut IndexPipelineState,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    ) {
        let manifest_paths = self
            .cache
            .project_scan
            .as_ref()
            .map(|scan| scan.manifest_paths.clone())
            .unwrap_or_default();
        let registry = crate::graph::ExternalDependencyRegistry::from_manifest_paths(
            &self.project_path,
            &manifest_paths,
        );
        let stats = crate::graph::annotate_external_nodes(pdg, &registry);
        if !registry.is_empty() {
            info!(
                "External dependency resolution: {}/{} resolved via lock files, {} recognized builtins ({} packages in registry)",
                stats.resolved,
                stats.total_external,
                stats.builtin,
                registry.len()
            );
        } else if stats.total_external > 0 {
            info!(
                "External dependency resolution: no lockfile registry found, {} builtins recognized, {} unresolved external imports",
                stats.builtin, stats.unresolved
            );
        }
        state.ext_in_lockfile = registry.len();
        state.ext_resolved = stats.resolved;
        state.ext_unresolved = stats.unresolved;
        state.ext_total = stats.total_external;
        state.ext_builtin = stats.builtin;
    }

    /// (Re)build the trigram text index used by `leindex_find`, with symbol
    /// spans read from the generation just published. Best effort: search
    /// falls back to live scanning, so a failure here never fails indexing.
    fn refresh_text_index(&self) {
        let root = self.project_path().to_path_buf();
        let storage = self.storage_path().to_path_buf();
        let db = crate::cli::live_project::LiveProject::resolve(&root.to_string_lossy())
            .map(|live| live.active_storage().join("leindex.db"))
            .ok()
            .filter(|db| db.is_file());
        if let Err(error) = crate::cli::textindex::build(&root, &storage, db.as_deref()) {
            warn!("Text index build failed (search will scan live): {error}");
        }
    }

    pub(crate) fn run_pdg(
        &mut self,
        _job: &JobPaths,
        parsed: &ParseCheckpoint,
    ) -> Result<PdgCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("PDG phase started without pipeline state")?;
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("PDG phase missing checkpoint store")?
            .clone();
        progress_stderr("Indexing: building PDG...");
        if !state.unchanged_files.is_empty() && self.pdg.is_none() && state.pdg.is_none() {
            self.load_pdg_from_storage().context(
                "Failed to load existing PDG for incremental reindex. Please reindex with --force if corruption persists.",
            )?;
        }
        let resumed_pdg_loaded = state.resumed_pdg.is_some() && state.pdg.is_some();
        let mut pdg = if resumed_pdg_loaded {
            state.pdg.take().unwrap_or_default()
        } else {
            state
                .pdg
                .take()
                .or_else(|| self.pdg.take())
                .unwrap_or_default()
        };
        let parsing_results = if resumed_pdg_loaded {
            Vec::new()
        } else {
            std::mem::take(&mut state.parsing_results)
        };
        let parse_stats = pdg_parse_stats(&parsing_results);
        // Route PDG construction: the streaming fragment/segment pipeline
        // (SP4) is the default; `LEINDEX_FEATURE_STREAMING_PDG=0` reverts to
        // the legacy per-file extraction + merge loop.
        let use_streaming = pdg_route_for_current_flag() == PdgBuildRoute::Streaming;
        info!(
            "PDG construction: streaming fragment pipeline {}",
            if use_streaming {
                "enabled"
            } else {
                "disabled (legacy merge)"
            }
        );
        self.apply_pdg_file_changes(&state, &mut pdg, parsing_results, use_streaming)?;
        // Resume-proof FileSummary pass: covers files loaded from storage on
        // resume (the merge_pdgs loop above only fires for freshly-parsed files).
        pdg.ensure_file_summary_nodes();
        if !parse_stats.all_signatures.is_empty() {
            crate::graph::resolve_cross_file_call_edges_for_files(
                &mut pdg,
                &parse_stats.all_signatures,
            );
            crate::graph::resolve_cross_file_flow_edges_for_files(
                &mut pdg,
                &parse_stats.all_signatures,
            );
        }
        self.annotate_external_dependencies(&mut state, &mut pdg);
        add_submodule_summary_nodes(&mut pdg, &self.project_path);
        index_builder::normalize_external_nodes(&mut pdg);
        // Precision must be merged before checkpoint counts and fingerprints are
        // captured; otherwise resumable artifacts describe a different graph.
        self.run_precision_ingest(&mut pdg);
        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();
        let pdg_checkpoint = checkpoint_store.write_pdg(parsed.scan_hash.clone(), &pdg)?;
        self.checkpoint_state(
            &checkpoint_store,
            "pdg",
            pdg_checkpoint.artifact_hash.clone(),
        );
        injected_phase_failure("pdg")?;
        self.mark_index_phase(super::IndexPhase::Pdg, super::ComponentStatus::Initializing);
        info!(
            "Updated PDG has {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );
        state.files_parsed = parse_stats.files_parsed;
        state.successful = parse_stats.successful;
        state.failed = parse_stats.failed;
        state.total_sigs = parse_stats.total_sigs;
        state.pdg_node_count = pdg_node_count;
        state.pdg_edge_count = pdg_edge_count;
        state.pdg_checkpoint = Some(pdg_checkpoint.clone());
        state.pdg = Some(pdg);
        self.pipeline = Some(state);
        Ok(pdg_checkpoint)
    }

    fn build_lexical_embedder(
        &mut self,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        resume_valid: bool,
    ) -> Result<(index_builder::HybridEmbedder, HashMap<String, String>)> {
        let batch_size = self.indexing_batch_size();
        let persisted = index_builder::TfIdfEmbedder::load_from_storage(&self.project_path)
            .ok()
            .flatten();
        if resume_valid {
            return match self.load_from_mutable_storage() {
                Ok(()) => {
                    self.search_engine.clear_neural_embeddings();
                    Ok((
                        self.embedder
                            .as_ref()
                            .map(|embedder| {
                                index_builder::HybridEmbedder::tfidf_only(embedder.tfidf().clone())
                            })
                            .or_else(|| {
                                persisted
                                    .clone()
                                    .map(index_builder::HybridEmbedder::tfidf_only)
                            })
                            .context("resumed lexical checkpoint has no TF-IDF embedder")?,
                        HashMap::new(),
                    ))
                }
                Err(error) => {
                    warn!(
                        "Failed to hydrate resumed lexical checkpoint; rebuilding core: {error:#}"
                    );
                    index_builder::index_nodes_tfidf_only(
                        pdg,
                        &mut self.search_engine,
                        &mut self.cache.file_stats_cache,
                        batch_size,
                        persisted.map(index_builder::HybridEmbedder::tfidf_only),
                    )
                }
            };
        }
        let persisted = match persisted {
            Some(embedder)
                if embedder.is_fresh(
                    pdg.node_count(),
                    pdg.edge_count(),
                    &crate::cli::index_builder::pdg_search_fingerprint(pdg),
                ) =>
            {
                info!("Loaded persisted embedder from storage");
                Some(embedder)
            }
            Some(_) => {
                info!("Persisted embedder is stale; rebuilding TF-IDF index");
                None
            }
            None => None,
        };
        index_builder::index_nodes_tfidf_only(
            pdg,
            &mut self.search_engine,
            &mut self.cache.file_stats_cache,
            batch_size,
            persisted.map(index_builder::HybridEmbedder::tfidf_only),
        )
    }

    pub(crate) fn run_lexical(
        &mut self,
        _job: &JobPaths,
        pdg_checkpoint: &PdgCheckpoint,
    ) -> Result<LexicalCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("lexical phase started without pipeline state")?;
        let mut pdg = state
            .pdg
            .take()
            .or_else(|| self.pdg.take())
            .context("lexical phase missing resident PDG")?;
        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();
        progress_stderr(&format!("Indexing: embedding {} nodes...", pdg_node_count));
        let lexical_resume_valid = state
            .resumed_lexical
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.pdg_hash == pdg_checkpoint.artifact_hash);
        let (embedder, content_cache) = self.build_lexical_embedder(&pdg, lexical_resume_valid)?;
        self.embedder = Some(embedder);
        let indexed_count = self.search_engine.node_count();
        state.admitted_node_ids = self.search_engine.live_node_ids().into_iter().collect();
        state.enriched_content_cache = content_cache;
        self.mark_index_phase(
            super::IndexPhase::Lexical,
            super::ComponentStatus::Initializing,
        );
        info!("Indexed {} nodes for search", indexed_count);
        progress_stderr("Indexing: saving to storage...");
        self.mark_index_phase(
            super::IndexPhase::Persist,
            super::ComponentStatus::Initializing,
        );
        index_builder::save_to_storage(&mut self.storage, &self.project_id, &pdg)?;
        // Snapshot/embedder freshness identity must describe the graph as the
        // DB reconstructs it (the in-memory graph may hold duplicate node_ids
        // the upsert collapses), so compute it from storage AFTER the save.
        let persisted_identity =
            index_builder::persisted_search_identity(&self.storage, &self.project_id);
        if let Some(embedder) = &self.embedder {
            embedder.persist_to_storage(&self.project_path, &pdg, persisted_identity.clone())?;
        }
        self.compute_and_persist_communities(&mut pdg);
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("lexical phase missing checkpoint store")?;
        self.checkpoint_state(
            checkpoint_store,
            "persist",
            pdg_checkpoint.artifact_hash.clone(),
        );
        injected_phase_failure("persist")?;
        self.stats = super::IndexStats {
            total_files: state.source_files_with_hashes.len(),
            files_parsed: state.files_parsed,
            successful_parses: state.successful,
            failed_parses: state.failed,
            total_signatures: state.total_sigs,
            // An incremental run parses only the changed subset; its signature
            // count is a delta, not the project total — label it so readers
            // stop mistaking "2" for a collapse from thousands.
            signature_scope: if state.files_parsed < state.source_files_with_hashes.len() {
                "delta".to_string()
            } else {
                "full".to_string()
            },
            pdg_nodes: pdg_node_count,
            pdg_edges: pdg_edge_count,
            indexed_nodes: indexed_count,
            indexing_time_ms: state.start_time.elapsed().as_millis() as u64,
            external_deps_in_lockfile: state.ext_in_lockfile,
            external_deps_resolved: state.ext_resolved,
            external_deps_unresolved: state.ext_unresolved,
            external_deps_total: state.ext_total,
            external_deps_builtin: state.ext_builtin,
        };
        self.pdg = Some(pdg);
        // A core generation is intentionally lexical/PDG-only. Existing
        // neural rows are reattached only by run_neural after CURRENT moves.
        self.search_engine.clear_neural_embeddings();
        self.build_file_stats_cache();
        index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)?;
        // Fragment layer (Task 7): incremental sync before the snapshot
        // persist. On failure the layer is CLEARED (not just logged) — see
        // `sync_fragment_layer_or_clear` (Codex wave-4 P2).
        self.sync_fragment_layer_or_clear();
        let (identity_nodes, identity_edges, identity_fingerprint) =
            persisted_identity.unwrap_or_else(|| (pdg_node_count, pdg_edge_count, String::new()));
        index_builder::persist_search_snapshot(
            &self.search_engine,
            &self.project_path,
            identity_nodes,
            identity_edges,
            identity_fingerprint,
        )?;
        let admitted_node_ids = sorted_admitted_node_ids(&state.admitted_node_ids);
        let lexical_checkpoint = LexicalCheckpoint {
            pdg_hash: pdg_checkpoint.artifact_hash.clone(),
            snapshot_path: self.project_path.join(".leindex/search_snapshot.bin"),
            tfidf_path: self.project_path.join(".leindex/tfidf_embedder.bin"),
            admitted_node_ids,
        };
        let lexical_hash = checkpoint_store.write_lexical(&lexical_checkpoint)?;
        self.checkpoint_state(checkpoint_store, "lexical", lexical_hash.clone());
        state.lexical_hash = Some(lexical_hash);
        state.lexical_checkpoint = Some(lexical_checkpoint.clone());
        state.pdg = None;
        state.pdg_checkpoint = Some(pdg_checkpoint.clone());
        self.pipeline = Some(state);
        Ok(lexical_checkpoint)
    }

    #[cfg(feature = "onnx")]
    fn configured_neural_embedder(&self) -> Result<Option<index_builder::HybridEmbedder>> {
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
    fn configured_neural_embedder(&self) -> Result<Option<index_builder::HybridEmbedder>> {
        Ok(None)
    }

    fn restore_neural_checkpoint(
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

    fn persist_neural_snapshot(
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
    fn persist_neural_mmap(&self, _neural_resume_loaded: bool, _neural_rows: usize) -> Result<()> {
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
    fn sync_fragment_layer_or_clear(&mut self) {
        if let Err(e) = self.sync_fragment_layer() {
            warn!(
                "Fragment layer sync failed (fragment layer disabled for this generation): {e:#}"
            );
            self.search_engine.set_fragment_embeddings(Vec::new());
        }
    }

    fn sync_fragment_layer(&mut self) -> Result<()> {
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
                    "Indexing: neural embedding {} admitted nodes (content cache may skip most)...",
                    state.admitted_node_ids.len()
                ));
                let (cache_hits_before, _) = neural_cache_counters();
                let rows = index_builder::enrich_neural_embeddings(
                    pdg,
                    neural_embedder,
                    &state.admitted_node_ids,
                    &state.enriched_content_cache,
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
                                        super::SOURCE_FILE_EXTENSIONS
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
            let mut health = super::IndexHealth {
                generation,
                phase: super::IndexPhase::Complete,
                status: super::ComponentStatus::Fresh,
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
            let mut health = super::IndexHealth {
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

    /// Update the last_indexed timestamp in project_metadata
    fn update_last_indexed_timestamp(&self) -> Result<()> {
        let conn = self.storage.conn();
        conn.execute(
            "UPDATE project_metadata SET last_indexed = CURRENT_TIMESTAMP WHERE unique_project_id = ?1",
            [&self.project_id],
        )
        .context("Failed to update last_indexed timestamp")?;
        Ok(())
    }

    /// Load a previously indexed project from the generation selected by
    /// `CURRENT` (or the legacy root when no generation is published).
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn load_from_storage(&mut self) -> Result<()> {
        self.load_from_active_storage()
    }

    /// Hydrate directly from the mutable root. Indexing recovery uses this
    /// only for validated checkpoint artifacts that are not current yet.
    fn load_from_mutable_storage(&mut self) -> Result<()> {
        self.load_from_storage_inner(false)
    }

    /// Hydrate queries from the generation selected by `CURRENT`.
    ///
    /// Indexing still writes the mutable root, but normal registry hydration
    /// must never read that in-progress state after a crash or concurrent job.
    pub(crate) fn load_from_active_storage(&mut self) -> Result<()> {
        // WS4 Task 14: when the `generation-readers` flag is enabled and the
        // project has a current generation, hydrate the read path from the
        // leased mmap generation instead of the legacy heap-mirror store. The
        // lease never touches the writer Mutex, so reads keep working while a
        // concurrent index/publish is in progress (VAL-EQUIV-002/003).
        if self.try_hydrate_from_generation()? {
            return Ok(());
        }
        let active = self.active_storage_path();
        if active == self.storage_path || !active.join("leindex.db").is_file() {
            return self.load_from_mutable_storage();
        }

        // Open the published (immutable) generation read-only: no WAL, no DDL,
        // no `INSERT OR REPLACE schema_version`. Mutating a published snapshot
        // would (a) make concurrent readers contend as writers and (b) fail on
        // read-only archived artifacts. See `Storage::open_readonly`.
        let active_storage =
            crate::storage::schema::Storage::open_readonly(active.join("leindex.db"))
                .with_context(|| {
                    format!("Failed to open active generation at {}", active.display())
                })?;
        self.load_from_storage_inner_at(false, Some(&active_storage), active)
    }

    /// WS4 Task 14: wire the read path (PDG + search engine) onto the leased
    /// mmap generation when `LEINDEX_FEATURE_GENERATION_READERS` is enabled
    /// and the project has a current generation.
    ///
    /// Returns `Ok(true)` when the read path is served from the generation
    /// (or was already wired on a previous call), `Ok(false)` when the flag is
    /// off or no generation exists (the caller falls back to the legacy
    /// heap-mirror path). The returned snapshot is retained on `self` so its
    /// [`GenerationLease`] keeps the generation's CAS blobs pinned for the
    /// lifetime of this process.
    pub(crate) fn try_hydrate_from_generation(&mut self) -> Result<bool> {
        self.try_hydrate_from_generation_inner(false)
    }

    /// Graph-only variant: hydrate the PDG from the leased generation WITHOUT
    /// restoring the search engine (snapshot + embedding mmaps). Graph-only
    /// tools (read-symbol, symbol-lookup, project-map) never query the search
    /// engine, so paying ~1s of artifact restoration per cold call was pure
    /// added latency.
    pub(crate) fn try_hydrate_generation_pdg_only(&mut self) -> Result<bool> {
        if self.pdg.is_some() {
            return Ok(true);
        }
        self.try_hydrate_from_generation_inner(true)
    }

    fn try_hydrate_from_generation_inner(&mut self, pdg_only: bool) -> Result<bool> {
        if !crate::feature_flags::FeatureFlag::GenerationReaders.is_enabled() {
            return Ok(false);
        }
        if pdg_only && self.pdg.is_some() {
            return Ok(true);
        }
        if self.generation_snapshot.is_some() {
            return Ok(true);
        }
        let Some(storage_path) =
            crate::cli::leindex::resolve_existing_storage_path(&self.project_path)
        else {
            return Ok(false);
        };
        let Some(generation) =
            crate::storage::generation::lease::read_current_generation(&storage_path)
        else {
            return Ok(false);
        };
        // Check whether a CAS manifest exists for this generation. Legacy
        // layouts (pre-migration full-copy) write a CURRENT file but do not
        // have a manifest. In that case gracefully fall back to the legacy
        // heap-mirror path instead of erroring.
        let manifest_path = storage_path
            .join(crate::storage::generation::lease::GENERATIONS_DIR)
            .join(generation.to_string())
            .join(crate::storage::generation::lease::MANIFEST_FILE);
        if !manifest_path.exists() {
            return Ok(false);
        }
        let snapshot = crate::storage::generation::GenerationSnapshot::open(&storage_path)
            .with_context(|| {
                format!(
                    "Failed to open generation snapshot at {}",
                    storage_path.display()
                )
            })?;
        let generation_db = crate::storage::schema::Storage::open_readonly(snapshot.db_path())?;
        // Artifact path points at the snapshot's temp dir, which holds no
        // search-snapshot/embedder artifacts, so hydration uses the rebuild
        // path and `persist_artifacts` stays false — the generation read path
        // never writes legacy artifacts back into the store.
        let artifact_path = snapshot
            .db_path()
            .parent()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| storage_path.clone());
        self.load_from_storage_inner_at(pdg_only, Some(&generation_db), artifact_path)?;
        self.hydrated_generation
            .store(snapshot.generation(), std::sync::atomic::Ordering::Release);
        self.generation_snapshot = Some(snapshot);
        info!(
            project = %self.project_path.display(),
            "Hydrated read path from leased mmap generation"
        );
        Ok(true)
    }

    /// Load PDG from storage without populating the search engine.
    /// Used by index_project() when it will call index_nodes() afterwards.
    pub fn load_pdg_from_storage(&mut self) -> Result<()> {
        self.load_from_storage_inner(true)
    }

    fn load_from_storage_inner(&mut self, pdg_only: bool) -> Result<()> {
        self.load_from_storage_inner_at(pdg_only, None, self.storage_path.clone())
    }
}

/// Which PDG construction route produced a given combined graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PdgBuildRoute {
    /// Compact streaming fragment/segment pipeline
    /// (`fragment_from_pdg` + `merge_fragments_to_segment` + `pdg_from_segment`).
    Streaming,
    /// Legacy per-file extraction + `merge_pdgs`.
    Legacy,
}

/// The construction route selected by the current `StreamingPdg` feature flag.
///
/// This is the single dispatch point the indexing phase consults so the flag
/// cleanly toggles between the streaming fragment pipeline (default) and the
/// legacy merge loop.
pub(crate) fn pdg_route_for_current_flag() -> PdgBuildRoute {
    if crate::feature_flags::FeatureFlag::StreamingPdg.is_enabled() {
        PdgBuildRoute::Streaming
    } else {
        PdgBuildRoute::Legacy
    }
}

/// Build a combined PDG for the freshly parsed files using the selected route.
fn build_changed_file_pdg(
    parsing_results: Vec<crate::parse::parallel::ParsingResult>,
    use_streaming: bool,
) -> (crate::graph::pdg::ProgramDependenceGraph, PdgBuildRoute) {
    if use_streaming {
        (
            build_pdg_streaming(parsing_results),
            PdgBuildRoute::Streaming,
        )
    } else {
        (build_pdg_legacy(parsing_results), PdgBuildRoute::Legacy)
    }
}

/// Legacy route: extract a per-file PDG via `extract_pdg_from_signatures` and
/// merge each into a combined graph (clone-free `merge_pdgs`).
fn build_pdg_legacy(
    parsing_results: Vec<crate::parse::parallel::ParsingResult>,
) -> crate::graph::pdg::ProgramDependenceGraph {
    let mut combined = crate::graph::pdg::ProgramDependenceGraph::new();
    let file_pdgs: Vec<_> = parsing_results
        .into_par_iter()
        .filter(|result| result.is_success())
        .map(|result| {
            let file_path = result.file_path.display().to_string();
            let language = result.language.as_deref().unwrap_or("unknown");
            let source_bytes = result.source_bytes.as_deref().unwrap_or(&[]);
            crate::graph::extract_pdg_from_signatures(
                result.signatures,
                source_bytes,
                &file_path,
                language,
            )
        })
        .collect();
    for file_pdg in file_pdgs {
        index_builder::merge_pdgs(&mut combined, file_pdg);
    }
    combined
}

/// Map a `SignatureInfo` to the canonical streaming node-type string (mirrors
/// the legacy `signature_to_node` mapping so graphs rebuilt from streamed
/// segments stay type-equivalent to the legacy route).
/// Streaming route: build a per-file `PdgFragment` from each parsed file
/// via the real extraction pipeline, merge fragments into a compact
/// segment, and materialize the combined graph from it.
/// them into a compact `PdgSegment` via `merge_fragments_to_segment`, then
/// rebuild the `ProgramDependenceGraph` via `pdg_from_segment`.
fn build_pdg_streaming(
    parsing_results: Vec<crate::parse::parallel::ParsingResult>,
) -> crate::graph::pdg::ProgramDependenceGraph {
    let fragments: Vec<_> = parsing_results
        .into_par_iter()
        .filter(|result| result.is_success())
        .map(|result| {
            let file_path = result.file_path.display().to_string();
            let language = result
                .language
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let source_bytes = result.source_bytes.as_deref().unwrap_or(&[]);
            // Route through the REAL extraction pipeline
            // (`extract_pdg_from_signatures` → `fragment_from_pdg`, its
            // documented production realization) instead of the skeleton
            // `build_fragment_from_parsed`. The skeleton flattened
            // signatures to name/kind/bytes, hardcoding complexity 0 and
            // dropping ALL intra-file call/data edges — which is why the
            // streaming route's stored graph showed complexity 0 on every
            // node, empty callee lists, and `forward_impact` returning
            // nothing. The per-file PDG here is single-file (cheap to
            // build), so the streaming memory contract (no whole-graph
            // clone, compact per-file records) is preserved.
            let file_pdg = crate::graph::extract_pdg_from_signatures(
                result.signatures,
                source_bytes,
                &file_path,
                &language,
            );
            streaming::pdg::fragment_from_pdg(&file_pdg)
        })
        .collect();
    let (segment, stats) = streaming::pdg::merge_fragments_to_segment(fragments);
    info!(
        "Streaming PDG merge: {} fragments, {} nodes, {} edges ({} cross-file resolved, {} unresolved)",
        stats.fragments,
        stats.node_count,
        stats.edge_count,
        stats.cross_file_resolved,
        stats.cross_file_unresolved
    );
    streaming::pdg::pdg_from_segment(&segment)
}
