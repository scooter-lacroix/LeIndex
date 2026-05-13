// Vector Search Implementation
//
// *Le Vector* (The Vector) - Semantic search with cosine similarity

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Vector index for semantic search
///
/// This index stores node embeddings and provides fast similarity search
/// using cosine similarity. For small to medium datasets, brute-force
/// search is sufficient. For larger datasets, this can be extended with HNSW.
#[derive(Debug, Clone)]
pub struct VectorIndex {
    /// Node ID to embedding mapping
    embeddings: HashMap<String, Vec<f32>>,

    /// Embedding dimension
    dimension: usize,

    /// Number of vectors in the index
    count: usize,
}

impl VectorIndex {
    /// Create a new vector index
    ///
    /// # Arguments
    ///
    /// * `dimension` - The dimension of the embedding vectors (e.g., 768 for CodeRank)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let index = VectorIndex::new(768);
    /// index.insert("func1", vec![0.1, 0.2, ...]);
    /// ```
    pub fn new(dimension: usize) -> Self {
        Self {
            embeddings: HashMap::new(),
            dimension,
            count: 0,
        }
    }

    /// Insert a vector into the index
    ///
    /// # Arguments
    ///
    /// * `node_id` - Unique identifier for the node
    /// * `embedding` - Embedding vector (must match dimension)
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, `Err(Error)` if dimension mismatch
    ///
    /// # Example
    ///
    /// ```ignore
    /// index.insert("my_func", vec![0.1, 0.2, 0.3, ...])?;
    /// ```
    pub fn insert(&mut self, node_id: String, embedding: Vec<f32>) -> Result<(), Error> {
        if embedding.len() != self.dimension {
            return Err(Error::DimensionMismatch {
                expected: self.dimension,
                got: embedding.len(),
            });
        }

        self.embeddings.insert(node_id, embedding);
        self.count += 1;
        Ok(())
    }

    /// Batch insert vectors into the index
    ///
    /// # Arguments
    ///
    /// * `vectors` - Iterator of (node_id, embedding) pairs
    ///
    /// # Returns
    ///
    /// Number of successfully inserted vectors
    ///
    /// # Example
    ///
    /// ```ignore
    /// let vectors = vec![
    ///     ("func1".to_string(), vec![0.1, 0.2, ...]),
    ///     ("func2".to_string(), vec![0.3, 0.4, ...]),
    /// ];
    /// let inserted = index.insert_batch(vectors);
    /// ```
    pub fn insert_batch(&mut self, vectors: impl IntoIterator<Item = (String, Vec<f32>)>) -> usize {
        let mut inserted = 0;
        for (node_id, embedding) in vectors {
            if self.insert(node_id, embedding).is_ok() {
                inserted += 1;
            }
        }
        inserted
    }

    /// Search for similar vectors
    ///
    /// Performs cosine similarity search and returns the top-K most similar nodes.
    ///
    /// # Arguments
    ///
    /// * `query` - Query embedding vector
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// Vector of (node_id, similarity_score) pairs, sorted by similarity (descending)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let query = vec![0.1, 0.2, 0.3, ...];
    /// let results = index.search(&query, 10);
    /// for (node_id, score) in results {
    ///     println!("{}: {}", node_id, score);
    /// }
    /// ```
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension {
            return Vec::new();
        }

        // Calculate cosine similarity for all vectors
        let mut results: Vec<(String, f32)> = self
            .embeddings
            .iter()
            .map(|(node_id, embedding)| {
                let similarity = cosine_similarity(query, embedding);
                (node_id.clone(), similarity)
            })
            .collect();

