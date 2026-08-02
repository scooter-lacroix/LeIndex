// Fragment-layer SearchEngine methods (fragment-embeddings 1.11.0, Task 5/6).
//
// Extracted from `mod.rs` (Codex wave-3 item 1 — Large File Detection gate:
// mod.rs was 2010 lines > the 2000 gate). These six methods form the fragment
// layer's engine surface: the master switch + fusion-weight setters, the
// owner-ref map, the index-time embedding population, the persistence
// collector, and the query-time content-hash → owner mapping (invariant 6).
// They live in a child module so they can reach `SearchEngine`'s private
// fields without widening any visibility; `collect_fragment_owners` is
// `pub(super)` because `search()` (in mod.rs) calls it.

use std::collections::HashMap;

use super::SearchEngine;
use super::VectorIndexImpl;
use crate::search::vector::VectorIndex;

impl SearchEngine {
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
    /// item 5), and the best-scoring fragment per owner is kept so the
    /// surfaced byte range corresponds to the fragment that actually drives
    /// the score, independent of HashMap iteration order (invariant 6).
    /// Within ONE hash the score is identical for every ref, so the first
    /// range kept is score-equivalent — no max selection is needed there.
    /// Extracted as a helper to keep `search` under the lizard CCN gate.
    /// `pub(super)`: called from `search()` in the parent `mod.rs`.
    pub(super) fn collect_fragment_owners(
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
}
