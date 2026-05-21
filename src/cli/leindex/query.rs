// Search, analysis, and context expansion methods for LeIndex.

use super::LeIndex;
use crate::cli::index_builder;
use crate::cli::memory::CacheEntry;
use crate::graph::{
    pdg::ProgramDependenceGraph,
    traversal::{GravityTraversal, TraversalConfig},
};
use crate::search::search::{SearchQuery, SearchResult};
use anyhow::{Context, Result};
use tracing::{debug, warn};

impl LeIndex {
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
    pub fn search(
        &mut self,
        query: &str,
        top_k: usize,
        query_type: Option<crate::search::ranking::QueryType>,
    ) -> Result<Vec<SearchResult>> {
        if self.search_engine.is_empty() {
            warn!("Search attempted on empty index");
            return Ok(Vec::new());
        }

        let search_cache_key = self.search_cache_key_for(query, top_k, query_type.as_ref());
        if let Some(CacheEntry::Binary {
            serialized_data, ..
        }) = self
            .cache
            .cache_spiller
            .store_mut()
            .get_or_load(&search_cache_key)?
        {
            if let Ok(cached_results) = bincode::deserialize::<Vec<SearchResult>>(&serialized_data)
            {
                debug!(
                    "Search cache hit for '{}' ({} results)",
                    query,
                    cached_results.len()
                );
                return Ok(cached_results);
            }
        }

        let search_query = SearchQuery {
            query: query.to_string(),
            top_k,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(self.generate_query_embedding(query)),
            threshold: Some(0.1), // Added default threshold for better quality
            query_type,
        };

        let mut results = self
            .search_engine
            .search(search_query)
            .context("Search operation failed")?;

        // Enrich results with PDG metadata: symbol_type, caller_count, dependency_count.
        // These require the in-memory PDG which is available here but not in lerecherche.
        if let Some(pdg) = &self.pdg {
            for result in &mut results {
                // Look up the PDG node by its string ID
                if let Some(node_idx) = pdg.find_by_id(&result.node_id) {
                    if let Some(node) = pdg.get_node(node_idx) {
                        result.symbol_type = Some(match node.node_type {
                            crate::graph::pdg::NodeType::Function => "function".to_string(),
                            crate::graph::pdg::NodeType::Class => "class".to_string(),
                            crate::graph::pdg::NodeType::Method => "method".to_string(),
                            crate::graph::pdg::NodeType::Variable => "variable".to_string(),
                            crate::graph::pdg::NodeType::Module => "module".to_string(),
                            crate::graph::pdg::NodeType::External => "external".to_string(),
                        });
                    }
                    result.caller_count = Some(pdg.predecessor_count(node_idx));
                    result.dependency_count = Some(pdg.neighbors(node_idx).len());
                }
            }
        }

        debug!("Search for '{}' returned {} results", query, results.len());

        if let Ok(serialized) = bincode::serialize(&results) {
            let entry = CacheEntry::Binary {
                metadata: std::collections::HashMap::from([
                    ("type".to_string(), "search_results".to_string()),
                    ("query".to_string(), query.to_string()),
                ]),
                serialized_data: serialized,
            };
            if self
                .cache
                .cache_spiller
                .store_mut()
                .insert(search_cache_key.clone(), entry)
                .is_ok()
            {
                let _ = self
                    .cache
                    .cache_spiller
                    .store_mut()
                    .persist_key(&search_cache_key);
            }
        }

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
    pub fn analyze(&mut self, query: &str, token_budget: usize) -> Result<super::AnalysisResult> {
        let start_time = std::time::Instant::now();

        let analysis_cache_key = self.analysis_cache_key_for(query, token_budget);
        if let Some(CacheEntry::Analysis {
            serialized_data, ..
        }) = self
            .cache
            .cache_spiller
            .store_mut()
            .get_or_load(&analysis_cache_key)?
        {
            if let Ok(mut cached) = bincode::deserialize::<super::AnalysisResult>(&serialized_data)
            {
                cached.processing_time_ms = start_time.elapsed().as_millis() as u64;
                debug!("Analysis cache hit for '{}'", query);
                return Ok(cached);
            }
        }

        // Step 1: Semantic search for entry points
        let search_query = SearchQuery {
            query: query.to_string(),
            top_k: 10,
            token_budget: Some(token_budget),
            semantic: true,
            expand_context: false,
            query_embedding: Some(self.generate_query_embedding(query)),
            threshold: Some(0.1), // Added threshold for better quality
            query_type: Some(crate::search::ranking::QueryType::Semantic),
        };

        let results = self
            .search_engine
            .search(search_query)
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
        let analysis = super::AnalysisResult {
            query: query.to_string(),
            results,
            context: Some(context),
            tokens_used,
            processing_time_ms: start_time.elapsed().as_millis() as u64,
        };

        if let Ok(serialized) = bincode::serialize(&analysis) {
            let entry = CacheEntry::Analysis {
                query: query.to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                serialized_data: serialized,
            };
            if self
                .cache
                .cache_spiller
                .store_mut()
                .insert(analysis_cache_key.clone(), entry)
                .is_ok()
            {
                let _ = self
                    .cache
                    .cache_spiller
                    .store_mut()
                    .persist_key(&analysis_cache_key);
            }
        }

        Ok(analysis)
    }

    /// Expand context around a specific node.
    ///
    /// Accepts flexible node identification:
    /// - Full node ID (`"file_path:qualified_name"`)
    /// - Short symbol name (`"health_check"`)
    /// - Qualified name (`"ClassName.method_name"`)
    /// - `"file_path:symbol_name"` partial IDs
    /// - Fuzzy/partial name match (e.g., `"event_loop"` matches `run_event_loop`)
    ///
    /// When the initial lookup fails, performs an on-demand expansion scan
    /// of the PDG to discover nodes whose names contain the query as a
    /// substring. This ensures event-loop-heavy files (e.g., winit
    /// entrypoints) are discoverable even when the exact symbol name
    /// differs from the query.
    ///
    /// Populates the SearchResult with real metadata from the PDG node.
    pub fn expand_node_context(
        &self,
        node_id: &str,
        token_budget: usize,
    ) -> Result<super::AnalysisResult> {
        let start_time = std::time::Instant::now();

        let pdg = self.pdg.as_ref().ok_or_else(|| {
            anyhow::anyhow!("No PDG available for context expansion. Has the project been indexed?")
        })?;

        // Resolve the node_id using multiple lookup strategies:
        // 1. Exact ID match (full "file_path:qualified_name")
        // 2. By name (short display name like "health_check")
        // 3. Case-insensitive substring match on name or id
        // 4. On-demand fuzzy scan: find nodes whose name contains the query
        let resolved_nid = pdg
            .find_by_symbol(node_id)
            .or_else(|| pdg.find_by_name(node_id))
            .or_else(|| pdg.find_by_name_in_file(node_id, None))
            .or_else(|| fuzzy_find_node(pdg, node_id));

        let (result_node_id, file_path, symbol_name, language, byte_range, complexity) =
            if let Some(nid) = resolved_nid {
                if let Some(node) = pdg.get_node(nid) {
                    (
                        node.id.clone(),
                        node.file_path.to_string(),
                        node.name.clone(),
                        node.language.clone(),
                        node.byte_range,
                        node.complexity,
                    )
                } else {
                    (
                        node_id.to_string(),
                        String::new(),
                        node_id.to_string(),
                        "unknown".to_string(),
                        (0, 0),
                        0,
                    )
                }
            } else {
                (
                    node_id.to_string(),
                    String::new(),
                    node_id.to_string(),
                    "unknown".to_string(),
                    (0, 0),
                    0,
                )
            };

        let results = vec![SearchResult {
            rank: 1,
            node_id: result_node_id,
            file_path,
            symbol_name,
            symbol_type: None,
            signature: None,
            complexity,
            caller_count: None,
            dependency_count: None,
            language,
            score: crate::search::ranking::Score::default(),
            context: None,
            byte_range,
        }];

        let context = self.expand_context(pdg, &results, token_budget)?;
        let tokens_used = context.len() / 4;

        Ok(super::AnalysisResult {
            query: format!("Context for node {}", node_id),
            results,
            context: Some(context),
            tokens_used,
            processing_time_ms: start_time.elapsed().as_millis() as u64,
        })
    }

    /// Generate an embedding for a query string.
    ///
    /// Uses the TF-IDF embedder built at index time when available, ensuring
    /// queries are projected into the same vector space as the indexed nodes.
    /// Falls back to deterministic hashing for edge cases (empty corpus, not yet indexed).
    pub fn generate_query_embedding(&self, query: &str) -> Vec<f32> {
        if let Some(ref emb) = self.embedder {
            let tokens = index_builder::tokenize_code(query);
            emb.embed_tfidf(&tokens)
        } else {
            generate_deterministic_embedding(query)
        }
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

        // Map SearchResult entries to PDG node IDs for the traversal call.
        // Try exact ID match first, then fall back to name-based lookup,
        // then fuzzy substring match for event-loop-heavy files.
        let entry_points: Vec<_> = results
            .iter()
            .filter_map(|r| {
                pdg.find_by_symbol(&r.node_id)
                    .or_else(|| pdg.find_by_name(&r.node_id))
                    .or_else(|| pdg.find_by_name(&r.symbol_name))
                    .or_else(|| fuzzy_find_node(pdg, &r.symbol_name))
            })
            .collect();

        let expanded_node_ids = traversal.expand_context(pdg, entry_points);

        let mut context = String::from("/* Context Expansion via Gravity Traversal */\n");

        for node_id in expanded_node_ids {
            if let Some(node) = pdg.get_node(node_id) {
                context.push_str(&format!("\n// Symbol: {}\n", node.name));
                context.push_str(&format!("// File: {}\n", node.file_path));
                context.push_str(&format!("// Type: {:?}\n", node.node_type));

                // Retrieve actual source code if byte_range is valid
                if node.byte_range.1 > node.byte_range.0 {
                    if let Ok(content) = std::fs::read(&*node.file_path) {
                        let start = node.byte_range.0;
                        let end = node.byte_range.1.min(content.len());
                        if let Ok(code) = std::str::from_utf8(&content[start..end]) {
                            context.push_str(code);
                            context.push('\n');
                        } else {
                            context.push_str("// [Error: Source code is not valid UTF-8]\n");
                        }
                    } else {
                        context.push_str(&format!(
                            "// [Error: Could not read file: {}]\n",
                            node.file_path
                        ));
                    }
                } else {
                    context.push_str("// [No source code range available for this node]\n");
                }
            }
        }

        Ok(context)
    }
}

/// Generate a deterministic 768-dimensional embedding for a query string.
/// Fallback when no TF-IDF embedder is available.
fn generate_deterministic_embedding(symbol_name: &str) -> Vec<f32> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut embedding = Vec::with_capacity(768);
    let mut base_hasher = DefaultHasher::new();
    symbol_name.to_lowercase().hash(&mut base_hasher);
    let base_hash = base_hasher.finish();