        // Sort by similarity (descending)
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Return top-K
        results.into_iter().take(top_k).collect()
    }

    /// Get the number of vectors in the index
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Get the embedding dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Remove a vector from the index
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to remove
    ///
    /// # Returns
    ///
    /// `true` if the node was found and removed, `false` otherwise
    pub fn remove(&mut self, node_id: &str) -> bool {
        if self.embeddings.remove(node_id).is_some() {
            self.count -= 1;
            true
        } else {
            false
        }
    }

    /// Clear all vectors from the index
    pub fn clear(&mut self) {
        self.embeddings.clear();
        self.count = 0;
    }

    /// Get a vector by node ID
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to retrieve
    ///
    /// # Returns
    ///
    /// `Some(&embedding)` if found, `None` otherwise
    pub fn get(&self, node_id: &str) -> Option<&Vec<f32>> {
        self.embeddings.get(node_id)
    }

    /// Get estimated memory usage in bytes
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        // HashMap overhead + keys (String) + values (Vec<f32>)
        let embeddings_size = self
            .embeddings
            .iter()
            .map(|(k, v)| {
                k.len() + std::mem::size_of::<String>() + // Key size
                v.len() * std::mem::size_of::<f32>() + std::mem::size_of::<Vec<f32>>()
                // Value size
            })
            .sum::<usize>();
        embeddings_size + std::mem::size_of::<Self>()
    }
}

/// Calculate cosine similarity between two vectors
///
/// Cosine similarity = (A · B) / (||A|| * ||B||)
/// Returns a value between -1.0 and 1.0, where 1.0 is identical.
///
/// # Arguments
///
/// * `a` - First vector
/// * `b` - Second vector
///
/// # Returns
///
/// Cosine similarity score, or 0.0 if either vector is zero-length
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let mut dot_product = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;

    for i in 0..a.len() {
        dot_product += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let norm_a = norm_a.sqrt();
    let norm_b = norm_b.sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

/// Vector search errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Provided embedding dimension does not match the index dimension
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Expected dimension
        expected: usize,
        /// Actual dimension received
        got: usize,
    },

    /// The index contains no vectors
    #[error("Index is empty")]
    EmptyIndex,

    /// The provided embedding is invalid (e.g., contains NaN or infinite values)
    #[error("Invalid embedding: {0}")]
    InvalidEmbedding(String),
}

/// Search result with node ID and similarity score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Node identifier
    pub node_id: String,

    /// Similarity score (0.0 to 1.0, higher is better)
    pub score: f32,
}

impl SearchResult {
    /// Create a new search result
    pub fn new(node_id: String, score: f32) -> Self {
        Self { node_id, score }
    }
}

impl Default for VectorIndex {
    fn default() -> Self {
        Self::new(768) // Default to 768-dim embeddings (CodeRank)
    }
}

// ============================================================================
// MMAP EMBEDDING INDEX (R10)
// ============================================================================

/// Magic bytes identifying a LeIndex embedding file: "LIEE"
const MMAP_MAGIC: [u8; 4] = [b'L', b'I', b'E', b'E'];

/// Current on-disk format version.
const MMAP_VERSION: u32 = 1;

/// On-disk header for the mmap embedding file.
///
/// All fields are stored little-endian.
///
/// ```text
/// [0..4]   magic: b"LIEE"
/// [4..8]   version: u32
/// [8..12]  node_count: u32
/// [12..16] dimension: u32
/// ```
#[derive(Debug, Clone)]
#[repr(C)]
struct MmapHeader {
    magic: [u8; 4],
    version: u32,
    node_count: u32,
    dimension: u32,
}

impl MmapHeader {
    const SIZE: usize = 16;

    fn read_from(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let node_count = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let dimension = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        Some(Self {
            magic,
            version,
            node_count,
            dimension,
        })
    }

    fn write_to(&self, bytes: &mut [u8]) {
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4..8].copy_from_slice(&self.version.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.node_count.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.dimension.to_le_bytes());
    }
}

/// Memory-mapped read-only embedding index.
///
/// Opens a binary file produced by [`write_mmap_embeddings`] and provides
/// fast lookups and brute-force cosine-similarity search without loading
/// the full embedding matrix into heap memory.
pub struct MmapEmbeddingIndex {
    mmap: memmap2::Mmap,
    node_count: u32,
    dimension: u32,
    /// Byte offset where the ID string section begins.
    ids_section_offset: usize,
    /// Byte offset where the embedding matrix begins (4-byte aligned).
    embedding_matrix_offset: usize,
}

