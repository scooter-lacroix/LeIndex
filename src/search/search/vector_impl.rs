//! Vector index implementations (mmap-backed and in-memory).

use super::*;

// ============================================================================
// VECTOR INDEX IMPLEMENTATION
// ============================================================================

/// Read-mostly TF-IDF vector index backed by a persisted mmap file.
///
/// The base rows stay outside the Rust heap. Mutations are kept in a small
/// owned delta and base rows are hidden with tombstones until the next index
/// publication compacts a new mmap generation.
#[derive(Debug)]
pub struct MmapVectorIndex {
    base: Arc<MmapEmbeddingIndex>,
    rows: HashMap<String, u32>,
    delta: HashMap<String, Vec<f32>>,
    tombstones: HashSet<String>,
    cleared: bool,
}

impl MmapVectorIndex {
    pub(super) fn from_snapshot(
        base: Arc<MmapEmbeddingIndex>,
        node_ids: &[String],
    ) -> Result<Self, String> {
        let mut rows = HashMap::with_capacity(node_ids.len());
        for node_id in node_ids {
            if let Some(row) = base.find_node_row(node_id) {
                rows.insert(node_id.clone(), row);
            } else {
                return Err(format!("mmap has no row for snapshot node '{}'", node_id));
            }
        }
        Ok(Self {
            base,
            rows,
            delta: HashMap::new(),
            tombstones: HashSet::new(),
            cleared: false,
        })
    }

    fn len(&self) -> usize {
        if self.cleared {
            return self.delta.len();
        }
        self.rows
            .len()
            .saturating_sub(self.tombstones.len())
            .saturating_add(
                self.delta
                    .keys()
                    .filter(|node_id| !self.rows.contains_key(*node_id))
                    .count(),
            )
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn dimension(&self) -> usize {
        self.base.dimension() as usize
    }

    fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension() || self.cleared && self.delta.is_empty() {
            return Vec::new();
        }
        let mut results = if self.cleared {
            Vec::new()
        } else {
            self.base
                .search(
                    query,
                    top_k
                        .saturating_add(self.tombstones.len())
                        .saturating_add(self.delta.len()),
                )
                .into_iter()
                .filter(|(id, _)| self.rows.contains_key(id) && !self.tombstones.contains(id))
                .collect::<Vec<_>>()
        };
        for (node_id, vector) in &self.delta {
            if vector.len() == query.len() {
                results.push((
                    node_id.clone(),
                    crate::search::vector::cosine_similarity(query, vector),
                ));
            }
        }
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k);
        results
    }

    fn insert(&mut self, node_id: String, vector: Vec<f32>) -> Result<(), VectorIndexError> {
        if vector.len() != self.dimension() {
            return Err(VectorIndexError::InsertionFailed(format!(
                "dimension mismatch: expected {}, got {}",
                self.dimension(),
                vector.len()
            )));
        }
        if self.rows.contains_key(&node_id) {
            self.tombstones.insert(node_id.clone());
        }
        self.delta.insert(node_id, vector);
        Ok(())
    }

    fn clear(&mut self) {
        self.delta.clear();
        self.tombstones.clear();
        self.cleared = true;
    }

    fn remove(&mut self, node_id: &str) -> bool {
        let removed_delta = self.delta.remove(node_id).is_some();
        let removed_base =
            self.rows.contains_key(node_id) && self.tombstones.insert(node_id.into());
        removed_delta || removed_base
    }

    fn estimated_memory_bytes(&self) -> usize {
        self.rows.keys().map(String::len).sum::<usize>()
            + self
                .delta
                .iter()
                .map(|(id, vector)| id.len() + vector.len() * std::mem::size_of::<f32>())
                .sum::<usize>()
            + self.tombstones.iter().map(String::len).sum::<usize>()
            + std::mem::size_of::<Self>()
    }

    fn embedding(&self, node_id: &str) -> Option<Vec<f32>> {
        if let Some(vector) = self.delta.get(node_id) {
            return Some(vector.clone());
        }
        if self.cleared || self.tombstones.contains(node_id) {
            return None;
        }
        let row = self.rows.get(node_id).copied()?;
        self.base.get_embedding_by_row(row)
    }
}

/// Vector index implementation
///
/// Enum that wraps either the brute-force VectorIndex or the HNSW-based HNSWIndex.
/// This allows switching between implementations at runtime.
pub enum VectorIndexImpl {
    /// Brute-force vector index (exact search)
    BruteForce(VectorIndex),

    /// Mmap-backed base rows with a bounded owned delta overlay.
    Mmap(MmapVectorIndex),

