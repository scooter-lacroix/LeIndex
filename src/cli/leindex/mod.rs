// leindex - Core Orchestration
//
// *L'Index* (The Index) - Unified API that brings together all LeIndex crates

mod diagnostics;
mod indexing;
mod query;
pub(crate) mod setup;
mod types;

#[cfg(test)]
mod tests;

// Re-export public types for external callers
pub use types::{AnalysisResult, CoverageReport, Diagnostics, FileStats, IndexStats};
// Re-export crate-internal types for sibling modules (index_builder, index_cache, etc.)
pub(crate) use types::{
    ProjectFileScan, DEPENDENCY_MANIFEST_NAMES, SKIP_DIRS, SOURCE_FILE_EXTENSIONS,
};

use crate::cli::index_builder;
use crate::cli::memory::WarmStrategy;
use crate::graph::pdg::ProgramDependenceGraph;
use crate::search::search::SearchEngine;
use crate::storage::{schema::Storage, UniqueProjectId};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// LeIndex - Main orchestration struct for the entire LeIndex system.
///
/// ```ignore
/// let leindex = LeIndex::new("/path/to/project")?;
/// leindex.index_project()?;
/// let results = leindex.search("authentication", 10).await?;
/// ```
pub struct LeIndex {
    /// Project path
    project_path: PathBuf,

    /// Resolved storage root for index artifacts (may be outside project)
    storage_path: PathBuf,

    /// Project identifier (legacy, for backward compatibility)
    project_id: String,

    /// Unique project identifier with BLAKE3-based path hashing
    unique_id: UniqueProjectId,

    /// Storage backend
    storage: Storage,

    /// Search engine
    search_engine: SearchEngine,

    /// Program Dependence Graph
    pdg: Option<ProgramDependenceGraph>,

    /// Cache subsystem (spiller, project scan, file stats)
    cache: crate::cli::index_cache::IndexCache,

    /// Cached project configuration.
    project_config: crate::cli::config::ProjectConfig,

    /// Indexing statistics
    stats: IndexStats,

    /// TF-IDF embedder (None until index_nodes() runs).
    embedder: Option<index_builder::HybridEmbedder>,
}

impl LeIndex {
    /// Try to create a directory and verify it is writable.
    fn try_create_dir(path: &Path) -> bool {
        std::fs::create_dir_all(path).is_ok()
            && std::fs::metadata(path)
                .map(|m| !m.permissions().readonly())
                .unwrap_or(false)
    }

    /// Resolve the storage directory (in-project → LEINDEX_HOME → XDG → tmp).
    fn resolve_storage_path(project_path: &Path) -> Result<PathBuf> {
        let path_hash = &blake3::hash(project_path.to_string_lossy().as_bytes()).to_hex()[..12];
        let dir_name = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // 1. Prefer in-project .leindex
        let in_project = project_path.join(".leindex");
        if Self::try_create_dir(&in_project) {
            return Ok(in_project);
        }

        // 2. LEINDEX_HOME env var
        if let Ok(home) = std::env::var("LEINDEX_HOME") {
            let env_path = PathBuf::from(home).join(format!("{}-{}", dir_name, path_hash));
            if Self::try_create_dir(&env_path) {
                warn!(
                    "Using LEINDEX_HOME fallback for storage: {}",
                    env_path.display()
                );
                return Ok(env_path);
            }
        }

        // 3. XDG data dir (~/.local/share/leindex/<hash>)
        let xdg_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        if Self::try_create_dir(&xdg_dir) {
            warn!(
                "Using XDG data dir fallback for storage: {}",
                xdg_dir.display()
            );
            return Ok(xdg_dir);
        }

        // 4. System temp dir
        let tmp_path = std::env::temp_dir()
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        std::fs::create_dir_all(&tmp_path).with_context(|| {
            format!(
                "Failed to create .leindex storage directory.\n\
                 Tried:\n\
                 1. {} (in-project)\n\
                 2. $LEINDEX_HOME (not set or not writable)\n\
                 3. {} (XDG data dir)\n\
                 4. {} (temp dir)\n\n\
                 Fix: Check directory permissions, or set LEINDEX_HOME env var to a writable path.",
                in_project.display(),
                xdg_dir.display(),
                tmp_path.display(),
            )
        })?;
        warn!(
            "Using temp dir fallback for storage: {}",
            tmp_path.display()
        );
        Ok(tmp_path)
    }