impl std::fmt::Debug for MmapEmbeddingIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapEmbeddingIndex")
            .field("node_count", &self.node_count)
            .field("dimension", &self.dimension)
            .field("ids_section_offset", &self.ids_section_offset)
            .field("embedding_matrix_offset", &self.embedding_matrix_offset)
            .finish_non_exhaustive()
    }
}

impl MmapEmbeddingIndex {
    /// Open an existing mmap embedding file for read-only access.
    pub fn open(path: &Path) -> Result<Self, MmapError> {
        let file = std::fs::File::open(path).map_err(MmapError::Io)?;
        let metadata = file.metadata().map_err(MmapError::Io)?;

        if metadata.len() < MmapHeader::SIZE as u64 {
            return Err(MmapError::Corrupt("file too small for header".into()));
        }

        // Safety: we open the file read-only and the mmap is used exclusively
        // for reads. The file must not be modified by another process while
        // this mapping is alive.
        let mmap =
            unsafe { memmap2::Mmap::map(&file).map_err(|e| MmapError::Mmap(e.to_string()))? };

        let header = MmapHeader::read_from(&mmap)
            .ok_or_else(|| MmapError::Corrupt("failed to read header".into()))?;

        if header.magic != MMAP_MAGIC {
            return Err(MmapError::Corrupt("invalid magic bytes".into()));
        }
        if header.version != MMAP_VERSION {
            return Err(MmapError::Corrupt(format!(
                "unsupported version {}",
                header.version
            )));
        }

        let node_count = header.node_count;
        let dimension = header.dimension;

        if node_count == 0 {
            return Ok(Self {
                mmap,
                node_count: 0,
                dimension,
                ids_section_offset: MmapHeader::SIZE,
                embedding_matrix_offset: MmapHeader::SIZE,
            });
        }

        let n = node_count as usize;

        // Offset table: node_count × 8 bytes (u64 offsets into ID string section)
        let offsets_start = MmapHeader::SIZE;
        let offsets_end = offsets_start + n * 8;

        // Length table: node_count × 4 bytes (u32 length per ID)
        let lengths_start = offsets_end;
        let lengths_end = lengths_start + n * 4;

        // ID strings start after offset+length tables.
        // Read the maximum offset+length to determine the end of the ID section.
        let ids_section_offset = lengths_end;

        // Verify offset and length tables fit in file before accessing
        // Use checked arithmetic to prevent overflow on malicious node_count
        let offsets_table_size = n.checked_mul(8)
            .ok_or_else(|| MmapError::Corrupt("offset table size overflow".to_string()))?;
        let lengths_table_size = n.checked_mul(4)
            .ok_or_else(|| MmapError::Corrupt("length table size overflow".to_string()))?;
        let offsets_end_checked = offsets_start.checked_add(offsets_table_size)
            .ok_or_else(|| MmapError::Corrupt("offset table end overflow".to_string()))?;
        let lengths_end_checked = lengths_start.checked_add(lengths_table_size)
            .ok_or_else(|| MmapError::Corrupt("length table end overflow".to_string()))?;

        if offsets_end_checked > mmap.len() {
            return Err(MmapError::Corrupt(format!(
                "offset table exceeds file size: table size {}, file size {}",
                offsets_table_size,
                mmap.len()
            )));
        }
        if lengths_end_checked > mmap.len() {
            return Err(MmapError::Corrupt(format!(
                "length table exceeds file size: table size {}, file size {}",
                lengths_table_size,
                mmap.len()
            )));
        }
        
        let ids_section_end = {
            let mut max_end: usize = ids_section_offset;
            for i in 0..n {
                // Use checked arithmetic for slice indices to prevent overflow
                let offset_idx_start = offsets_start.checked_add(i.checked_mul(8).ok_or_else(|| MmapError::Corrupt("offset index overflow".to_string()))?)
                    .ok_or_else(|| MmapError::Corrupt("offset start overflow".to_string()))?;
                let offset_idx_end = offset_idx_start.checked_add(8)
                    .ok_or_else(|| MmapError::Corrupt("offset end overflow".to_string()))?;
                let len_idx_start = lengths_start.checked_add(i.checked_mul(4).ok_or_else(|| MmapError::Corrupt("length index overflow".to_string()))?)
                    .ok_or_else(|| MmapError::Corrupt("length start overflow".to_string()))?;
                let len_idx_end = len_idx_start.checked_add(4)
                    .ok_or_else(|| MmapError::Corrupt("length end overflow".to_string()))?;

                if offset_idx_end > mmap.len() {
                    return Err(MmapError::Corrupt(format!(
                        "offset slice exceeds file size: [{}, {}], file size {}",
                        offset_idx_start, offset_idx_end, mmap.len()
                    )));
                }
                if len_idx_end > mmap.len() {
                    return Err(MmapError::Corrupt(format!(
                        "length slice exceeds file size: [{}, {}], file size {}",
                        len_idx_start, len_idx_end, mmap.len()
                    )));
                }

                let off = u64::from_le_bytes(
                    mmap[offset_idx_start..offset_idx_end]
                        .try_into()
                        .unwrap(),
                ) as usize;
                let len = u32::from_le_bytes(
                    mmap[len_idx_start..len_idx_end]
                        .try_into()
                        .unwrap(),
                ) as usize;
                // Use checked addition for ID section end calculation to prevent overflow
                let section_end = ids_section_offset.checked_add(off)
                    .and_then(|v| v.checked_add(len))
                    .ok_or_else(|| MmapError::Corrupt("ID section end overflow".to_string()))?;
                max_end = max_end.max(section_end);
            }
            max_end
        };

        // Pad to 4-byte alignment for the embedding matrix
        let embedding_matrix_offset = ids_section_end
            .checked_add(3)
            .ok_or_else(|| MmapError::Corrupt("embedding matrix offset overflow".to_string()))? & !3;

        // Validate total file size
        let embedding_matrix_size = (dimension as usize)
            .checked_mul(4)
            .and_then(|v| n.checked_mul(v))
            .ok_or_else(|| MmapError::Corrupt("embedding matrix size overflow".to_string()))?;
        let expected_size = embedding_matrix_offset
            .checked_add(embedding_matrix_size)
            .ok_or_else(|| MmapError::Corrupt("expected file size overflow".to_string()))?;
        if mmap.len() < expected_size {
            return Err(MmapError::Corrupt(format!(
                "file too small: expected {} bytes, got {}",
                expected_size,
                mmap.len()
            )));
        }

        Ok(Self {
            mmap,
            node_count,
            dimension,
            ids_section_offset,
            embedding_matrix_offset,
        })
    }

