// leindex - Core Orchestration
//
// *L'Index* (The Index) - Unified API that brings together all LeIndex crates

use crate::memory::MemoryManager;
use anyhow::{Context, Result};
use legraphe::{
    pdg::{Node, NodeType, ProgramDependenceGraph},
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

    /// Memory manager
    memory_manager: MemoryManager,

    /// Indexing statistics
    stats: IndexStats,
}

/// Statistics from indexing operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    /// Number of files parsed
    pub files_parsed: usize,

    /// Number of successful parses
    pub successful_parses: usize,

    /// Number of failed parses
    pub failed_parses: usize,

    /// Total signatures extracted
    pub total_signatures: usize,

    /// Total nodes in PDG
    pub pdg_nodes: usize,

    /// Total edges in PDG
    pub pdg_edges: usize,

    /// Total nodes indexed for search
    pub indexed_nodes: usize,

    /// Indexing time in milliseconds
    pub indexing_time_ms: u64,
}

/// Result from a deep analysis operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// Original query
    pub query: String,

    /// Search results
    pub results: Vec<SearchResult>,

    /// Expanded context (if requested)
    pub context: Option<String>,

    /// Tokens used
    pub tokens_used: usize,

    /// Processing time in milliseconds
    pub processing_time_ms: u64,
}

/// Diagnostics information about the indexed project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    /// Project path
    pub project_path: String,

    /// Project ID
    pub project_id: String,

    /// Index statistics
    pub stats: IndexStats,

    /// Memory usage in bytes
    pub memory_usage_bytes: usize,

    /// Total system memory in bytes
    pub total_memory_bytes: usize,

    /// Memory usage percentage
    pub memory_usage_percent: f64,

    /// Whether memory threshold is exceeded
    pub memory_threshold_exceeded: bool,
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

        // Initialize memory manager
        let memory_manager = MemoryManager::default();

        Ok(Self {
            project_path,
            project_id,
            storage,
            search_engine,
            pdg: None,
            memory_manager,
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
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
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

        let results = self.search_engine.search(search_query).await
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
    pub async fn analyze(&mut self, query: &str, token_budget: usize) -> Result<AnalysisResult> {
        let start_time = std::time::Instant::now();

        // Step 1: Semantic search for entry points
        let search_query = SearchQuery {
            query: query.to_string(),
            top_k: 10,
            token_budget: Some(token_budget),
            semantic: true,
            expand_context: false,
        };

        let results = self.search_engine.search(search_query).await
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
        let memory_usage = self.memory_manager.get_rss_bytes()
            .context("Failed to get memory usage")?;
        let total_memory = self.memory_manager.get_total_memory()
            .context("Failed to get total memory")?;
        let memory_percent = (memory_usage as f64 / total_memory as f64) * 100.0;
        let threshold_exceeded = self.memory_manager.is_threshold_exceeded()
            .unwrap_or(false);

        Ok(Diagnostics {
            project_path: self.project_path.display().to_string(),
            project_id: self.project_id.clone(),
            stats: self.stats.clone(),
            memory_usage_bytes: memory_usage,
            total_memory_bytes: total_memory,
            memory_usage_percent: memory_percent,
            memory_threshold_exceeded: threshold_exceeded,
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
        let pdg = pdg_store::load_pdg(&mut self.storage, &self.project_id)
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

        // Add nodes for each signature
        for (file_path, signatures) in signatures_by_file {
            for sig in signatures {
                let node_type = match sig.visibility {
                    leparse::traits::Visibility::Public => NodeType::Function,
                    leparse::traits::Visibility::Private => NodeType::Function,
                    leparse::traits::Visibility::Protected => NodeType::Function,
                    leparse::traits::Visibility::Internal => NodeType::Function,
                    leparse::traits::Visibility::Package => NodeType::Function,
                };

                let node = Node {
                    id: format!("{}:{}:{}", file_path, sig.name, sig.parameters.len()),
                    node_type,
                    name: sig.name.clone(),
                    file_path: file_path.clone(),
                    byte_range: (0, 100), // Placeholder - would need actual byte range
                    complexity: 10, // Placeholder - would calculate from node
                    embedding: None, // Placeholder - would be computed later
                };

                // Add node to PDG
                pdg.add_node(node);
            }
        }

        info!("PDG construction complete");

        Ok(pdg)
    }

    /// Index nodes from PDG for search
    fn index_nodes(&mut self, pdg: &ProgramDependenceGraph) -> Result<()> {
        let mut nodes = Vec::new();

        // Convert PDG nodes to NodeInfo for indexing
        for node_idx in pdg.node_indices() {
            if let Some(node) = pdg.get_node(node_idx) {
                let node_info = NodeInfo {
                    node_id: node.id.clone(),
                    file_path: node.file_path.clone(),
                    symbol_name: node.name.clone(),
                    content: format!("// {} in {}\n{}", node.name, node.file_path,
                                   "// Source code would be here"),
                    byte_range: node.byte_range,
                    embedding: node.embedding.clone(),
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
        let mut context = String::from("/* Context Expansion */\n");

        let mut config = TraversalConfig::default();
        config.max_tokens = token_budget;

        let traversal = GravityTraversal::with_config(config.clone());

        for result in results.iter().take(5) {
            context.push_str(&format!("\n// Entry Point: {}\n", result.symbol_name));
            context.push_str(&format!("// File: {}\n", result.file_path));
            context.push_str(&format!("// Score: {:.2}\n", result.score.overall));

            // Note: Actual context expansion would use GravityTraversal::expand_context
            // This requires NodeId values which would need to be mapped from SearchResult
            context.push_str("// Context expansion would happen here\n");
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
        };

        let json = serde_json::to_string(&diagnostics).unwrap();
        let deserialized: Diagnostics = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.project_id, "test");
        assert_eq!(deserialized.memory_usage_bytes, 1024);
    }
}
