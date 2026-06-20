// Indexing pipeline methods for LeIndex: index_project and load_from_storage.

use super::LeIndex;
use crate::cli::index_builder;
use crate::cli::memory_cap::MemoryCapGuard;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use tracing::{info, warn};

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
    pub(crate) fn incremental_reindex_from_watcher(&mut self) -> Result<super::IndexStats> {
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

        // Wrap in HybridEmbedder for compatibility
        // VAL-ONNX-001: When onnx feature is enabled, wrap as hybrid_local
        // so neural embeddings are available during incremental reindex.
        #[cfg(feature = "onnx")]
        let embedder = {
            match index_builder::HybridEmbedder::hybrid_local(tfidf_embedder.clone(), None) {
                Ok(hybrid) => hybrid,
                Err(e) => {
                    tracing::warn!(
                        "Failed to create hybrid_local embedder for incremental reindex (ONNX), falling back to tfidf_only: {}",
                        e
                    );
                    index_builder::HybridEmbedder::tfidf_only(tfidf_embedder)
                }
            }
        };
        #[cfg(not(feature = "onnx"))]
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

        // Two-pass approach: first collect node data, then batch neural embeddings.
        let mut updated_nodes: Vec<crate::search::search::NodeInfo> = Vec::new();
        let mut neural_pending: Vec<usize> = Vec::new();
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
            let file_content = String::from_utf8_lossy(&file_bytes);
            let content_bytes = file_content.as_bytes();
            let start = node.byte_range.0.min(content_bytes.len());
            let end = node.byte_range.1.min(content_bytes.len());

            let mut enrichment = format!(
                "// type:{} lang:{}",
                match node.node_type {
                    crate::graph::pdg::NodeType::Function => "function",
                    crate::graph::pdg::NodeType::Class => "class",
                    crate::graph::pdg::NodeType::Method => "method",
                    crate::graph::pdg::NodeType::Variable => "variable",
                    crate::graph::pdg::NodeType::Module => "module",
                    crate::graph::pdg::NodeType::External => "external",
                },
                node.language,
            );
            let callers = pdg.backward_impact(node_idx, &connectivity_config);
            let callees = pdg.forward_impact(node_idx, &connectivity_config);
            enrichment.push_str(&format!(
                " callers:{} callees:{} complexity:{}",
                callers.len().min(50),
                callees.len().min(50),
                node.complexity,
            ));

            let node_content = if start < end {
                let snippet = String::from_utf8_lossy(&content_bytes[start..end]);
                format!(
                    "{}\n// {} in {}\n{}",
                    enrichment, node.name, node.file_path, snippet
                )
            } else {
                format!(
                    "{}\n// {} in {}\n{}",
                    enrichment, node.name, node.file_path, "// [No source code available]"
                )
            };

            // Pruning gate: skip low-information / generated nodes (same as index_builder).
            let pruning_decision = pruner.evaluate(&node.file_path, &node_content, &node.name);
            if pruning_decision != crate::search::search::PruningDecision::Keep {
                continue;
            }

            let tokens = index_builder::tokenize_code(&node_content);

            let signature =
                crate::search::search::SearchEngine::extract_signature_from_content(&node_content);
            let tfidf_embedding = embedder.embed_tfidf(&tokens);

            // Defer neural embedding to batch call below
            let node_vec_idx = updated_nodes.len();
            if embedder.has_neural() {
                neural_pending.push(node_vec_idx);
            }

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

        // Batch neural embedding: one IPC call for all pending nodes.
        if !neural_pending.is_empty() {
            #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
            let batch_results = {
                let texts: Vec<String> = neural_pending
                    .iter()
                    .map(|&idx| updated_nodes[idx].content.clone())
                    .collect();
                embedder.embed_neural_batch_blocking(&texts)
            };
            #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
            let batch_results: Vec<Option<Vec<f32>>> = vec![None; neural_pending.len()];

            for (i, &node_vec_idx) in neural_pending.iter().enumerate() {
                if let Some(neural) = batch_results.get(i).and_then(|r| r.clone()) {
                    updated_nodes[node_vec_idx].neural_embedding = Some(neural);
                }
            }
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
            if let Err(err) =
                embedder.persist_to_storage(&self.project_path, self.pdg.as_ref().unwrap())
            {
                warn!("Failed to persist embedder: {err:#}");
            }
        }
        self.build_file_stats_cache();
        self.stats.indexing_time_ms = start_time.elapsed().as_millis() as u64;

        // R10: Persist embeddings to mmap file after watcher incremental reindex
        if let Err(err) =
            index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)
        {
            warn!("Failed to persist mmap embeddings: {err:#}");
        }
        // Persist neural embeddings separately for fast load_from_storage
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            if let Err(err) = index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
            ) {
                warn!("Failed to persist neural mmap embeddings: {err:#}");
            }
        }

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

    /// Shared indexing implementation used by both `index_project` and
    /// `index_project_with_memory_cap`.
    fn index_project_inner(
        &mut self,
        force: bool,
        mut cap_guard: Option<&mut MemoryCapGuard>,
    ) -> Result<super::IndexStats> {
        let start_time = std::time::Instant::now();

        info!(
            "Starting project indexing for: {} (force={})",
            self.project_id, force
        );

        // Step 1: Get currently indexed files from storage
        progress_stderr("Indexing: scanning files...");
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .context("Failed to load indexed files from storage")?;

        // Step 2: Collect all source files and compute hashes.
        // Use a shared file cache so files are read only once across both
        // hash collection and node indexing (Issue 2 fix).
        let old_scan = self.get_project_scan(false).ok();
        let mut shared_file_cache = index_builder::FileReadCache::new(200);
        let source_files_with_hashes =
            self.collect_source_files_with_hashes(true, Some(&mut shared_file_cache))?;
        info!("Found {} source files", source_files_with_hashes.len());

        // Memory cap checkpoint: after file scanning (file cache populated)
        if let Some(ref mut guard) = cap_guard {
            guard.check_now()?;
        }

        // Step 3: Identify changed/new/deleted files
        let mut files_to_parse = Vec::new();
        let mut unchanged_files = std::collections::HashSet::new();
        let source_file_hashes: std::collections::HashMap<String, String> =
            source_files_with_hashes
                .iter()
                .map(|(path, hash)| (path.display().to_string(), hash.clone()))
                .collect();

        let current_file_paths: std::collections::HashSet<String> = source_files_with_hashes
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect();

        for (path, hash) in &source_files_with_hashes {
            let path_str = path.display().to_string();
            if force
                || !indexed_files.contains_key(&path_str)
                || indexed_files.get(&path_str) != Some(hash)
            {
                files_to_parse.push(path.clone());
            } else {
                unchanged_files.insert(path_str);
            }
        }

        let deleted_files: Vec<String> = indexed_files
            .keys()
            .filter(|p| !current_file_paths.contains(*p))
            .cloned()
            .collect();

        info!(
            "Incremental analysis: {} to parse, {} unchanged, {} deleted",
            files_to_parse.len(),
            unchanged_files.len(),
            deleted_files.len()
        );

        if files_to_parse.is_empty() && deleted_files.is_empty() && self.is_indexed() {
            let manifest_dirty = self.check_manifest_stale();
            if !manifest_dirty {
                let scan = self.get_project_scan(false)?;
                let changed_manifests = match &old_scan {
                    Some(old) => {
                        let mut changed = Vec::new();
                        for mp in &scan.manifest_paths {
                            let key = mp.display().to_string();
                            let new_hash = scan.manifest_hashes.get(&key);
                            let old_hash = old.manifest_hashes.get(&key);
                            if new_hash != old_hash {
                                let path_str = key.to_lowercase();
                                let skip = path_str.contains("node_modules")
                                    || path_str.contains("/build/")
                                    || path_str.contains("\\build\\")
                                    || path_str.contains("/dist/")
                                    || path_str.contains("\\dist\\")
                                    || path_str.contains("/target/")
                                    || path_str.contains(".cache");
                                if !skip {
                                    changed.push(mp.clone());
                                }
                            }
                        }
                        changed
                    }
                    None => index_builder::detect_changed_manifests(
                        &scan,
                        &self.project_id,
                        &self.cache.cache_spiller,
                    ),
                };
                if changed_manifests.is_empty() {
                    info!("No changes detected, skipping indexing");
                    return Ok(self.stats.clone());
                }
                info!(
                    "Manifest content changed ({} files) — re-annotating",
                    changed_manifests.len()
                );
            } else {
                info!("Manifest files changed — running external dependency annotation");
            }
        }

        // Step 4: Parse changed files
        progress_stderr(&format!(
            "Indexing: parsing {} files...",
            files_to_parse.len()
        ));
        let parsing_results = if !files_to_parse.is_empty() {
            let parser = crate::parse::parallel::ParallelParser::new();
            parser.parse_files(files_to_parse)
        } else {
            Vec::new()
        };

        // Memory cap checkpoint: after parallel parsing (ASTs in memory)
        if let Some(ref mut guard) = cap_guard {
            guard.check_now()?;
        }

        // Step 5: Update PDG
        progress_stderr("Indexing: building PDG...");
        if !unchanged_files.is_empty() && self.pdg.is_none() {
            self.load_pdg_from_storage()
                .context("Failed to load existing PDG for incremental reindex. Please reindex with --force if corruption persists.")?;
        }

        let mut pdg = self.pdg.take().unwrap_or_default();
        let files_parsed = parsing_results.len();

        let successful = parsing_results.iter().filter(|r| r.is_success()).count();
        let failed = parsing_results.iter().filter(|r| r.is_failure()).count();
        let total_sigs: usize = parsing_results.iter().map(|r| r.signatures.len()).sum();

        // Log individual parse failures with file path context
        for result in parsing_results.iter().filter(|r| r.is_failure()) {
            warn!(
                "Parse failure for '{}' during indexing: {}",
                result.file_path.display(),
                result
                    .error
                    .as_deref()
                    .filter(|s| !s.is_empty())
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

        for path in &deleted_files {
            index_builder::remove_file_from_pdg(&mut pdg, path)?;
            if let Err(e) = crate::storage::pdg_store::delete_file_data(
                &mut self.storage,
                &self.project_id,
                path,
            ) {
                warn!(
                    "Failed to delete file data from storage for '{}' during indexing: {}",
                    path, e
                );
            }
        }

        // Collect all signatures from successful parse results for cross-file
        // call edge resolution. We clone the signatures here because they are
        // moved into extract_pdg_from_signatures below.
        let all_signatures: Vec<crate::parse::prelude::SignatureInfo> = parsing_results
            .iter()
            .filter(|r| r.is_success())
            .flat_map(|r| r.signatures.iter().cloned())
            .collect();

        // Iterate over parsing_results directly, avoiding intermediate HashMap construction
        // and the associated cloning of source_bytes, language, and signatures.
        for result in parsing_results.into_iter() {
            if !result.is_success() {
                continue;
            }

            let file_path = result.file_path.display().to_string();
            let language = result.language.as_deref().unwrap_or("unknown");
            let source_bytes = result.source_bytes.as_deref().unwrap_or(&[]);

            // Only replace the old subgraph once parsing succeeds. If parsing fails,
            // keep the previous graph intact so the saved PDG remains usable.
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
                        "Failed to update indexed file record for '{}' during indexing: {}",
                        file_path, e
                    );
                }
            }
        }

        // Step 5a: Resolve cross-file call edges
        // Per-file PDG extraction can only resolve calls within the same file.
        // This pass uses all signatures to resolve cross-file call relationships.
        if !all_signatures.is_empty() {
            crate::graph::resolve_cross_file_call_edges(&mut pdg, &all_signatures);
        }

        // Step 5b: Resolve external dependencies via lock files
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
        let (ext_in_lockfile, ext_resolved, ext_unresolved, ext_total, ext_builtin) = (
            ext_registry.len(),
            annotation_stats.resolved,
            annotation_stats.unresolved,
            annotation_stats.total_external,
            annotation_stats.builtin,
        );

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!(
            "Updated PDG has {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );

        // Memory cap checkpoint: after PDG construction (peak PDG memory usage)
        if let Some(ref mut guard) = cap_guard {
            guard.check_now()?;
        }

        // Step 6: Re-index nodes for search
        progress_stderr(&format!("Indexing: embedding {} nodes...", pdg_node_count));
        let batch_size = self.indexing_batch_size();
        let persisted_embedder =
            index_builder::TfIdfEmbedder::load_from_storage(&self.project_path)
                .ok()
                .flatten();
        let embedder = if let Some(embedder) = persisted_embedder {
            if embedder.is_fresh(pdg_node_count, pdg_edge_count) {
                info!("Loaded persisted embedder from storage");
                // VAL-ONNX-001: Wrap as hybrid_local when onnx feature is enabled
                #[cfg(feature = "onnx")]
                let hybrid_embedder = {
                    match index_builder::HybridEmbedder::hybrid_local(embedder.clone(), None) {
                        Ok(h) => h,
                        Err(e) => {
                            warn!("Failed to create hybrid_local from persisted embedder (ONNX), falling back to tfidf_only: {}", e);
                            index_builder::HybridEmbedder::tfidf_only(embedder)
                        }
                    }
                };
                #[cfg(not(feature = "onnx"))]
                let hybrid_embedder = index_builder::HybridEmbedder::tfidf_only(embedder);
                index_builder::index_nodes_with_embedder(
                    &pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    Some(hybrid_embedder),
                    Some(shared_file_cache),
                )?
            } else {
                info!("Persisted embedder is stale; rebuilding TF-IDF index");
                index_builder::index_nodes_with_embedder(
                    &pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    None,
                    Some(shared_file_cache),
                )?
            }
        } else {
            index_builder::index_nodes_with_embedder(
                &pdg,
                &mut self.search_engine,
                &mut self.cache.file_stats_cache,
                batch_size,
                None,
                Some(shared_file_cache),
            )?
        };
        self.embedder = Some(embedder);
        if let Some(embedder) = &self.embedder {
            if let Err(err) = embedder.persist_to_storage(&self.project_path, &pdg) {
                warn!("Failed to persist embedder: {err:#}");
            }
        }

        let indexed_count = self.search_engine.node_count();

        info!("Indexed {} nodes for search", indexed_count);

        // Step 7: Persist to storage
        progress_stderr("Indexing: saving to storage...");
        index_builder::save_to_storage(&mut self.storage, &self.project_id, &pdg)?;

        // Update statistics
        self.stats = super::IndexStats {
            total_files: source_files_with_hashes.len(),
            files_parsed,
            successful_parses: successful,
            failed_parses: failed,
            total_signatures: total_sigs,
            pdg_nodes: pdg_node_count,
            pdg_edges: pdg_edge_count,
            indexed_nodes: indexed_count,
            indexing_time_ms: start_time.elapsed().as_millis() as u64,
            external_deps_in_lockfile: ext_in_lockfile,
            external_deps_resolved: ext_resolved,
            external_deps_unresolved: ext_unresolved,
            external_deps_total: ext_total,
            external_deps_builtin: ext_builtin,
        };

        // Normalize external nodes (legacy compat)
        index_builder::normalize_external_nodes(&mut pdg);

        // Keep PDG in memory
        self.pdg = Some(pdg);

        // Build file stats cache for performance
        self.build_file_stats_cache();

        // R10: Persist embeddings to mmap file for fast read-only access
        if let Err(err) =
            index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)
        {
            warn!("Failed to persist mmap embeddings: {err:#}");
        }
        // Persist neural embeddings separately for fast load_from_storage
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            if let Err(err) = index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
            ) {
                warn!("Failed to persist neural mmap embeddings: {err:#}");
            }
        }

        // Update last_indexed timestamp in project_metadata
        if let Err(err) = self.update_last_indexed_timestamp() {
            warn!("Failed to update last_indexed timestamp: {err:#}");
        }
        // Persist IndexStats so diagnostics can report accurate totals
        // after loading from storage (without a full re-index).
        if let Err(err) = self.save_stats_to_storage() {
            warn!("Failed to persist index stats: {err:#}");
        }

        // Clear search query and analysis caches so stale results are not
        // served after a reindex (VAL-INDEX-005). The cache key fingerprint
        // now includes more stats fields, but we also proactively remove
        // old cached entries to handle edge cases and keep the cache dir clean.
        index_builder::clear_query_caches(&mut self.cache.cache_spiller, &self.project_id);

        // Note: ONNX worker idle-unload is handled by the worker's own idle
        // timeout (see leindex-embed runtime.rs). We do NOT call
        // unload_onnx() here because this path runs on every incremental
        // reindex (file save with 500ms debounce), and killing the worker
        // process on each save causes high latency for subsequent requests.

        info!("Indexing completed in {}ms", self.stats.indexing_time_ms);

        // Record RSS observation after indexing for memory report.
        crate::cli::memory_report::observe_rss("post_index");

        // Clear the progress line so the final output is clean.
        progress_clear();

        Ok(self.stats.clone())
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

    /// Load a previously indexed project from storage
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn load_from_storage(&mut self) -> Result<()> {
        self.load_from_storage_inner(false)
    }

    /// Load PDG from storage without populating the search engine.
    /// Used by index_project() when it will call index_nodes() afterwards.
    pub fn load_pdg_from_storage(&mut self) -> Result<()> {
        self.load_from_storage_inner(true)
    }

    fn load_from_storage_inner(&mut self, pdg_only: bool) -> Result<()> {
        info!(
            "Loading project from storage: {} (pdg_only={})",
            self.project_id, pdg_only
        );

        let mut pdg = crate::storage::pdg_store::load_pdg(&self.storage, &self.project_id)
            .context("Failed to load PDG from storage")?;

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

        let batch_size = self.indexing_batch_size();
        let persisted_embedder =
            index_builder::TfIdfEmbedder::load_from_storage(&self.project_path)
                .ok()
                .flatten();
        let embedder = if let Some(embedder) = persisted_embedder {
            if embedder.is_fresh(pdg_node_count, pdg_edge_count) {
                info!("Loaded persisted embedder from storage");
                // Use tfidf_only during load_from_storage to avoid expensive
                // batch neural embedding of all nodes. Neural embeddings are
                // restored from the persisted mmap file below.
                let tfidf_embedder = index_builder::HybridEmbedder::tfidf_only(embedder);
                index_builder::index_nodes_with_embedder(
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
                index_builder::index_nodes_with_embedder(
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
            index_builder::index_nodes_with_embedder(
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
                index_builder::try_load_neural_mmap_embeddings(&self.project_path)
            {
                let restored = self.search_engine.restore_neural_embeddings(&neural_mmap);
                if restored > 0 {
                    info!(
                        "Restored {} neural embeddings from persisted neural mmap file",
                        restored
                    );
                } else {
                    info!("Neural mmap file loaded but no matching node IDs found; neural scores will be 0");
                }
            } else {
                info!("No persisted neural embeddings found; run 'leindex index --force' with onnx feature to generate them");
            }
        }

        // VAL-ONNX-001: After indexing with tfidf_only, upgrade the embedder
        // to hybrid_local for query-time neural embedding (single query, not batch).
        #[cfg(feature = "onnx")]
        {
            if let Some(tfidf) = index_builder::TfIdfEmbedder::load_from_storage(&self.project_path)
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
        if let Some(embedder) = &self.embedder {
            if let Err(err) = embedder.persist_to_storage(&self.project_path, &pdg) {
                warn!("Failed to persist embedder: {err:#}");
            }
        }
        let indexed_count = self.search_engine.node_count();

        info!("Rebuilt search index with {} nodes", indexed_count);

        // Load persisted stats first (to restore total_signatures, total_files, etc.),
        // then overwrite the live PDG/search counts with freshly computed values.
        if let Err(err) = self.load_stats_from_storage() {
            warn!("Failed to load persisted index stats: {err:#}");
        }
        self.stats.pdg_nodes = pdg_node_count;
        self.stats.pdg_edges = pdg_edge_count;
        self.stats.indexed_nodes = indexed_count;

        self.pdg = Some(pdg);

        self.build_file_stats_cache();

        // R10: Persist embeddings to mmap file for fast read-only access
        if let Err(err) =
            index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)
        {
            warn!("Failed to persist mmap embeddings: {err:#}");
        }
        // Persist neural embeddings separately for fast load_from_storage
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            if let Err(err) = index_builder::persist_neural_embeddings_to_mmap(
                &self.search_engine,
                &self.project_path,
            ) {
                warn!("Failed to persist neural mmap embeddings: {err:#}");
            }
        }

        Ok(())
    }
}