    /// Retrieve the embedding for a given node ID.
    ///
    /// Performs a linear scan over the ID table. This is O(n) but is only used
    /// for targeted lookups; bulk operations should use [`Self::search`].
    pub fn get_embedding(&self, node_id: &str) -> Option<Vec<f32>> {
        let idx = self.find_node_index(node_id)?;
        Some(self.read_embedding(idx))
    }

    /// Search for the top-K most similar vectors by cosine similarity.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if self.node_count == 0 || query.len() != self.dimension as usize {
            return Vec::new();
        }

        let _dim = self.dimension as usize;
        let query_norm: f32 = query.iter().map(|v| v * v).sum::<f32>().sqrt();
        if query_norm < 1e-9 {
            return Vec::new();
        }

        let mut results: Vec<(String, f32)> = (0..self.node_count as usize)
            .filter_map(|i| {
                let id = self.read_node_id(i)?;
                let embedding = self.read_embedding(i);
                let dot: f32 = query.iter().zip(embedding.iter()).map(|(a, b)| a * b).sum();
                let emb_norm: f32 = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
                if emb_norm < 1e-9 {
                    return None;
                }
                let similarity = dot / (query_norm * emb_norm);
                Some((id, similarity))
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k);
        results
    }

    /// Return the number of nodes in the mmap index.
    pub fn len(&self) -> usize {
        self.node_count as usize
    }

    /// Return true if the index contains no nodes.
    pub fn is_empty(&self) -> bool {
        self.node_count == 0
    }

    /// Return the embedding dimension.
    pub fn dimension(&self) -> u32 {
        self.dimension
    }

    // ---- Internal helpers ----

    /// Find the numeric index for a given node ID (linear scan).
    fn find_node_index(&self, node_id: &str) -> Option<usize> {
        (0..self.node_count as usize).find(|&i| self.read_node_id(i).as_deref() == Some(node_id))
    }

    /// Read the node ID string at the given index.
    fn read_node_id(&self, idx: usize) -> Option<String> {
        if idx >= self.node_count as usize {
            return None;
        }
        let n = self.node_count as usize;
        let offsets_start = MmapHeader::SIZE;
        let lengths_start = offsets_start + n * 8;

        let off = u64::from_le_bytes(
            self.mmap[offsets_start + idx * 8..offsets_start + idx * 8 + 8]
                .try_into()
                .ok()?,
        ) as usize;
        let len = u32::from_le_bytes(
            self.mmap[lengths_start + idx * 4..lengths_start + idx * 4 + 4]
                .try_into()
                .ok()?,
        ) as usize;

        let id_start = self.ids_section_offset + off;
        let id_end = id_start + len;
        if id_end > self.mmap.len() {
            return None;
        }
        String::from_utf8(self.mmap[id_start..id_end].to_vec()).ok()
    }

    /// Read the embedding vector at the given index.
    fn read_embedding(&self, idx: usize) -> Vec<f32> {
        let dim = self.dimension as usize;
        let byte_offset = self.embedding_matrix_offset + idx * dim * 4;
        let mut vec = Vec::with_capacity(dim);
        for d in 0..dim {
            let start = byte_offset + d * 4;
            let bytes: [u8; 4] = self.mmap[start..start + 4].try_into().unwrap();
            vec.push(f32::from_le_bytes(bytes));
        }
        vec
    }
}

