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
        self.search_internal(query, top_k, query_type, true)
    }

    /// Search with request-scoped context without writing that context into
    /// the persistent query cache.
    pub(crate) fn search_ephemeral(
        &mut self,
        query: &str,
        top_k: usize,
        query_type: Option<crate::search::ranking::QueryType>,
    ) -> Result<Vec<SearchResult>> {
        self.search_internal(query, top_k, query_type, false)
    }

    fn enrich_results_with_pdg_metadata(
        &self,
        results: &mut [SearchResult],
        pdg: &ProgramDependenceGraph,
        file_cache: &mut std::collections::HashMap<String, Option<Vec<u8>>>,
    ) {
        for result in results {
            if let Some(node_idx) = pdg.find_by_id(&result.node_id) {
                if let Some(node) = pdg.get_node(node_idx) {
                    result.symbol_type = Some(match node.node_type {
                        crate::graph::pdg::NodeType::Function => "function".to_string(),
                        crate::graph::pdg::NodeType::Class => "class".to_string(),
                        crate::graph::pdg::NodeType::Method => "method".to_string(),
                        crate::graph::pdg::NodeType::Variable => "variable".to_string(),
                        crate::graph::pdg::NodeType::Module => "module".to_string(),
                        crate::graph::pdg::NodeType::External => "external".to_string(),
                        crate::graph::pdg::NodeType::FileSummary => "file_summary".to_string(),
                    });

                    let file_path_str = node.file_path.to_string();
                    if result.line_number.is_none() {
                        let abs_path = self.resolve_indexed_file_path(&file_path_str);
                        let content = file_cache
                            .entry(file_path_str.clone())
                            .or_insert_with(|| std::fs::read(abs_path).ok());
                        if let Some(content) = content.as_ref() {
                            if node.byte_range.0 > 0 || node.byte_range.1 > 0 {
                                let byte_offset = node.byte_range.0.min(content.len());
                                let line_num = content[..byte_offset]
                                    .iter()
                                    .filter(|&&b| b == b'\n')
                                    .count()
                                    + 1;
                                result.line_number = Some(line_num);
                            } else if !node.name.is_empty() {
                                if let Some(pos) = find_subsequence(content, node.name.as_bytes()) {
                                    let line_num =
                                        content[..pos].iter().filter(|&&b| b == b'\n').count() + 1;
                                    result.line_number = Some(line_num);
                                }
                            }
                        }
                    }
                }
                result.caller_count = Some(pdg.predecessor_count(node_idx));
                result.dependency_count = Some(pdg.neighbors(node_idx).len());
            }
        }
    }

    fn rerank_results(
        &self,
        results: &mut Vec<SearchResult>,
        query: &str,
        pdg: &ProgramDependenceGraph,
        file_cache: &mut std::collections::HashMap<String, Option<Vec<u8>>>,
        exact_route: bool,
    ) {
        let rerank_cfg = &crate::config::LeIndexConfig::load_cached().search;
        if rerank_cfg.rerank_enabled && !exact_route && !results.is_empty() {
            if let Some(embedder) = self.embedder.as_ref() {
                let n = (rerank_cfg.rerank_top_n as usize).min(results.len());
                let mut docs: Vec<(String, String, f32)> = Vec::with_capacity(n);
                for result in results[..n].iter() {
                    let content = pdg
                        .find_by_id(&result.node_id)
                        .and_then(|idx| pdg.get_node(idx))
                        .and_then(|node| {
                            let path = self.resolve_indexed_file_path(&node.file_path);
                            let bytes = file_cache
                                .entry(node.file_path.to_string())
                                .or_insert_with(|| std::fs::read(&path).ok());
                            bytes
                                .as_ref()
                                .filter(|bytes| {
                                    node.byte_range.0 < node.byte_range.1
                                        && node.byte_range.1 <= bytes.len()
                                })
                                .map(|bytes| {
                                    String::from_utf8_lossy(
                                        &bytes[node.byte_range.0..node.byte_range.1],
                                    )
                                    .to_string()
                                })
                        })
                        .unwrap_or_else(|| format!("{} {}", result.symbol_name, result.file_path));
                    docs.push((result.node_id.clone(), content, result.score.overall));
                }

                match embedder.rerank_blocking(query, docs) {
                    Some(Ok(reranked)) => {
                        let by_id: std::collections::HashMap<String, SearchResult> = results[..n]
                            .iter()
                            .map(|result| (result.node_id.clone(), result.clone()))
                            .collect();
                        let mut new_top: Vec<SearchResult> = Vec::with_capacity(n);
                        for (id, _) in &reranked {
                            if let Some(result) = by_id.get(id) {
                                new_top.push(result.clone());
                            }
                        }
                        for result in results[..n].iter() {
                            if !new_top.iter().any(|item| item.node_id == result.node_id) {
                                new_top.push(result.clone());
                            }
                        }
                        new_top.extend(results[n..].iter().cloned());
                        *results = new_top;
                        for (index, result) in results.iter_mut().enumerate() {
                            result.rank = index + 1;
                        }
                        debug!("reranker re-ordered top-{} for '{}'", n, query);
                    }
                    Some(Err(error)) => {
                        tracing::warn!(error = %error, "reranker failed; keeping original order")
                    }
                    None => {}
                }
            }
        }
    }

    fn cache_search_results(&mut self, results: &[SearchResult], key: &str, query: &str) {
        if let Ok(serialized) = bincode::serialize(results) {
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
                .insert(key.to_string(), entry)
                .is_ok()
            {
                let _ = self.cache.cache_spiller.store_mut().persist_key(key);
            }
        }
    }

    fn search_internal(
        &mut self,
        query: &str,
        top_k: usize,
        query_type: Option<crate::search::ranking::QueryType>,
        cache_results: bool,
    ) -> Result<Vec<SearchResult>> {
        if self.search_engine.is_empty() {
            warn!("Search attempted on empty index");
            return Ok(Vec::new());
        }

        if cache_results {
            if let Some(cached_results) =
                self.cached_search_results(query, top_k, query_type.as_ref())?
            {
                return Ok(cached_results);
            }
        }

        // Derive the effective query type: an explicit caller-provided type
        // wins; otherwise fall back to the configured [search] search_mode so
        // the documented config knob actually controls retrieval. This is the
        // fix for search_mode being a dead string (VAL-CONFIG).
        let search_config = &crate::config::LeIndexConfig::load_cached().search;
        let effective_query_type = match query_type {
            Some(explicit) => Some(explicit),
            None => crate::config::query_type_for_mode(&search_config.search_mode),
        };
        let exact_route = matches!(
            effective_query_type,
            Some(crate::search::ranking::QueryType::Exact)
        );
        // Exact identifier/text queries stay lexical. In particular, do not
        // construct either query embedding: this keeps a concrete code-review
        // lookup independent of TF-IDF vocabulary and neural worker readiness.
        let query_neural_embedding = if exact_route {
            None
        } else {
            self.generate_query_neural_embedding(query)
        };
        let neural_available = query_neural_embedding.is_some();
        let search_cache_key = self.search_cache_key_for(
            query,
            top_k,
            effective_query_type.as_ref(),
            neural_available,
        );

        // Adaptive relevance threshold: pure-neural/semantic matches routinely
        // score below the hybrid 0.1 cutoff (cosine similarities land lower), so
        // loosen to 0.05 for Semantic mode. analyze_search already uses 0.05.
        // ponytail: two fixed values by mode; add a toml knob only if recall tuning is requested.
        let threshold = match effective_query_type {
            Some(crate::search::ranking::QueryType::Semantic) => Some(0.05),
            _ => Some(0.1),
        };

        // When rerank is enabled, over-fetch candidates so the cross-encoder
        // sees a wider pool than the final top_k (it can only reorder what was
        // retrieved). We truncate back to top_k after reranking below.
        let rerank_enabled = search_config.rerank_enabled && !exact_route;
        let search_top_k = if rerank_enabled {
            top_k.max(search_config.rerank_top_n as usize)
        } else {
            top_k
        };

        let search_query = SearchQuery {
            query: query.to_string(),
            top_k: search_top_k,
            token_budget: None,
            semantic: !exact_route,
            expand_context: false,
            query_embedding: (!exact_route).then(|| self.generate_query_embedding(query)),
            query_neural_embedding,
            threshold,
            query_type: effective_query_type,
        };

        let mut results = self
            .search_engine
            .search(search_query)
            .context("Search operation failed")?;

        if let Some(pdg) = &self.pdg {
            let mut file_cache = std::collections::HashMap::new();
            self.enrich_results_with_pdg_metadata(&mut results, pdg, &mut file_cache);
            self.rerank_results(&mut results, query, pdg, &mut file_cache, exact_route);
        }

        // Truncate the over-fetched candidate pool (search_top_k) back to the
        // requested top_k after reranking.
        results.truncate(top_k);

        debug!("Search for '{}' returned {} results", query, results.len());

        if cache_results {
            self.cache_search_results(&results, &search_cache_key, query);
        }

        Ok(results)
    }

    /// A hybrid index must not return a stale TF-IDF-only cache entry before
    /// giving its configured neural provider a chance to answer. Once the
    /// provider reports a terminal failure, the persisted fallback is valid
    /// until the next index generation.
    fn neural_search_should_be_attempted(&self) -> bool {
        self.embedder
            .as_ref()
            .is_some_and(|embedder| embedder.has_neural() && embedder.neural_status() != "failed")
    }

    fn cached_search_results(
        &mut self,
        query: &str,
        top_k: usize,
        query_type: Option<&crate::search::ranking::QueryType>,
    ) -> Result<Option<Vec<SearchResult>>> {
        // Neural availability is part of the persisted cache key. A live
        // hybrid provider probes only the neural key; otherwise a stale
        // TF-IDF-only result would prevent the cold worker from starting.
        let neural_required = !matches!(query_type, Some(crate::search::ranking::QueryType::Exact))
            && self.neural_search_should_be_attempted();
        for neural_available in [true, false] {
            if neural_required && !neural_available {
                continue;
            }
            let search_cache_key =
                self.search_cache_key_for(query, top_k, query_type, neural_available);
            if let Some(CacheEntry::Binary {
                serialized_data, ..
            }) = self
                .cache
                .cache_spiller
                .store_mut()
                .get_or_load(&search_cache_key)?
            {
                if let Ok(cached_results) =
                    bincode::deserialize::<Vec<SearchResult>>(&serialized_data)
                {
                    debug!(
                        "Search cache hit for '{}' ({} results)",
                        query,
                        cached_results.len()
                    );
                    return Ok(Some(cached_results));
                }
            }
        }

        Ok(None)
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
        self.analyze_internal(query, token_budget, true)
    }

    /// Analyze with request-scoped context without persisting the expanded
    /// query or result in the durable analysis cache.
    pub(crate) fn analyze_ephemeral(
        &mut self,
        query: &str,
        token_budget: usize,
    ) -> Result<super::AnalysisResult> {
        self.analyze_internal(query, token_budget, false)
    }

    fn analyze_internal(
        &mut self,
        query: &str,
        token_budget: usize,
        cache_results: bool,
    ) -> Result<super::AnalysisResult> {
        let start_time = std::time::Instant::now();

        let analysis_cache_key = self.analysis_cache_key_for(query, token_budget);
        let neural_search_requested = self.neural_search_should_be_attempted();
        if cache_results {
            if let Some(CacheEntry::Analysis {
                serialized_data, ..
            }) = self
                .cache
                .cache_spiller
                .store_mut()
                .get_or_load(&analysis_cache_key)?
            {
                // New entries carry a one-bit provenance marker so a cached
                // hybrid analysis can be reused without starting another
                // model request, while an old/raw TF-IDF-only entry cannot
                // suppress a configured neural attempt.
                if let Ok((cached_with_neural, mut cached)) =
                    bincode::deserialize::<(bool, super::AnalysisResult)>(&serialized_data)
                {
                    if cached_with_neural || !neural_search_requested {
                        cached.processing_time_ms = start_time.elapsed().as_millis() as u64;
                        debug!("Analysis cache hit for '{}'", query);
                        return Ok(cached);
                    }
                } else if !neural_search_requested {
                    // Preserve compatibility with entries written before the
                    // provenance marker was introduced.
                    if let Ok(mut cached) =
                        bincode::deserialize::<super::AnalysisResult>(&serialized_data)
                    {
                        cached.processing_time_ms = start_time.elapsed().as_millis() as u64;
                        debug!("Analysis cache hit for '{}'", query);
                        return Ok(cached);
                    }
                }
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

        if cache_results {
            let cached_with_neural = self
                .embedder
                .as_ref()
                .is_some_and(|embedder| embedder.neural_status() == "ready");
            if let Ok(serialized) = bincode::serialize(&(cached_with_neural, &analysis)) {
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
    /// Returns `None` only when the configured provider reaches a terminal
    /// failure/absence; the caller then reports the mandatory TF-IDF result.
    /// A cold default worker is started and awaited through its explicit
    /// lifecycle state; model loading/inference are never cancelled by an
    /// elapsed-time request timeout.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub fn generate_query_neural_embedding(&self, query: &str) -> Option<Vec<f32>> {
        let emb = self.embedder.as_ref()?;
        match emb.embed_neural_blocking(query) {
            Some(Ok(embedding)) => Some(embedding),
            Some(Err(error)) => {
                debug!("Neural query embedding failed ({error}); using TF-IDF fallback");
                None
            }
            None => {
                debug!("Neural query embedding unavailable; using TF-IDF fallback");
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
        let primary_neural_embedding = self.generate_query_neural_embedding(query);
        let try_additional_neural = primary_neural_embedding.is_some();
        let primary_results = self
            .search_engine
            .search(self.analysis_search_query(query, primary_neural_embedding))
            .context("Search for analysis failed")?;

        let key_terms = extract_analysis_keywords(query);
        let secondary_results = self.optional_analysis_search(
            &key_terms,
            key_terms != query.to_lowercase() && !key_terms.is_empty(),
            try_additional_neural,
        );
        let stemmed_terms = extract_stemmed_keywords(query);
        let stemmed_results = self.optional_analysis_search(
            &stemmed_terms,
            !stemmed_terms.is_empty() && stemmed_terms != key_terms,
            try_additional_neural,
        );

        let mut final_results =
            Self::merge_analysis_results(primary_results, secondary_results, stemmed_results);
        if let Some(pdg) = &self.pdg {
            let mut file_cache = std::collections::HashMap::new();
            self.enrich_results_with_pdg_metadata(&mut final_results, pdg, &mut file_cache);
        }
        Ok(final_results)
    }

    fn analysis_search_query(
        &self,
        query: &str,
        query_neural_embedding: Option<Vec<f32>>,
    ) -> SearchQuery {
        SearchQuery {
            query: query.to_string(),
            top_k: 15,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(self.generate_query_embedding(query)),
            query_neural_embedding,
            threshold: Some(0.05),
            query_type: Some(crate::search::ranking::QueryType::Semantic),
        }
    }

    fn optional_analysis_search(
        &mut self,
        query: &str,
        should_search: bool,
        try_neural: bool,
    ) -> Vec<SearchResult> {
        if !should_search {
            return Vec::new();
        }
        let neural_embedding = try_neural
            .then(|| self.generate_query_neural_embedding(query))
            .flatten();
        self.search_engine
            .search(self.analysis_search_query(query, neural_embedding))
            .unwrap_or_default()
    }

    fn merge_analysis_results(
        primary_results: Vec<SearchResult>,
        secondary_results: Vec<SearchResult>,
        stemmed_results: Vec<SearchResult>,
    ) -> Vec<SearchResult> {
        let mut merged: std::collections::HashMap<String, SearchResult> =
            std::collections::HashMap::new();
        for result in primary_results
            .into_iter()
            .chain(secondary_results)
            .chain(stemmed_results)
        {
            match merged.entry(result.node_id.clone()) {
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if result.score.overall > entry.get().score.overall =>
                {
                    entry.insert(result);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(result);
                }
                _ => {}
            }
        }

        let mut results: Vec<SearchResult> = merged.into_values().collect();
        for result in &mut results {
            if !is_source_code_file(&result.file_path) {
                result.score.overall *= 0.3;
            }
        }
        Self::sort_results_by_score(&mut results);

        let mut file_counts = std::collections::HashMap::new();
        for result in &mut results {
            let count = file_counts.get(&result.file_path).copied().unwrap_or(0);
            if count > 0 {
                result.score.overall *= (0.9_f32).powi(count);
            }
            *file_counts.entry(result.file_path.clone()).or_default() += 1;
        }

        Self::sort_results_by_score(&mut results);
        let mut final_results: Vec<SearchResult> = results.into_iter().take(10).collect();
        for (index, result) in final_results.iter_mut().enumerate() {
            result.rank = index + 1;
        }
        final_results
    }

    fn sort_results_by_score(results: &mut [SearchResult]) {
        results.sort_by(|left, right| {
            right
                .score
                .overall
                .partial_cmp(&left.score.overall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

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
                let abs_path = self.resolve_indexed_file_path(&node.file_path);
                if let Ok(content) = std::fs::read(&abs_path) {
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

// On-demand fuzzy node discovery for event-loop-heavy files.
//
// When exact lookup fails, this function scans the PDG for nodes whose
// name or ID contains the query as a case-insensitive substring. This
// ensures that winit event-loop entrypoints (e.g., `run_event_loop`,
// `EventLoop::run`, `main`) are discoverable even when the user's query
// doesn't exactly match the symbol name.
//
// Returns the best-matching NodeId, preferring name, then ID, then
// higher-complexity event-loop aliases.
const EVENT_LOOP_ALIASES: &[&str] = &[
    "run",
    "main",
    "event_loop",
    "event loop",
    "winit",
    "app_runner",
];
const MAX_FUZZY_FALLBACK_SCAN: usize = 10_000;

fn fuzzy_find_node(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    query: &str,
) -> Option<crate::graph::pdg::NodeId> {
    let query_lower = query.to_lowercase();
    if query_lower.is_empty() {
        return None;
    }

    let is_event_loop_query = EVENT_LOOP_ALIASES
        .iter()
        .any(|alias| query_lower == *alias || query_lower.split_whitespace().any(|w| w == *alias));
    let candidates = fuzzy_candidate_nodes(pdg, &query_lower, is_event_loop_query);
    let mut best_match = None;

    for node_id in candidates {
        let Some(node) = pdg.get_node(node_id) else {
            continue;
        };
        let Some(score) = fuzzy_node_score(node, &query_lower, is_event_loop_query) else {
            continue;
        };
        if best_match
            .as_ref()
            .is_none_or(|(_, best_score)| score > *best_score)
        {
            best_match = Some((node_id, score));
        }
    }

    best_match.map(|(node_id, _)| node_id)
}

fn fuzzy_candidate_nodes(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    query: &str,
    is_event_loop_query: bool,
) -> Vec<crate::graph::pdg::NodeId> {
    if is_event_loop_query {
        return event_loop_candidate_nodes(pdg);
    }

    pdg.trigram_index()
        .query(query)
        .map(|indices| {
            indices
                .iter()
                .map(|index| crate::graph::pdg::NodeId::new(*index as usize))
                .collect()
        })
        .unwrap_or_else(|| bounded_node_indices(pdg))
}

fn event_loop_candidate_nodes(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
) -> Vec<crate::graph::pdg::NodeId> {
    let mut candidates: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for alias in EVENT_LOOP_ALIASES {
        if let Some(indices) = pdg.trigram_index().query(alias) {
            candidates.extend(indices.iter());
        }
    }

    if candidates.is_empty() {
        return bounded_node_indices(pdg);
    }

    let mut candidates: Vec<_> = candidates.into_iter().collect();
    candidates.sort_unstable();
    candidates
        .into_iter()
        .map(|index| crate::graph::pdg::NodeId::new(index as usize))
        .collect()
}

fn bounded_node_indices(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
) -> Vec<crate::graph::pdg::NodeId> {
    pdg.node_indices().take(MAX_FUZZY_FALLBACK_SCAN).collect()
}

fn fuzzy_node_score(
    node: &crate::graph::pdg::Node,
    query: &str,
    is_event_loop_query: bool,
) -> Option<usize> {
    const NAME_MATCH_SCORE: usize = 100;
    const ID_MATCH_SCORE: usize = 50;
    const ALIAS_MATCH_SCORE: usize = 25;
    const COMPLEXITY_SCORE_CAP: u32 = 50;

    let name_lower = node.name.to_lowercase();
    let base_score = if name_lower.contains(query) {
        NAME_MATCH_SCORE
    } else if node.id.to_lowercase().contains(query) {
        ID_MATCH_SCORE
    } else if is_event_loop_query
        && EVENT_LOOP_ALIASES
            .iter()
            .any(|alias| name_lower.contains(alias))
    {
        ALIAS_MATCH_SCORE
    } else {
        return None;
    };

    Some(base_score + node.complexity.min(COMPLEXITY_SCORE_CAP) as usize)
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
    use super::{LeIndex, simple_stem};
    use crate::cli::memory::CacheEntry;
    use crate::search::{ranking::Score, search::SearchResult};

    fn cached_result(node_id: &str) -> SearchResult {
        SearchResult {
            rank: 1,
            node_id: node_id.to_string(),
            file_path: "src/lib.rs".to_string(),
            symbol_name: "cached_symbol".to_string(),
            symbol_type: Some("function".to_string()),
            signature: Some("fn cached_symbol() {}".to_string()),
            complexity: 1,
            caller_count: Some(0),
            dependency_count: Some(0),
            language: "rust".to_string(),
            score: Score::default(),
            context: Some(String::new()),
            byte_range: (0, 0),
            line_number: Some(1),
        }
    }

    #[test]
    fn cached_search_results_use_fallback_key_without_neural_embedder() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_path = temp_dir.path().join("project");
        std::fs::create_dir_all(&project_path).unwrap();
        std::fs::write(project_path.join("lib.rs"), "fn cached_symbol() {}\n").unwrap();

        let mut leindex = LeIndex::new(&project_path).unwrap();
        let results = vec![cached_result("fallback-cache-hit")];
        let serialized = bincode::serialize(&results).unwrap();
        assert!(!serialized.is_empty());
        let fallback_key = leindex.search_cache_key_for("cached_symbol", 5, None, false);
        leindex
            .cache
            .cache_spiller
            .store_mut()
            .insert(
                fallback_key.clone(),
                CacheEntry::Binary {
                    metadata: std::collections::HashMap::from([(
                        "type".to_string(),
                        "search_results".to_string(),
                    )]),
                    serialized_data: serialized,
                },
            )
            .unwrap();
        let direct = leindex
            .cache
            .cache_spiller
            .store_mut()
            .get(&fallback_key)
            .expect("seeded fallback cache entry should be readable");
        let CacheEntry::Binary {
            serialized_data, ..
        } = direct
        else {
            panic!("seeded fallback cache entry should be binary");
        };
        let direct_results: Vec<SearchResult> = bincode::deserialize(&serialized_data).unwrap();
        assert_eq!(direct_results[0].node_id, "fallback-cache-hit");

        let cached = leindex
            .cached_search_results("cached_symbol", 5, None)
            .unwrap()
            .expect("fallback cache entry should be returned without neural probing");
        assert_eq!(cached[0].node_id, "fallback-cache-hit");
    }

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
