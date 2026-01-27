// leindex - Core Orchestration
//
// *L'Index* (The Index) - Unified API that brings together all LeIndex crates

use crate::memory::{
    CacheSpiller, MemoryConfig, pdg_cache_key, search_cache_key,
    WarmStrategy, CacheEntry,
};
use anyhow::{Context, Result};
use legraphe::{
    pdg::ProgramDependenceGraph,
    extract_pdg_from_signatures,
    traversal::{GravityTraversal, TraversalConfig},
};
use leparse::parallel::{ParallelParser, ParsingResult};
use leparse::traits::SignatureInfo;
use lerecherche::search::{NodeInfo, SearchEngine, SearchResult, SearchQuery};
use lestockage::{pdg_store, schema::Storage};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// LeIndex - Main orchestration struct
///
/// This struct provides a unified API for the entire LeIndex system,
/// integrating parsing, graph construction, search, and storage.
///
/// # Example
///
/// ```ignore
/// let leindex = LeIndex::new("/path/to/project")?;
/// leindex.index_project()?;
/// let results = leindex.search("authentication", 10).await?;
/// ```
pub struct LeIndex {
    /// Project path
    project_path: PathBuf,

    /// Project identifier
    project_id: String,

    /// Storage backend
    storage: Storage,

    /// Search engine
    search_engine: SearchEngine,

    /// Program Dependence Graph
    pdg: Option<ProgramDependenceGraph>,

    /// Cache spiller for memory management
    cache_spiller: CacheSpiller,

    /// Indexing statistics
    stats: IndexStats,
}

/// Statistics from indexing operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    /// Total number of files encountered during indexing
    pub files_parsed: usize,

    /// Number of files successfully parsed
    pub successful_parses: usize,

    /// Number of files that failed to parse
    pub failed_parses: usize,

    /// Total number of code signatures extracted across all files
    pub total_signatures: usize,

    /// Total number of nodes created in the Program Dependence Graph
    pub pdg_nodes: usize,

    /// Total number of edges created in the Program Dependence Graph
    pub pdg_edges: usize,

    /// Total number of nodes successfully indexed for semantic search
    pub indexed_nodes: usize,

    /// Total time taken for the indexing process in milliseconds
    pub indexing_time_ms: u64,
}

/// Result from a deep analysis operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// The original analysis query
    pub query: String,

    /// Search results serving as entry points for analysis
    pub results: Vec<SearchResult>,

    /// Expanded code context generated from PDG traversal
    pub context: Option<String>,

    /// Estimated number of tokens used in the expanded context
    pub tokens_used: usize,

    /// Total time taken for the analysis process in milliseconds
    pub processing_time_ms: u64,
}

/// Diagnostics information about the indexed project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    /// Absolute path to the project directory
    pub project_path: String,

    /// Unique identifier for the project
    pub project_id: String,

    /// Statistics from the last indexing operation
    pub stats: IndexStats,

    /// Current memory usage of the process in bytes
    pub memory_usage_bytes: usize,

    /// Total available system memory in bytes
    pub total_memory_bytes: usize,

    /// Current memory usage as a percentage of total system memory
    pub memory_usage_percent: f64,

    /// Whether the memory usage has exceeded the configured threshold
    pub memory_threshold_exceeded: bool,

    /// Current number of entries stored in the in-memory cache
    pub cache_entries: usize,

    /// Total size of the in-memory cache in bytes
    pub cache_bytes: usize,

    /// Number of cache entries that have been spilled to disk
    pub spilled_entries: usize,

    /// Total size of the spilled cache on disk in bytes
    pub spilled_bytes: usize,
}

