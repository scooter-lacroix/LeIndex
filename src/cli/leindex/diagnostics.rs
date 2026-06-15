// Diagnostics and coverage reporting methods for LeIndex.

use super::LeIndex;
use anyhow::{Context, Result};
use std::collections::HashSet;

impl LeIndex {
    /// Get diagnostics about the indexed project
    ///
    /// # Returns
    ///
    /// `Result<Diagnostics>` - Project diagnostics information
    ///
    /// # Example
    ///
    /// ```ignore
    /// let diag = leindex.get_diagnostics()?;
    /// println!("Memory usage: {:.1}%", diag.memory_usage_percent);
    /// ```
    pub fn get_diagnostics(&self) -> Result<super::Diagnostics> {
        let memory_stats = self
            .cache
            .cache_spiller
            .memory_stats()
            .context("Failed to get memory stats")?;
        let memory_percent = memory_stats.memory_percent();
        let threshold_exceeded = self.cache.cache_spiller.store().total_bytes() > 0
            && self
                .cache
                .cache_spiller
                .is_threshold_exceeded()
                .unwrap_or(false);

        let pdg_loaded = self.pdg.is_some();
        let (pdg_nodes, pdg_edges) = self
            .pdg
            .as_ref()
            .map(|p| (p.node_count(), p.edge_count()))
            .unwrap_or((0, 0));
        // Rough estimate: ~200 bytes per node (name, file_path, id strings + overhead)
        // ~64 bytes per edge (two indices + edge metadata)
        let pdg_estimated_bytes = pdg_nodes * 200 + pdg_edges * 64;

        let search_index_nodes = self.search_engine.node_count();

        let index_health = if !pdg_loaded || pdg_nodes == 0 {
            "empty".to_string()
        } else if self.stats.failed_parses > 0 {
            "stale".to_string()
        } else {
            "healthy".to_string()
        };

        let cache_temperature = if memory_stats.cache_hits == 0 {
            "cold".to_string()
        } else if memory_stats.cache_hit_rate >= 0.70 && memory_stats.cache_hits >= 5 {
            "hot".to_string()
        } else {
            "warm".to_string()
        };

        // Determine embedding model status from the embedder variant.
        let embedding_model = match &self.embedder {
            None => "unknown".to_string(),
            Some(crate::cli::index_builder::HybridEmbedder::TfIdfOnly(_)) => {
                "tfidf_only".to_string()
            }
            #[cfg(feature = "onnx")]
            Some(crate::cli::index_builder::HybridEmbedder::HybridLocal { .. }) => {
                "onnx_hybrid".to_string()
            }
            #[cfg(feature = "remote-embeddings")]
            Some(crate::cli::index_builder::HybridEmbedder::HybridRemote { .. }) => {
                "remote_hybrid".to_string()
            }
        };

        Ok(super::Diagnostics {
            project_path: self.project_path.display().to_string(),
            project_id: self.project_id.clone(),
            unique_project_id: self.unique_id.to_string(),
            display_name: self.unique_id.display(),
            stats: self.stats.clone(),
            memory_usage_bytes: memory_stats.rss_bytes,
            total_memory_bytes: memory_stats.total_bytes,
            memory_usage_percent: memory_percent,
            memory_threshold_exceeded: threshold_exceeded,
            cache_entries: memory_stats.cache_entries,
            cache_bytes: memory_stats.cache_bytes,
            spilled_entries: memory_stats.spilled_entries,
            spilled_bytes: memory_stats.spilled_bytes,
            cache_hits: memory_stats.cache_hits,
            cache_memory_hits: memory_stats.cache_memory_hits,
            cache_disk_hits: memory_stats.cache_disk_hits,
            cache_misses: memory_stats.cache_misses,
            cache_hit_rate: memory_stats.cache_hit_rate,
            cache_writes: memory_stats.cache_writes,
            cache_spills: memory_stats.cache_spills,
            cache_restores: memory_stats.cache_restores,
            cache_temperature,
            pdg_loaded,
            pdg_estimated_bytes,
            search_index_nodes,
            index_health,
            pdg_nodes,
            pdg_edges,
            embedding_model,
        })
    }

    /// Report which files are indexed and which are not.
    pub fn coverage_report(&mut self) -> Result<super::CoverageReport> {
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .context("Failed to load indexed files from storage")?;
        let source_files = self.collect_source_file_paths(true)?;

        let indexed_set: HashSet<String> = indexed_files.keys().cloned().collect();
        let source_set: HashSet<String> = source_files
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        let missing: Vec<String> = source_set.difference(&indexed_set).cloned().collect();
        let orphaned: Vec<String> = indexed_set.difference(&source_set).cloned().collect();
        let indexed_present = indexed_set.intersection(&source_set).count();

        Ok(super::CoverageReport {
            total_source_files: source_files.len(),
            indexed_files: indexed_files.len(),
            missing_files: missing,
            orphaned_entries: orphaned,
            coverage_pct: if source_files.is_empty() {
                100.0
            } else {
                (indexed_present as f64 / source_files.len() as f64) * 100.0
            },
        })
    }
}
