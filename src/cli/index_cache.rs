// Index Cache — cache subsystem extracted from LeIndex

use crate::cli::memory::{
    pdg_cache_key, project_scan_cache_key, search_cache_key, CacheEntry,
    CacheSpiller, MemoryConfig, WarmStrategy,
};
use crate::graph::pdg::ProgramDependenceGraph;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info};

use super::leindex::{FileStats, ProjectFileScan};

/// Cache subsystem for LeIndex.
///
/// Owns the `CacheSpiller`, in-memory `project_scan`, and `file_stats_cache`.
pub(crate) struct IndexCache {
    pub cache_spiller: CacheSpiller,
    pub project_scan: Option<ProjectFileScan>,
    pub file_stats_cache: Option<HashMap<String, FileStats>>,
}

impl IndexCache {
    /// Create a new IndexCache with the given cache directory.
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        let memory_config = MemoryConfig {
            cache_dir,
            ..Default::default()
        };
        let mut cache_spiller =
            CacheSpiller::new(memory_config).context("Failed to initialize cache spiller")?;
        if let Ok(restored) = cache_spiller.auto_restore() {
            debug!(
                "Cache auto-restore on startup: restored={} failed={}",
                restored.entries_restored,
                restored.entries_failed.len()
            );
        }
        Ok(Self {
            cache_spiller,
            project_scan: None,
            file_stats_cache: None,
        })
    }

    /// Get or load the project scan, using cache first, filesystem second.
    pub fn get_project_scan(
        &mut self,
        project_id: &str,
        refresh: bool,
        scan_fn: impl Fn() -> Result<ProjectFileScan>,
    ) -> Result<ProjectFileScan> {
        if !refresh {
            // Check in-memory cache first
            if let Some(scan) = &self.project_scan {
                return Ok(scan.clone());
            }

            // Try persistent cache
            let cache_key = project_scan_cache_key(project_id);
            if let Some(CacheEntry::Binary {
                serialized_data, ..
            }) = self.cache_spiller.store_mut().get_or_load(&cache_key)?
            {
                if let Ok(scan) = bincode::deserialize::<ProjectFileScan>(&serialized_data) {
                    self.project_scan = Some(scan.clone());
                    return Ok(scan);
                }
            }
        }

        // Cache miss or forced refresh — scan and cache
        let scan = scan_fn()?;
        self.cache_project_scan(project_id, &scan);
        self.project_scan = Some(scan.clone());
        Ok(scan)
    }

    /// Persist a project scan to the cache spiller.
    pub fn cache_project_scan(&mut self, project_id: &str, scan: &ProjectFileScan) {
        if let Ok(serialized) = bincode::serialize(scan) {
            let entry = CacheEntry::Binary {
                metadata: std::collections::HashMap::from([
                    ("type".to_string(), "project_scan".to_string()),
                    ("project_id".to_string(), project_id.to_string()),
                ]),
                serialized_data: serialized,
            };
            let cache_key = project_scan_cache_key(project_id);
            if self
                .cache_spiller
                .store_mut()
                .insert(cache_key.clone(), entry)
                .is_ok()
            {
                let _ = self.cache_spiller.store_mut().persist_key(&cache_key);
            }
        }
    }

    /// Build file statistics cache from PDG.
    pub fn build_file_stats_cache(&mut self, pdg: &ProgramDependenceGraph) {
        let mut cache: HashMap<String, FileStats> = HashMap::new();
        // First pass: collect symbol counts and complexity
        for nid in pdg.node_indices() {
            if let Some(node) = pdg.get_node(nid) {
                if matches!(node.node_type, crate::graph::pdg::NodeType::External) {
                    continue;
                }
                let entry = cache.entry(node.file_path.clone()).or_insert_with(|| FileStats {
                    symbol_count: 0,
                    total_complexity: 0,
                    symbol_names: Vec::new(),
                    outgoing_deps: 0,
                    incoming_deps: 0,
                });
                entry.symbol_count += 1;
                entry.total_complexity += node.complexity;
                if entry.symbol_names.len() < 5 {
                    entry.symbol_names.push(node.name.clone());
                }
            }
        }
        // Second pass: compute cross-file dependency degrees
        let file_paths: Vec<String> = cache.keys().cloned().collect();
        for file_path in &file_paths {
            let nodes = pdg.nodes_in_file(file_path);
            let mut incoming_files = std::collections::HashSet::new();
            let mut outgoing_files = std::collections::HashSet::new();
            for nid in &nodes {
                for dep_id in pdg.neighbors(*nid) {
                    if let Some(dep) = pdg.get_node(dep_id) {
                        if !matches!(dep.node_type, crate::graph::pdg::NodeType::External)
                            && dep.file_path != *file_path
                        {
                            outgoing_files.insert(dep.file_path.clone());
                        }
                    }
                }
                for dep_id in pdg.predecessors(*nid) {
                    if let Some(dep) = pdg.get_node(dep_id) {
                        if !matches!(dep.node_type, crate::graph::pdg::NodeType::External)
                            && dep.file_path != *file_path
                        {
                            incoming_files.insert(dep.file_path.clone());
                        }
                    }
                }
            }
            if let Some(entry) = cache.get_mut(file_path) {
                entry.outgoing_deps = outgoing_files.len();
                entry.incoming_deps = incoming_files.len();
            }
        }
        self.file_stats_cache = Some(cache);
    }

    /// Get file statistics cache reference.
    pub fn file_stats(&self) -> Option<&HashMap<String, FileStats>> {
        self.file_stats_cache.as_ref()
    }

    /// Check memory and spill cache if threshold exceeded.
    pub fn check_memory_and_spill(&mut self) -> Result<bool> {
        if self.cache_spiller.is_threshold_exceeded()? {
            info!("Memory threshold exceeded, initiating cache spilling");
            let result = self.cache_spiller.check_and_spill()?;
            info!(
                "Spilled {} entries, freed {} bytes",
                result.entries_spilled, result.memory_freed
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Spill PDG cache to disk.
    pub fn spill_pdg_cache(
        &mut self,
        project_id: &str,
        pdg: &mut Option<ProgramDependenceGraph>,
    ) -> Result<()> {
        let pdg_ref = pdg
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No PDG in memory to spill"))?;

        let node_count = pdg_ref.node_count();
        let edge_count = pdg_ref.edge_count();

        let cache_key = pdg_cache_key(project_id);
        let entry = CacheEntry::PDG {
            project_id: project_id.to_string(),
            node_count,
            edge_count,
            serialized_data: vec![],
        };

        // Insert the spill marker *before* taking the PDG so a failed insert
        // does not discard the in-memory graph.
        self.cache_spiller
            .store_mut()
            .insert(cache_key.clone(), entry)
            .context("Failed to create PDG spill marker")?;
        self.cache_spiller
            .store_mut()
            .persist_key(&cache_key)
            .context("Failed to persist PDG spill marker")?;

        pdg.take();

        info!(
            "Spilled PDG from memory: {} nodes, {} edges (persisted to lestockage)",
            node_count, edge_count
        );

        Ok(())
    }

    /// Spill vector search cache to disk.
    pub fn spill_vector_cache(
        &mut self,
        project_id: &str,
        search_node_count: usize,
    ) -> Result<()> {
        let cache_key = search_cache_key(project_id);
        let entry = CacheEntry::SearchIndex {
            project_id: project_id.to_string(),
            entry_count: search_node_count,
            serialized_data: vec![],
        };

        self.cache_spiller
            .store_mut()
            .insert(cache_key.clone(), entry)
            .context("Failed to spill vector cache marker")?;
        self.cache_spiller
            .store_mut()
            .persist_key(&cache_key)
            .context("Failed to persist vector cache marker")?;

        info!("Spilled vector cache marker: {} entries", search_node_count);

        Ok(())
    }

    /// Spill all caches (PDG and vector) to disk.
    pub fn spill_all_caches(
        &mut self,
        project_id: &str,
        pdg: &mut Option<ProgramDependenceGraph>,
        search_node_count: usize,
    ) -> Result<(usize, usize)> {
        let mut pdg_bytes = 0;

        if pdg.is_some() {
            let before = self.cache_spiller.store().total_bytes();
            self.spill_pdg_cache(project_id, pdg)?;
            let after = self.cache_spiller.store().total_bytes();
            pdg_bytes = after.saturating_sub(before);
        }

        let before_vec = self.cache_spiller.store().total_bytes();
        self.spill_vector_cache(project_id, search_node_count)?;
        let after_vec = self.cache_spiller.store().total_bytes();
        let vector_bytes = after_vec.saturating_sub(before_vec);

        info!(
            "Spilled all caches: PDG ({} bytes), Vector ({} bytes)",
            pdg_bytes, vector_bytes
        );

        Ok((pdg_bytes, vector_bytes))
    }

    /// Warm caches with frequently accessed data.
    pub fn warm_cache(
        &mut self,
        strategy: WarmStrategy,
    ) -> Result<crate::cli::memory::WarmResult> {
        info!("Warming caches with strategy: {:?}", strategy);
        Ok(self.cache_spiller.warm_cache(strategy)?)
    }

    /// Get cache statistics.
    pub fn get_cache_stats(&self) -> Result<crate::cli::memory::MemoryStats> {
        self.cache_spiller
            .memory_stats()
            .map_err(|e| anyhow::anyhow!("Failed to get cache stats: {}", e))
    }
}
