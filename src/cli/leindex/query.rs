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
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Maximum wall-clock time allowed for generating a single query neural
/// embedding via ONNX/remote backends.
///
/// On CPU-only systems the 600M-parameter model can take >120 seconds per
/// inference, which would hang the search path for the full 300-second IPC
/// timeout.  When the embedding does not complete within this duration we
/// abandon it and fall back to TF-IDF for the query embedding.  Pre-computed
/// neural node embeddings from indexing are still used for scoring, so search
/// results remain useful.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
const QUERY_EMBED_TIMEOUT_SECS: u64 = 15;

impl LeIndex {
    fn resolve_indexed_file_path(&self, file_path: &str) -> PathBuf {
        let path = Path::new(file_path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project_path.join(path)
        }
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

        let query_neural_embedding = self.generate_query_neural_embedding(query);
        let neural_available = query_neural_embedding.is_some();
        let search_cache_key =
            self.search_cache_key_for(query, top_k, query_type.as_ref(), neural_available);
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
            query_neural_embedding,
            threshold: Some(0.1), // Added default threshold for better quality
            query_type,
        };

        let mut results = self
            .search_engine
            .search(search_query)
            .context("Search operation failed")?;

        // Enrich results with PDG metadata: symbol_type, caller_count, dependency_count, line_number.
        // These require the in-memory PDG which is available here but not in lerecherche.
        if let Some(pdg) = &self.pdg {
            // Cache file contents to avoid re-reading the same file for multiple results
            let mut file_cache: std::collections::HashMap<String, Vec<u8>> =
                std::collections::HashMap::new();

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

                        // Compute line number from byte_range, or fall back to
                        // searching for the symbol name in the file content
                        let file_path_str = node.file_path.to_string();
                        let needs_line = result.line_number.is_none();
                        if needs_line {
                            let abs_path = self.resolve_indexed_file_path(&file_path_str);
                            let content = file_cache
                                .entry(file_path_str.clone())
                                .or_insert_with(|| std::fs::read(abs_path).unwrap_or_default());

                            if node.byte_range.0 > 0 || node.byte_range.1 > 0 {
                                // Compute from byte_range
                                let byte_offset = node.byte_range.0.min(content.len());
                                let line_num = content[..byte_offset]
                                    .iter()
                                    .filter(|&&b| b == b'\n')
                                    .count()
                                    + 1;
                                result.line_number = Some(line_num);
                            } else if !node.name.is_empty() {
                                // Fallback: find the symbol name in the file content
                                let name_bytes = node.name.as_bytes();
                                if let Some(pos) = find_subsequence(content, name_bytes) {
                                    let line_num =
                                        content[..pos].iter().filter(|&&b| b == b'\n').count() + 1;
                                    result.line_number = Some(line_num);
                                }
                            }
                        }
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
        // For natural language queries like "How does search scoring work?",
        // we perform multiple searches with different query formulations
        // and merge the results to get better coverage of relevant code.
        let results = self.analyze_search(query)?;

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
                // Node not found: return a clear error instead of a
                // degenerate empty result that confuses the caller.
                return Err(anyhow::anyhow!(
                    "Node '{}' not found in the project index. \
                    Use LeIndex [Search] or LeIndex [Grep Symbols] to find valid node IDs. \
                    The index uses short symbol names (e.g., 'handle_tool_call', not 'server.rs:handle_tool_call').",
                    node_id
                ));
            };