/// Errors that can occur when working with mmap embedding files.
#[derive(Debug, thiserror::Error)]
pub enum MmapError {
    /// I/O error reading or writing the file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The file is corrupt or has an incompatible format.
    #[error("Corrupt embedding file: {0}")]
    Corrupt(String),

    /// The memory mapping failed.
    #[error("Mmap error: {0}")]
    Mmap(String),
}

/// Write embeddings to a binary file in the mmap format.
///
/// # Binary layout
///
/// ```text
/// Header (16 bytes):
///   magic: 4 bytes ("LIEE")
///   version: u32 LE
///   node_count: u32 LE
///   dimension: u32 LE
///
/// ID offset table: node_count × 8 bytes (u64 LE offset into ID string section)
/// ID length table: node_count × 4 bytes (u32 LE length per ID)
/// ID strings: concatenated UTF-8 bytes
/// Padding to 4-byte alignment
/// Embedding matrix: node_count × dimension × 4 bytes (f32 LE)
/// ```
pub fn write_mmap_embeddings(
    path: &Path,
    embeddings: &[(String, Vec<f32>)],
) -> Result<(), MmapError> {
    let node_count = embeddings.len() as u32;
    let dimension = if embeddings.is_empty() {
        0u32
    } else {
        embeddings[0].1.len() as u32
    };

    // Build the ID string section and offset/length tables.
    let mut id_bytes = Vec::new();
    let mut id_offsets: Vec<u64> = Vec::with_capacity(embeddings.len());
    let mut id_lengths: Vec<u32> = Vec::with_capacity(embeddings.len());

    for (id, _) in embeddings {
        id_offsets.push(id_bytes.len() as u64);
        let id_bytes_len = id.len() as u32;
        id_lengths.push(id_bytes_len);
        id_bytes.extend_from_slice(id.as_bytes());
    }

    // Compute section sizes.
    let header_size = MmapHeader::SIZE;
    let offsets_table_size = embeddings.len() * 8;
    let lengths_table_size = embeddings.len() * 4;
    let ids_end = header_size + offsets_table_size + lengths_table_size + id_bytes.len();
    let embedding_matrix_offset = (ids_end + 3) & !3; // align to 4 bytes
    let _padding_len = embedding_matrix_offset - ids_end;
    let embedding_matrix_size = embeddings.len() * (dimension as usize) * 4;
    let total_size = embedding_matrix_offset + embedding_matrix_size;

    // Allocate buffer and write.
    let mut buf = vec![0u8; total_size];

    // Header
    let header = MmapHeader {
        magic: MMAP_MAGIC,
        version: MMAP_VERSION,
        node_count,
        dimension,
    };
    header.write_to(&mut buf[0..header_size]);

    // Offset table
    let offsets_start = header_size;
    for (i, off) in id_offsets.iter().enumerate() {
        let start = offsets_start + i * 8;
        buf[start..start + 8].copy_from_slice(&off.to_le_bytes());
    }

    // Length table
    let lengths_start = offsets_start + offsets_table_size;
    for (i, len) in id_lengths.iter().enumerate() {
        let start = lengths_start + i * 4;
        buf[start..start + 4].copy_from_slice(&len.to_le_bytes());
    }

    // ID strings
    let ids_start = lengths_start + lengths_table_size;
    buf[ids_start..ids_start + id_bytes.len()].copy_from_slice(&id_bytes);

    // Padding (already zeroed)

    // Embedding matrix
    let dim = dimension as usize;
    for (i, (_, embedding)) in embeddings.iter().enumerate() {
        for (d, val) in embedding.iter().enumerate().take(dim) {
            let byte_offset = embedding_matrix_offset + i * dim * 4 + d * 4;
            buf[byte_offset..byte_offset + 4].copy_from_slice(&val.to_le_bytes());
        }
    }

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(MmapError::Io)?;
    }

    std::fs::write(path, &buf).map_err(MmapError::Io)?;
    Ok(())
}