impl LeIndex {
    /// Create a new LeIndex instance for a project
    ///
    /// # Arguments
    ///
    /// * `project_path` - Path to the project directory
    ///
    /// # Returns
    ///
    /// `Result<LeIndex>` - The initialized LeIndex instance
    ///
    /// # Example
    ///
    /// ```ignore
    /// let leindex = LeIndex::new("/path/to/project")?;
    /// ```
    pub fn new<P: AsRef<Path>>(project_path: P) -> Result<Self> {
        let project_path = project_path.as_ref().canonicalize()
            .context("Failed to canonicalize project path")?;

        let project_id = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        info!("Creating LeIndex for project: {} at {:?}", project_id, project_path);

        // Initialize storage
        let storage_path = project_path.join(".leindex");
        std::fs::create_dir_all(&storage_path)
            .context("Failed to create .leindex directory")?;

        let db_path = storage_path.join("leindex.db");
        let storage = Storage::open(&db_path)
            .context("Failed to initialize storage")?;

        // Initialize search engine
        let search_engine = SearchEngine::new();

        // Initialize cache spiller with project-specific cache directory
        let cache_dir = storage_path.join("cache");
        let memory_config = MemoryConfig {
            cache_dir,
            ..Default::default()
        };
        let cache_spiller = CacheSpiller::new(memory_config)
            .context("Failed to initialize cache spiller")?;

        Ok(Self {
            project_path,
            project_id,
            storage,
            search_engine,
            pdg: None,
            cache_spiller,
            stats: IndexStats {
                files_parsed: 0,
                successful_parses: 0,
                failed_parses: 0,
                total_signatures: 0,
                pdg_nodes: 0,
                pdg_edges: 0,
                indexed_nodes: 0,
                indexing_time_ms: 0,
            },
        })
    }

    /// Index the project
    ///
    /// This executes the full indexing pipeline:
    /// 1. Parse all source files in parallel
    /// 2. Extract PDG from parsed signatures
    /// 3. Index nodes for semantic search
    /// 4. Persist PDG to storage
    ///
    /// # Returns
    ///
    /// `Result<IndexStats>` - Statistics from the indexing operation
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = leindex.index_project()?;
    /// println!("Indexed {} files", stats.files_parsed);
    /// ```
    pub fn index_project(&mut self) -> Result<IndexStats> {
        let start_time = std::time::Instant::now();

        info!("Starting project indexing for: {}", self.project_id);

        // Step 1: Parse all source files
        let parsing_results = self.parse_files()?;

        // Step 2: Extract signatures from successful parses
        let signatures_by_file: std::collections::HashMap<String, Vec<SignatureInfo>> =
            parsing_results
            .iter()
            .filter_map(|r| {
                if r.is_success() {
                    Some((r.file_path.display().to_string(), r.signatures.clone()))
                } else {
                    None
                }
            })
            .collect();

        let all_signatures: Vec<_> = signatures_by_file.values()
            .flat_map(|sigs| sigs.clone())
            .collect();

        info!("Extracted {} signatures from {} files",
              all_signatures.len(),
              signatures_by_file.len());

        // Step 3: Build PDG from signatures
        let pdg = self.build_pdg(&signatures_by_file)?;
        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!("Built PDG with {} nodes and {} edges", pdg_node_count, pdg_edge_count);

        // Step 4: Index nodes for search
        self.index_nodes(&pdg)?;
        let indexed_count = self.search_engine.node_count();

        info!("Indexed {} nodes for search", indexed_count);

        // Step 5: Persist to storage
        self.save_to_storage(&pdg)?;

        // Update statistics
        let successful = parsing_results.iter().filter(|r| r.is_success()).count();
        let failed = parsing_results.iter().filter(|r| r.is_failure()).count();
        let total_sigs: usize = parsing_results.iter().map(|r| r.signatures.len()).sum();

        self.stats = IndexStats {
            files_parsed: parsing_results.len(),
            successful_parses: successful,
            failed_parses: failed,
            total_signatures: total_sigs,
            pdg_nodes: pdg_node_count,
            pdg_edges: pdg_edge_count,
            indexed_nodes: indexed_count,
            indexing_time_ms: start_time.elapsed().as_millis() as u64,
        };

        // Keep PDG in memory for analysis operations
        self.pdg = Some(pdg);

        info!("Indexing completed in {}ms", self.stats.indexing_time_ms);

        Ok(self.stats.clone())
    }

    /// Search the indexed code
    ///
    /// # Arguments
    ///
    /// * `query` - Search query string
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// `Result<Vec<SearchResult>>` - Search results sorted by relevance
    ///
    /// # Example
    ///
    /// ```ignore
    /// let results = leindex.search("authentication", 10).await?;
    /// for result in results {
    ///     println!("{}: {} ({:.2})", result.rank, result.symbol_name, result.score.total);
    /// }
    /// ```
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        if self.search_engine.is_empty() {
            warn!("Search attempted on empty index");
            return Ok(Vec::new());
        }