    /// HNSW-based approximate nearest neighbor index
    HNSW(Box<HNSWIndex>),
    /// INT8 quantized HNSW-based approximate nearest neighbor index
    HNSWQuantized(Box<Int8HnswIndex>),
}

impl VectorIndexImpl {
    /// Get the number of vectors in the index
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::BruteForce(idx) => idx.len(),
            Self::Mmap(idx) => idx.len(),
            Self::HNSW(idx) => idx.len(),
            Self::HNSWQuantized(idx) => idx.len(),
        }
    }

    /// Check if the index is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::BruteForce(idx) => idx.is_empty(),
            Self::Mmap(idx) => idx.is_empty(),
            Self::HNSW(idx) => idx.is_empty(),
            Self::HNSWQuantized(idx) => idx.is_empty(),
        }
    }

    /// Get the embedding dimension
    #[must_use]
    pub fn dimension(&self) -> usize {
        match self {
            Self::BruteForce(idx) => idx.dimension(),
            Self::Mmap(idx) => idx.dimension(),
            Self::HNSW(idx) => idx.dimension(),
            Self::HNSWQuantized(idx) => idx.dimension(),
        }
    }

    /// Search for similar vectors
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        match self {
            Self::BruteForce(idx) => idx.search(query, top_k),
            Self::Mmap(idx) => idx.search(query, top_k),
            Self::HNSW(idx) => idx.search(query, top_k),
            Self::HNSWQuantized(idx) => idx.search(query, top_k),
        }
    }

    /// Insert a vector into the index
    pub fn insert(&mut self, node_id: String, vector: Vec<f32>) -> Result<(), VectorIndexError> {
        match self {
            Self::BruteForce(idx) => idx
                .insert(node_id, vector)
                .map_err(|e| VectorIndexError::InsertionFailed(e.to_string())),
            Self::Mmap(idx) => idx.insert(node_id, vector),
            Self::HNSW(idx) => idx
                .insert(node_id, vector)
                .map_err(|e| VectorIndexError::InsertionFailed(e.to_string())),
            Self::HNSWQuantized(idx) => idx
                .insert(node_id, vector)
                .map_err(|e| VectorIndexError::InsertionFailed(e.to_string())),
        }
    }

    /// Clear all vectors from the index
    pub fn clear(&mut self) {
        match self {
            Self::BruteForce(idx) => idx.clear(),
            Self::Mmap(idx) => idx.clear(),
            Self::HNSW(idx) => idx.clear(),
            Self::HNSWQuantized(idx) => idx.clear(),
        }
    }

    /// Remove a vector from the index by node ID.
    ///
    /// Returns `true` if the node was found and removed, `false` otherwise.
    /// For HNSW indexes, removal is lazy (marks as deleted); use `rebuild()` to reclaim memory.
    pub fn remove(&mut self, node_id: &str) -> bool {
        match self {
            Self::BruteForce(idx) => idx.remove(node_id),
            Self::Mmap(idx) => idx.remove(node_id),
            Self::HNSW(idx) => idx.remove(node_id),
            Self::HNSWQuantized(idx) => idx.remove(node_id),
        }
    }

    /// Return an owned vector for persistence or compatibility callers.
    pub fn embedding(&self, node_id: &str) -> Option<Vec<f32>> {
        match self {
            Self::BruteForce(idx) => idx.get(node_id).cloned(),
            Self::Mmap(idx) => idx.embedding(node_id),
            Self::HNSW(idx) => idx.get(node_id).cloned(),
            Self::HNSWQuantized(_) => None,
        }
    }

    /// Check if HNSW is enabled
    #[must_use]
    pub fn is_hnsw_enabled(&self) -> bool {
        matches!(self, Self::HNSW(_) | Self::HNSWQuantized(_))
    }

    /// Get estimated memory usage in bytes
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        match self {
            Self::BruteForce(idx) => (*idx).estimated_memory_bytes(),
            Self::Mmap(idx) => idx.estimated_memory_bytes(),
            Self::HNSW(idx) => (*idx).estimated_memory_bytes(),
            Self::HNSWQuantized(idx) => (*idx).estimated_memory_bytes(),
        }
    }
}

/// Vector index errors
#[derive(Debug, thiserror::Error)]
pub enum VectorIndexError {
    /// Failed to insert a vector into the index
    #[error("Insertion failed: {0}")]
    InsertionFailed(String),

    /// General index operation failure
    #[error("Index operation failed: {0}")]
    IndexOperationFailed(String),
}
