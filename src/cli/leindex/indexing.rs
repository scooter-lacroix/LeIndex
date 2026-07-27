// Indexing pipeline methods for LeIndex: index_project and load_from_storage.

use super::{LeIndex, ProjectFileScan};
use crate::cli::index_builder;
use crate::cli::index_job::{
    latest_incomplete_job, CheckpointStore, FileFingerprint, JobPaths, LexicalCheckpoint,
    NeuralCheckpoint, ParseCheckpoint, ParsedFileCheckpoint, PdgCheckpoint, PublishedGeneration,
    ScanCheckpoint,
};
use crate::cli::memory_cap::MemoryCapGuard;
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;
use tracing::{info, warn};

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
    pub(crate) shared_file_cache: Option<index_builder::FileReadCache>,
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
    pub(crate) skip: bool,
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
            shared_file_cache: None,
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
            skip: false,
        }
    }
}

fn injected_phase_failure(phase: &str) -> Result<()> {
    if std::env::var("LEINDEX_INJECT_FAILURE_PHASE")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case(phase))
    {
        bail!("injected indexing failure after reusable {phase} artifact")
    }
    Ok(())
}

fn add_submodule_summary_nodes(
    pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    project_path: &std::path::Path,
) {
    let Ok(summaries) = crate::cli::git::submodule_summaries(project_path) else {
        return;
    };
    for summary in summaries {
        let relative = summary
            .path
            .strip_prefix(project_path)
            .unwrap_or(&summary.path)
            .to_string_lossy()
            .replace('\\', "/");
        let node_id = format!("submodule:{relative}:{}", summary.commit_oid);
        if pdg
            .node_indices()
            .filter_map(|index| pdg.get_node(index))
            .any(|node| node.id == node_id)
        {
            continue;
        }
        let name = summary
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&relative)
            .to_string();
        let module_index = pdg.add_node(crate::graph::pdg::Node {
            id: node_id,
            node_type: crate::graph::pdg::NodeType::Module,
            name,
            file_path: std::sync::Arc::from(summary.path.to_string_lossy().to_string()),
            byte_range: (0, 0),
            complexity: 0,
            language: format!("git-submodule:{}", summary.commit_oid),
        });
        let importers = pdg
            .node_indices()
            .filter(|index| *index != module_index)
            .filter_map(|index| pdg.get_node(index).map(|node| (index, node)))
            .filter(|(_, node)| {
                node.node_type == crate::graph::pdg::NodeType::External
                    && (node.name.contains(&relative) || node.id.contains(&relative))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for importer in importers {
            pdg.add_edge(
                importer,
                module_index,
                crate::graph::pdg::Edge {
                    edge_type: crate::graph::pdg::EdgeType::Import,
                    metadata: crate::graph::pdg::EdgeMetadata::empty(),
                },
            );
        }
    }
}

/// Write a progress line to stderr if stderr is a terminal.
/// Uses `\r` to overwrite the current line (no newline).
/// This is a no-op when stderr is not a terminal (e.g., MCP/stdio mode).
fn progress_stderr(msg: &str) {
    use std::io::{IsTerminal, Write};
    let stderr = std::io::stderr();
    if stderr.is_terminal() {
        let mut handle = stderr.lock();
        // Clear the line first, then write the new content
        let _ = write!(handle, "\r\x1b[K{}", msg);
        let _ = handle.flush();
    }
}

/// Clear the progress line on stderr (when terminal).
fn progress_clear() {
    use std::io::{IsTerminal, Write};
    let stderr = std::io::stderr();
    if stderr.is_terminal() {
        let mut handle = stderr.lock();
        let _ = write!(handle, "\r\x1b[K");
        let _ = handle.flush();
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

    fn publish_generation_snapshot(
        &self,
        generation: u64,
        health: &super::IndexHealth,
        include_neural: bool,
    ) -> Result<PublishedGeneration> {
        let generations = self.storage_path().join("generations");
        std::fs::create_dir_all(&generations)?;
        let target = generations.join(generation.to_string());
        if target.exists() {
            bail!(
                "generation {} already exists; refusing to overwrite an immutable snapshot",
                generation
            );
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
        // WAL is checkpointed before copying the immutable catalog snapshot;
        // query readers never observe a half-written generation.
        self.storage
            .conn()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .context("checkpoint SQLite WAL before generation publication")?;
        let mut copied = std::collections::HashSet::new();
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
        crate::cli::index_freshness::save_health(&staging, health)?;
        #[cfg(unix)]
        std::fs::File::open(&staging)?.sync_all()?;
        // Rename the complete directory once. Readers either see no new
        // generation or the fully materialized immutable snapshot.
        std::fs::rename(&staging, &target).with_context(|| {
            format!(
                "promote staged generation {} -> {}",
                staging.display(),
                target.display()
            )
        })?;
        #[cfg(unix)]
        std::fs::File::open(&generations)?.sync_all()?;
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
        Ok(PublishedGeneration {
            generation,
            storage_path: target,
            health: health.clone(),
        })
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
        // Serialize concurrent writers across processes (e.g. MCP + CLI) so two
        // processes never write leindex.db at once. Blocks until exclusive; RAII
        // releases on return. See `ProjectWriteLock`.
        let _write_lock = self.acquire_write_lock()?;
        let start_time = std::time::Instant::now();
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .context("Failed to load indexed files from storage")?;

        // Use a shared file cache so that file reads during hash collection
        // can be reused later when building NodeInfo content.
        let mut shared_file_cache = index_builder::FileReadCache::new(100);
        let source_files_with_hashes =
            self.collect_source_files_with_hashes(true, Some(&mut shared_file_cache))?;
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
        let mut removed_node_ids = Vec::new();
        for path in &deleted_files {
            removed_node_ids.extend(
                pdg.node_indices()
                    .filter_map(|node_idx| pdg.get_node(node_idx))
                    .filter(|node| node.file_path.as_ref() == path.as_str())
                    .map(|node| node.id.clone()),
            );
            index_builder::remove_file_from_pdg(&mut pdg, path)?;
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

        for result in parsing_results.into_iter() {
            if !result.is_success() {
                continue;
            }

            let file_path = result.file_path.display().to_string();
            let language = result.language.as_deref().unwrap_or("unknown");
            let source_bytes = result.source_bytes.as_deref().unwrap_or(&[]);
            index_builder::remove_file_from_pdg(&mut pdg, &file_path)?;
            let file_pdg = crate::graph::extract_pdg_from_signatures(
                result.signatures,
                source_bytes,
                &file_path,
                language,
            );
            index_builder::merge_pdgs(&mut pdg, file_pdg);
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

        // Resume-proof FileSummary pass: covers ALL files (the incremental merge
        // loop only touched changed files; existing files keep/refresh summaries).
        pdg.ensure_file_summary_nodes();

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

        // Read actual file content for changed files to populate NodeInfo
        // entries with real source and pre-tokenized tokens.
        let mut file_cache = shared_file_cache;
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

        // Build the changed-node TF-IDF delta first. Existing neural rows are
        // retained where possible; missing rows are filled by a later owned
        // full index job without blocking this watcher path.
        let mut updated_nodes: Vec<crate::search::search::NodeInfo> = Vec::new();
        let pruner = crate::search::search::ContentPruner::new();

        for node_idx in pdg.node_indices() {
            let node = match pdg.get_node(node_idx) {
                Some(n) => n,
                None => continue,
            };
            let file_path_str = node.file_path.as_ref();
            // Only include nodes belonging to changed files
            if !changed_file_set.contains(file_path_str) {
                continue;
            }
            // Read actual file content and extract the node's source
            let file_bytes = file_cache
                .get_or_read(std::path::Path::new(file_path_str))
                .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));
            let node_content = index_builder::enriched_node_content(
                &pdg,
                node_idx,
                node,
                file_bytes.as_ref(),
                &connectivity_config,
            );

            // Pruning gate: skip low-information / generated nodes (same as index_builder).
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

        self.search_engine
            .incremental_reindex(crate::search::search::TextIndexDelta {
                removed_node_ids,
                updated_nodes,
            });

        // Persist the updated PDG to storage so changes survive restart
        index_builder::save_to_storage(&mut self.storage, &self.project_id, &pdg)?;

        self.pdg = Some(pdg);
        self.embedder = Some(embedder);
        if let Some(embedder) = &self.embedder {
            embedder.persist_to_storage(&self.project_path, self.pdg.as_ref().unwrap())?;
        }
        self.build_file_stats_cache();
        self.stats.indexing_time_ms = start_time.elapsed().as_millis() as u64;

        // R10: Persist embeddings to mmap file after watcher incremental reindex
        index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)?;
        let (pdg_node_count, pdg_edge_count) = self
            .pdg
            .as_ref()
            .map(|pdg| (pdg.node_count(), pdg.edge_count()))
            .unwrap_or((self.stats.pdg_nodes, self.stats.pdg_edges));
        index_builder::persist_search_snapshot(
            &self.search_engine,
            &self.project_path,
            pdg_node_count,
            pdg_edge_count,
            self.pdg
                .as_ref()
                .map(index_builder::pdg_search_fingerprint)
                .unwrap_or_default(),
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
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let health = super::IndexHealth {
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
        self.publish_generation_snapshot(generation, &health, true)?;
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

    /// Index the project with an optional memory cap.
    ///
    /// This is the same as `index_project(force)` but additionally monitors RSS
    /// memory usage throughout the indexing pipeline. When `max_memory_bytes` is
    /// `Some(bytes)`, a `MemoryCapGuard` is created that:
    /// - Logs a warning when RSS exceeds 90% of the cap
    /// - Returns an error when RSS exceeds 100% of the cap
    ///
    /// The memory check is performed at key checkpoints during indexing to avoid
    /// excessive overhead while still catching runaway memory usage.
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
        if let Some(guard) = cap_guard.as_mut() {
            guard.check_now()?;
        }
        self.mark_index_phase(
            super::IndexPhase::Parse,
            super::ComponentStatus::Initializing,
        );
        let parsed = self.run_parse(&job, &scan)?;
        if let Some(guard) = cap_guard.as_mut() {
            guard.check_now()?;
        }
        if self.pipeline.as_ref().is_some_and(|state| state.skip) {
            self.pipeline = None;
            progress_clear();
            return Ok(self.stats.clone());
        }

        let pdg = self.run_pdg(&job, &parsed)?;
        if let Some(guard) = cap_guard.as_mut() {
            guard.check_now()?;
        }
        let lexical = self.run_lexical(&job, &pdg)?;
        let _core = self.publish_generation(&job, None)?;
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
        let mut shared_file_cache = index_builder::FileReadCache::new(200);
        let source_files_with_hashes =
            self.collect_source_files_with_hashes(true, Some(&mut shared_file_cache))?;
        info!("Found {} source files", source_files_with_hashes.len());
        let mut fingerprints = Vec::with_capacity(source_files_with_hashes.len());
        for (path, hash) in &source_files_with_hashes {
            fingerprints.push(FileFingerprint {
                canonical_path: path.clone(),
                blake3: hash.clone(),
                bytes: std::fs::metadata(path)
                    .map(|metadata| metadata.len())
                    .unwrap_or_default(),
                language: path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
            });
        }
        let scan = ScanCheckpoint {
            input_hash: crate::cli::index_job::scan_hash(&fingerprints),
            files: fingerprints,
        };
        let generation = state.job.generation;
        let (checkpoint_store, resumed_scan) = latest_incomplete_job(self.storage_path())
            .and_then(|(paths, _)| {
                let store = CheckpointStore::from_paths(paths);
                let saved = store.read_scan().ok().flatten()?;
                (saved.input_hash == scan.input_hash).then_some((store, saved))
            })
            .map(|(store, saved)| (store, Some(saved)))
            .unwrap_or_else(|| (self.checkpoint_store(generation), None));
        state.job = checkpoint_store.paths.clone();
        if resumed_scan.is_none() {
            let scan_hash = checkpoint_store.write_scan(&scan)?;
            self.checkpoint_state(&checkpoint_store, "scan", scan_hash);
        }
        let checkpoint_state = checkpoint_store.read_state().ok().flatten();
        let resumed_parse = checkpoint_state
            .as_ref()
            .and_then(|checkpoint| checkpoint.artifact_hashes.get("parse").cloned())
            .and_then(|expected_hash| {
                let bytes = std::fs::read(checkpoint_store.paths.parse()).ok()?;
                (blake3::hash(&bytes).to_hex().as_str() == expected_hash).then_some(())?;
                checkpoint_store.read_parse().ok().flatten()
            })
            .filter(|checkpoint| checkpoint.scan_hash == scan.input_hash);
        let resumed_pdg = if resumed_scan.is_some() {
            checkpoint_state
                .as_ref()
                .and_then(|checkpoint| checkpoint.artifact_hashes.get("pdg").cloned())
                .and_then(|hash| {
                    let metadata = checkpoint_store.read_pdg_checkpoint().ok().flatten();
                    let checkpoint = match metadata {
                        Some(checkpoint)
                            if checkpoint.scan_hash == scan.input_hash
                                && checkpoint.artifact_hash == hash =>
                        {
                            checkpoint
                        }
                        Some(_) => return None,
                        None => PdgCheckpoint {
                            scan_hash: scan.input_hash.clone(),
                            artifact_path: checkpoint_store.paths.pdg(),
                            artifact_hash: hash,
                            nodes: 0,
                            edges: 0,
                        },
                    };
                    let pdg = checkpoint_store
                        .read_pdg_artifact(&checkpoint.artifact_hash)
                        .ok()
                        .flatten()?;
                    Some((
                        PdgCheckpoint {
                            nodes: if checkpoint.nodes == 0 {
                                pdg.node_count()
                            } else {
                                checkpoint.nodes
                            },
                            edges: if checkpoint.edges == 0 {
                                pdg.edge_count()
                            } else {
                                checkpoint.edges
                            },
                            ..checkpoint
                        },
                        pdg,
                    ))
                })
        } else {
            None
        };
        let resumed_lexical = checkpoint_state
            .as_ref()
            .and_then(|checkpoint| checkpoint.artifact_hashes.get("lexical").cloned())
            .and_then(|expected_hash| {
                let bytes = std::fs::read(checkpoint_store.paths.lexical()).ok()?;
                (blake3::hash(&bytes).to_hex().as_str() == expected_hash).then_some(())?;
                checkpoint_store.read_lexical().ok().flatten()
            })
            .filter(|checkpoint| {
                checkpoint.snapshot_path.is_file()
                    && checkpoint.tfidf_path.is_file()
                    && checkpoint
                        .snapshot_path
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() > 0)
                    && checkpoint
                        .tfidf_path
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() > 0)
            });
        let resumed_neural = checkpoint_state
            .as_ref()
            .and_then(|checkpoint| checkpoint.artifact_hashes.get("neural").cloned())
            .and_then(|expected_hash| {
                let bytes = std::fs::read(checkpoint_store.paths.neural()).ok()?;
                (blake3::hash(&bytes).to_hex().as_str() == expected_hash).then_some(())?;
                checkpoint_store.read_neural().ok().flatten()
            });
        state.indexed_files = indexed_files;
        state.old_scan = old_scan;
        state.shared_file_cache = Some(shared_file_cache);
        state.source_files_with_hashes = source_files_with_hashes;
        state.resumed_scan = resumed_scan;
        state.resumed_parse = resumed_parse;
        state.resumed_pdg = resumed_pdg
            .as_ref()
            .map(|(checkpoint, _)| checkpoint.clone());
        state.pdg = resumed_pdg.map(|(_, pdg)| pdg);
        state.resumed_lexical = resumed_lexical;
        state.resumed_neural = resumed_neural;
        state.checkpoint_store = Some(checkpoint_store);
        injected_phase_failure("scan")?;
        let result = scan.clone();
        self.pipeline = Some(state);
        Ok(result)
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
        let source_file_hashes: HashMap<String, String> = state
            .source_files_with_hashes
            .iter()
            .map(|(path, hash)| (path.display().to_string(), hash.clone()))
            .collect();
        let current_file_paths: HashSet<String> = state
            .source_files_with_hashes
            .iter()
            .map(|(path, _)| path.display().to_string())
            .collect();
        let mut files_to_parse = Vec::new();
        let mut unchanged_files = HashSet::new();
        for (path, hash) in &state.source_files_with_hashes {
            let path_str = path.display().to_string();
            if state.force
                || !state.indexed_files.contains_key(&path_str)
                || state.indexed_files.get(&path_str) != Some(hash)
            {
                files_to_parse.push(path.clone());
            } else {
                unchanged_files.insert(path_str);
            }
        }
        let mut resumed_parse_results = Vec::new();
        if state.resumed_scan.is_some() {
            let cache = state
                .shared_file_cache
                .as_mut()
                .context("parse phase missing shared file cache")?;
            let mut reused_paths = HashSet::new();
            for path in &files_to_parse {
                let Some(parse_checkpoint) = state.resumed_parse.as_ref() else {
                    break;
                };
                let path_key = path.display().to_string();
                let Some(source_hash) = source_file_hashes.get(&path_key) else {
                    continue;
                };
                let Some(expected_hash) = parse_checkpoint.artifact_hashes.get(source_hash) else {
                    continue;
                };
                let Some(parsed) = checkpoint_store.read_parsed_for_path_verified(
                    source_hash,
                    expected_hash,
                    path,
                )?
                else {
                    continue;
                };
                let source_bytes = cache.get_or_read(path)?.as_ref().clone();
                resumed_parse_results.push(crate::parse::parallel::ParsingResult {
                    file_path: parsed.file_path,
                    language: Some(parsed.language),
                    signatures: parsed.signatures,
                    source_bytes: Some(source_bytes),
                    error: None,
                    parse_time_ms: parsed.parse_time_ms,
                });
                reused_paths.insert(path.clone());
            }
            files_to_parse.retain(|path| !reused_paths.contains(path));
        }
        let deleted_files: Vec<String> = state
            .indexed_files
            .keys()
            .filter(|path| !current_file_paths.contains(*path))
            .cloned()
            .collect();
        info!(
            "Incremental analysis: {} to parse, {} unchanged, {} deleted",
            files_to_parse.len(),
            unchanged_files.len(),
            deleted_files.len()
        );
        if files_to_parse.is_empty()
            && deleted_files.is_empty()
            && self.is_indexed()
            && state.resumed_lexical.is_none()
        {
            let manifest_dirty = self.check_manifest_stale();
            if !manifest_dirty {
                let current_scan = self.get_project_scan(false)?;
                let changed_manifests = match state.old_scan.as_ref() {
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
                    info!("No changes detected, skipping indexing");
                    self.mark_index_phase(
                        super::IndexPhase::Complete,
                        super::ComponentStatus::Fresh,
                    );
                    state.skip = true;
                    let result = ParseCheckpoint {
                        scan_hash: scan.input_hash.clone(),
                        artifact_paths: Vec::new(),
                        artifact_hashes: std::collections::BTreeMap::new(),
                    };
                    self.pipeline = Some(state);
                    return Ok(result);
                }
                info!(
                    "Manifest content changed ({} files) — re-annotating",
                    changed_manifests.len()
                );
            } else {
                info!("Manifest files changed — running external dependency annotation");
            }
        }
        progress_stderr(&format!(
            "Indexing: parsing {} files...",
            files_to_parse.len()
        ));
        let parser = crate::parse::parallel::ParallelParser::new();
        let mut parsing_results = if files_to_parse.is_empty() {
            Vec::new()
        } else {
            parser.parse_files(files_to_parse)
        };
        parsing_results.extend(resumed_parse_results);
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
            let artifact_hash = checkpoint_store.write_parsed_batch(&bucket, &files)?;
            artifact_paths.push(checkpoint_store.paths.parsed_bucket(&bucket));
            for source_hash in files.keys() {
                artifact_hashes.insert(source_hash.clone(), artifact_hash.clone());
            }
        }
        artifact_paths.sort();
        let parse_checkpoint = ParseCheckpoint {
            scan_hash: scan.input_hash.clone(),
            artifact_paths,
            artifact_hashes,
        };
        let parse_hash = checkpoint_store.write_parse(&parse_checkpoint)?;
        self.checkpoint_state(checkpoint_store, "parse", parse_hash);
        injected_phase_failure("parse")?;
        state.source_file_hashes = source_file_hashes;
        state.current_file_paths = current_file_paths;
        state.files_to_parse = Vec::new();
        state.unchanged_files = unchanged_files;
        state.deleted_files = deleted_files;
        state.parsing_results = parsing_results;
        state.parse_checkpoint = Some(parse_checkpoint.clone());
        self.pipeline = Some(state);
        Ok(parse_checkpoint)
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
            .context("PDG phase missing checkpoint store")?;
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
        let files_parsed = parsing_results.len();
        let successful = parsing_results
            .iter()
            .filter(|result| result.is_success())
            .count();
        let failed = parsing_results
            .iter()
            .filter(|result| result.is_failure())
            .count();
        let total_sigs: usize = parsing_results
            .iter()
            .map(|result| result.signatures.len())
            .sum();
        for result in parsing_results.iter().filter(|result| result.is_failure()) {
            warn!(
                "Parse failure for '{}' during indexing: {}",
                result.file_path.display(),
                result
                    .error
                    .as_deref()
                    .filter(|error| !error.is_empty())
                    .unwrap_or("unknown error")
            );
        }
        if failed > 0 {
            warn!(
                "Indexing completed with {} parse failure(s) out of {} file(s)",
                failed,
                successful + failed
            );
        }
        for path in &state.deleted_files {
            index_builder::remove_file_from_pdg(&mut pdg, path)?;
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
        let all_signatures: Vec<(String, crate::parse::prelude::SignatureInfo)> = parsing_results
            .iter()
            .filter(|result| result.is_success())
            .flat_map(|result| {
                let file_path = result.file_path.display().to_string();
                result
                    .signatures
                    .iter()
                    .cloned()
                    .map(move |signature| (file_path.clone(), signature))
            })
            .collect();
        for result in parsing_results {
            if !result.is_success() {
                continue;
            }
            let file_path = result.file_path.display().to_string();
            let language = result.language.as_deref().unwrap_or("unknown");
            let source_bytes = result.source_bytes.as_deref().unwrap_or(&[]);
            index_builder::remove_file_from_pdg(&mut pdg, &file_path)?;
            let file_pdg = crate::graph::extract_pdg_from_signatures(
                result.signatures,
                source_bytes,
                &file_path,
                language,
            );
            index_builder::merge_pdgs(&mut pdg, file_pdg);
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
        }
        // Resume-proof FileSummary pass: covers files loaded from storage on
        // resume (the merge_pdgs loop above only fires for freshly-parsed files).
        pdg.ensure_file_summary_nodes();
        if !all_signatures.is_empty() {
            crate::graph::resolve_cross_file_call_edges_for_files(&mut pdg, &all_signatures);
            crate::graph::resolve_cross_file_flow_edges_for_files(&mut pdg, &all_signatures);
        }
        let manifest_paths = self
            .cache
            .project_scan
            .as_ref()
            .map(|scan| scan.manifest_paths.clone())
            .unwrap_or_default();
        let ext_registry = crate::graph::ExternalDependencyRegistry::from_manifest_paths(
            &self.project_path,
            &manifest_paths,
        );
        let annotation_stats = crate::graph::annotate_external_nodes(&mut pdg, &ext_registry);
        if !ext_registry.is_empty() {
            info!(
                "External dependency resolution: {}/{} resolved via lock files, {} recognized builtins ({} packages in registry)",
                annotation_stats.resolved,
                annotation_stats.total_external,
                annotation_stats.builtin,
                ext_registry.len()
            );
        } else if annotation_stats.total_external > 0 {
            info!(
                "External dependency resolution: no lockfile registry found, {} builtins recognized, {} unresolved external imports",
                annotation_stats.builtin,
                annotation_stats.unresolved
            );
        }
        state.ext_in_lockfile = ext_registry.len();
        state.ext_resolved = annotation_stats.resolved;
        state.ext_unresolved = annotation_stats.unresolved;
        state.ext_total = annotation_stats.total_external;
        state.ext_builtin = annotation_stats.builtin;
        add_submodule_summary_nodes(&mut pdg, &self.project_path);
        index_builder::normalize_external_nodes(&mut pdg);
        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();
        let pdg_checkpoint = checkpoint_store.write_pdg(parsed.scan_hash.clone(), &pdg)?;
        self.checkpoint_state(
            checkpoint_store,
            "pdg",
            pdg_checkpoint.artifact_hash.clone(),
        );
        injected_phase_failure("pdg")?;
        self.mark_index_phase(super::IndexPhase::Pdg, super::ComponentStatus::Initializing);
        info!(
            "Updated PDG has {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );
        state.files_parsed = files_parsed;
        state.successful = successful;
        state.failed = failed;
        state.total_sigs = total_sigs;
        state.pdg_node_count = pdg_node_count;
        state.pdg_edge_count = pdg_edge_count;
        state.pdg_checkpoint = Some(pdg_checkpoint.clone());
        state.pdg = Some(pdg);
        self.pipeline = Some(state);
        Ok(pdg_checkpoint)
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
        let pdg = state
            .pdg
            .take()
            .or_else(|| self.pdg.take())
            .context("lexical phase missing resident PDG")?;
        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();
        progress_stderr(&format!("Indexing: embedding {} nodes...", pdg_node_count));
        let batch_size = self.indexing_batch_size();
        let persisted_embedder =
            index_builder::TfIdfEmbedder::load_from_storage(&self.project_path)
                .ok()
                .flatten();
        let lexical_resume_valid = state
            .resumed_lexical
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.pdg_hash == pdg_checkpoint.artifact_hash);
        let shared_file_cache = state.shared_file_cache.take();
        let embedder = if lexical_resume_valid {
            match self.load_from_mutable_storage() {
                Ok(()) => {
                    self.search_engine.clear_neural_embeddings();
                    self.embedder
                        .as_ref()
                        .map(|embedder| {
                            index_builder::HybridEmbedder::tfidf_only(embedder.tfidf().clone())
                        })
                        .or_else(|| {
                            persisted_embedder
                                .clone()
                                .map(index_builder::HybridEmbedder::tfidf_only)
                        })
                        .context("resumed lexical checkpoint has no TF-IDF embedder")?
                }
                Err(error) => {
                    warn!(
                        "Failed to hydrate resumed lexical checkpoint; rebuilding core: {error:#}"
                    );
                    index_builder::index_nodes_tfidf_only(
                        &pdg,
                        &mut self.search_engine,
                        &mut self.cache.file_stats_cache,
                        batch_size,
                        persisted_embedder
                            .clone()
                            .map(index_builder::HybridEmbedder::tfidf_only),
                        shared_file_cache,
                    )?
                }
            }
        } else if let Some(embedder) = persisted_embedder {
            if embedder.is_fresh(pdg_node_count, pdg_edge_count) {
                info!("Loaded persisted embedder from storage");
                index_builder::index_nodes_tfidf_only(
                    &pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    Some(index_builder::HybridEmbedder::tfidf_only(embedder)),
                    shared_file_cache,
                )?
            } else {
                info!("Persisted embedder is stale; rebuilding TF-IDF index");
                index_builder::index_nodes_tfidf_only(
                    &pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    None,
                    shared_file_cache,
                )?
            }
        } else {
            index_builder::index_nodes_tfidf_only(
                &pdg,
                &mut self.search_engine,
                &mut self.cache.file_stats_cache,
                batch_size,
                None,
                shared_file_cache,
            )?
        };
        self.embedder = Some(embedder);
        if let Some(embedder) = &self.embedder {
            embedder.persist_to_storage(&self.project_path, &pdg)?;
        }
        let indexed_count = self.search_engine.node_count();
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
        index_builder::persist_search_snapshot(
            &self.search_engine,
            &self.project_path,
            pdg_node_count,
            pdg_edge_count,
            self.pdg
                .as_ref()
                .map(index_builder::pdg_search_fingerprint)
                .unwrap_or_default(),
        )?;
        let lexical_checkpoint = LexicalCheckpoint {
            pdg_hash: pdg_checkpoint.artifact_hash.clone(),
            snapshot_path: self.project_path.join(".leindex/search_snapshot.bin"),
            tfidf_path: self.project_path.join(".leindex/tfidf_embedder.bin"),
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
        #[cfg(feature = "onnx")]
        let mut neural_embedder = index_builder::HybridEmbedder::hybrid_local(
            self.embedder
                .as_ref()
                .context("core embedder is set before neural enrichment")?
                .tfidf()
                .clone(),
            None,
        )
        .ok();
        #[cfg(not(feature = "onnx"))]
        let neural_embedder: Option<index_builder::HybridEmbedder> = None;

        // T5: if a GPU provider was requested but the worker fell back to CPU,
        // skip neural enrichment and ship TF-IDF. Silently running 100-1000x
        // slower CPU inference the user did not ask for is worse than no neural
        // signal at all. A deliberate `cpu`/`auto` configuration returns `None`
        // here and is left untouched, so the CPU neural path stays fully
        // operational — neural is optional to configure, never optional in
        // function.
        #[cfg(feature = "onnx")]
        if let Some(reason) = neural_embedder
            .as_ref()
            .and_then(index_builder::HybridEmbedder::cpu_fallback_reason)
        {
            tracing::warn!("{}", reason);
            neural_embedder = None;
        }
        // search_mode="text": the user opted into lexical-only retrieval. Skip
        // neural enrichment entirely — the query path already routes "text" mode
        // to the Text QueryType weighting arm, so neural rows would never be
        // scored. hybrid/neural modes still enrich. (VAL-CONFIG.)
        #[cfg(feature = "onnx")]
        if crate::cli::neural_config::LeIndexConfig::load_cached()
            .search
            .search_mode
            == "text"
        {
            neural_embedder = None;
        }
        // Cache-key fix: a model swap must NOT silently resume the previous
        // model's embeddings. The checkpoint stores the embedder model_name that
        // produced its rows; a mismatch forces a full re-embed.
        let current_embed_model = crate::cli::neural_config::LeIndexConfig::load_cached()
            .neural
            .model_name
            .clone();
        let neural_resume_requested = state.resumed_neural.as_ref().is_some_and(|checkpoint| {
            checkpoint.lexical_hash == lexical_hash
                && checkpoint.model == current_embed_model
                && (checkpoint.rows == 0 || checkpoint.mmap_path.is_file())
        });
        let mut neural_rows = 0;
        let neural_resume_loaded = neural_resume_requested
            && state
                .resumed_neural
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.rows == 0);
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        let mut neural_resume_loaded = neural_resume_loaded;
        if neural_resume_requested {
            #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
            if let Some(neural_mmap) =
                index_builder::try_load_neural_mmap_embeddings(&self.project_path)
            {
                neural_rows = self.search_engine.restore_neural_embeddings(&neural_mmap);
                neural_resume_loaded = neural_rows > 0;
            }
        }
        if neural_rows == 0 && !neural_resume_loaded {
            if let Some(neural_embedder) = neural_embedder.as_ref() {
                let rows = index_builder::enrich_neural_embeddings(
                    self.pdg
                        .as_ref()
                        .context("PDG is resident before neural enrichment")?,
                    neural_embedder,
                    &mut index_builder::FileReadCache::new(200),
                );
                neural_rows = self.search_engine.update_neural_embeddings(rows);
            }
        }
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        if !neural_resume_loaded {
            index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
            )?;
        }
        if neural_rows > 0 {
            if neural_embedder.is_some() {
                self.embedder = neural_embedder;
            }
            index_builder::persist_search_snapshot(
                &self.search_engine,
                &self.project_path,
                state.pdg_node_count,
                state.pdg_edge_count,
                self.pdg
                    .as_ref()
                    .map(index_builder::pdg_search_fingerprint)
                    .unwrap_or_default(),
            )?;
        }
        let neural_checkpoint = NeuralCheckpoint {
            lexical_hash,
            mmap_path: self.project_path.join(".leindex/neural_embeddings.bin"),
            rows: neural_rows,
            provider: if neural_rows == 0 {
                "unavailable".to_string()
            } else {
                std::env::var("LEINDEX_NEURAL_PROVIDER").unwrap_or_else(|_| "onnx".to_string())
            },
            model: crate::cli::neural_config::LeIndexConfig::load_cached()
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
        if neural.is_none() {
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
            let health = super::IndexHealth {
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
            let published = self.publish_generation_snapshot(generation, &health, false)?;
            crate::cli::index_freshness::save_health(self.storage_path(), &health)?;
            state.core_health = Some(health);
            injected_phase_failure("lexical")?;
            self.pipeline = Some(state);
            return Ok(published);
        }
        let core_health = state
            .core_health
            .clone()
            .context("neural publication missing core health")?;
        let published = if neural.is_some_and(|checkpoint| checkpoint.rows > 0) {
            let health = super::IndexHealth {
                generation: core_health.generation.saturating_add(1),
                ..core_health.clone()
            };
            let published = self.publish_generation_snapshot(health.generation, &health, true)?;
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
        let active = self.active_storage_path();
        if active == self.storage_path || !active.join("leindex.db").is_file() {
            return self.load_from_mutable_storage();
        }

        let active_storage = crate::storage::schema::Storage::open(active.join("leindex.db"))
            .with_context(|| format!("Failed to open active generation at {}", active.display()))?;
        self.load_from_storage_inner_at(false, Some(&active_storage), active)
    }

    /// Load PDG from storage without populating the search engine.
    /// Used by index_project() when it will call index_nodes() afterwards.
    pub fn load_pdg_from_storage(&mut self) -> Result<()> {
        self.load_from_storage_inner(true)
    }

    fn load_from_storage_inner(&mut self, pdg_only: bool) -> Result<()> {
        self.load_from_storage_inner_at(pdg_only, None, self.storage_path.clone())
    }

    fn load_from_storage_inner_at(
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

        if let (Some(snapshot), Some(tfidf_mmap), Some(tfidf_embedder)) = (
            index_builder::try_load_search_snapshot_from_storage(&artifact_path),
            index_builder::try_load_mmap_embeddings_from_storage(&artifact_path),
            persisted_embedder.clone(),
        ) {
            if snapshot.pdg_nodes == pdg_node_count
                && snapshot.pdg_edges == pdg_edge_count
                && snapshot.pdg_fingerprint == current_pdg_fingerprint
                && tfidf_embedder.is_fresh(pdg_node_count, pdg_edge_count)
            {
                #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
                let neural_mmap =
                    index_builder::try_load_neural_mmap_embeddings_from_storage(&artifact_path)
                        .map(std::sync::Arc::new);
                #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
                let neural_mmap: Option<
                    std::sync::Arc<crate::search::vector::MmapEmbeddingIndex>,
                > = None;

                match self.search_engine.restore_from_search_snapshot(
                    snapshot,
                    std::sync::Arc::new(tfidf_mmap),
                    neural_mmap,
                ) {
                    Ok(indexed_count) => {
                        #[cfg(feature = "onnx")]
                        {
                            match index_builder::HybridEmbedder::hybrid_local(tfidf_embedder, None)
                            {
                                Ok(hybrid) => {
                                    self.embedder = Some(hybrid);
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to create hybrid_local embedder for query embedding: {}",
                                        e
                                    );
                                    self.embedder = persisted_embedder
                                        .clone()
                                        .map(index_builder::HybridEmbedder::tfidf_only);
                                }
                            }
                        }
                        #[cfg(not(feature = "onnx"))]
                        {
                            self.embedder =
                                Some(index_builder::HybridEmbedder::tfidf_only(tfidf_embedder));
                        }

                        if let Err(err) = self.load_stats_from_path(&artifact_path) {
                            warn!("Failed to load persisted index stats: {err:#}");
                        }
                        self.stats.pdg_nodes = pdg_node_count;
                        self.stats.pdg_edges = pdg_edge_count;
                        self.stats.indexed_nodes = indexed_count;
                        self.pdg = Some(pdg);
                        self.build_file_stats_cache();
                        info!(
                            "Hydrated search index from snapshot with {} nodes",
                            indexed_count
                        );
                        return Ok(());
                    }
                    Err(err) => {
                        warn!(
                            "Failed to hydrate search index from snapshot; rebuilding from PDG: {}",
                            err
                        );
                    }
                }
            } else {
                info!("Search snapshot/embedder stale for current PDG; rebuilding search index");
            }
        }

        let batch_size = self.indexing_batch_size();
        let embedder = if let Some(embedder) = persisted_embedder {
            if embedder.is_fresh(pdg_node_count, pdg_edge_count) {
                info!("Loaded persisted embedder from storage");
                // Use tfidf_only during load_from_storage to avoid expensive
                // batch neural embedding of all nodes. Neural embeddings are
                // restored from the persisted mmap file below.
                let tfidf_embedder = index_builder::HybridEmbedder::tfidf_only(embedder);
                index_builder::index_nodes_tfidf_only(
                    &pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    Some(tfidf_embedder),
                    None,
                )?
            } else {
                info!("Persisted embedder is stale; rebuilding TF-IDF index");
                // Pass the stale embedder wrapped as tfidf_only to avoid
                // triggering batch neural embedding during search-time index
                // reconstruction. Neural embeddings are restored from the
                // persisted mmap file below.
                let stale_tfidf = index_builder::HybridEmbedder::tfidf_only(embedder);
                index_builder::index_nodes_tfidf_only(
                    &pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    Some(stale_tfidf),
                    None,
                )?
            }
        } else {
            // No persisted embedder at all; pass None to let the function
            // build a fresh vocab. This will create a hybrid_local embedder
            // when onnx is enabled, but only on first run (no existing index).
            index_builder::index_nodes_tfidf_only(
                &pdg,
                &mut self.search_engine,
                &mut self.cache.file_stats_cache,
                batch_size,
                None,
                None,
            )?
        };

        // Restore neural embeddings from persisted neural mmap file (if available).
        // This avoids re-computing neural embeddings for all nodes during search.
        // Neural embeddings are generated during a full `leindex index` run and
        // persisted to .leindex/neural_embeddings.bin. If the file is missing
        // (e.g., first run, or index was built without onnx feature), neural
        // scores will be 0 until a full reindex with ONNX is performed.
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            if let Some(neural_mmap) =
                index_builder::try_load_neural_mmap_embeddings_from_storage(&artifact_path)
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

        // VAL-ONNX-001: After indexing with tfidf_only, upgrade the embedder
        // to hybrid_local for query-time neural embedding (single query, not batch).
        #[cfg(feature = "onnx")]
        {
            if let Some(tfidf) =
                index_builder::TfIdfEmbedder::load_from_artifact_path(&artifact_path)
                    .ok()
                    .flatten()
            {
                match index_builder::HybridEmbedder::hybrid_local(tfidf, None) {
                    Ok(hybrid) => {
                        self.embedder = Some(hybrid);
                    }
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

        // Load persisted stats first (to restore total_signatures, total_files, etc.),
        // then overwrite the live PDG/search counts with freshly computed values.
        if let Err(err) = self.load_stats_from_path(&artifact_path) {
            warn!("Failed to load persisted index stats: {err:#}");
        }
        self.stats.pdg_nodes = pdg_node_count;
        self.stats.pdg_edges = pdg_edge_count;
        self.stats.indexed_nodes = indexed_count;

        self.pdg = Some(pdg);

        self.build_file_stats_cache();

        // R10: Persist embeddings to mmap file for fast read-only access
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
        // Persist neural embeddings separately for fast load_from_storage
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

fn git_tree_oid(project_path: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD^{tree}"])
        .current_dir(project_path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_delta_publishes_current_generation() {
        let temp = tempfile::tempdir().expect("watcher fixture");
        std::fs::create_dir_all(temp.path().join("src")).expect("source directory");
        let source = temp.path().join("src/lib.rs");
        std::fs::write(&source, "pub fn watcher_marker() -> usize { 1 }\n")
            .expect("initial source");

        let mut index = LeIndex::new(temp.path()).expect("create index");
        index.index_project(true).expect("initial generation");
        let storage = temp.path().join(".leindex");
        let initial = std::fs::read_to_string(storage.join("CURRENT"))
            .expect("initial CURRENT")
            .trim()
            .parse::<u64>()
            .expect("initial generation number");

        std::fs::write(&source, "pub fn watcher_marker() -> usize { 2 }\n")
            .expect("changed source");
        index
            .incremental_reindex_from_watcher()
            .expect("watcher delta");

        let published = std::fs::read_to_string(storage.join("CURRENT"))
            .expect("published CURRENT")
            .trim()
            .parse::<u64>()
            .expect("published generation number");
        assert!(published > initial);
        assert!(storage
            .join("generations")
            .join(published.to_string())
            .join("leindex.db")
            .is_file());
    }
}
