// Core search engine implementation
//
// # Thread Safety
//
// `SearchEngine` is NOT thread-safe for concurrent writes. However:
// - `&SearchEngine` (shared reference) can be safely used for concurrent reads
// - `&mut SearchEngine` requires exclusive access for writes
// - VectorIndex uses internal HashMap which is not thread-safe
//
// For concurrent access, wrap in `Arc<RwLock<SearchEngine>>`.

use crate::search::hnsw::{HNSWIndex, HNSWParams};
use crate::search::quantization::int8_hnsw::{Int8HnswIndex, Int8HnswParams};
use crate::search::query::{MAX_EMBEDDING_DIMENSION, MIN_EMBEDDING_DIMENSION};
use crate::search::ranking::{HybridScorer, Score};
use crate::search::vector::VectorIndex;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;

// ============================================================================
// CONSTANTS & VALIDATION
// ============================================================================

/// Default embedding dimension (CodeRank-compatible)
pub const DEFAULT_EMBEDDING_DIMENSION: usize = 768;

/// Maximum number of nodes that can be indexed (prevents memory exhaustion)
pub const MAX_NODES: usize = 1_000_000;

// ============================================================================
// A+ BOUND-GATED INDEXING, SELECTIVE PRUNING, AND WORK HOISTING
// ============================================================================

/// Conservative bound for the maximum number of nodes admitted in a single
/// indexing batch. When a batch exceeds this, the gate sheds or defers
/// additional nodes instead of admitting unbounded resident state growth.
pub const INDEXING_BATCH_NODE_CAP: usize = 50_000;

/// Conservative bound for the maximum total content bytes admitted in a single
/// indexing batch. Nodes whose cumulative content exceeds this are shed.
pub const INDEXING_BATCH_BYTE_CAP: usize = 512 * 1024 * 1024; // 512 MiB

/// Maximum number of entries in the work-hoister cache.
pub const WORK_HOISTER_MAX_ENTRIES: usize = 4_096;

/// Maximum byte budget for the work-hoister cache.
pub const WORK_HOISTER_MAX_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

mod int8_quality;
mod node_info;
mod pruner;
#[cfg(feature = "storage")]
mod snapshot;
mod staged_retrieval;
mod vector_impl;

pub use int8_quality::*;
pub use node_info::*;
pub use pruner::*;
pub use staged_retrieval::*;
pub use vector_impl::*;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests_extra.rs"]
mod tests_extra;

/// Search engine combining vector and graph search
///
/// This is the main entry point for search operations. It combines:
/// - Text-based search for keyword matching
/// - Vector-based semantic search for similarity
/// - Hybrid scoring combining multiple signals
///
/// Supports both brute-force and HNSW vector search backends.
///
/// # Thread Safety
///
/// - Reads (`&SearchEngine`) are thread-safe for concurrent access
/// - Writes (`&mut SearchEngine`) require exclusive access
/// - The internal VectorIndexImpl is NOT thread-safe for concurrent writes
///
/// For concurrent read-write access, wrap in `Arc<RwLock<SearchEngine>>`.
///
/// # Example
///
/// ```ignore
/// let mut engine = SearchEngine::new();
/// engine.index_nodes(nodes);
/// let results = engine.search(query)?;
/// ```
pub struct SearchEngine {
    nodes: Vec<NodeInfo>,
    scorer: HybridScorer,
    vector_index: VectorIndexImpl,
    /// Lazy-paged ANN over the neural embeddings (built from neural_embeddings
    /// mmap at search-load). This is the semantic RETRIEVAL signal: its top-K
    /// hits are unioned into the candidate pool alongside the lexical inverted
    /// index + the tfidf vector_index. Reuses MmapVectorIndex — same pattern as
    /// `vector_index` — so no hand-rolled scan. None when no neural mmap is
    /// loaded (tests, tfidf-only indexes).
    neural_vector_index: Option<VectorIndexImpl>,
    /// Fragment embeddings (sub-symbol semantic chunks), hydrated from
    /// `.leindex/fragments_embeddings.bin` at search-load. Same lazy-paged
    /// MmapVectorIndex pattern as the neural index; None when the fragment
    /// layer is off or no fragment mmap is present.
    fragment_vector_index: Option<VectorIndexImpl>,
    /// Master switch for the fragment layer (from `[search]
    /// fragment_index_enabled`). Gating renormalization on this — NOT on
    /// `fragment_weight > 0` — keeps the default path byte-identical
    /// (fragment-embeddings 1.11.0 Task 6, invariant 7).
    fragment_index_enabled: bool,
    /// Fragment fusion weight from `[search] fragment_weight` (default 0.35,
    /// empirically tuned: smallest weight with real margin that surfaces
    /// fragments over strong tfidf matches without regressing node rank).
    /// Only applied when `fragment_index_enabled` (renormalized into the five
    /// weights).
    fragment_weight: f32,
    /// content_hash → ALL (owner node id, byte range) refs for mapping
    /// fragment hits back to their Tier-1 owners (invariant 6). A Vec (not a
    /// single ref) because identical content can legitimately live under N
    /// owners — dedup must not collapse multi-owner fragments to the first
    /// (Codex wave-2 item 5). Populated by the cli hydration path from the
    /// fragment store.
    fragment_refs: HashMap<String, Vec<(String, (usize, usize))>>,
    /// Complexity cache for O(1) lookups (fixes O(n²) bug)
    complexity_cache: HashMap<String, u32>,
    /// Inverted index for O(1) text lookups: token -> set of node IDs
    /// This allows sub-linear text search instead of O(N) scan
    text_index: HashMap<String, HashSet<String>>,
    /// Node ID to index mapping for O(1) node lookups (fixes A1)
    /// Populated during index_nodes() and maintained on updates
    node_id_to_idx: HashMap<String, usize>,
    /// Per-node token cache: node_id -> set of normalized tokens
    /// Populated during index_nodes() to avoid re-tokenization in scoring
    node_tokens: HashMap<String, HashSet<String>>,
    /// Result cache for repeated queries (A+ Section 8.1: bounded by entries and bytes)
    search_cache: LruCache<String, Vec<SearchResult>>,
    /// Tracked byte estimate for the search cache
    search_cache_bytes: usize,
    /// Configured neural-score weight for the hybrid (None query_type) scoring
    /// arm. Set from `[search] neural_weight` in leindex.toml via
    /// `set_neural_weight` (config is the single source of truth; `src/config.rs`
    /// `default_neural_weight()` = 0.4, matching `HybridScorer::for_code()`).
    neural_weight: f32,
}

