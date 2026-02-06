use crate::hnsw::{HNSWIndex, HNSWParams, IndexError};
use lestockage::{HybridStorage, TursoConfig};
use std::collections::{HashMap, HashSet};

/// Default hot-tier budget for in-memory HNSW vectors (256 MiB)
pub const DEFAULT_HOT_VECTOR_MEMORY_BYTES: usize = 256 * 1024 * 1024;

/// Configuration for tiered HNSW + Turso vector storage.
#[derive(Debug, Clone)]
pub struct TieredHnswConfig {
    /// Maximum bytes allowed for the in-memory HNSW hot tier.
    pub max_hot_bytes: usize,
    /// Turso configuration for cold-tier persisted vectors.
    pub turso: TursoConfig,
}

impl Default for TieredHnswConfig {
    fn default() -> Self {
        Self {
            max_hot_bytes: DEFAULT_HOT_VECTOR_MEMORY_BYTES,
            turso: TursoConfig::local_only().with_vectors(true),
        }
    }
}

/// Tiered vector index:
/// - hot tier: in-memory HNSW
/// - cold tier: Turso-backed persisted vectors (local by default, remote optional)
pub struct TieredHnswIndex {
    hot: HNSWIndex,
    cold: Option<HybridStorage>,
    max_hot_bytes: usize,
    cold_count: usize,
    hot_ids: HashSet<String>,
    cold_ids: HashSet<String>,
    dimension: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tiered_spills_to_cold_tier_when_budget_exceeded() {
        let mut turso = TursoConfig::local_only().with_vectors(true);
        let unique = format!("leindex-tiered-{}.db", std::process::id());
        let path = std::env::temp_dir().join(unique);
        turso.database_url = format!("file:{}", path.display());

        let config = TieredHnswConfig {
            max_hot_bytes: 1, // force immediate spill
            turso,
        };

        let mut index = TieredHnswIndex::new(4, HNSWParams::default(), config);
        assert!(index.has_cold_tier());

        index
            .insert("node_1".to_string(), vec![1.0, 0.0, 0.0, 0.0])
            .expect("insert should succeed");

        assert_eq!(index.len(), 1);

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "node_1");

        let _ = std::fs::remove_file(path);
    }
}

impl TieredHnswIndex {
    /// Create a new tiered index.
    #[must_use]
    pub fn new(dimension: usize, params: HNSWParams, config: TieredHnswConfig) -> Self {
        let hot = HNSWIndex::with_params(dimension, params);

        let mut cold = HybridStorage::new(config.turso).ok();
        if let Some(cold_store) = cold.as_mut() {
            if let Err(err) = cold_store.init_vectors() {
                tracing::warn!("Failed to initialize Turso vector tier: {err}");
                cold = None;
            }
        }

        Self {
            hot,
            cold,
            max_hot_bytes: config.max_hot_bytes,
            cold_count: 0,
            hot_ids: HashSet::new(),
            cold_ids: HashSet::new(),
            dimension,
        }
    }

    /// Insert a vector into the tiered index.
    ///
    /// If the hot-tier memory budget is exceeded, vector spills to Turso tier.
    pub fn insert(&mut self, node_id: String, embedding: Vec<f32>) -> Result<(), IndexError> {
        if embedding.len() != self.dimension {
            return Err(IndexError::InvalidParameter(format!(
                "Dimension mismatch: expected {}, got {}",
                self.dimension,
                embedding.len()
            )));
        }

        // Best-effort replacement semantics
        self.remove(&node_id);

        let projected_hot_bytes = self
            .hot
            .estimated_memory_bytes()
            .saturating_add(self.dimension * 4 + 128);

        if projected_hot_bytes <= self.max_hot_bytes || self.cold.is_none() {
            self.hot.insert(node_id.clone(), embedding)?;
            self.hot_ids.insert(node_id);
            return Ok(());
        }

        if let Some(cold) = self.cold.as_ref() {
            match cold.store_embedding(&node_id, &node_id, "<tiered>", "vector", &embedding) {
                Ok(()) => {
                    self.cold_ids.insert(node_id);
                    self.cold_count += 1;
                    Ok(())
                }
                Err(err) => {
                    tracing::warn!(
                        "Turso spill failed for node {}, falling back to hot tier: {}",
                        node_id,
                        err
                    );
                    self.hot.insert(node_id.clone(), embedding)?;
                    self.hot_ids.insert(node_id);
                    Ok(())
                }
            }
        } else {
            self.hot.insert(node_id.clone(), embedding)?;
            self.hot_ids.insert(node_id);
            Ok(())
        }
    }

    /// Search both hot and cold tiers and return merged top-k.
    #[must_use]
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        let mut merged: HashMap<String, f32> = HashMap::new();

        for (id, score) in self.hot.search(query, top_k) {
            merged.insert(id, score);
        }

        if let Some(cold) = self.cold.as_ref() {
            match cold.search_similar(query, top_k) {
                Ok(results) => {
                    for (id, score) in results {
                        let entry = merged.entry(id).or_insert(score);
                        if score > *entry {
                            *entry = score;
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("Turso cold-tier search failed: {}", err);
                }
            }
        }

        let mut out: Vec<(String, f32)> = merged.into_iter().collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(top_k);
        out
    }

    /// Remove vector by node id (best effort for cold tier).
    pub fn remove(&mut self, node_id: &str) -> bool {
        let mut removed = false;

        if self.hot.remove(node_id) {
            self.hot_ids.remove(node_id);
            removed = true;
        }

        if self.cold_ids.remove(node_id) {
            self.cold_count = self.cold_count.saturating_sub(1);
            // Best effort: cold-row deletion is not yet exposed by HybridStorage.
            removed = true;
        }

        removed
    }

    /// Clear all hot-tier vectors and tier metadata.
    pub fn clear(&mut self) {
        self.hot.clear();
        self.hot_ids.clear();
        self.cold_ids.clear();
        self.cold_count = 0;
    }

    /// Number of indexed vectors across hot and tracked cold tiers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hot.len() + self.cold_count
    }

    /// Returns true if no vectors are indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Embedding dimension.
    #[must_use]
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Returns true if a cold tier is available.
    #[must_use]
    pub fn has_cold_tier(&self) -> bool {
        self.cold.is_some()
    }
}