    for i in 0..768 {
        let mut hasher = DefaultHasher::new();
        base_hash.hash(&mut hasher);
        i.hash(&mut hasher);
        let hash_val = hasher.finish();
        let val = (hash_val as f64 / u64::MAX as f64) * 2.0 - 1.0;
        embedding.push(val as f32);
    }

    embedding
}

/// On-demand fuzzy node discovery for event-loop-heavy files.
///
/// When exact lookup fails, this function scans the PDG for nodes whose
/// name or ID contains the query as a case-insensitive substring. This
/// ensures that winit event-loop entrypoints (e.g., `run_event_loop`,
/// `EventLoop::run`, `main`) are discoverable even when the user's query
/// doesn't exactly match the symbol name.
///
/// Returns the best-matching NodeId, preferring:
/// 1. Nodes whose name contains the query as a substring
/// 2. Nodes whose ID contains the query as a substring
/// 3. Higher-complexity nodes (event loops tend to be complex)
fn fuzzy_find_node(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    query: &str,
) -> Option<crate::graph::pdg::NodeId> {
    const NAME_MATCH_SCORE: usize = 100;
    const ID_MATCH_SCORE: usize = 50;
    const ALIAS_MATCH_SCORE: usize = 25;
    const COMPLEXITY_SCORE_CAP: u32 = 50;

    let query_lower = query.to_lowercase();

    // Empty query matches nothing — not a wildcard.
    if query_lower.is_empty() {
        return None;
    }

    let event_loop_aliases: &[&str] = &["run", "main", "event_loop", "event loop", "winit", "app_runner"];

    let is_event_loop_query = event_loop_aliases
        .iter()
        .any(|alias| query_lower.contains(alias));

    let mut best_match: Option<(crate::graph::pdg::NodeId, usize)> = None;

    let mut score_node = |node_id: crate::graph::pdg::NodeId, node: &crate::graph::pdg::Node| {
        let name_lower = node.name.to_lowercase();

        let score = if name_lower.contains(&query_lower) {
            NAME_MATCH_SCORE + node.complexity.min(COMPLEXITY_SCORE_CAP) as usize
        } else {
            let id_lower = node.id.to_lowercase();
            if id_lower.contains(&query_lower) {
                ID_MATCH_SCORE + node.complexity.min(COMPLEXITY_SCORE_CAP) as usize
            } else if is_event_loop_query && event_loop_aliases.iter().any(|alias| name_lower.contains(alias)) {
                ALIAS_MATCH_SCORE + node.complexity.min(COMPLEXITY_SCORE_CAP) as usize
            } else {
                return;
            }
        };

        match &best_match {
            None => best_match = Some((node_id, score)),
            Some((_, best_score)) if score > *best_score => {
                best_match = Some((node_id, score));
            }
            _ => {}
        }
    };

    let candidate_indices = pdg.trigram_index().query(&query_lower);

    match candidate_indices {
        Some(indices) if !is_event_loop_query => {
            for node_idx in indices {
                let node_id = crate::graph::pdg::NodeId::new(node_idx as usize);
                if let Some(node) = pdg.get_node(node_id) {
                    score_node(node_id, node);
                }
            }
        }
        _ => {
            for node_id in pdg.node_indices() {
                if let Some(node) = pdg.get_node(node_id) {
                    score_node(node_id, node);
                }
            }
        }
    }

    best_match.map(|(nid, _)| nid)
}