    /// Open storage with retry and exponential backoff.
    fn open_storage_with_retry(db_path: &Path, max_retries: u32) -> Result<Storage> {
        let mut attempt = 0;
        loop {
            match Storage::open(db_path) {
                Ok(s) => return Ok(s),
                Err(e) if attempt < max_retries => {
                    attempt += 1;
                    let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempt));
                    warn!(
                        "Storage open attempt {}/{} failed: {}. Retrying in {:?}",
                        attempt, max_retries, e, delay
                    );
                    std::thread::sleep(delay);
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to open storage at {} after {} attempts.\n\
                             Suggestion: Delete {} and re-index, or check disk space.",
                            db_path.display(),
                            max_retries,
                            db_path.display()
                        )
                    });
                }
            }
        }
    }

    /// Create a new LeIndex instance for a project.
    ///
    /// ```ignore
    /// let leindex = LeIndex::new("/path/to/project")?;
    /// ```
    pub fn new<P: AsRef<Path>>(project_path: P) -> Result<Self> {
        let project_path = project_path
            .as_ref()
            .canonicalize()
            .context("Failed to canonicalize project path")?;

        let project_id = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Initialize storage with multi-location fallback and retry
        let storage_path = Self::resolve_storage_path(&project_path)?;

        // Write artifact ownership marker for GC (only for non-in-project storage)
        crate::cli::cleanup::write_artifact_marker(&storage_path);

        // Register at-exit cleanup for temp-based storage
        crate::cli::cleanup::register_at_exit_cleanup(storage_path.clone());

        let db_path = storage_path.join("leindex.db");
        let storage = Self::open_storage_with_retry(&db_path, 3)?;

        // Generate unique project ID with conflict resolution
        // Load existing projects with same base name
        let existing_ids = storage
            .load_existing_ids(&project_id)
            .context("Failed to load existing project IDs from storage")?;
        let unique_id = UniqueProjectId::generate(&project_path, &existing_ids);

        // Store the project metadata
        storage
            .store_project_metadata(&unique_id, &project_path)
            .context("Failed to store project metadata")?;

        info!(
            "Creating LeIndex for project: {} (unique ID: {}) at {:?}",
            project_id,
            unique_id.to_string(),
            project_path
        );

        // Initialize search engine
        let search_engine = SearchEngine::new();

        // Initialize cache subsystem
        let cache_dir = storage_path.join("cache");
        let cache = crate::cli::index_cache::IndexCache::new(cache_dir)?;
        let project_config =
            crate::cli::config::ProjectConfig::load(&project_path).unwrap_or_default();

        let instance = Self {
            project_path,
            storage_path,
            project_id,
            unique_id,
            storage,
            search_engine,
            pdg: None,
            cache,
            project_config,
            stats: IndexStats {
                total_files: 0,
                files_parsed: 0,
                successful_parses: 0,
                failed_parses: 0,
                total_signatures: 0,
                pdg_nodes: 0,
                pdg_edges: 0,
                indexed_nodes: 0,
                indexing_time_ms: 0,
                external_deps_in_lockfile: 0,
                external_deps_resolved: 0,
                external_deps_unresolved: 0,
                external_deps_total: 0,
                external_deps_builtin: 0,
            },
            embedder: None,
        };

        // Restore persisted index stats (if any) so diagnostics can report
        // accurate totals without requiring a full re-index.
        let mut instance = instance;
        if let Err(err) = instance.load_stats_from_storage() {
            warn!("Failed to load persisted index stats: {err:#}");
        }

        Ok(instance)
    }

    // ---- Internal helpers ----

    fn collect_source_files_with_hashes(
        &mut self,
        refresh: bool,
        file_cache: Option<&mut index_builder::FileReadCache>,
    ) -> Result<Vec<(PathBuf, String)>> {
        let scan = self.get_project_scan(refresh)?;
        index_builder::collect_source_files_with_hashes(&scan, file_cache)
    }

    fn collect_source_file_paths(&mut self, refresh: bool) -> Result<Vec<PathBuf>> {
        Ok(self.get_project_scan(refresh)?.source_paths)
    }

    fn get_project_scan(&mut self, refresh: bool) -> Result<ProjectFileScan> {
        if !refresh {
            if let Some(scan) = &self.cache.project_scan {
                return Ok(scan.clone());
            }
        }
        let project_id = self.project_id.clone();
        if !refresh {
            if let result @ Ok(_) = self
                .cache
                .get_project_scan(&project_id, false, || Err(anyhow::anyhow!("cache miss")))
            {
                return result;
            }
        }
        let scan = self.scan_project_files()?;
        self.cache.cache_project_scan(&project_id, &scan);
        self.cache.project_scan = Some(scan.clone());
        Ok(scan)
    }

    fn scan_project_files(&self) -> Result<ProjectFileScan> {
        index_builder::scan_project_files(&self.project_path)
    }

    /// Build a FreshnessContext for delegation to index_freshness module.
    fn freshness_context(&self) -> crate::cli::index_freshness::FreshnessContext<'_> {
        crate::cli::index_freshness::FreshnessContext {
            project_path: &self.project_path,
            storage_path: &self.storage_path,
            project_id: &self.project_id,
            storage: &self.storage,
            project_scan: self.cache.project_scan.as_ref(),
            cache_spiller: &self.cache.cache_spiller,
        }
    }

    pub(crate) fn indexing_batch_size(&self) -> usize {
        self.project_config.indexing.batch_size
    }

    fn search_cache_key_for(
        &self,
        query: &str,
        top_k: usize,
        query_type: Option<&crate::search::ranking::QueryType>,
    ) -> String {
        index_builder::search_cache_key_for(
            &self.project_id,
            &self.project_path,
            &self.stats,
            query,
            top_k,
            query_type,
        )
    }

    fn analysis_cache_key_for(&self, query: &str, token_budget: usize) -> String {
        index_builder::analysis_cache_key_for(
            &self.project_id,
            &self.project_path,
            &self.stats,
            query,
            token_budget,
        )
    }

    // ---- Freshness delegation ----

    /// Check which source files have changed since last index.
    /// Returns (changed_paths, deleted_paths).
    pub fn check_freshness(&self) -> Result<(Vec<PathBuf>, Vec<String>)> {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::check_freshness(
            &ctx,
            || self.scan_project_files(),
            index_builder::hash_file,
        )
    }

    /// Check if any dependency manifest/lockfile has changed since last index.
    fn check_manifest_stale(&self) -> bool {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::check_manifest_stale(&ctx, || self.scan_project_files())
    }

    /// Given changed manifests, find source files importing from those packages.
    #[allow(dead_code)]
    fn files_importing_from_manifests(
        &self,
        changed_manifests: &[PathBuf],
        all_source_paths: &[PathBuf],
        pdg: &ProgramDependenceGraph,
    ) -> Vec<PathBuf> {
        index_builder::files_importing_from_manifests(changed_manifests, all_source_paths, pdg)
    }

    /// Fast-path freshness check: O(1) for indexed files, O(D) for source
    /// directories (typically 10-20), and O(M) for manifest files.
    pub fn is_stale_fast(&self) -> bool {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::is_stale_fast(&ctx, || self.scan_project_files())
    }

    // ---- Accessors ----

    /// Get the project path.
    #[inline]
    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    /// Get the storage path used for index artifacts.
    #[inline]
    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }

    /// Get the project ID (legacy, for backward compatibility).
    #[inline]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Get the unique project identifier (BLAKE3-based, conflict-free).
    #[inline]
    pub fn unique_id(&self) -> &UniqueProjectId {
        &self.unique_id
    }

    /// Get the display name for the project.
    #[inline]
    pub fn display_name(&self) -> String {
        self.unique_id.display()
    }

    /// Get a reference to the search engine.
    #[inline]
    pub fn search_engine(&self) -> &SearchEngine {
        &self.search_engine
    }

    /// Get the PDG, if the project has been indexed.
    #[inline]
    pub fn pdg(&self) -> Option<&ProgramDependenceGraph> {
        self.pdg.as_ref()
    }

    /// Create a LogicValidator for this project's PDG and storage.
    ///
    /// Returns `None` if no PDG is available (project not yet indexed).
    /// The validator can be used to check edit changes for syntax errors,
    /// reference issues, semantic drift, and impact before applying.
    ///
    /// Opens a new Storage connection for the validator to avoid cloning
    /// the main connection (rusqlite::Connection is not Clone).
    pub fn create_validator(&self) -> Option<crate::validation::LogicValidator> {
        let pdg = self.pdg.as_ref()?;

        // Open a separate Storage connection for the validator.
        // Storage wraps rusqlite::Connection which is not Clone, so we
        // create a new read-only handle to the same database.
        let db_path = self.storage_path.join("leindex.db");
        let storage = crate::storage::schema::Storage::open(&db_path).ok()?;

        Some(crate::validation::LogicValidator::new(
            std::sync::Arc::new(pdg.clone()),
            // Storage wraps rusqlite::Connection which is not Sync;
            // Arc is required by the LogicValidator interface for shared ownership.
            #[allow(clippy::arc_with_non_send_sync)]
            std::sync::Arc::new(storage),
        ))
    }

    /// Ensure the PDG is loaded from storage (deferred load on first use).
    pub fn ensure_pdg_loaded(&mut self) -> Result<()> {
        if self.pdg.is_none() {
            let has_content =
                crate::storage::pdg_store::has_indexed_files(&self.storage, &self.project_id);
            if has_content {
                self.load_from_storage()?;
            }
        }
        Ok(())
    }

    /// Ensure the searchable context is ready for deep analysis / context tools.
    ///
    /// This loads the PDG if needed and performs a focused refresh when the
    /// in-memory search index is empty but indexed files already exist.
    pub fn ensure_analysis_context_loaded(&mut self) -> Result<()> {
        self.ensure_pdg_loaded()?;
        if self.search_engine.is_empty()
            && crate::storage::pdg_store::has_indexed_files(&self.storage, &self.project_id)
        {
            self.load_from_storage()?;
        }
        Ok(())
    }

    /// Get the current indexing statistics.
    #[inline]
    pub fn get_stats(&self) -> &IndexStats {
        &self.stats
    }

    /// Build file statistics cache from PDG
    pub(crate) fn build_file_stats_cache(&mut self) {
        if let Some(pdg) = &self.pdg {
            self.cache.build_file_stats_cache(pdg);
        }
    }

    /// Get file statistics cache.
    #[inline]
    pub fn file_stats(&self) -> Option<&HashMap<String, FileStats>> {
        self.cache.file_stats()
    }

    /// Get source file paths for the project (uses cached scan).
    pub fn source_file_paths(&mut self) -> Result<Vec<PathBuf>> {
        self.collect_source_file_paths(false)
    }

    /// Check if the project has been indexed.
    #[inline]
    pub fn is_indexed(&self) -> bool {
        self.search_engine.node_count() > 0
    }

    /// Close the LeIndex and ensure WAL is checkpointed.
    pub fn close(&mut self) -> Result<()> {
        self.storage.close().context("Failed to close storage")?;
        info!("Closed LeIndex for project: {}", self.project_id);
        Ok(())
    }

    // ---- Cache Spilling & Reloading ----

    /// Check memory and spill cache if threshold exceeded.
    #[inline]
    pub fn check_memory_and_spill(&mut self) -> Result<bool> {
        self.cache.check_memory_and_spill()
    }

    /// Spill PDG cache to disk.
    #[inline]
    pub fn spill_pdg_cache(&mut self) -> Result<()> {
        self.cache.spill_pdg_cache(&self.project_id, &mut self.pdg)
    }

    /// Spill vector search cache to disk.
    #[inline]
    pub fn spill_vector_cache(&mut self) -> Result<()> {
        self.cache
            .spill_vector_cache(&self.project_id, self.search_engine.node_count())
    }

    /// Spill all caches (PDG and vector) to disk.
    #[inline]
    pub fn spill_all_caches(&mut self) -> Result<(usize, usize)> {
        self.cache.spill_all_caches(
            &self.project_id,
            &mut self.pdg,
            self.search_engine.node_count(),
        )
    }

    /// Reload PDG from cache (load from storage if not in memory).
    pub fn reload_pdg_from_cache(&mut self) -> Result<()> {
        if self.pdg.is_some() {
            info!("PDG already in memory, no reload needed");
            return Ok(());
        }
        info!("PDG not in memory, attempting to load from lestockage");
        self.load_from_storage()
    }

    /// Reload vector index from PDG.
    pub fn reload_vector_from_pdg(&mut self) -> Result<usize> {
        let pdg = self
            .pdg
            .take()
            .ok_or_else(|| anyhow::anyhow!("No PDG available for vector rebuild"))?;

        let batch_size = self.indexing_batch_size();
        self.embedder = Some(index_builder::index_nodes(
            &pdg,
            &mut self.search_engine,
            &mut self.cache.file_stats_cache,
            batch_size,
        )?);
        let indexed_count = self.search_engine.node_count();

        self.pdg = Some(pdg);
        self.build_file_stats_cache();

        info!("Rebuilt vector index from PDG: {} nodes", indexed_count);
        Ok(indexed_count)
    }

    /// Warm caches with frequently accessed data
    pub fn warm_caches(
        &mut self,
        strategy: WarmStrategy,
    ) -> Result<crate::cli::memory::WarmResult> {
        let result = self.cache.warm_cache(strategy)?;

        if (strategy == crate::cli::memory::WarmStrategy::PDGOnly
            || strategy == crate::cli::memory::WarmStrategy::All
            || strategy == crate::cli::memory::WarmStrategy::RecentFirst)
            && self.pdg.is_none()
        {
            info!("PDG warming requested but not in memory, reloading from lestockage");
            self.load_from_storage()?;
        }

        Ok(result)
    }

    /// Get cache statistics.
    #[inline]
    pub fn get_cache_stats(&self) -> Result<crate::cli::memory::MemoryStats> {
        self.cache.get_cache_stats()
    }

    // ---- Index Stats Persistence ----

    /// Persist IndexStats to a JSON file in the storage directory so that
    /// diagnostics can report accurate totals after loading from storage.
    pub(crate) fn save_stats_to_storage(&self) -> Result<()> {
        let stats_path = self.storage_path.join("index_stats.json");
        let json = serde_json::to_string(&self.stats).context("Failed to serialize IndexStats")?;
        std::fs::write(&stats_path, json)
            .with_context(|| format!("Failed to write index stats to {:?}", stats_path))?;
        Ok(())
    }

    /// Load IndexStats from the JSON file in the storage directory.
    /// Returns silently if the file does not exist (first run or pre-feature).
    pub(crate) fn load_stats_from_storage(&mut self) -> Result<()> {
        let stats_path = self.storage_path.join("index_stats.json");
        if !stats_path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&stats_path)
            .with_context(|| format!("Failed to read index stats from {:?}", stats_path))?;
        let stored: IndexStats =
            serde_json::from_str(&json).context("Failed to deserialize IndexStats")?;
        self.stats = stored;
        Ok(())
    }
}