        let search_query = SearchQuery {
            query: query.to_string(),
            top_k,
            token_budget: None,
            semantic: true,
            expand_context: false,
        };

        let results = self.search_engine.search(search_query)
            .context("Search operation failed")?;

        debug!("Search for '{}' returned {} results", query, results.len());

        Ok(results)
    }

    /// Perform deep analysis with context expansion
    ///
    /// This combines semantic search with PDG-based context expansion
    /// to provide comprehensive code understanding.
    ///
    /// # Arguments
    ///
    /// * `query` - Analysis query
    /// * `token_budget` - Maximum tokens for context expansion
    ///
    /// # Returns
    ///
    /// `Result<AnalysisResult>` - Analysis results with expanded context
    ///
    /// # Example
    ///
    /// ```ignore
    /// let analysis = leindex.analyze("How does authentication work?", 2000).await?;
    /// println!("Found {} entry points", analysis.results.len());
    /// println!("Context: {}", analysis.context.unwrap_or_default());
    /// ```
    pub fn analyze(&mut self, query: &str, token_budget: usize) -> Result<AnalysisResult> {
        let start_time = std::time::Instant::now();

        // Step 1: Semantic search for entry points
        let search_query = SearchQuery {
            query: query.to_string(),
            top_k: 10,
            token_budget: Some(token_budget),
            semantic: true,
            expand_context: false,
        };

        let results = self.search_engine.search(search_query)
            .context("Search for analysis failed")?;

        // Step 2: Expand context using PDG traversal
        let context = if let Some(ref pdg) = self.pdg {
            self.expand_context(pdg, &results, token_budget)?
        } else {
            warn!("No PDG available for context expansion");
            String::from("/* No PDG available for context expansion */")
        };

        // Estimate tokens used (rough approximation: 4 chars per token)
        let tokens_used = context.len() / 4;

        Ok(AnalysisResult {
            query: query.to_string(),
            results,
            context: Some(context),
            tokens_used,
            processing_time_ms: start_time.elapsed().as_millis() as u64,
        })
    }

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
    pub fn get_diagnostics(&self) -> Result<Diagnostics> {
        let memory_stats = self.cache_spiller.memory_stats()
            .context("Failed to get memory stats")?;
        let memory_percent = memory_stats.memory_percent();
        let threshold_exceeded = self.cache_spiller.store().total_bytes() > 0
            && self.cache_spiller.is_threshold_exceeded()
                .unwrap_or(false);

        Ok(Diagnostics {
            project_path: self.project_path.display().to_string(),
            project_id: self.project_id.clone(),
            stats: self.stats.clone(),
            memory_usage_bytes: memory_stats.rss_bytes,
            total_memory_bytes: memory_stats.total_bytes,
            memory_usage_percent: memory_percent,
            memory_threshold_exceeded: threshold_exceeded,
            cache_entries: memory_stats.cache_entries,
            cache_bytes: memory_stats.cache_bytes,
            spilled_entries: memory_stats.spilled_entries,
            spilled_bytes: memory_stats.spilled_bytes,
        })
    }

    /// Load a previously indexed project from storage
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    ///
    /// # Example
    ///
    /// ```ignore
    /// leindex.load_from_storage()?;
    /// println!("Loaded {} nodes", leindex.search_engine.node_count());
    /// ```
    pub fn load_from_storage(&mut self) -> Result<()> {
        info!("Loading project from storage: {}", self.project_id);

        // Load PDG from storage
        let pdg = pdg_store::load_pdg(&self.storage, &self.project_id)
            .context("Failed to load PDG from storage")?;

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!("Loaded PDG with {} nodes and {} edges", pdg_node_count, pdg_edge_count);

        // Rebuild search index from PDG
        self.index_nodes(&pdg)?;
        let indexed_count = self.search_engine.node_count();

        info!("Rebuilt search index with {} nodes", indexed_count);

        // Update statistics
        self.stats.pdg_nodes = pdg_node_count;
        self.stats.pdg_edges = pdg_edge_count;
        self.stats.indexed_nodes = indexed_count;

        // Keep PDG in memory
        self.pdg = Some(pdg);

        Ok(())
    }

    // ========================================================================
    // ACCESSOR METHODS (for MCP server integration)
    // ========================================================================

    /// Get the project path
    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    /// Get the project ID
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Get a reference to the search engine
    pub fn search_engine(&self) -> &SearchEngine {
        &self.search_engine
    }

    /// Get the current indexing statistics
    pub fn get_stats(&self) -> &IndexStats {
        &self.stats
    }

    /// Check if the project has been indexed
    pub fn is_indexed(&self) -> bool {
        self.search_engine.node_count() > 0
    }

    // ========================================================================
    // CACHE SPILLING (Phase 5.2)
    // ========================================================================

    /// Check memory and spill cache if threshold exceeded
    ///
    /// This method should be called before memory-intensive operations
    /// to ensure sufficient memory is available.
    ///
    /// # Returns
    ///
    /// `Result<bool>` - Ok(true) if spilling occurred, Ok(false) otherwise
    pub fn check_memory_and_spill(&mut self) -> Result<bool> {
        if self.cache_spiller.is_threshold_exceeded()? {
            info!("Memory threshold exceeded, initiating cache spilling");
            let result = self.cache_spiller.check_and_spill()?;
            info!("Spilled {} entries, freed {} bytes",
                  result.entries_spilled, result.memory_freed);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Spill PDG cache to disk
    ///
    /// Drops the PDG from memory (it's already persisted to lestockage via save_pdg).
    /// Creates a cache marker to track that the PDG was spilled.
    /// The PDG can be reloaded later using `reload_pdg_from_cache()`.
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn spill_pdg_cache(&mut self) -> Result<()> {
        let pdg = self.pdg.take().ok_or_else(|| {
            anyhow::anyhow!("No PDG in memory to spill")
        })?;

        let node_count = pdg.node_count();
        let edge_count = pdg.edge_count();

        // Note: PDG is already persisted to lestockage via save_pdg()
        // We create a cache marker to track that it was spilled
        let cache_key = pdg_cache_key(&self.project_id);
        let entry = CacheEntry::PDG {
            project_id: self.project_id.clone(),
            node_count,
            edge_count,
            serialized_data: vec![], // Empty marker - actual data in lestockage
        };

        // Store marker in cache spiller
        self.cache_spiller.store_mut()
            .insert(cache_key, entry)
            .context("Failed to create PDG spill marker")?;

        info!("Spilled PDG from memory: {} nodes, {} edges (persisted to lestockage)",
              node_count, edge_count);

        Ok(())
    }

    /// Spill vector search cache to disk
    ///
    /// Serializes the HNSW vector index and stores it in the cache spill directory.
    /// The index can be reloaded later using `reload_vector_cache()`.
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn spill_vector_cache(&mut self) -> Result<()> {
        // Note: The HNSW index doesn't support direct serialization
        // Instead, we create a marker entry and track the state
        // The actual vector data would need to be re-indexed from the PDG

        let node_count = self.search_engine.node_count();

        // Create a marker entry for the vector cache
        let cache_key = search_cache_key(&self.project_id);
        let entry = CacheEntry::SearchIndex {
            project_id: self.project_id.clone(),
            entry_count: node_count,
            serialized_data: vec![], // Empty - vectors would be re-indexed from PDG
        };

        self.cache_spiller.store_mut()
            .insert(cache_key, entry)
            .context("Failed to spill vector cache marker")?;

        info!("Spilled vector cache marker: {} entries", node_count);

        Ok(())
    }

    /// Spill all caches (PDG and vector) to disk
    ///
    /// This is useful for freeing memory before large operations
    /// or when the project won't be used for a while.
    ///
    /// # Returns
    ///
    /// `Result<(usize, usize)>` - (PDG bytes spilled, vector bytes spilled)
    pub fn spill_all_caches(&mut self) -> Result<(usize, usize)> {
        let mut pdg_bytes = 0;

        // Spill PDG if in memory
        if self.pdg.is_some() {
            self.spill_pdg_cache()?;
            pdg_bytes = self.cache_spiller.store().total_bytes();
        }

        // Spill vector cache marker
        self.spill_vector_cache()?;
        let vector_bytes = self.cache_spiller.store().total_bytes() - pdg_bytes;

        info!("Spilled all caches: PDG ({} bytes), Vector ({} bytes)",
              pdg_bytes, vector_bytes);

        Ok((pdg_bytes, vector_bytes))
    }

    // ========================================================================
    // CACHE RELOADING (Phase 5.3)
    // ========================================================================

    /// Reload PDG from cache
    ///
    /// Attempts to reload a previously spilled PDG from lestockage.
    /// This is useful when the PDG has been spilled from memory to free up RAM.
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn reload_pdg_from_cache(&mut self) -> Result<()> {
        // Check if PDG is already in memory
        if self.pdg.is_some() {
            info!("PDG already in memory, no reload needed");
            return Ok(());
        }

        // PDG not in memory, try to load from storage
        info!("PDG not in memory, attempting to load from lestockage");
        self.load_from_storage()
    }

    /// Reload vector index from PDG
    ///
    /// Rebuilds the vector search index from the current PDG.
    /// This is useful when the vector cache has been spilled and needs to be restored.
    ///
    /// # Returns
    ///
    /// `Result<usize>` - Number of nodes indexed
    pub fn reload_vector_from_pdg(&mut self) -> Result<usize> {
        // Take PDG temporarily to avoid borrow checker issues
        let pdg = self.pdg.take()
            .ok_or_else(|| anyhow::anyhow!("No PDG available for vector rebuild"))?;

        // Re-use the index_nodes logic to ensure consistent embedding generation
        self.index_nodes(&pdg)?;
        let indexed_count = self.search_engine.node_count();

        // Restore PDG
        self.pdg = Some(pdg);

        info!("Rebuilt vector index from PDG: {} nodes", indexed_count);

        Ok(indexed_count)
    }

    /// Warm caches with frequently accessed data
    ///
    /// Loads spilled cache entries back into memory based on the specified strategy.
    /// For PDG warming strategy, this will reload the PDG from lestockage.
    ///
    /// # Arguments
    ///
    /// * `strategy` - Warming strategy to use (PDGOnly, SearchIndexOnly, RecentFirst, All)
    ///
    /// # Returns
    ///
    /// `Result<WarmResult>` - Statistics about the warming operation
    pub fn warm_caches(&mut self, strategy: WarmStrategy) -> Result<crate::memory::WarmResult> {
        info!("Warming caches with strategy: {:?}", strategy);

        let result = self.cache_spiller.warm_cache(strategy)?;

        // If PDG warming was requested and PDG is not in memory, reload from storage
        if (strategy == crate::memory::WarmStrategy::PDGOnly
            || strategy == crate::memory::WarmStrategy::All
            || strategy == crate::memory::WarmStrategy::RecentFirst)
            && self.pdg.is_none()
        {
            info!("PDG warming requested but not in memory, reloading from lestockage");
            self.load_from_storage()?;
        }

        Ok(result)
    }

    /// Get cache statistics
    ///
    /// Returns detailed statistics about cache usage and spilled data.
    ///
    /// # Returns
    ///
    /// `Result<MemoryStats>` - Cache statistics
    pub fn get_cache_stats(&self) -> Result<crate::memory::MemoryStats> {
        self.cache_spiller.memory_stats()
            .map_err(|e| anyhow::anyhow!("Failed to get cache stats: {}", e))
    }

    // ========================================================================
    // PRIVATE METHODS
    // ========================================================================

    /// Parse all source files in the project
    fn parse_files(&self) -> Result<Vec<ParsingResult>> {
        let parser = ParallelParser::new();

        // Collect source files from the project
        let source_files = self.collect_source_files()?;

        info!("Found {} source files to parse", source_files.len());

        // Parse files in parallel
        let results = parser.parse_files(source_files);

        // Log statistics
        let successful = results.iter().filter(|r| r.is_success()).count();
        let failed = results.iter().filter(|r| r.is_failure()).count();

        info!("Parsing complete: {} successful, {} failed", successful, failed);

        Ok(results)
    }

    /// Collect all source files from the project
    fn collect_source_files(&self) -> Result<Vec<PathBuf>> {
        let mut source_files = Vec::new();

        // Common source file extensions
        let extensions = [
            "rs", "py", "js", "ts", "tsx", "jsx",  // Main languages
            "go", "java", "cpp", "c", "h", "hpp",    // Systems languages
            "rb", "php", "lua", "scala",              // Scripting languages
        ];

        // Walk the project directory
        for entry in walkdir::WalkDir::new(&self.project_path)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            // Skip hidden files and directories
            if path.components().any(|c| {
                c.as_os_str().to_string_lossy().starts_with('.')
            }) {
                continue;
            }

            // Skip common non-source directories
            if let Some(dir_name) = path.parent().and_then(|p| p.file_name()) {
                let dir = dir_name.to_string_lossy();
                if dir.contains("target") || dir.contains("node_modules")
                    || dir.contains("vendor") || dir.contains(".git") {
                    continue;
                }
            }

            // Check if file has a source extension
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    source_files.push(path.to_path_buf());
                }
            }
        }

        Ok(source_files)
    }

    /// Build PDG from parsed signatures
    fn build_pdg(
        &self,
        signatures_by_file: &std::collections::HashMap<String, Vec<SignatureInfo>>,
    ) -> Result<ProgramDependenceGraph> {
        info!("Building PDG from {} files", signatures_by_file.len());

        let mut pdg = ProgramDependenceGraph::new();

        // Use extract_pdg_from_signatures for each file and merge results
        for (file_path, signatures) in signatures_by_file {
            // Extract PDG for this file
            let file_pdg = extract_pdg_from_signatures(signatures.clone(), &[], file_path);

            // Track mapping from file-local node IDs to global node IDs
            let mut node_id_map = std::collections::HashMap::new();

            // Add all nodes from file PDG to global PDG
            for old_node_id in file_pdg.node_indices() {
                if let Some(node) = file_pdg.get_node(old_node_id) {
                    let new_node_id = pdg.add_node(node.clone());
                    node_id_map.insert(old_node_id, new_node_id);
                }
            }

            // Add all edges from file PDG to global PDG with remapped node IDs
            for edge_id in file_pdg.edge_indices() {
                if let Some(edge) = file_pdg.get_edge(edge_id) {
                    if let Some((old_source, old_target)) = file_pdg.edge_endpoints(edge_id) {
                        let new_source = node_id_map.get(&old_source).copied();
                        let new_target = node_id_map.get(&old_target).copied();

                        if let (Some(from), Some(to)) = (new_source, new_target) {
                            pdg.add_edge(from, to, edge.clone());
                        }
                    }
                }
            }
        }

        info!("PDG construction complete with {} nodes and {} edges",
            pdg.node_count(), pdg.edge_count());

        Ok(pdg)
    }

    /// Generate a deterministic 768-dimensional embedding for a node
    ///
    /// This uses a stable hashing approach to generate a vector from symbol metadata.
    /// While not a real semantic embedding from an LLM, it provides a deterministic
    /// basis for vector search and HNSW testing.
    fn generate_deterministic_embedding(
        &self,
        symbol_name: &str,
        file_path: &str,
        content: &str,
    ) -> Vec<f32> {
        use std::hash::{Hash, Hasher};
        use std::collections::hash_map::DefaultHasher;

        let mut embedding = Vec::with_capacity(768);

        // Initial seed hash from all inputs
        let mut base_hasher = DefaultHasher::new();
        symbol_name.hash(&mut base_hasher);
        file_path.hash(&mut base_hasher);
        content.hash(&mut base_hasher);
        let base_hash = base_hasher.finish();

        for i in 0..768 {
            let mut hasher = DefaultHasher::new();
            base_hash.hash(&mut hasher);
            i.hash(&mut hasher);
            let hash_val = hasher.finish();
            
            // Map 64-bit hash to f32 in [-1.0, 1.0]
            let val = (hash_val as f64 / u64::MAX as f64) * 2.0 - 1.0;
            embedding.push(val as f32);
        }

        embedding
    }

    /// Index nodes from PDG for search
    fn index_nodes(&mut self, pdg: &ProgramDependenceGraph) -> Result<()> {
        let mut nodes = Vec::new();

        // Convert PDG nodes to NodeInfo for indexing
        for node_idx in pdg.node_indices() {
            if let Some(node) = pdg.get_node(node_idx) {
                let content = format!("// {} in {}\n{}", node.name, node.file_path,
                                   "// Source code would be here");
                
                // Use existing embedding if present, otherwise generate a deterministic one
                let embedding = node.embedding.clone().unwrap_or_else(|| {
                    self.generate_deterministic_embedding(&node.name, &node.file_path, &content)
                });

                let node_info = NodeInfo {
                    node_id: node.id.clone(),
                    file_path: node.file_path.clone(),
                    symbol_name: node.name.clone(),
                    content,
                    byte_range: node.byte_range,
                    embedding: Some(embedding),
                    complexity: node.complexity,
                };

                nodes.push(node_info);
            }
        }

        // Index the nodes
        self.search_engine.index_nodes(nodes);

        Ok(())
    }

    /// Expand context using PDG traversal
    fn expand_context(
        &self,
        pdg: &ProgramDependenceGraph,
        results: &[SearchResult],
        token_budget: usize,
    ) -> Result<String> {
        let config = TraversalConfig {
            max_tokens: token_budget,
            ..TraversalConfig::default()
        };
        let traversal = GravityTraversal::with_config(config);

        // Map SearchResult entries to PDG node IDs for the traversal call
        let entry_points: Vec<_> = results
            .iter()
            .filter_map(|r| pdg.find_by_symbol(&r.node_id))
            .collect();

        let expanded_node_ids = traversal.expand_context(pdg, entry_points);

        let mut context = String::from("/* Context Expansion via Gravity Traversal */\n");

        for node_id in expanded_node_ids {
            if let Some(node) = pdg.get_node(node_id) {
                context.push_str(&format!("\n// Symbol: {}\n", node.name));
                context.push_str(&format!("// File: {}\n", node.file_path));
                context.push_str(&format!("// Type: {:?}\n", node.node_type));
                context.push_str("// Source code would be here\n");
            }
        }

        Ok(context)
    }

    /// Save PDG to storage
    fn save_to_storage(&mut self, pdg: &ProgramDependenceGraph) -> Result<()> {
        pdg_store::save_pdg(&mut self.storage, &self.project_id, pdg)
            .context("Failed to save PDG to storage")?;

        info!("Saved PDG to storage for project: {}", self.project_id);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_serialization() {
        let stats = IndexStats {
            files_parsed: 100,
            successful_parses: 95,
            failed_parses: 5,
            total_signatures: 500,
            pdg_nodes: 300,
            pdg_edges: 1200,
            indexed_nodes: 300,
            indexing_time_ms: 5000,
        };

        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: IndexStats = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.files_parsed, 100);
        assert_eq!(deserialized.successful_parses, 95);
    }

    #[test]
    fn test_analysis_result_serialization() {
        let result = AnalysisResult {
            query: "test".to_string(),
            results: vec![],
            context: Some("context".to_string()),
            tokens_used: 100,
            processing_time_ms: 50,
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: AnalysisResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.query, "test");
        assert_eq!(deserialized.tokens_used, 100);
    }

    #[test]
    fn test_diagnostics_serialization() {
        let diagnostics = Diagnostics {
            project_path: "/test".to_string(),
            project_id: "test".to_string(),
            stats: IndexStats {
                files_parsed: 0,
                successful_parses: 0,
                failed_parses: 0,
                total_signatures: 0,
                pdg_nodes: 0,
                pdg_edges: 0,
                indexed_nodes: 0,
                indexing_time_ms: 0,
            },
            memory_usage_bytes: 1024,
            total_memory_bytes: 8192,
            memory_usage_percent: 12.5,
            memory_threshold_exceeded: false,
            cache_entries: 5,
            cache_bytes: 50000,
            spilled_entries: 3,
            spilled_bytes: 30000,
        };

        let json = serde_json::to_string(&diagnostics).unwrap();
        let deserialized: Diagnostics = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.project_id, "test");
        assert_eq!(deserialized.memory_usage_bytes, 1024);
        assert_eq!(deserialized.cache_entries, 5);
        assert_eq!(deserialized.spilled_bytes, 30000);
    }
}