        // Compute line number from byte range.
        // Use byte-counting (count '\n' + 1) so that byte 0 correctly maps
        // to line 1.  The previous implementation used `.lines().count()`
        // which returns 0 for an empty slice (byte 0) and then filtered
        // that out, producing None instead of line 1.
        let line_number = if !file_path.is_empty() {
            let abs_path = self.resolve_indexed_file_path(&file_path);
            std::fs::read(abs_path).ok().map(|content| {
                let offset = byte_range.0.min(content.len());
                content[..offset].iter().filter(|&&b| b == b'\n').count() + 1
            })
        } else {
            None
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
            line_number,
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

    /// Generate a neural embedding for a query string.
    ///
    /// Uses ONNX (or remote) neural embeddings when available, projecting
    /// the query into the same neural vector space as the indexed nodes.
    /// Returns `None` when neural embeddings are unavailable (TF-IDF fallback).
    ///
    /// A [`QUERY_EMBED_TIMEOUT_SECS`]-second timeout prevents CPU-only ONNX
    /// inference (which can take >120 s with the 600M-parameter model) from
    /// hanging the search path.  When the timeout fires the function logs a
    /// warning and returns `None`, causing the caller to fall back to TF-IDF
    /// for the query embedding.  Pre-computed neural node embeddings from
    /// indexing are still used for scoring via cosine similarity, so search
    /// results remain useful even with a TF-IDF query embedding.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub fn generate_query_neural_embedding(&self, query: &str) -> Option<Vec<f32>> {
        let emb = self.embedder.as_ref()?;

        // Channel-based timeout pattern: spawn a detached worker thread that
        // runs the (potentially very slow) blocking embedding call, then wait
        // on the receiver with a bounded timeout.  If the timeout fires we
        // proceed without the neural embedding; the worker thread is left to
        // complete (or be killed at process exit) since we cannot cancel
        // ONNX inference mid-flight.
        let (tx, rx) = std::sync::mpsc::channel();
        let emb_clone = emb.clone();
        let query_owned = query.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(emb_clone.embed_neural_blocking(&query_owned));
        });

        match rx.recv_timeout(std::time::Duration::from_secs(QUERY_EMBED_TIMEOUT_SECS)) {
            Ok(Some(Ok(vec))) => Some(vec),
            Ok(Some(Err(e))) => {
                debug!(
                    "Neural embedding failed for query, using TF-IDF fallback: {}",
                    e
                );
                None
            }
            Ok(None) => None, // TF-IDF only mode, no neural available
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                warn!(
                    timeout_secs = QUERY_EMBED_TIMEOUT_SECS,
                    "Neural query embedding timed out after {}s, using TF-IDF fallback",
                    QUERY_EMBED_TIMEOUT_SECS
                );
                None
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                warn!("Neural embedding thread panicked or disconnected, using TF-IDF fallback");
                None
            }
        }
    }

    /// Generate a neural embedding for a query string (no-op without ONNX feature).
    ///
    /// Always returns `None` when compiled without the `onnx` or `remote-embeddings`
    /// feature flag, ensuring TF-IDF fallback is used.
    #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
    pub fn generate_query_neural_embedding(&self, _query: &str) -> Option<Vec<f32>> {
        None
    }

    /// Perform multi-query search for deep analysis.
    ///
    /// For natural language queries like "How does search scoring work?",
    /// this method:
    /// 1. Searches with the original query (semantic mode)
    /// 2. Extracts key technical terms and searches with those
    /// 3. Merges and deduplicates results, prioritizing source code files
    fn analyze_search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        // Primary search with the full query
        let primary_neural_embedding = self.generate_query_neural_embedding(query);
        let try_additional_neural = primary_neural_embedding.is_some();

        let primary_query = SearchQuery {
            query: query.to_string(),
            top_k: 15,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(self.generate_query_embedding(query)),
            query_neural_embedding: primary_neural_embedding,
            threshold: Some(0.05),
            query_type: Some(crate::search::ranking::QueryType::Semantic),
        };

        let primary_results = self
            .search_engine
            .search(primary_query)
            .context("Search for analysis failed")?;

        // Extract key terms from the query for a secondary search.
        // This helps find relevant code that doesn't contain the exact
        // query words but contains related technical terms.
        let key_terms = extract_analysis_keywords(query);

        // Only do secondary search if key terms differ significantly from original
        let secondary_results = if key_terms != query.to_lowercase() && !key_terms.is_empty() {
            let secondary_query = SearchQuery {
                query: key_terms.clone(),
                top_k: 15,
                token_budget: None,
                semantic: true,
                expand_context: false,
                query_embedding: Some(self.generate_query_embedding(&key_terms)),
                query_neural_embedding: if try_additional_neural {
                    self.generate_query_neural_embedding(&key_terms)
                } else {
                    None
                },
                threshold: Some(0.05),
                query_type: Some(crate::search::ranking::QueryType::Semantic),
            };

            self.search_engine
                .search(secondary_query)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Third search: use stemmed keywords to find related code.
        // For example, "scoring" → "score" to find score_hybrid, calculate_text_score, etc.
        let stemmed_terms = extract_stemmed_keywords(query);
        let stemmed_results = if !stemmed_terms.is_empty() && stemmed_terms != key_terms {
            let stemmed_query = SearchQuery {
                query: stemmed_terms.clone(),
                top_k: 15,
                token_budget: None,
                semantic: true,
                expand_context: false,
                query_embedding: Some(self.generate_query_embedding(&stemmed_terms)),
                query_neural_embedding: if try_additional_neural {
                    self.generate_query_neural_embedding(&stemmed_terms)
                } else {
                    None
                },
                threshold: Some(0.05),
                query_type: Some(crate::search::ranking::QueryType::Semantic),
            };

            self.search_engine.search(stemmed_query).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Merge results: deduplicate by node_id, keeping the highest score.
        // Apply source code prioritization: boost source files, penalize
        // non-source files (docs, scripts, configs).
        let mut merged: std::collections::HashMap<String, SearchResult> =
            std::collections::HashMap::new();

        for result in primary_results
            .into_iter()
            .chain(secondary_results)
            .chain(stemmed_results)
        {
            let node_id = result.node_id.clone();
            let new_score = result.score.overall;
            match merged.entry(node_id) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    // Keep the result with the higher overall score
                    if new_score > e.get().score.overall {
                        e.insert(result);
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(result);
                }
            }
        }

        let mut results: Vec<SearchResult> = merged.into_values().collect();

        // Apply source code prioritization
        for result in &mut results {
            if !is_source_code_file(&result.file_path) {
                // Penalize non-source files (docs, scripts, configs)
                result.score.overall *= 0.3;
            }
        }

        // Apply diversity boost: ensure results from different files
        // get representation. Group by file path and apply a small penalty
        // to results from files that already have many entries, so that
        // results from a variety of files appear in the top 10.
        let mut file_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        // Sort first by score to establish ranking order
        results.sort_by(|a, b| {
            b.score
                .overall
                .partial_cmp(&a.score.overall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for result in &mut results {
            let count = file_counts.get(&result.file_path).copied().unwrap_or(0);
            // Apply diminishing returns: each additional result from the
            // same file gets a 10% penalty
            if count > 0 {
                result.score.overall *= (0.9_f32).powi(count as i32);
            }
            *file_counts.entry(result.file_path.clone()).or_default() += 1;
        }

        // Sort by adjusted score
        results.sort_by(|a, b| {
            b.score
                .overall
                .partial_cmp(&a.score.overall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Take top 10 and re-rank
        let mut final_results: Vec<SearchResult> = results.into_iter().take(10).collect();
        for (i, result) in final_results.iter_mut().enumerate() {
            result.rank = i + 1;
        }

        // Enrich with PDG metadata (same as regular search)
        if let Some(pdg) = &self.pdg {
            let mut file_cache: std::collections::HashMap<String, Vec<u8>> =
                std::collections::HashMap::new();

            for result in &mut final_results {
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

                        // Compute line number from byte_range, or fall back to
                        // searching for the symbol name in the file content
                        let file_path_str = node.file_path.to_string();
                        let needs_line = result.line_number.is_none();
                        if needs_line {
                            let abs_path = self.resolve_indexed_file_path(&file_path_str);
                            let content = file_cache
                                .entry(file_path_str.clone())
                                .or_insert_with(|| std::fs::read(abs_path).unwrap_or_default());

                            if node.byte_range.0 > 0 || node.byte_range.1 > 0 {
                                let byte_offset = node.byte_range.0.min(content.len());
                                let line_num = content[..byte_offset]
                                    .iter()
                                    .filter(|&&b| b == b'\n')
                                    .count()
                                    + 1;
                                result.line_number = Some(line_num);
                            } else if !node.name.is_empty() {
                                let name_bytes = node.name.as_bytes();
                                if let Some(pos) = find_subsequence(content, name_bytes) {
                                    let line_num =
                                        content[..pos].iter().filter(|&&b| b == b'\n').count() + 1;
                                    result.line_number = Some(line_num);
                                }
                            }
                        }
                    }
                    result.caller_count = Some(pdg.predecessor_count(node_idx));
                    result.dependency_count = Some(pdg.neighbors(node_idx).len());
                }
            }
        }

        Ok(final_results)
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
                let found = pdg
                    .find_by_symbol(&r.node_id)
                    .or_else(|| pdg.find_by_name(&r.node_id))
                    .or_else(|| pdg.find_by_name(&r.symbol_name))
                    .or_else(|| fuzzy_find_node(pdg, &r.symbol_name));
                if found.is_none() {
                    debug!(
                        "expand_context: could not find node for node_id='{}', symbol_name='{}'",
                        r.node_id, r.symbol_name
                    );
                }
                found
            })
            .collect();

        debug!(
            "expand_context: {} entry points from {} results, pdg node_count={}",
            entry_points.len(),
            results.len(),
            pdg.node_count()
        );

        let expanded_node_ids = traversal.expand_context(pdg, entry_points);

        debug!("expand_context: {} expanded nodes", expanded_node_ids.len());

        let mut context = String::from("/* Context Expansion via Gravity Traversal */\n");

        for node_id in expanded_node_ids {
            if let Some(node) = pdg.get_node(node_id) {
                context.push_str(&format!("\n// Symbol: {}\n", node.name));
                context.push_str(&format!("// File: {}\n", node.file_path));
                context.push_str(&format!("// Type: {:?}\n", node.node_type));

                // Compute line number from byte range.
                // byte_range.0 == 0 is valid (file start, line 1) so we
                // must not use `> 0` as the guard.
                if let Ok(content) = std::fs::read(&*node.file_path) {
                    let start = node.byte_range.0;
                    let end = node.byte_range.1.min(content.len());

                    // Compute starting line number
                    let line_num = content[..start.min(content.len())]
                        .iter()
                        .filter(|&&b| b == b'\n')
                        .count()
                        + 1;
                    context.push_str(&format!("// Line: {}\n", line_num));

                    if end > start {
                        if let Ok(code) = std::str::from_utf8(&content[start..end]) {
                            context.push_str(code);
                            context.push('\n');
                        } else {
                            context.push_str("// [Error: Source code is not valid UTF-8]\n");
                        }
                    } else {
                        context.push_str("// [No source code range available for this node]\n");
                    }
                } else {
                    context.push_str(&format!(
                        "// [Error: Could not read file: {}]\n",
                        node.file_path
                    ));
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
    const MAX_FALLBACK_SCAN: usize = 10_000;

    let query_lower = query.to_lowercase();

    // Empty query matches nothing — not a wildcard.
    if query_lower.is_empty() {
        return None;
    }

    let event_loop_aliases: &[&str] = &[
        "run",
        "main",
        "event_loop",
        "event loop",
        "winit",
        "app_runner",
    ];

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
            } else if is_event_loop_query
                && event_loop_aliases
                    .iter()
                    .any(|alias| name_lower.contains(alias))
            {
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

    if is_event_loop_query {
        // Query trigram index for each alias, collect union of candidates
        let mut alias_candidates: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for alias in event_loop_aliases {
            if let Some(indices) = pdg.trigram_index().query(alias) {
                alias_candidates.extend(indices.iter());
            }
        }
        if alias_candidates.is_empty() {
            // No trigram hits for any alias — fall back to full scan (bounded)
            for (scanned, node_id) in pdg.node_indices().enumerate() {
                if scanned >= MAX_FALLBACK_SCAN {
                    break;
                }
                if let Some(node) = pdg.get_node(node_id) {
                    score_node(node_id, node);
                }
            }
        } else {
            for &node_idx in &alias_candidates {
                let node_id = crate::graph::pdg::NodeId::new(node_idx as usize);
                if let Some(node) = pdg.get_node(node_id) {
                    score_node(node_id, node);
                }
            }
        }
    } else if let Some(indices) = candidate_indices {
        for node_idx in indices {
            let node_id = crate::graph::pdg::NodeId::new(node_idx as usize);
            if let Some(node) = pdg.get_node(node_id) {
                score_node(node_id, node);
            }
        }
    } else {
        // No trigram index — fall back to full scan (bounded)
        for (scanned, node_id) in pdg.node_indices().enumerate() {
            if scanned >= MAX_FALLBACK_SCAN {
                break;
            }
            if let Some(node) = pdg.get_node(node_id) {
                score_node(node_id, node);
            }
        }
    }

    best_match.map(|(nid, _)| nid)
}

/// Extract stemmed technical terms from a natural language query.
///
/// Applies simple suffix-stripping to convert English word forms to their
/// likely root forms found in code identifiers:
/// - "scoring" → "score" (drop -ing)
/// - "running" → "run" (drop -ning → -n)
/// - "indexed" → "index" (drop -ed)
/// - "queries" → "query" (drop -ies → -y)
/// - "handlers" → "handler" (drop -s)
///
/// This helps find code symbols that use the base form of words
/// appearing in natural language questions.
fn extract_stemmed_keywords(query: &str) -> String {
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut stemmed: Vec<String> = Vec::new();

    for word in words {
        let lower = word
            .to_lowercase()
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
            .to_string();

        // Skip stop words
        if ANALYSIS_STOP_WORDS.contains(&lower.as_str()) || lower.len() <= 1 {
            continue;
        }

        // Apply simple stemming rules
        let stem = simple_stem(&lower);
        if !stem.is_empty() && !stemmed.contains(&stem) {
            stemmed.push(stem);
        }
    }

    stemmed.join(" ")
}

/// Apply simple suffix-stripping stemming to a word.
///
/// This is a very basic stemmer that handles common English suffixes
/// found in technical writing. It's not a full Porter stemmer, but it
/// covers the most common cases for code search.
fn simple_stem(word: &str) -> String {
    let word_chars = word.chars().count();
    if word_chars <= 3 {
        return word.to_string();
    }

    // Order matters: check longer suffixes first

    // -ing → base (e.g., "scoring" → "scor" → "score")
    // But handle "ning" → "n" (e.g., "running" → "runn" → "run")
    if let Some(base) = word.strip_suffix("ing") {
        // If base ends in double consonant, remove one (e.g., "runn" → "run")
        let chars: Vec<char> = base.chars().collect();
        if chars.len() >= 2
            && chars[chars.len() - 1] == chars[chars.len() - 2]
            && !is_vowel(chars[chars.len() - 1])
        {
            return drop_last_char(base);
        }
        // Try adding 'e' back (e.g., "scor" → "score", "rat" → "rate")
        if chars.len() >= 2 {
            let base_with_e = format!("{}e", base);
            // Heuristic: if base ends in consonant-vowel-consonant, add 'e'
            if !is_vowel(chars[chars.len() - 1]) && is_vowel(chars[chars.len() - 2]) {
                return base_with_e;
            }
        }
        return base.to_string();
    }

    // -ied → -y (e.g., "applied" → "apply")
    if let Some(base) = word.strip_suffix("ied") {
        if word_chars > 4 {
            return format!("{}y", base);
        }
    }

    // -ed → base (e.g., "indexed" → "index", "scored" → "score")
    if let Some(base) = word.strip_suffix("ed") {
        // If base ends in double consonant, remove one
        let chars: Vec<char> = base.chars().collect();
        if chars.len() >= 2
            && chars[chars.len() - 1] == chars[chars.len() - 2]
            && !is_vowel(chars[chars.len() - 1])
        {
            return drop_last_char(base);
        }
        return base.to_string();
    }

    // -ies → -y (e.g., "queries" → "query")
    if let Some(base) = word.strip_suffix("ies") {
        if word_chars > 4 {
            return format!("{}y", base);
        }
    }

    // -es → base (e.g., "boxes" → "box", but not "score" → "scor")
    if let Some(base) = word.strip_suffix("es") {
        if word_chars > 4 {
            // Only strip 'es' if base ends in 's', 'x', 'z', 'ch', 'sh'
            if base.ends_with('s')
                || base.ends_with('x')
                || base.ends_with('z')
                || base.ends_with("ch")
                || base.ends_with("sh")
            {
                return base.to_string();
            }
        }
    }

    // -s → base (e.g., "handlers" → "handler", but not "is" → "i")
    if let Some(base) = word.strip_suffix('s') {
        if !base.ends_with('s') && word_chars > 3 {
            return base.to_string();
        }
    }

    word.to_string()
}

fn drop_last_char(s: &str) -> String {
    let mut out = s.to_string();
    out.pop();
    out
}

/// Check if a character is a vowel.
fn is_vowel(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u')
}

/// Common English stop words to filter out from analysis queries.
///
/// These are removed so that natural language questions like
/// "How does search scoring work?" are reduced to their key
/// technical terms ("search scoring") for more targeted code search.
const ANALYSIS_STOP_WORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "must", "shall",
    "can", "need", "dare", "ought", "used", "to", "of", "in", "for", "on", "with", "at", "by",
    "from", "as", "into", "through", "during", "before", "after", "above", "below", "up", "down",
    "out", "off", "over", "under", "again", "further", "then", "once", "here", "there", "when",
    "where", "why", "how", "all", "each", "every", "both", "few", "more", "most", "other", "some",
    "such", "no", "nor", "not", "only", "own", "same", "so", "than", "too", "very", "just", "also",
    "now", "and", "or", "but", "if", "while", "about", "against", "between", "into", "this",
    "that", "these", "those", "it", "its", "i", "me", "my", "we", "us", "our", "you", "your", "he",
    "him", "his", "she", "her", "they", "them", "their", "what", "which", "who", "whom", "whose",
];

/// Extract key technical terms from a natural language analysis query.
///
/// Removes common English stop words and question words, leaving the
/// technical terms that are most likely to match code symbols.
///
/// # Examples
/// - "How does search scoring work?" → "search scoring"
/// - "Where is user data stored?" → "user data stored"
/// - "score_hybrid" → "score_hybrid" (unchanged, already technical)
fn extract_analysis_keywords(query: &str) -> String {
    let words: Vec<&str> = query.split_whitespace().collect();
    let filtered: Vec<&str> = words
        .iter()
        .filter(|word| {
            let lower = word.to_lowercase();
            let lower_trimmed = lower.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            // Keep words that are:
            // 1. Not stop words
            // 2. Not single characters (unless they're part of a technical term)
            // 3. Longer than 1 character
            !ANALYSIS_STOP_WORDS.contains(&lower_trimmed) && lower_trimmed.len() > 1
        })
        .copied()
        .collect();

    if filtered.is_empty() {
        // If all words were stop words, return the original query
        query.to_string()
    } else {
        filtered.join(" ")
    }
}

/// Check if a file path points to a source code file.
///
/// Source code files have extensions like .rs, .py, .ts, .js, .go, etc.
/// Non-source files include documentation (.md, .txt), scripts (.sh, .bat),
/// and configuration files (.yaml, .json, .toml).
fn is_source_code_file(file_path: &str) -> bool {
    const SOURCE_EXTENSIONS: &[&str] = &[
        ".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".java", ".kt", ".swift", ".c", ".h",
        ".cpp", ".cc", ".cxx", ".hpp", ".hxx", ".cs", ".rb", ".php", ".scala", ".clj", ".ex",
        ".exs", ".erl", ".hs", ".ml", ".fs", ".fsx", ".lua", ".r", ".dart", ".vim", ".el", ".lisp",
        ".scm", ".jl",
    ];

    let lower = file_path.to_ascii_lowercase();
    SOURCE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// Find the first occurrence of `needle` in `haystack`.
///
/// Used to locate a symbol name in file content as a fallback for
/// computing line numbers when byte_range is unavailable (e.g., import nodes).
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::simple_stem;

    #[test]
    fn simple_stem_handles_multibyte_double_consonant() {
        assert_eq!(simple_stem("ååing"), "å");
        assert_eq!(simple_stem("ååed"), "å");
    }

    #[test]
    fn simple_stem_handles_single_multibyte_base_before_suffix() {
        assert_eq!(simple_stem("𐍈ing"), "𐍈");
        assert_eq!(simple_stem("𐍈ed"), "𐍈ed");
    }
}