/// Return the path to the mmap embedding file for a given project.
///
/// The file is stored at `<project_path>/.leindex/embeddings.bin`.
pub fn mmap_embeddings_path(project_path: &Path) -> PathBuf {
    project_path.join(".leindex").join("embeddings.bin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_vector_index_creation() {
        let index = VectorIndex::new(128);
        assert_eq!(index.dimension(), 128);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_vector_index_insert() {
        let mut index = VectorIndex::new(3);
        let result = index.insert("test".to_string(), vec![0.1, 0.2, 0.3]);
        assert!(result.is_ok());
        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_vector_index_dimension_mismatch() {
        let mut index = VectorIndex::new(3);
        let result = index.insert("test".to_string(), vec![0.1, 0.2]);
        assert!(result.is_err());
    }

    #[test]
    fn test_vector_index_search() {
        let mut index = VectorIndex::new(3);
        index.insert("a".to_string(), vec![1.0, 0.0, 0.0]).unwrap();
        index.insert("b".to_string(), vec![0.0, 1.0, 0.0]).unwrap();
        index.insert("c".to_string(), vec![0.9, 0.1, 0.0]).unwrap();

        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "a"); // Most similar (identical)
        assert_eq!(results[1].0, "c"); // Second most similar
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < f32::EPSILON);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_vector_index_remove() {
        let mut index = VectorIndex::new(3);
        index
            .insert("test".to_string(), vec![0.1, 0.2, 0.3])
            .unwrap();
        assert_eq!(index.len(), 1);

        assert!(index.remove("test"));
        assert_eq!(index.len(), 0);
        assert!(!index.remove("nonexistent"));
    }

    #[test]
    fn test_vector_index_batch_insert() {
        let mut index = VectorIndex::new(3);
        let vectors = vec![
            ("a".to_string(), vec![1.0, 0.0, 0.0]),
            ("b".to_string(), vec![0.0, 1.0, 0.0]),
            ("c".to_string(), vec![0.0, 0.0, 1.0]),
        ];

        let inserted = index.insert_batch(vectors);
        assert_eq!(inserted, 3);
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn test_vector_index_get() {
        let mut index = VectorIndex::new(3);
        let embedding = vec![0.1, 0.2, 0.3];
        index.insert("test".to_string(), embedding.clone()).unwrap();

        let retrieved = index.get("test");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), &embedding);

        assert!(index.get("nonexistent").is_none());
    }

    #[test]
    fn test_vector_index_clear() {
        let mut index = VectorIndex::new(3);
        index.insert("a".to_string(), vec![1.0, 0.0, 0.0]).unwrap();
        index.insert("b".to_string(), vec![0.0, 1.0, 0.0]).unwrap();
        assert_eq!(index.len(), 2);

        index.clear();
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_search_with_zero_query() {
        let mut index = VectorIndex::new(3);
        index
            .insert("test".to_string(), vec![0.1, 0.2, 0.3])
            .unwrap();

        let results = index.search(&[0.0, 0.0, 0.0], 10);
        // Should still return results, just with 0.0 similarity
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "test");
    }

    #[test]
    fn test_search_empty_index() {
        let index = VectorIndex::new(3);
        let results = index.search(&[0.1, 0.2, 0.3], 10);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_search_respects_top_k() {
        let mut index = VectorIndex::new(3);
        for i in 0..10 {
            let node_id = format!("node{}", i);
            let embedding = vec![1.0 / (i + 1) as f32, 0.0, 0.0];
            index.insert(node_id, embedding).unwrap();
        }

        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 3);
        assert_eq!(results.len(), 3);
    }

    // ========================================================================
    // Mmap embedding tests
    // ========================================================================

    #[test]
    fn test_mmap_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.bin");

        let embeddings: Vec<(String, Vec<f32>)> = vec![
            ("func_a".to_string(), vec![1.0, 0.0, 0.0]),
            ("func_b".to_string(), vec![0.0, 1.0, 0.0]),
            ("func_c".to_string(), vec![0.0, 0.0, 1.0]),
        ];

        write_mmap_embeddings(&path, &embeddings).unwrap();
        let index = MmapEmbeddingIndex::open(&path).unwrap();

        assert_eq!(index.len(), 3);
        assert_eq!(index.dimension(), 3);

        // Verify each embedding roundtrips correctly
        let emb_a = index.get_embedding("func_a").unwrap();
        assert_eq!(emb_a, vec![1.0, 0.0, 0.0]);

        let emb_b = index.get_embedding("func_b").unwrap();
        assert_eq!(emb_b, vec![0.0, 1.0, 0.0]);

        let emb_c = index.get_embedding("func_c").unwrap();
        assert_eq!(emb_c, vec![0.0, 0.0, 1.0]);

        // Missing node returns None
        assert!(index.get_embedding("nonexistent").is_none());
    }

    #[test]
    fn test_mmap_search_consistency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.bin");

        let embeddings: Vec<(String, Vec<f32>)> = vec![
            ("a".to_string(), vec![1.0, 0.0, 0.0]),
            ("b".to_string(), vec![0.0, 1.0, 0.0]),
            ("c".to_string(), vec![0.9, 0.1, 0.0]),
        ];

        write_mmap_embeddings(&path, &embeddings).unwrap();
        let index = MmapEmbeddingIndex::open(&path).unwrap();

        // Query identical to "a" — should rank "a" first, "c" second
        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "a");
        assert_eq!(results[1].0, "c");
        assert!(results[0].1 > results[1].1);
    }

    #[test]
    fn test_mmap_empty_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.bin");

        let embeddings: Vec<(String, Vec<f32>)> = vec![];
        write_mmap_embeddings(&path, &embeddings).unwrap();

        let index = MmapEmbeddingIndex::open(&path).unwrap();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);

        // Search on empty index returns nothing
        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_mmap_embeddings_path_helper() {
        let project = PathBuf::from("/tmp/myproject");
        let path = mmap_embeddings_path(&project);
        assert_eq!(
            path,
            PathBuf::from("/tmp/myproject/.leindex/embeddings.bin")
        );
    }

    #[test]
    fn test_mmap_invalid_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.bin");
        std::fs::write(
            &path,
            b"XXXX\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        )
        .unwrap();

        let result = MmapEmbeddingIndex::open(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid magic"), "unexpected error: {err}");
    }

    #[test]
    fn test_mmap_truncated_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trunc.bin");
        std::fs::write(&path, b"LIEE").unwrap();

        let result = MmapEmbeddingIndex::open(&path);
        assert!(result.is_err());
    }
}