impl SearchEngine {
    /// Create a new search engine with default 768-dim embeddings
    ///
    /// Uses brute-force vector search by default.
    ///
    /// # Panics
    ///
    /// This never panics - all initialization is infallible.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndexImpl::BruteForce(VectorIndex::new(
                DEFAULT_EMBEDDING_DIMENSION,
            )),
            complexity_cache: HashMap::new(),
            text_index: HashMap::new(),
            node_id_to_idx: HashMap::new(),
            node_tokens: HashMap::new(),
            search_cache: LruCache::new(NonZeroUsize::new(SEARCH_CACHE_MAX_ENTRIES).unwrap()),
            search_cache_bytes: 0,
            neural_weight: 0.4,
            neural_vector_index: None,
            fragment_vector_index: None,
            fragment_index_enabled: false,
            fragment_weight: 0.35,
            fragment_refs: HashMap::new(),
        }
    }

    /// Create a new search engine with custom embedding dimension
    ///
    /// Uses brute-force vector search by default.
    ///
    /// # Arguments
    ///
    /// * `dimension` - Embedding vector dimension (1-10000)
    ///
    /// # Panics
    ///
    /// Panics if dimension is 0 or exceeds MAX_EMBEDDING_DIMENSION.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let engine = SearchEngine::with_dimension(128);
    /// ```
    #[must_use]
    pub fn with_dimension(dimension: usize) -> Self {
        // Validate dimension at construction time
        if !(MIN_EMBEDDING_DIMENSION..=MAX_EMBEDDING_DIMENSION).contains(&dimension) {
            panic!(
                "Invalid embedding dimension: {} (must be between {} and {})",
                dimension, MIN_EMBEDDING_DIMENSION, MAX_EMBEDDING_DIMENSION
            );
        }

        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndexImpl::BruteForce(VectorIndex::new(dimension)),
            complexity_cache: HashMap::new(),
            text_index: HashMap::new(),
            node_id_to_idx: HashMap::new(),
            node_tokens: HashMap::new(),
            search_cache: LruCache::new(NonZeroUsize::new(SEARCH_CACHE_MAX_ENTRIES).unwrap()),
            search_cache_bytes: 0,
            neural_weight: 0.4,
            neural_vector_index: None,
            fragment_vector_index: None,
            fragment_index_enabled: false,
            fragment_weight: 0.35,
            fragment_refs: HashMap::new(),
        }
    }

    /// Set the neural-score weight used by the hybrid (None query_type) scoring
    /// arm. Loaded from `[search] neural_weight` in leindex.toml by the CLI
    /// layer (which can read config; this `search` crate cannot, to keep the
    /// dependency direction cli -> search). Clamped to [0, 1].
    pub fn set_neural_weight(&mut self, weight: f32) {
        let clamped = weight.clamp(0.0, 1.0);
        if (clamped - self.neural_weight).abs() > f32::EPSILON {
            self.search_cache.clear();
            self.search_cache_bytes = 0;
        }
        self.neural_weight = clamped;
    }

    /// Enable/disable the fragment layer (master switch, from `[search]
    /// fragment_index_enabled`). Clearing the result cache keeps a config flip
    /// from serving stale cached results.
    pub fn set_fragment_index_enabled(&mut self, enabled: bool) {
        if enabled != self.fragment_index_enabled {
            self.search_cache.clear();
            self.search_cache_bytes = 0;
        }
        self.fragment_index_enabled = enabled;
    }

    /// Set the fragment fusion weight (from `[search] fragment_weight`).
    pub fn set_fragment_weight(&mut self, weight: f32) {
        let clamped = weight.clamp(0.0, 1.0);
        if (clamped - self.fragment_weight).abs() > f32::EPSILON {
            self.search_cache.clear();
            self.search_cache_bytes = 0;
        }
        self.fragment_weight = clamped;
    }

    /// Populate the fragment reference map (content hash → ALL (owner node id,
    /// byte range) refs) from the cli-side fragment store (invariant 6 owner
    /// mapping, fragment-embeddings 1.11.0 Task 6).
    pub fn set_fragment_refs(&mut self, refs: HashMap<String, Vec<(String, (usize, usize))>>) {
        self.fragment_refs = refs;
    }

    /// Index nodes for searching
    ///
    /// This builds the internal indexes needed for search:
    /// - Text search index (stored in self.nodes)
    /// - Vector index (built from embeddings)
    /// - Complexity cache (for O(1) complexity lookups)
    ///
    /// # Arguments
    ///
    /// * `nodes` - Vector of nodes to index
    ///
    /// # Performance
    ///
    /// - Time complexity: O(n) where n is number of nodes
    /// - Space complexity: O(n) for storage + O(n) for embeddings
    ///
    /// # Panics
    ///
    /// Panics if node count exceeds MAX_NODES (prevents memory exhaustion).
    pub fn index_nodes(&mut self, nodes: Vec<NodeInfo>) {
        if nodes.len() > MAX_NODES {
            panic!(
                "Cannot index more than {} nodes (provided: {})",
                MAX_NODES,
                nodes.len()
            );
        }

        // Clear all state for a full reindex
        self.clear_index();
        // Append nodes incrementally
        self.append_nodes(nodes);
    }

    /// Clear all indexed nodes and internal data structures.
    ///
    /// Called before a full reindex to reset the search engine state.
    pub fn clear_index(&mut self) {
        self.nodes.clear();
        self.complexity_cache.clear();
        self.text_index.clear();
        self.search_cache.clear();
        self.search_cache_bytes = 0;
        self.node_id_to_idx.clear();
        self.node_tokens.clear();
        self.vector_index.clear();
        // Also drop the lazy-paged neural ANN so a stale mmap isn't reused
        // after a full reindex rebuilds the lexical index from scratch.
        self.neural_vector_index = None;
        self.fragment_vector_index = None;
        // And drop the fragment owner refs — a full reindex rebuilds the
        // fragment layer too, so stale hash → owner mappings must not survive
        // (phantom fragment hits after reindex; Kilo wave-2 item 6).
        self.fragment_refs.clear();
    }

    /// Append nodes to the existing index without clearing.
    ///
    /// This supports incremental/batch indexing where nodes are processed
    /// in chunks and appended to the search engine across multiple calls.
    /// Internal data structures (inverted index, vector index, caches) are
    /// updated incrementally.
    ///
    /// # Panics
    ///
    /// Panics if total node count exceeds MAX_NODES after appending.
    pub fn append_nodes(&mut self, mut nodes: Vec<NodeInfo>) {
        // Deduplicate nodes by node_id. Duplicate IDs can occur when the parser
        // produces unqualified names (e.g., two `fn new()` in different impls
        // of the same file both resolve to qualified_name "new"). Panicking on
        // a parser limitation would crash production indexing. Instead, keep the
        // first occurrence and silently drop subsequent duplicates.
        let mut kept: HashSet<String> = HashSet::new();
        let original_len = nodes.len();
        nodes.retain(|n| {
            if self.node_id_to_idx.contains_key(&n.node_id) {
                return false;
            }
            kept.insert(n.node_id.clone())
        });
        let dropped = original_len - nodes.len();
        if dropped > 0 {
            tracing::warn!(
                "append_nodes: dropped {} duplicate node_id(s) (kept {} of {})",
                dropped,
                nodes.len(),
                original_len
            );
        }

        if self.nodes.len() + nodes.len() > MAX_NODES {
            panic!(
                "Cannot index more than {} nodes (current: {}, appending: {})",
                MAX_NODES,
                self.nodes.len(),
                nodes.len()
            );
        }

        self.search_cache.clear();
        self.search_cache_bytes = 0;

        // Build node_id_to_idx for O(1) node lookups (A1 optimization)
        // Build complexity cache, inverted index, and token cache before taking ownership
        for (idx, node) in nodes.iter().enumerate() {
            let global_idx = self.nodes.len() + idx;
            self.node_id_to_idx.insert(node.node_id.clone(), global_idx);
            self.complexity_cache
                .insert(node.node_id.clone(), node.complexity);

            // Build inverted index for O(1) text lookups
            // This maps each token to the set of node IDs containing it
            // Also build per-node token cache for scoring (T14 optimization)
            //
            // R8: Use pre-tokenized tokens when available to skip re-tokenization.
            // Falls back to content-based tokenization for backward compatibility.
            let mut tokens = HashSet::new();
            if let Some(pre_tok) = &node.pre_tokenized {
                // Use pre-computed tokens directly (already lowercased, filtered >= 2 chars)
                for token in pre_tok {
                    self.text_index
                        .entry(token.clone())
                        .or_default()
                        .insert(node.node_id.clone());
                    tokens.insert(token.clone());
                }
            } else {
                for token in node.content.split(|c: char| !c.is_alphanumeric()) {
                    let normalized_token: String = token.to_ascii_lowercase();
                    // Skip empty tokens and very short ones (< 2 chars) to reduce noise
                    if normalized_token.len() >= 2 {
                        self.text_index
                            .entry(normalized_token.clone())
                            .or_default()
                            .insert(node.node_id.clone());
                        tokens.insert(normalized_token);
                    }
                }
            }
            self.node_tokens.insert(node.node_id.clone(), tokens);
        }

        // Build vector index from TF-IDF embeddings — clone only embeddings (A4 optimization)
        // All other node content is moved via ownership, avoiding a full Vec clone
        for node in nodes.iter_mut() {
            // Use tfidf_embedding (always present) instead of optional embedding
            if !node.tfidf_embedding.is_empty() {
                if let Err(e) = self
                    .vector_index
                    .insert(node.node_id.clone(), node.tfidf_embedding.clone())
                {
                    tracing::warn!(
                        "Failed to insert TF-IDF embedding for node {}: {:?}",
                        node.node_id,
                        e
                    );
                }
            }
        }

        // Extract signatures before clearing content (for search results)
        // This must happen before T13 optimization clears the content
        for node in &mut nodes {
            if node.signature.is_none() {
                node.signature = Self::extract_signature_from_content(&node.content);
            }
        }

        // Free content memory after all indexes are built (T13 optimization)
        // The inverted index (text_index) already captures all tokens,
        // and the Storage layer retains original source files on disk.
        // This reduces memory by ~15MB at 5K nodes.
        for node in &mut nodes {
            node.content.clear();
        }

        // Append nodes to storage
        self.nodes.extend(nodes);
    }

    /// Restore neural embeddings from a persisted mmap embedding index.
    ///
    /// This updates the `neural_embedding` field on each node that has a
    /// matching entry in the mmap index. Nodes without a matching entry
    /// retain their existing neural embedding (typically `None`).
    ///
    /// Returns the number of nodes that were updated.
    pub fn restore_neural_embeddings(
        &mut self,
        mmap_index: &crate::search::vector::MmapEmbeddingIndex,
    ) -> usize {
        let mut updated = 0;
        for node in &mut self.nodes {
            if let Some(embedding) = mmap_index.get_embedding(&node.node_id) {
                if !embedding.is_empty() {
                    node.neural_embedding = Some(embedding);
                    updated += 1;
                }
            }
        }
        tracing::info!(
            "Restored {} neural embeddings from mmap ({} total nodes)",
            updated,
            self.nodes.len()
        );
        updated
    }

    /// Add or replace neural rows after the TF-IDF index is already queryable.
    ///
    /// Neural enrichment is deliberately a separate mutation: the core
    /// lexical/vector index can be published first, then this bounded delta is
    /// applied and persisted as a later immutable generation.
    pub fn update_neural_embeddings(
        &mut self,
        embeddings: impl IntoIterator<Item = (String, Vec<f32>)>,
    ) -> usize {
        let mut updated = 0;
        for (node_id, embedding) in embeddings {
            if embedding.is_empty() {
                continue;
            }
            if let Some(&idx) = self.node_id_to_idx.get(&node_id) {
                if let Some(node) = self.nodes.get_mut(idx) {
                    node.neural_embedding = Some(embedding);
                    updated += 1;
                }
            }
        }
        if updated > 0 {
            self.search_cache.clear();
            self.search_cache_bytes = 0;
        }
        updated
    }

    /// Remove all neural rows while retaining the mandatory lexical index.
    pub fn clear_neural_embeddings(&mut self) {
        let mut changed = false;
        for node in &mut self.nodes {
            changed |= node.neural_embedding.take().is_some();
        }
        if changed {
            self.search_cache.clear();
            self.search_cache_bytes = 0;
        }
    }

    /// Extract signature from node content.
    ///
    /// Returns the first non-empty, non-comment line after the header.
    pub fn extract_signature_from_content(content: &str) -> Option<String> {
        content
            .lines()
            .skip(1) // skip "// name in path" header
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with("// [No source") && !l.starts_with("// ["))
            .map(|l| l.to_string())
    }

    /// Apply an incremental delta update to the text index.
    ///
    /// This removes and adds/updates nodes without rebuilding the entire index,
    /// making it significantly faster than `index_nodes()` for small changes.
    ///
    /// # Arguments
    ///
    /// * `delta` - The delta describing nodes to remove and add/update.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(K) where K is the number of changed nodes
    /// - Full rebuild is O(N) — incremental is faster when K << N
    ///
    /// # Panics
    ///
    /// Panics if the total node count after the update exceeds `MAX_NODES`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let delta = TextIndexDelta {
    ///     removed_node_ids: vec!["old_func".to_string()],
    ///     updated_nodes: vec![new_node],
    /// };
    /// engine.incremental_reindex(delta);
    /// ```
    pub fn incremental_reindex(&mut self, delta: TextIndexDelta) {
        // Invalidate search cache — results may change
        self.search_cache.clear();
        self.search_cache_bytes = 0;

        // Phase 1: Remove nodes
        for node_id in &delta.removed_node_ids {
            self.remove_node_from_index(node_id);
        }

        // Phase 2: Add/update nodes
        for node in delta.updated_nodes {
            self.add_node_to_index(node);
        }

        // Verify we don't exceed limits
        if self.nodes.len() > MAX_NODES {
            panic!(
                "Cannot index more than {} nodes (current: {})",
                MAX_NODES,
                self.nodes.len()
            );
        }
    }

    /// Remove a single node from all index structures.
    ///
    /// This is O(T) where T is the number of unique tokens in the removed node.
    fn remove_node_from_index(&mut self, node_id: &str) {
        // Remove from node_id_to_idx
        let Some(removed_idx) = self.node_id_to_idx.remove(node_id) else {
            return; // Node not in index, nothing to do
        };

        // Remove from text_index: for each token the node contributed to,
        // remove the node_id from the token's set. Clean up empty sets.
        if let Some(tokens) = self.node_tokens.remove(node_id) {
            for token in tokens {
                if let Entry::Occupied(mut entry) = self.text_index.entry(token) {
                    entry.get_mut().remove(node_id);
                    if entry.get().is_empty() {
                        entry.remove();
                    }
                }
            }
        }

        // Remove from complexity_cache
        self.complexity_cache.remove(node_id);

        // Remove from vector_index
        self.vector_index.remove(node_id);

        // Remove from nodes Vec and fix indices
        // Swap-remove for O(1) removal, then fix the swapped node's index
        if removed_idx < self.nodes.len() {
            self.nodes.swap_remove(removed_idx);
            // If we didn't remove the last element, the swapped element needs
            // its index updated in node_id_to_idx
            if removed_idx < self.nodes.len() {
                let swapped_id = self.nodes[removed_idx].node_id.clone();
                self.node_id_to_idx.insert(swapped_id, removed_idx);
            }
        }
    }

    /// Add or update a single node in all index structures.
    ///
    /// If the node already exists (same `node_id`), it is removed first, then
    /// re-added with the new data.
    fn add_node_to_index(&mut self, mut node: NodeInfo) {
        // If node already exists, remove the old version first
        if self.node_id_to_idx.contains_key(&node.node_id) {
            self.remove_node_from_index(&node.node_id);
        }

        let node_id = node.node_id.clone();
        let new_idx = self.nodes.len();

        // Build inverted index entries and token cache for this node
        //
        // R8: Use pre-tokenized tokens when available to skip re-tokenization.
        // Falls back to content-based tokenization for backward compatibility.
        let mut tokens = HashSet::new();
        if let Some(pre_tok) = &node.pre_tokenized {
            for token in pre_tok {
                self.text_index
                    .entry(token.clone())
                    .or_default()
                    .insert(node_id.clone());
                tokens.insert(token.clone());
            }
        } else {
            for token in node.content.split(|c: char| !c.is_alphanumeric()) {
                let normalized_token: String = token.to_ascii_lowercase();
                if normalized_token.len() >= 2 {
                    self.text_index
                        .entry(normalized_token.clone())
                        .or_default()
                        .insert(node_id.clone());
                    tokens.insert(normalized_token);
                }
            }
        }
        self.node_tokens.insert(node_id.clone(), tokens);

        // Update node_id_to_idx
        self.node_id_to_idx.insert(node_id.clone(), new_idx);

        // Update complexity_cache
        self.complexity_cache
            .insert(node_id.clone(), node.complexity);

        // Insert TF-IDF embedding into vector index (always present)
        if !node.tfidf_embedding.is_empty() {
            if let Err(e) = self
                .vector_index
                .insert(node_id.clone(), node.tfidf_embedding.clone())
            {
                tracing::warn!(
                    "Failed to insert TF-IDF embedding for node {}: {:?}",
                    node_id,
                    e
                );
            }
        }

        // Extract signature before clearing content (same as index_nodes does).
        // Hydrated nodes can already carry a persisted signature with empty
        // content, so preserve it when present.
        if node.signature.is_none() {
            node.signature = Self::extract_signature_from_content(&node.content);
        }

        // Clear content to save memory (same as index_nodes does)
        node.content.clear();

        // Add to nodes Vec
        self.nodes.push(node);
    }

    /// Get the number of indexed nodes
    ///
    /// # Returns
    ///
    /// The number of nodes currently indexed.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Collect all (node_id, embedding) pairs from the indexed nodes.
    ///
    /// Returns only nodes that have a TF-IDF embedding. Used by the mmap
    /// persistence layer to write embeddings to disk.
    pub fn collect_embeddings(&self) -> Vec<(String, Vec<f32>)> {
        self.nodes
            .iter()
            .filter_map(|node| {
                if !node.tfidf_embedding.is_empty() {
                    Some((node.node_id.clone(), node.tfidf_embedding.clone()))
                } else {
                    self.vector_index
                        .embedding(&node.node_id)
                        .map(|embedding| (node.node_id.clone(), embedding))
                }
            })
            .collect()
    }

    /// Collect neural embeddings for persistence.
    ///
    /// Returns `(node_id, neural_embedding)` pairs for all nodes that have
    /// non-empty neural embeddings.
    pub fn collect_neural_embeddings(&self) -> Vec<(String, Vec<f32>)> {
        self.nodes
            .iter()
            .filter_map(|n| {
                // Prefer the in-memory per-node embedding; after snapshot
                // hydration the neural vectors live in the mmap-backed index
                // (no heap mirror), so fall back to it to avoid underreporting
                // — collect is also used to persist neural embeddings.
                let embedding = n
                    .neural_embedding
                    .as_ref()
                    .filter(|e| !e.is_empty())
                    .cloned()
                    .or_else(|| {
                        self.neural_vector_index
                            .as_ref()
                            .and_then(|idx| idx.embedding(&n.node_id))
                    });
                embedding.map(|e| (n.node_id.clone(), e))
            })
            .collect()
    }

    /// Populate the fragment vector index from freshly embedded rows at
    /// INDEX time (fragment-embeddings 1.11.0 Task 7).
    ///
    /// The sync engine embeds only content hashes missing from the store and
    /// hands the `(content_hash, embedding)` pairs here; they are inserted
    /// into a BruteForce index so `collect_fragment_embeddings` + the query
    /// path work immediately (hydration later swaps in the Mmap-backed twin).
    /// Empty input clears the layer. Dimension is taken from the first row.
    pub fn set_fragment_embeddings(&mut self, rows: Vec<(String, Vec<f32>)>) {
        let dimension = rows.first().map(|(_, emb)| emb.len()).unwrap_or(0);
        if dimension == 0 || rows.is_empty() {
            self.fragment_vector_index = None;
        } else {
            let mut index = VectorIndex::new(dimension);
            index.insert_batch(rows);
            self.fragment_vector_index = Some(VectorIndexImpl::BruteForce(index));
        }
        // Clear the owner refs whenever the index is replaced or cleared —
        // stale hash → owner mappings for rows no longer in the index would
        // surface phantom fragment hits (Kilo wave-2 item 7). The cli caller
        // re-populates refs from the fresh store right after.
        self.fragment_refs.clear();
        self.search_cache.clear();
        self.search_cache_bytes = 0;
    }

    /// Collect fragment embeddings for persistence.
    ///
    /// Returns `(content_hash, embedding)` pairs for every row in the fragment
    /// index — mmap-backed (hydration) OR BruteForce-backed (index time, Task
    /// 7). Empty when the fragment layer is off or no fragment index exists.
    pub fn collect_fragment_embeddings(&self) -> Vec<(String, Vec<f32>)> {
        match &self.fragment_vector_index {
            Some(VectorIndexImpl::Mmap(idx)) => idx.entries(),
            Some(VectorIndexImpl::BruteForce(idx)) => idx.entries(),
            _ => Vec::new(),
        }
    }

    /// Check if the index is empty
    ///
    /// # Returns
    ///
    /// `true` if no nodes are indexed, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    // ----------------------------------------------------------------
    // B-phase residency accessors (Plan 2)
    // ----------------------------------------------------------------

    /// Return the internal index position for a given node ID.
    ///
    /// Returns `None` if the node is not in the index. This is the
    /// row-oriented position used by the residency layer.
    pub fn node_index(&self, node_id: &str) -> Option<usize> {
        self.node_id_to_idx.get(node_id).copied()
    }

    /// Return the number of live (non-tombstoned) nodes in the index.
    ///
    /// Equivalent to `node_count()` but named for clarity in residency
    /// contexts where the distinction between live and tombstoned matters.
    pub fn live_node_count(&self) -> usize {
        self.node_id_to_idx.len()
    }

    /// Check whether a node ID is currently in the live index.
    pub fn contains_node(&self, node_id: &str) -> bool {
        self.node_id_to_idx.contains_key(node_id)
    }

    /// Return the node IDs currently in the live index.
    pub fn live_node_ids(&self) -> Vec<String> {
        self.node_id_to_idx.keys().cloned().collect()
    }

    /// Whether the base TF-IDF rows are already resident in an mmap index.
    pub fn is_mmap_backed(&self) -> bool {
        matches!(self.vector_index, VectorIndexImpl::Mmap(_))
    }

    /// Return the complexity score for a given node.
    pub fn node_complexity(&self, node_id: &str) -> Option<u32> {
        self.complexity_cache.get(node_id).copied()
    }

    /// Return the tokens associated with a given node.
    pub fn node_tokens(&self, node_id: &str) -> Option<&HashSet<String>> {
        self.node_tokens.get(node_id)
    }

    /// Check whether a token exists in the text index and, if so, which
    /// node IDs contain it.
    pub fn token_lookup(&self, token: &str) -> Option<&HashSet<String>> {
        self.text_index.get(token)
    }

    /// Return the number of entries currently in the search cache.
    ///
    /// Part of the B-phase memory accounting surface (VAL-BPHASE-024).
    pub fn search_cache_len(&self) -> usize {
        self.search_cache.len()
    }

    /// Return the tracked byte estimate for the search cache.
    ///
    /// Part of the B-phase memory accounting surface (VAL-BPHASE-024).
    pub fn search_cache_bytes(&self) -> usize {
        self.search_cache_bytes
    }

    /// Produce a compact, row-oriented snapshot of the resident search
    /// metadata.
    ///
    /// The returned [`CompactNodeMetadata`] uses compact integer-backed
    /// addressing (row indices as `u32`) instead of string-heavy
    /// identifier-based maps. This is the B-phase compressed resident
    /// state (VAL-BPHASE-041).
    pub fn compact_metadata(&self) -> CompactNodeMetadata {
        // Build compact node-id → row index map (u32 keys)
        let mut row_map: Vec<(String, u32)> = Vec::with_capacity(self.nodes.len());
        let mut complexity_by_row: Vec<u32> = Vec::with_capacity(self.nodes.len());

        for (idx, node) in self.nodes.iter().enumerate() {
            row_map.push((node.node_id.clone(), idx as u32));
            complexity_by_row.push(node.complexity);
        }

        // Build compact token → set-of-rows index
        let mut token_rows: HashMap<String, HashSet<u32>> = HashMap::new();
        for (token, node_ids) in &self.text_index {
            let mut rows = HashSet::new();
            for node_id in node_ids {
                if let Some(&idx) = self.node_id_to_idx.get(node_id) {
                    rows.insert(idx as u32);
                }
            }
            token_rows.insert(token.clone(), rows);
        }

        CompactNodeMetadata {
            row_map,
            complexity_by_row,
            token_index: CompactTokenIndex { token_rows },
        }
    }

    /// Validate internal coherence of all index structures.
    ///
    /// Returns `Ok(())` if all structures agree, or a description of the
    /// first incoherence found. Used by residency tests to verify that
    /// compaction and delta updates maintain structural coherence.
    pub fn validate_coherence(&self) -> Result<(), String> {
        // node_id_to_idx and nodes must agree on count
        if self.node_id_to_idx.len() != self.nodes.len() {
            return Err(format!(
                "node_id_to_idx len ({}) != nodes len ({})",
                self.node_id_to_idx.len(),
                self.nodes.len()
            ));
        }

        // Every entry in node_id_to_idx must point to the correct node
        for (id, &idx) in &self.node_id_to_idx {
            if idx >= self.nodes.len() {
                return Err(format!(
                    "node_id_to_idx[{}] = {} >= nodes.len() = {}",
                    id,
                    idx,
                    self.nodes.len()
                ));
            }
            if self.nodes[idx].node_id != *id {
                return Err(format!(
                    "nodes[{}].node_id = '{}' != node_id_to_idx key '{}'",
                    idx, self.nodes[idx].node_id, id
                ));
            }
        }

        // complexity_cache must have an entry for every live node
        for node in &self.nodes {
            match self.complexity_cache.get(&node.node_id) {
                Some(c) if *c == node.complexity => {}
                Some(c) => {
                    return Err(format!(
                        "complexity_cache[{}] = {} != node.complexity = {}",
                        node.node_id, c, node.complexity
                    ));
                }
                None => {
                    return Err(format!(
                        "complexity_cache missing entry for {}",
                        node.node_id
                    ));
                }
            }
        }

        // node_tokens must have an entry for every live node
        for node in &self.nodes {
            if !self.node_tokens.contains_key(&node.node_id) {
                return Err(format!("node_tokens missing entry for {}", node.node_id));
            }
        }

        // text_index must not reference removed nodes
        for (token, node_ids) in &self.text_index {
            for id in node_ids {
                if !self.node_id_to_idx.contains_key(id) {
                    return Err(format!(
                        "text_index token '{}' references non-live node '{}'",
                        token, id
                    ));
                }
            }
        }

        Ok(())
    }

    /// Execute a search query
    ///
    /// This performs a hybrid search combining:
    /// - Text matching (substring + token overlap)
    /// - Semantic similarity (if embeddings available)
    /// - Structural relevance (complexity-based)
    ///
    /// # Arguments
    ///
    /// * `query` - Search query with all parameters
    ///
    /// # Returns
    ///
    /// Vector of search results sorted by relevance (highest first).
    ///
    /// # Performance
    ///
    /// - Time complexity: O(n) where n is number of nodes
    /// - Space complexity: O(k) where k is top_k (results)
    ///
    /// # Errors
    ///
    /// Returns `Error::QueryFailed` if the search operation fails.
    pub fn search(&mut self, query: SearchQuery) -> Result<Vec<SearchResult>, Error> {
        if self.nodes.is_empty() {
            return Ok(Vec::new());
        }

        // Check cache first
        let cache_key = format!(
            "{}:{}:{:?}:{}:{:?}:neural={}",
            query.query,
            query.top_k,
            query.threshold,
            query.semantic,
            query.query_type,
            query.query_neural_embedding.is_some()
        );
        if let Some(cached) = self.search_cache.get(&cache_key) {
            return Ok(cached.clone());
        }

        let mut results = Vec::new();

        // Pre-compute vector search if semantic search is requested
        let vector_results = self.compute_vector_results(&query);

        // Pre-compute query data for optimized text scoring
        // This reduces allocations from O(N) to O(1) per search
        let text_query = TextQueryPreprocessed::from_query(&query.query);

        // Use inverted index to filter candidates - only check nodes that contain query terms
        // This reduces search complexity from O(N) to O(M) where M is number of matching nodes
        // Semantic retrieval: query the lazy-paged neural mmap ANN (built at
        // search-load from neural_embeddings) and union its top-K into the
        // candidate pool. This reuses MmapVectorIndex::search — the same top-K
        // mechanism as the tfidf vector_index — so there is no hand-rolled scan,
        // no per-query O(N) allocation/sort, and no in-memory duplication (the
        // index is lazy-paged). This is what makes conceptual queries work:
        // nodes with ZERO lexical overlap but high semantic relevance enter
        // scoring. Falls back to empty when there is no neural index (tfidf-only)
        // or no query embedding (Exact route).
        let neural_candidates: HashSet<String> =
            match (&query.query_neural_embedding, &self.neural_vector_index) {
                (Some(q_emb), Some(idx)) => idx
                    .search(q_emb, query.top_k.saturating_mul(10).max(100))
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect(),
                _ => HashSet::new(),
            };

        // Fragment layer (Task 6, invariant 6): query the fragment ANN with the
        // neural query embedding and map content-hash hits back to their Tier-1
        // owner nodes. Owners enter the candidate pool and carry the best
        // fragment byte range for result surfacing. Participates only when the
        // master switch is on AND a neural query embedding exists AND the query
        // is not Exact-route (exact identifier lookups stay purely lexical —
        // the CLI zeroes the neural embedding for them; this guard is
        // defense-in-depth for direct `search()` callers, Codex wave-2 item 4).
        // (CCN extraction — the whole fragment-owner mapping lives in
        // `collect_fragment_owners` to keep `search` under the lizard gate.)
        let (fragment_owner_scores, fragment_owner_ranges) = self.collect_fragment_owners(
            &query.query_neural_embedding,
            query.query_type,
            query.top_k,
        );

        // Fragment fusion RENORMALIZATION gate (Codex wave-2 item 4): renorm
        // must be gated on the fragment layer ACTUALLY participating in this
        // query — master switch ON AND at least one fragment candidate mapped
        // to an owner. Gating on the config switch alone would downscale the
        // four base weights by 1/(1+fragment_weight) even when every fragment
        // score is necessarily 0 (exact/neural=None queries; or a hydrated
        // layer whose mmap/refs are unavailable), silently distorting the
        // default path. `fragment_owner_scores` being non-empty is exactly the
        // participation signal (it is populated only when candidates mapped).
        let fragment_active = self.fragment_index_enabled && !fragment_owner_scores.is_empty();

        let candidates = self.collect_search_candidates(
            &text_query,
            &vector_results,
            &neural_candidates,
            &fragment_owner_scores,
            query.semantic,
        );

        for node in candidates {
            if let Some(result) = self.score_and_collect(
                node,
                &query,
                &text_query,
                &vector_results,
                &fragment_owner_scores,
                &fragment_owner_ranges,
                fragment_active,
            ) {
                results.push(result);
            }
        }

        let final_results = self.finalize_results(results, query.top_k, cache_key);
        Ok(final_results)
    }
    /// Query the fragment ANN with the neural query embedding and map
    /// content-hash hits back to their Tier-1 owner nodes (Task 6, invariant
    /// 6). Returns `(owner → best fragment score, owner → byte range of that
    /// best fragment)`.
    ///
    /// Participates only when the master switch is on AND a neural query
    /// embedding AND the owner refs exist AND the query is not Exact-route
    /// (exact identifier lookups stay purely lexical — the CLI zeroes the
    /// neural embedding for them; this guard is defense-in-depth for direct
    /// `search()` callers, Codex wave-2 item 4); otherwise both maps are
    /// empty. ALL owners of a content hash are preserved (identical fragment
    /// text is embedded once but can be referenced by N owners — Codex wave-2
    /// item 5), and the BEST-scoring fragment per owner is kept so the
    /// surfaced byte range corresponds to the fragment that actually drives
    /// the score, independent of HashMap iteration order (invariant 6).
    /// Extracted as a helper to keep `search` under the lizard CCN gate.
    fn collect_fragment_owners(
        &self,
        query_neural_embedding: &Option<Vec<f32>>,
        query_type: Option<crate::search::ranking::QueryType>,
        top_k: usize,
    ) -> (HashMap<String, f32>, HashMap<String, (usize, usize)>) {
        let mut fragment_owner_scores: HashMap<String, f32> = HashMap::new();
        let mut fragment_owner_ranges: HashMap<String, (usize, usize)> = HashMap::new();
        let exact_route = matches!(query_type, Some(crate::search::ranking::QueryType::Exact));
        let fragment_candidates: HashMap<String, f32> =
            match (query_neural_embedding, &self.fragment_vector_index) {
                (Some(q_emb), Some(idx))
                    if !exact_route
                        && self.fragment_index_enabled
                        && !self.fragment_refs.is_empty() =>
                {
                    idx.search(q_emb, top_k.saturating_mul(10).max(100))
                        .into_iter()
                        .collect()
                }
                _ => HashMap::new(),
            };
        if !fragment_candidates.is_empty() && !self.fragment_refs.is_empty() {
            for (hash, score) in &fragment_candidates {
                if let Some(refs) = self.fragment_refs.get(hash) {
                    for (owner, range) in refs {
                        let entry = fragment_owner_scores.entry(owner.clone()).or_insert(0.0);
                        if *score > *entry {
                            *entry = *score;
                            fragment_owner_ranges.insert(owner.clone(), *range);
                        }
                    }
                }
            }
        }
        (fragment_owner_scores, fragment_owner_ranges)
    }

    /// Pre-compute TF-IDF vector search results for semantic queries using the
    /// caller-provided query embedding. Returns empty when semantic retrieval
    /// has no query vector or the query is non-semantic.
    fn compute_vector_results(&self, query: &SearchQuery) -> HashMap<String, f32> {
        if !query.semantic {
            return HashMap::new();
        }
        let Some(embedding) = query.query_embedding.as_ref() else {
            // Semantic retrieval without a query vector has no valid vector
            // space to search. Do not substitute an arbitrary node vector.
            return HashMap::new();
        };
        // Over-fetch beyond top_k for good coverage of relevant nodes.
        let vector_search_k = query.top_k.saturating_mul(10).max(100);
        self.vector_index
            .search(embedding, vector_search_k)
            .into_iter()
            .collect()
    }

    /// Build the candidate node pool: all nodes when the query has no tokens,
    /// otherwise the union of inverted-index text matches, TF-IDF vector hits,
    /// and neural top-K. Returns empty when nothing matches and the query is
    /// non-semantic (semantic queries with no matches scan all nodes).
    fn collect_search_candidates<'a>(
        &'a self,
        text_query: &TextQueryPreprocessed,
        vector_results: &HashMap<String, f32>,
        neural_candidates: &HashSet<String>,
        fragment_owner_scores: &HashMap<String, f32>,
        semantic: bool,
    ) -> Vec<&'a NodeInfo> {
        if text_query.query_tokens.is_empty() {
            return self.nodes.iter().collect();
        }
        let mut candidate_ids: HashSet<&str> = HashSet::new();
        for token in &text_query.query_tokens {
            if let Some(node_ids) = self.text_index.get(token) {
                for node_id in node_ids {
                    candidate_ids.insert(node_id.as_str());
                }
            }
        }
        let no_matches = candidate_ids.is_empty()
            && neural_candidates.is_empty()
            && fragment_owner_scores.is_empty()
            && vector_results.is_empty();
        if no_matches && !semantic {
            return Vec::new();
        }
        if no_matches {
            // Semantic search with no lexical/vector matches — scan all nodes.
            self.nodes.iter().collect()
        } else {
            self.nodes
                .iter()
                .filter(|node| {
                    candidate_ids.contains(node.node_id.as_str())
                        || vector_results.contains_key(&node.node_id)
                        || neural_candidates.contains(&node.node_id)
                        || fragment_owner_scores.contains_key(node.node_id.as_str())
                })
                .collect()
        }
    }

    /// Execute a staged retrieval search: coarse candidate generation followed
    /// by exact rerank (Plan 2 — VAL-BPHASE-044, VAL-BPHASE-045).
    ///
    /// When `config.enabled` is `false`, this falls back to the standard
    /// [`search`](Self::search) path and reports `staged_used: false`.
    ///
    /// When enabled, the pipeline is:
    /// 1. **Coarse phase**: Retrieve `top_k * coarse_multiplier` candidates
    ///    using TF-IDF vector similarity only (cheap).
    /// 2. **Exact rerank phase**: Apply the full hybrid scoring (text +
    ///    TF-IDF + structural) only to the coarse candidate set, then return
    ///    the top-K results.
    ///
    /// This reduces exact-stage work without replacing the approved
    /// INT8/default quality-gated path with binary-quantization-first search.
    /// The staged path is opt-in; the existing `search()` method remains the
    /// authoritative default.
    ///
    /// # Returns
    ///
    /// A tuple of `(results, metrics)` where `metrics` carries observability
    /// data about the staged pipeline (candidate counts, whether staged was
    /// used, etc.).
    ///
    /// # Errors
    ///
    /// Same error conditions as [`search`](Self::search).
    pub fn search_staged(
        &mut self,
        query: SearchQuery,
        config: &StagedRetrievalConfig,
    ) -> Result<(Vec<SearchResult>, StagedRetrievalMetrics), Error> {
        // If staged retrieval is disabled, fall back to the standard path.
        if !config.enabled {
            let results = self.search(query)?;
            let count = results.len();
            return Ok((
                results,
                StagedRetrievalMetrics {
                    coarse_candidates: 0,
                    exact_scored: count,
                    results_returned: count,
                    staged_used: false,
                },
            ));
        }

        if self.nodes.is_empty() {
            return Ok((
                Vec::new(),
                StagedRetrievalMetrics {
                    staged_used: true,
                    ..Default::default()
                },
            ));
        }

        // Check staged-search cache (key includes query, top_k, threshold, semantic, coarse_multiplier, query_type)
        let cache_key = format!(
            "staged:{}:{}:{:?}:{}:{:?}:{:?}:neural={}",
            query.query,
            query.top_k,
            query.threshold,
            query.semantic,
            config.coarse_multiplier,
            query.query_type,
            query.query_neural_embedding.is_some()
        );
        if let Some(cached) = self.search_cache.get(&cache_key) {
            let count = cached.len();
            return Ok((
                cached.clone(),
                StagedRetrievalMetrics {
                    coarse_candidates: 0,
                    exact_scored: count,
                    results_returned: count,
                    staged_used: true,
                },
            ));
        }

        let mut metrics = StagedRetrievalMetrics {
            staged_used: true,
            ..Default::default()
        };

        // ====================================================================
        // Phase 1: Coarse candidate generation
        //
        // Use TF-IDF vector similarity AND text-index lookup to retrieve a
        // larger candidate set. This is cheap because vector search computes
        // cosine similarity against the index without text/structural scoring,
        // and text lookup is O(1) per token via the inverted index.
        //
        // The coarse candidate set is the UNION of vector-similarity hits and
        // text-index hits, ensuring that nodes relevant by either signal are
        // included for the exact rerank phase.
        // ====================================================================
        let coarse_top_k = query.top_k.saturating_mul(config.coarse_multiplier);

        // Start with text-index candidates (always included)
        let text_query = TextQueryPreprocessed::from_query(&query.query);
        let mut coarse_candidate_ids: HashSet<String> = HashSet::new();
        for token in &text_query.query_tokens {
            if let Some(node_ids) = self.text_index.get(token) {
                for id in node_ids {
                    coarse_candidate_ids.insert(id.clone());
                }
            }
        }

        // Add vector-similarity candidates if semantic search is requested
        let vector_results: HashMap<String, f32> = if query.semantic {
            if let Some(ref emb) = query.query_embedding {
                let vec_hits = self.vector_index.search(emb, coarse_top_k);
                for (id, _) in &vec_hits {
                    coarse_candidate_ids.insert(id.clone());
                }
                vec_hits.into_iter().collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        metrics.coarse_candidates = coarse_candidate_ids.len();

        // If no coarse candidates, return early
        if coarse_candidate_ids.is_empty() {
            return Ok((Vec::new(), metrics));
        }

        // ====================================================================
        // Phase 2: Exact rerank
        //
        // Apply the full hybrid scoring (text + TF-IDF + structural) only to
        // the reduced candidate set from the coarse phase.
        // ====================================================================

        let mut results = Vec::new();

        for node in &self.nodes {
            // Only score nodes that passed the coarse filter
            if !coarse_candidate_ids.contains(&node.node_id) {
                continue;
            }
            // The staged (coarse-then-exact) path does not fuse fragment
            // candidates (Task 6 wires the authoritative `search` path); empty
            // fragment maps + fragment_active=false keep its behavior
            // byte-identical to before (no fragment renorm either).
            if let Some(result) = self.score_and_collect(
                node,
                &query,
                &text_query,
                &vector_results,
                &HashMap::new(),
                &HashMap::new(),
                false,
            ) {
                results.push(result);
            }
        }

        metrics.exact_scored = results.len();

        let final_results = self.finalize_results(results, query.top_k, cache_key);

        metrics.results_returned = final_results.len();

        Ok((final_results, metrics))
    }
    /// Score a single node against the query and return a `SearchResult` when it
    /// clears the skip-zero, composite-score, and optional threshold gates.
    /// Shared by `search` and `search_staged` (single source of truth).
    fn score_and_collect(
        &self,
        node: &NodeInfo,
        query: &SearchQuery,
        text_query: &TextQueryPreprocessed,
        vector_results: &HashMap<String, f32>,
        fragment_scores: &HashMap<String, f32>,
        fragment_ranges: &HashMap<String, (usize, usize)>,
        fragment_active: bool,
    ) -> Option<SearchResult> {
        let text_score = self.calculate_text_score_optimized(
            text_query,
            &node.node_id,
            &node.symbol_name,
            &node.file_path,
        );

        // Get TF-IDF score from vector search results
        let tfidf_score = if query.semantic {
            *vector_results.get(&node.node_id).unwrap_or(&0.0)
        } else {
            0.0
        };

        // Skip nodes with no text match and no semantic contribution.
        if text_score == 0.0 && !query.semantic && tfidf_score == 0.0 {
            return None;
        }

        // Fragment score (Task 6): best fragment-similarity for this owner, or
        // 0.0 when the fragment layer is off / this node had no fragment hits.
        let fragment_score = fragment_scores.get(&node.node_id).copied().unwrap_or(0.0);

        // Compute composite score using the shared scoring logic.
        let score = self.compute_score(
            query,
            text_query,
            node,
            text_score,
            tfidf_score,
            fragment_score,
            fragment_active,
        );
        if score.overall <= 0.0 {
            return None;
        }

        // Apply relevance threshold if specified.
        if let Some(threshold) = query.threshold {
            if score.overall < threshold {
                return None;
            }
        }

        let signature = node.signature.clone();
        Some(SearchResult {
            rank: 0, // Assigned after sorting in finalize_results
            node_id: node.node_id.clone(),
            file_path: node.file_path.clone(),
            symbol_name: node.symbol_name.clone(),
            symbol_type: None, // enriched by LeIndex::search()
            signature,
            complexity: node.complexity,
            caller_count: None,     // enriched by LeIndex::search()
            dependency_count: None, // enriched by LeIndex::search()
            language: node.language.clone(),
            score,
            context: None,
            byte_range: node.byte_range,
            fragment_byte_range: fragment_ranges.get(&node.node_id).copied(),
            line_number: None, // enriched by LeIndex::search()
        })
    }

    /// Sort results by score (desc), truncate to `top_k`, assign ranks, and cache
    /// under `cache_key` with byte-budget LRU eviction. Shared finalization step.
    fn finalize_results(
        &mut self,
        mut results: Vec<SearchResult>,
        top_k: usize,
        cache_key: String,
    ) -> Vec<SearchResult> {
        // Sort by score (descending)
        results.sort_by(|a, b| {
            b.score
                .overall
                .partial_cmp(&a.score.overall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Take top_k and assign ranks.
        let mut final_results: Vec<SearchResult> = results.into_iter().take(top_k).collect();
        for (i, result) in final_results.iter_mut().enumerate() {
            result.rank = i + 1;
        }

        // Cache results with byte-budget enforcement.
        let results_bytes = Self::estimate_search_results_bytes(&final_results);
        // Guard: skip insertion if a single entry exceeds the cache budget.
        if results_bytes < SEARCH_CACHE_MAX_BYTES {
            // If replacing an existing entry, subtract its bytes first.
            // Remove an existing value before accounting for the replacement.
            if let Some(existing) = self.search_cache.pop(&cache_key) {
                self.search_cache_bytes = self
                    .search_cache_bytes
                    .saturating_sub(Self::estimate_search_results_bytes(&existing));
            }
            // Evict until there is room for the new value.
            while self.search_cache_bytes + results_bytes > SEARCH_CACHE_MAX_BYTES
                && !self.search_cache.is_empty()
            {
                if let Some((_, evicted)) = self.search_cache.pop_lru() {
                    self.search_cache_bytes = self
                        .search_cache_bytes
                        .saturating_sub(Self::estimate_search_results_bytes(&evicted));
                }
            }
            self.search_cache_bytes += results_bytes;
            if let Some((_, evicted)) = self.search_cache.push(cache_key, final_results.clone()) {
                self.search_cache_bytes = self
                    .search_cache_bytes
                    .saturating_sub(Self::estimate_search_results_bytes(&evicted));
            }
        }

        final_results
    }

    // ----------------------------------------------------------------
    // Shared scoring weights (VAL-QUALITY-005)
    //
    // Both `search()` and `search_staged()` delegate to `compute_score()`
    // so that the weight distribution lives in exactly one place.
    // ----------------------------------------------------------------

    /// Compute the composite [`Score`] for a single candidate node.
    ///
    /// This is the single source of truth for scoring weight selection.
    /// Both [`search`](Self::search) and [`search_staged`](Self::search_staged)
    /// call this method, ensuring the weight distributions cannot drift apart.
    ///
    /// The weight tuple `(tfidf, neural, structural, text)` is chosen based on:
    /// - `query.query_type` (Text, Exact, Semantic, Structural, or None)
    /// - Whether neural embeddings are available
    /// - Whether semantic (TF-IDF vector) search is enabled
    ///
    /// After computing the base composite score, the method applies:
    /// - Qualified-reference penalty (0.7x for `::` or `.` in symbol name)
    /// - Exact-name-match boost (stronger in Exact mode)
    /// - Partial-name-match boost (stronger in Exact mode)
    /// - Archive directory penalty (0.1x for archive paths)
    fn compute_score(
        &self,
        query: &SearchQuery,
        text_query: &TextQueryPreprocessed,
        node: &NodeInfo,
        text_score: f32,
        tfidf_score: f32,
        fragment_score: f32,
        fragment_active: bool,
    ) -> Score {
        let structural_score = (node.complexity as f32 / 100.0).min(1.0);
        let neural_score = self.neural_score(query, node);
        let neural_available = query.query_neural_embedding.is_some();
        let is_exact_mode = matches!(
            query.query_type,
            Some(crate::search::ranking::QueryType::Exact)
        );
        let (tfidf_weight, neural_weight, structural_weight, text_weight) =
            self.scoring_weights(query, neural_available);
        // Fragment fusion (Task 6): renormalize ONLY when the fragment layer
        // actually participates in this query (`fragment_active` — master
        // switch AND owner-score participation; Codex wave-2 item 4). NOT
        // `fragment_weight > 0` (the default 0.35 would renormalize with the
        // feature off) and NOT the bare config switch (a layer with no mmap /
        // refs / neural query embedding yields fragment score 0 for every
        // node, so renorm would distort the base weights for nothing). When
        // inactive the fragment weight is 0.0 and the base four are untouched.
        let (tfidf_weight, neural_weight, structural_weight, text_weight, fragment_weight) =
            if fragment_active {
                crate::search::ranking::HybridScorer::renormalize_weights(
                    tfidf_weight,
                    neural_weight,
                    structural_weight,
                    text_weight,
                    self.fragment_weight.clamp(0.0, 1.0),
                )
            } else {
                (
                    tfidf_weight,
                    neural_weight,
                    structural_weight,
                    text_weight,
                    0.0,
                )
            };
        let mut score = self
            .scorer
            .with_weights_hybrid5(
                tfidf_weight,
                neural_weight,
                structural_weight,
                text_weight,
                fragment_weight,
            )
            .score_hybrid5(
                tfidf_score,
                neural_score,
                structural_score,
                text_score,
                fragment_score,
            );

        Self::apply_score_adjustments(&mut score, text_query, node, is_exact_mode);
        score
    }

    fn neural_score(&self, query: &SearchQuery, node: &NodeInfo) -> f32 {
        let Some(query_embedding) = query.query_neural_embedding.as_ref() else {
            return 0.0;
        };
        self.neural_vector_index
            .as_ref()
            .and_then(|index| index.similarity(&node.node_id, query_embedding))
            .or_else(|| {
                node.neural_embedding
                    .as_ref()
                    .filter(|embedding| embedding.len() == query_embedding.len())
                    .map(|embedding| {
                        crate::search::vector::cosine_similarity(query_embedding, embedding)
                    })
            })
            .unwrap_or(0.0)
    }

    fn scoring_weights(&self, query: &SearchQuery, neural_available: bool) -> (f32, f32, f32, f32) {
        match query.query_type {
            Some(crate::search::ranking::QueryType::Text) => (0.2, 0.05, 0.05, 0.7),
            Some(crate::search::ranking::QueryType::Exact) if neural_available => {
                (0.15, 0.15, 0.25, 0.45)
            }
            Some(crate::search::ranking::QueryType::Exact) => (0.15, 0.0, 0.30, 0.55),
            Some(crate::search::ranking::QueryType::Semantic) if neural_available => {
                (0.3, 0.3, 0.1, 0.3)
            }
            Some(crate::search::ranking::QueryType::Semantic) => (0.55, 0.0, 0.15, 0.30),
            Some(crate::search::ranking::QueryType::Structural) => (0.3, 0.0, 0.5, 0.2),
            None if neural_available => {
                let remaining = 1.0 - self.neural_weight;
                (
                    remaining * 0.5,
                    self.neural_weight,
                    remaining * 0.25,
                    remaining * 0.25,
                )
            }
            None if query.semantic => (0.40, 0.0, 0.20, 0.40),
            None => (0.0, 0.0, 0.15, 0.85),
        }
    }

    fn apply_score_adjustments(
        score: &mut Score,
        text_query: &TextQueryPreprocessed,
        node: &NodeInfo,
        is_exact_mode: bool,
    ) {
        let symbol_lower = node.symbol_name.to_ascii_lowercase();
        let is_qualified_reference = symbol_lower.contains("::") || symbol_lower.contains('.');
        if is_qualified_reference {
            score.overall *= 0.7;
        } else if symbol_lower == text_query.query_lower {
            let boost = if is_exact_mode { 2.2 } else { 1.8 };
            score.overall = (score.overall * boost).min(1.0);
        } else if !symbol_lower.is_empty() && text_query.query_lower.contains(&symbol_lower) {
            let boost = if is_exact_mode { 1.5 } else { 1.3 };
            score.overall = (score.overall * boost).min(1.0);
        }

        if Self::is_archive_path(&node.file_path) {
            score.overall *= 0.1;
        }
    }

    /// Optimized text score calculation using cached node tokens and pre-computed query data
    ///
    /// Uses the node_tokens HashMap for O(1) token overlap calculation instead of
    /// iterating over the inverted index per query token. Tokens are cached during
    /// index_nodes() — no re-tokenization in the scoring hot path.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(min(q, t)) where q = query tokens, t = node tokens (set intersection)
    /// - Space complexity: O(1) — no allocations per call
    fn calculate_text_score_optimized(
        &self,
        precomputed: &TextQueryPreprocessed,
        node_id: &str,
        symbol_name: &str,
        file_path: &str,
    ) -> f32 {
        let symbol_lower = symbol_name.to_ascii_lowercase();

        // Detect fully-qualified external references (e.g., "crate::module::function_name").
        // These should rank lower than actual definitions.
        let is_qualified_ref = symbol_lower.contains("::") || symbol_lower.contains('.');

        // Exact symbol name match: maximum boost
        let symbol_boost = if symbol_lower == precomputed.query_lower {
            1.0
        } else if symbol_lower.contains(&precomputed.query_lower) {
            // Query is a substring of symbol name (e.g., query "tool_call" in "handle_tool_call")
            if is_qualified_ref {
                // Lower boost for qualified references (e.g., "crate.module.handle_tool_call")
                0.3
            } else {
                0.7
            }
        } else if precomputed.query_lower.contains(&symbol_lower) && !symbol_lower.is_empty() {
            // Symbol name is a substring of query (e.g., query "handle_tool_call" in "tool_call")
            0.4
        } else {
            0.0
        };

        // Penalty for test-related files to address Limitation 4
        let test_penalty =
            if file_path.to_ascii_lowercase().contains("test") || symbol_lower.contains("test") {
                0.3
            } else {
                0.0
            };

        // Use cached node tokens for overlap calculation (T14 optimization)
        // Tokens were cached during index_nodes() — no re-tokenization needed.
        // This avoids iterating over each query token and checking the inverted index,
        // replacing it with a single set intersection on pre-cached per-node tokens.
        let base_score = if precomputed.query_tokens.is_empty() {
            // No meaningful tokens in query
            0.0
        } else if let Some(node_tokens) = self.node_tokens.get(node_id) {
            // Count overlap between query tokens and cached node tokens
            let matching = precomputed.query_tokens.intersection(node_tokens).count();
            matching as f32 / precomputed.query_tokens.len() as f32
        } else {
            0.0
        };

        ((base_score + symbol_boost) - test_penalty).clamp(0.0, 1.0)
    }

    /// Semantic search for entry points
    ///
    /// This method performs vector similarity search using cosine similarity.
    /// For now, it requires pre-computed embeddings in the indexed nodes.
    ///
    /// # Arguments
    ///
    /// * `query_embedding` - Query embedding vector (must match index dimension)
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// Vector of semantic entries sorted by similarity score
    ///
    /// # Example
    ///
    /// ```ignore
    /// let query_embedding = vec![0.1, 0.2, 0.3, ...]; // 768-dim vector
    /// let results = engine.semantic_search(&query_embedding, 10).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Error::QueryFailed` if dimension mismatch or search fails.
    pub fn semantic_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SemanticEntry>, Error> {
        // Return early if index is empty (no need to validate dimensions in this case)
        if self.vector_index.is_empty() {
            return Ok(Vec::new());
        }

        // Validate embedding dimension (only needed when we actually have embeddings)
        if query_embedding.len() != self.vector_index.dimension() {
            return Err(Error::QueryFailed(format!(
                "Embedding dimension mismatch: expected {}, got {}",
                self.vector_index.dimension(),
                query_embedding.len()
            )));
        }

        // Perform vector similarity search
        let results = self.vector_index.search(query_embedding, top_k);

        // Convert to SemanticEntry format using O(1) HashMap lookup
        let entries = results
            .into_iter()
            .map(|(node_id, score)| {
                // O(1) lookup via node_id_to_idx instead of O(N) linear scan
                let entry_type = self
                    .node_id_to_idx
                    .get(&node_id)
                    .and_then(|&idx| self.nodes.get(idx))
                    .map(|_| EntryType::Function)
                    .unwrap_or(EntryType::Function);

                SemanticEntry {
                    node_id,
                    relevance: score,
                    entry_type,
                }
            })
            .collect();

        Ok(entries)
    }

    /// Get the vector index for direct access
    ///
    /// This provides access to the underlying vector index for advanced use cases.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let dimension = engine.vector_index().dimension();
    /// let count = engine.vector_index().len();
    /// ```
    #[must_use]
    pub fn vector_index(&self) -> &VectorIndexImpl {
        &self.vector_index
    }

    /// Get mutable access to the vector index
    ///
    /// This allows direct manipulation of the vector index.
    ///
    /// # Thread Safety
    ///
    /// **WARNING:** This method requires `&mut self` which ensures exclusive access.
    /// Never call this concurrently with any other method on the same instance.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let index = engine.vector_index_mut();
    /// index.insert("new_node", embedding)?;
    /// ```
    pub fn vector_index_mut(&mut self) -> &mut VectorIndexImpl {
        &mut self.vector_index
    }

    /// Enable HNSW for faster approximate search
    ///
    /// This converts the vector index from brute-force to HNSW-based.
    /// Existing indexed vectors are **NOT** automatically migrated - you must
    /// re-index your data after enabling HNSW.
    ///
    /// # Arguments
    ///
    /// * `params` - Optional HNSW parameters (uses defaults if None)
    ///
    /// # Example
    ///
    /// ```ignore
    /// engine.enable_hnsw(None);
    /// engine.index_nodes(nodes); // Re-index with HNSW
    /// ```
    pub fn enable_hnsw(&mut self, params: Option<HNSWParams>) {
        let dimension = self.vector_index.dimension();
        let params = params.unwrap_or_default();
        self.vector_index =
            VectorIndexImpl::HNSW(Box::new(HNSWIndex::with_params(dimension, params)));
    }

    /// Check if HNSW is currently enabled
    #[must_use]
    pub fn is_hnsw_enabled(&self) -> bool {
        matches!(
            self.vector_index,
            VectorIndexImpl::HNSW(_) | VectorIndexImpl::HNSWQuantized(_)
        )
    }

    /// Disable HNSW and switch back to brute-force search
    ///
    /// This clears the current vector index and creates a new brute-force index.
    /// You'll need to re-index your data after disabling HNSW.
    pub fn disable_hnsw(&mut self) {
        let dimension = self.vector_index.dimension();
        self.vector_index = VectorIndexImpl::BruteForce(VectorIndex::new(dimension));
    }

    /// Create a new search engine with HNSW enabled
    ///
    /// # Arguments
    ///
    /// * `dimension` - Embedding vector dimension
    /// * `params` - HNSW parameters
    ///
    /// # Example
    ///
    /// ```ignore
    /// let engine = SearchEngine::with_hnsw(128, HNSWParams::default());
    /// ```
    #[must_use]
    pub fn with_hnsw(dimension: usize, params: HNSWParams) -> Self {
        let mut engine = Self::with_dimension(dimension);
        engine.enable_hnsw(Some(params));
        engine
    }

    /// Enable INT8 quantized HNSW for memory-efficient search
    ///
    /// This provides ~74% memory reduction compared to f32 HNSW while
    /// maintaining search accuracy through asymmetric distance computation.
    ///
    /// # Arguments
    ///
    /// * `params` - Optional INT8 HNSW parameters (uses defaults if None)
    ///
    /// # Example
    ///
    /// ```ignore
    /// engine.enable_int8_hnsw(None);
    /// engine.index_nodes(nodes); // Re-index with INT8 quantization
    /// ```
    pub fn enable_int8_hnsw(&mut self, params: Option<Int8HnswParams>) {
        let dimension = self.vector_index.dimension();
        let params = params.unwrap_or_default();
        let mut index = Int8HnswIndex::with_params(dimension, params);
        // Re-insert existing nodes' TF-IDF embeddings. The prior implementation
        // replaced `vector_index` with an EMPTY Int8HnswIndex, silently
        // discarding every vector inserted during indexing — quantization was
        // not just unused, it was actively destructive if called. Only nodes
        // with a populated tfidf_embedding (the active indexing phase) migrate;
        // mmap-hydrated nodes carry no on-node tfidf and are skipped (their
        // vectors live in the mmap, a separate efficiency strategy).
        let vectors = self
            .nodes
            .iter()
            .filter(|n| !n.tfidf_embedding.is_empty())
            .map(|n| (n.node_id.clone(), n.tfidf_embedding.clone()));
        let migrated = index.insert_batch(vectors);
        tracing::debug!(
            migrated,
            total_nodes = self.nodes.len(),
            "enabled int8 HNSW (mmap-hydrated nodes have no on-node tfidf and are skipped)"
        );
        self.vector_index = VectorIndexImpl::HNSWQuantized(Box::new(index));
    }

    /// Check if the current index is quantized
    #[must_use]
    pub fn is_quantized(&self) -> bool {
        matches!(self.vector_index, VectorIndexImpl::HNSWQuantized(_))
    }

    /// Estimate memory usage in bytes
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        // Rough estimate based on implementation
        // Content is cleared after indexing (T13), so no +256 content estimate needed
        let nodes_size = self.nodes.len() * std::mem::size_of::<NodeInfo>();
        let cache_size = self.complexity_cache.len()
            * (std::mem::size_of::<String>() + std::mem::size_of::<u32>());
        let text_index_size = self
            .text_index
            .values()
            .map(|set| set.len() * std::mem::size_of::<String>())
            .sum::<usize>();

        nodes_size + cache_size + text_index_size + self.vector_index.estimated_memory_bytes()
    }

    /// Estimate byte size of a slice of search results for cache accounting.
    fn estimate_search_results_bytes(results: &[SearchResult]) -> usize {
        results
            .iter()
            .map(|r| {
                r.node_id.len()
                    + r.file_path.len()
                    + r.symbol_name.len()
                    + r.symbol_type.as_ref().map_or(0, |s| s.len())
                    + r.signature.as_ref().map_or(0, |s| s.len())
                    + r.language.len()
                    + r.context.as_ref().map_or(0, |c| c.len())
                    + 128 // overhead estimate for rank, score, complexity, byte_range, etc.
            })
            .sum()
    }

    /// Check if a file path is inside an archive directory.
    ///
    /// Returns `true` if any path component is `archive` or `.archive`.
    /// This catches paths like:
    /// - `archive/src/foo.rs`
    /// - `.archive/old_code.rs`
    /// - `docs/archive/leindex_pre_step5.rs`
    /// - `src/.archive/backup.rs`
    ///
    /// Used to apply a ranking penalty so archived files don't appear
    /// in top search results unless the query explicitly targets them.
    fn is_archive_path(file_path: &str) -> bool {
        file_path
            .split(['/', '\\'])
            .any(|component| component == "archive" || component == ".archive")
    }
}
