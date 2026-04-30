// Indexing pipeline methods for LeIndex: index_project and load_from_storage.

use super::LeIndex;
use crate::cli::index_builder;
use anyhow::{Context, Result};
use tracing::info;

impl LeIndex {
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
        let start_time = std::time::Instant::now();

        info!(
            "Starting project indexing for: {} (force={})",
            self.project_id, force
        );

        // Step 1: Get currently indexed files from storage
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .context("Failed to load indexed files from storage")?;

        // Step 2: Collect all source files and compute hashes.
        let old_scan = self.get_project_scan(false).ok();
        let source_files_with_hashes = self.collect_source_files_with_hashes(true)?;
        info!("Found {} source files", source_files_with_hashes.len());

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
        let parsing_results = if !files_to_parse.is_empty() {
            let parser = crate::parse::parallel::ParallelParser::new();
            parser.parse_files(files_to_parse)
        } else {
            Vec::new()
        };

        // Step 5: Update PDG
        if !unchanged_files.is_empty() && self.pdg.is_none() {
            self.load_from_storage()
                .context("Failed to load existing PDG for incremental reindex. Please reindex with --force if corruption persists.")?;
        }

        let mut pdg = self.pdg.take().unwrap_or_default();
        let files_parsed = parsing_results.len();

        let successful = parsing_results.iter().filter(|r| r.is_success()).count();
        let failed = parsing_results.iter().filter(|r| r.is_failure()).count();
        let total_sigs: usize = parsing_results.iter().map(|r| r.signatures.len()).sum();

        for path in &deleted_files {
            index_builder::remove_file_from_pdg(&mut pdg, path)?;
            let _ = crate::storage::pdg_store::delete_file_data(
                &mut self.storage,
                &self.project_id,
                path,
            );
        }

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
                let _ = crate::storage::pdg_store::update_indexed_file(
                    &mut self.storage,
                    &self.project_id,
                    &file_path,
                    hash,
                );
            }
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
        let (ext_in_lockfile, ext_resolved, ext_unresolved) = (
            ext_registry.len(),
            annotation_stats.resolved,
            annotation_stats.unresolved,
        );

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!(
            "Updated PDG has {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );

        // Step 6: Re-index nodes for search
        self.embedder = Some(index_builder::index_nodes(
            &pdg,
            &mut self.search_engine,
            &mut self.cache.file_stats_cache,
        )?);
        let indexed_count = self.search_engine.node_count();

        info!("Indexed {} nodes for search", indexed_count);

        // Step 7: Persist to storage
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
        };

        // Normalize external nodes (legacy compat)
        index_builder::normalize_external_nodes(&mut pdg);

        // Keep PDG in memory
        self.pdg = Some(pdg);

        // Build file stats cache for performance
        self.build_file_stats_cache();

        info!("Indexing completed in {}ms", self.stats.indexing_time_ms);

        Ok(self.stats.clone())
    }

    /// Load a previously indexed project from storage
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn load_from_storage(&mut self) -> Result<()> {
        info!("Loading project from storage: {}", self.project_id);

        let mut pdg = crate::storage::pdg_store::load_pdg(&self.storage, &self.project_id)
            .context("Failed to load PDG from storage")?;

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!(
            "Loaded PDG with {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );

        index_builder::normalize_external_nodes(&mut pdg);

        self.embedder = Some(index_builder::index_nodes(
            &pdg,
            &mut self.search_engine,
            &mut self.cache.file_stats_cache,
        )?);
        let indexed_count = self.search_engine.node_count();

        info!("Rebuilt search index with {} nodes", indexed_count);

        self.stats.pdg_nodes = pdg_node_count;
        self.stats.pdg_edges = pdg_edge_count;
        self.stats.indexed_nodes = indexed_count;

        self.pdg = Some(pdg);

        self.build_file_stats_cache();

        Ok(())
    }
}
