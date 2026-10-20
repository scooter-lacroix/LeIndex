// Vector Search Implementation
//
// *Le Vector* (The Vector) - Semantic search with cosine similarity

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
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

    /// Return all `(node_id, embedding)` records.
    ///
    /// Used by the fragment layer to collect index-time rows for mmap
    /// persistence (fragment-embeddings 1.11.0 Task 7): the engine's
    /// in-memory fragment index is BruteForce-backed at index time and
    /// Mmap-backed at hydration, and both must expose the same row
    /// enumeration so persist and hydrate stay structurally identical.
    pub fn entries(&self) -> Vec<(String, Vec<f32>)> {
        self.embeddings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
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
const MMAP_MAGIC: [u8; 4] = *b"LIEE";

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
    /// Heap-resident ID→row map; vector payloads remain mmap-backed.
    id_rows: HashMap<String, u32>,
    /// Byte offset where the ID string section begins.
    ids_section_offset: usize,
    /// Byte offset where the embedding matrix begins (4-byte aligned).
    embedding_matrix_offset: usize,
    identity: Option<String>,
}

impl std::fmt::Debug for MmapEmbeddingIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapEmbeddingIndex")
            .field("node_count", &self.node_count)
            .field("dimension", &self.dimension)
            .field("id_rows", &self.id_rows.len())
            .field("ids_section_offset", &self.ids_section_offset)
            .field("embedding_matrix_offset", &self.embedding_matrix_offset)
            .finish_non_exhaustive()
    }
}

fn map_embedding_file(path: &Path) -> Result<memmap2::Mmap, MmapError> {
    let file = std::fs::File::open(path).map_err(MmapError::Io)?;
    let metadata = file.metadata().map_err(MmapError::Io)?;
    if metadata.len() < MmapHeader::SIZE as u64 {
        return Err(MmapError::Corrupt("file too small for header".into()));
    }

    // Safety: the file is opened read-only and the mapping is used exclusively
    // for reads. The file must not be modified while this mapping is alive.
    unsafe { memmap2::Mmap::map(&file).map_err(|error| MmapError::Mmap(error.to_string())) }
}

fn validate_v1_header(mmap: &[u8]) -> Result<MmapHeader, MmapError> {
    let header = MmapHeader::read_from(mmap)
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
    Ok(header)
}

fn checked_id_table_bounds(node_count: u32, file_len: usize) -> Result<(usize, usize), MmapError> {
    let n = node_count as usize;
    let offsets_start = MmapHeader::SIZE;
    let offsets_table_size = n
        .checked_mul(8)
        .ok_or_else(|| MmapError::Corrupt("offset table size overflow".into()))?;
    let offsets_end = offsets_start
        .checked_add(offsets_table_size)
        .ok_or_else(|| MmapError::Corrupt("offset table end overflow".into()))?;
    let lengths_table_size = n
        .checked_mul(4)
        .ok_or_else(|| MmapError::Corrupt("length table size overflow".into()))?;
    let lengths_end = offsets_end
        .checked_add(lengths_table_size)
        .ok_or_else(|| MmapError::Corrupt("length table end overflow".into()))?;

    if offsets_end > file_len {
        return Err(MmapError::Corrupt(format!(
            "offset table exceeds file size: table size {}, file size {}",
            offsets_table_size, file_len
        )));
    }
    if lengths_end > file_len {
        return Err(MmapError::Corrupt(format!(
            "length table exceeds file size: table size {}, file size {}",
            lengths_table_size, file_len
        )));
    }
    Ok((offsets_end, lengths_end))
}

fn scan_ids_section_end(
    mmap: &[u8],
    node_count: u32,
    lengths_start: usize,
    ids_section_offset: usize,
) -> Result<usize, MmapError> {
    let mut max_end = ids_section_offset;
    for index in 0..node_count as usize {
        let offset_start = MmapHeader::SIZE + index * 8;
        let length_start = lengths_start + index * 4;
        let offset = u64::from_le_bytes(
            mmap[offset_start..offset_start + 8]
                .try_into()
                .expect("validated offset table entry"),
        ) as usize;
        let length = u32::from_le_bytes(
            mmap[length_start..length_start + 4]
                .try_into()
                .expect("validated length table entry"),
        ) as usize;
        let section_end = ids_section_offset
            .checked_add(offset)
            .and_then(|value| value.checked_add(length))
            .ok_or_else(|| MmapError::Corrupt("ID section end overflow".into()))?;
        max_end = max_end.max(section_end);
    }
    Ok(max_end)
}

fn checked_embedding_matrix_size(node_count: u32, dimension: u32) -> Result<usize, MmapError> {
    (dimension as usize)
        .checked_mul(4)
        .and_then(|row_size| (node_count as usize).checked_mul(row_size))
        .ok_or_else(|| MmapError::Corrupt("embedding matrix size overflow".into()))
}

fn build_id_rows(
    mmap: &[u8],
    node_count: u32,
    ids_section_offset: usize,
) -> Result<HashMap<String, u32>, MmapError> {
    let mut id_rows = HashMap::with_capacity(node_count as usize);
    for index in 0..node_count as usize {
        match read_node_id_from_mmap(mmap, node_count, ids_section_offset, index) {
            Ok(id) => {
                id_rows.insert(id, index as u32);
            }
            Err(error) if error.to_string().contains("not UTF-8") => {}
            Err(error) => return Err(error),
        }
    }
    Ok(id_rows)
}

fn read_mmap_identity(mmap: &[u8], matrix_end: usize) -> Option<String> {
    let length_end = matrix_end.checked_add(4)?;
    let identity_len =
        u32::from_le_bytes(mmap.get(matrix_end..length_end)?.try_into().ok()?) as usize;
    let identity_end = length_end.checked_add(identity_len)?;
    if identity_end != mmap.len() {
        return None;
    }
    std::str::from_utf8(mmap.get(length_end..identity_end)?)
        .ok()
        .map(str::to_owned)
}

impl MmapEmbeddingIndex {
    /// Open an existing mmap embedding file for read-only access.
    pub fn open(path: &Path) -> Result<Self, MmapError> {
        let mmap = map_embedding_file(path)?;
        let header = validate_v1_header(&mmap)?;

        let node_count = header.node_count;
        let dimension = header.dimension;

        if node_count == 0 {
            let embedding_matrix_offset = MmapHeader::SIZE;
            let identity = read_mmap_identity(&mmap, embedding_matrix_offset);
            return Ok(Self {
                mmap,
                node_count: 0,
                dimension,
                id_rows: HashMap::new(),
                ids_section_offset: MmapHeader::SIZE,
                embedding_matrix_offset,
                identity,
            });
        }

        let (lengths_start, ids_section_offset) = checked_id_table_bounds(node_count, mmap.len())?;
        let ids_section_end =
            scan_ids_section_end(&mmap, node_count, lengths_start, ids_section_offset)?;

        // Pad to 4-byte alignment for the embedding matrix
        let embedding_matrix_offset = ids_section_end
            .checked_add(3)
            .ok_or_else(|| MmapError::Corrupt("embedding matrix offset overflow".to_string()))?
            & !3;

        // Validate total file size
        let embedding_matrix_size = checked_embedding_matrix_size(node_count, dimension)?;
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

        // Malformed UTF-8 remains deferred to `entries()`; valid IDs stay searchable.
        let id_rows = build_id_rows(&mmap, node_count, ids_section_offset)?;
        let identity = read_mmap_identity(&mmap, expected_size);

        Ok(Self {
            mmap,
            node_count,
            dimension,
            id_rows,
            ids_section_offset,
            embedding_matrix_offset,
            identity,
        })
    }

    /// Retrieve the embedding for a given node ID.
    ///
    /// Uses the resident ID-to-row map, so targeted lookups are O(1).
    pub fn get_embedding(&self, node_id: &str) -> Option<Vec<f32>> {
        let row = self.find_node_row(node_id)?;
        self.get_embedding_by_row(row)
    }

    // ----------------------------------------------------------------
    // B-phase row-oriented access (Plan 2)
    // ----------------------------------------------------------------

    /// Return a borrowed slice of the embedding at the given row index.
    ///
    /// This is the primary read path for mmap-backed vector access. It returns
    /// a zero-copy `&[f32]` into the memory-mapped file, avoiding heap
    /// allocation entirely.
    ///
    /// Returns `None` if the row index is out of range.
    ///
    /// # Stability
    ///
    /// The returned slice remains valid for the lifetime of this
    /// `MmapEmbeddingIndex`. Row indexes are stable for the lifetime of the
    /// mmap file unless explicit compaction occurs.
    pub fn embedding_slice_by_index(&self, idx: u32) -> Option<&[f32]> {
        let idx = idx as usize;
        if idx >= self.node_count as usize {
            return None;
        }
        let dim = self.dimension as usize;
        let byte_offset = self.embedding_matrix_offset + idx * dim * 4;
        let end = byte_offset + dim * 4;
        if end > self.mmap.len() {
            return None;
        }
        // Safety: the mmap bytes are aligned and we validated bounds above.
        // f32 is 4 bytes and the embedding matrix is 4-byte aligned, so
        // we can safely reinterpret the byte slice as &[f32].
        let ptr = self.mmap[byte_offset..end].as_ptr() as *const f32;
        // Verify alignment (embedding_matrix_offset is 4-byte aligned by construction)
        assert!(
            (ptr as usize) % std::mem::align_of::<f32>() == 0,
            "embedding matrix not aligned for f32 access"
        );
        Some(unsafe { std::slice::from_raw_parts(ptr, dim) })
    }

    /// Look up the row index for a given node ID.
    ///
    /// Returns `Some(row_index)` if the node exists, `None` otherwise.
    /// The returned row index is stable for the lifetime of the mmap file
    /// unless explicit compaction occurs.
    ///
    /// Lookup is O(1) using the resident ID-to-row map; vector payloads remain
    /// memory-mapped and are not copied until explicitly requested.
    pub fn find_node_row(&self, node_id: &str) -> Option<u32> {
        self.id_rows.get(node_id).copied()
    }

    /// Return an owned copy of the embedding at the given row index.
    ///
    /// This is a compatibility shim that allocates a `Vec<f32>`. Prefer
    /// [`Self::embedding_slice_by_index`] for the hot path to avoid
    /// allocation.
    ///
    /// Returns `None` if the row index is out of range.
    pub fn get_embedding_by_row(&self, idx: u32) -> Option<Vec<f32>> {
        let idx = idx as usize;
        if idx >= self.node_count as usize {
            return None;
        }
        Some(self.read_embedding(idx))
    }

    /// Search for the top-K most similar vectors by cosine similarity.
    ///
    /// Uses borrowed mmap slices for scoring, avoiding per-row heap allocation.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if self.node_count == 0 || query.len() != self.dimension as usize {
            return Vec::new();
        }

        let query_norm: f32 = query.iter().map(|v| v * v).sum::<f32>().sqrt();
        if query_norm < 1e-9 {
            return Vec::new();
        }

        let mut results: Vec<(String, f32)> = (0..self.node_count as usize)
            .filter_map(|i| {
                let id = self.read_node_id(i)?;
                // Use borrowed slice path — no heap allocation per row.
                let embedding = self.embedding_slice_by_index(i as u32)?;
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

    /// Return the persisted generation identity, if the mmap has a valid trailer.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    /// Return all `(node_id, embedding)` records in row order.
    ///
    /// This is intended for cold-start hydration from the persisted mmap file:
    /// callers can rebuild in-memory indexes without recomputing embeddings
    /// from source content.
    pub fn entries(&self) -> Result<Vec<(String, Vec<f32>)>, MmapError> {
        (0..self.node_count as usize)
            .map(|i| {
                let id = self.read_node_id_result(i)?;
                let embedding = self
                    .embedding_slice_by_index(i as u32)
                    .ok_or_else(|| {
                        MmapError::Corrupt(format!(
                            "embedding row {} exceeds mapped file bounds",
                            i
                        ))
                    })?
                    .to_vec();
                Ok((id, embedding))
            })
            .collect()
    }

    // ---- Internal helpers ----

    /// Read the node ID string at the given index.
    fn read_node_id(&self, idx: usize) -> Option<String> {
        self.read_node_id_result(idx).ok()
    }

    fn read_node_id_result(&self, idx: usize) -> Result<String, MmapError> {
        if idx >= self.node_count as usize {
            return Err(MmapError::Corrupt(format!(
                "node ID row {} exceeds row count {}",
                idx, self.node_count
            )));
        }
        let n = self.node_count as usize;
        let offsets_start = MmapHeader::SIZE;
        let lengths_start = offsets_start + n * 8;
        let offset_start = offsets_start + idx * 8;
        let length_start = lengths_start + idx * 4;

        let off = u64::from_le_bytes(
            self.mmap
                .get(offset_start..offset_start + 8)
                .ok_or_else(|| {
                    MmapError::Corrupt(format!(
                        "node ID offset row {} exceeds mapped file bounds",
                        idx
                    ))
                })?
                .try_into()
                .map_err(|_| MmapError::Corrupt(format!("invalid node ID offset row {}", idx)))?,
        ) as usize;
        let len = u32::from_le_bytes(
            self.mmap
                .get(length_start..length_start + 4)
                .ok_or_else(|| {
                    MmapError::Corrupt(format!(
                        "node ID length row {} exceeds mapped file bounds",
                        idx
                    ))
                })?
                .try_into()
                .map_err(|_| MmapError::Corrupt(format!("invalid node ID length row {}", idx)))?,
        ) as usize;

        let id_start = self
            .ids_section_offset
            .checked_add(off)
            .ok_or_else(|| MmapError::Corrupt(format!("node ID row {} offset overflow", idx)))?;
        let id_end = id_start
            .checked_add(len)
            .ok_or_else(|| MmapError::Corrupt(format!("node ID row {} length overflow", idx)))?;
        let bytes = self.mmap.get(id_start..id_end).ok_or_else(|| {
            MmapError::Corrupt(format!("node ID row {} exceeds mapped file bounds", idx))
        })?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|e| MmapError::Corrupt(format!("node ID row {} is not UTF-8: {}", idx, e)))
    }

    /// Read the embedding vector at the given index.
    fn read_embedding(&self, idx: usize) -> Vec<f32> {
        self.embedding_slice_by_index(idx as u32)
            .map(<[f32]>::to_vec)
            .unwrap_or_default()
    }
}

fn read_node_id_from_mmap(
    mmap: &[u8],
    node_count: u32,
    ids_section_offset: usize,
    idx: usize,
) -> Result<String, MmapError> {
    let n = node_count as usize;
    let offsets_start = MmapHeader::SIZE;
    let lengths_start = offsets_start + n * 8;
    let offset_start = offsets_start + idx * 8;
    let length_start = lengths_start + idx * 4;
    let off = u64::from_le_bytes(
        mmap.get(offset_start..offset_start + 8)
            .ok_or_else(|| MmapError::Corrupt("node ID offset exceeds mapped file".into()))?
            .try_into()
            .map_err(|_| MmapError::Corrupt("invalid node ID offset".into()))?,
    ) as usize;
    let len = u32::from_le_bytes(
        mmap.get(length_start..length_start + 4)
            .ok_or_else(|| MmapError::Corrupt("node ID length exceeds mapped file".into()))?
            .try_into()
            .map_err(|_| MmapError::Corrupt("invalid node ID length".into()))?,
    ) as usize;
    let start = ids_section_offset
        .checked_add(off)
        .ok_or_else(|| MmapError::Corrupt("node ID offset overflow".into()))?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| MmapError::Corrupt("node ID length overflow".into()))?;
    let bytes = mmap
        .get(start..end)
        .ok_or_else(|| MmapError::Corrupt("node ID exceeds mapped file".into()))?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| MmapError::Corrupt(format!("node ID is not UTF-8: {error}")))
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

fn build_id_tables(embeddings: &[(String, Vec<f32>)]) -> (Vec<u8>, Vec<u64>, Vec<u32>) {
    let mut id_bytes = Vec::new();
    let mut id_offsets: Vec<u64> = Vec::with_capacity(embeddings.len());
    let mut id_lengths: Vec<u32> = Vec::with_capacity(embeddings.len());

    for (id, _) in embeddings {
        id_offsets.push(id_bytes.len() as u64);
        let id_bytes_len = id.len() as u32;
        id_lengths.push(id_bytes_len);
        id_bytes.extend_from_slice(id.as_bytes());
    }
    (id_bytes, id_offsets, id_lengths)
}

fn write_id_tables<W: std::io::Write>(
    writer: &mut W,
    id_offsets: &[u64],
    id_lengths: &[u32],
) -> std::io::Result<()> {
    let mut table_buf = Vec::with_capacity(id_offsets.len() * 8);
    for off in id_offsets {
        table_buf.extend_from_slice(&off.to_le_bytes());
    }
    writer.write_all(&table_buf)?;

    table_buf.clear();
    table_buf.reserve(id_lengths.len() * 4);
    for len in id_lengths {
        table_buf.extend_from_slice(&len.to_le_bytes());
    }
    writer.write_all(&table_buf)
}

fn write_matrix_rows<W: std::io::Write>(
    writer: &mut W,
    embeddings: &[(String, Vec<f32>)],
    dim: usize,
) -> std::io::Result<()> {
    let mut row_buf = Vec::with_capacity(dim.saturating_mul(4));
    for (_, embedding) in embeddings {
        row_buf.clear();
        for val in embedding.iter().take(dim) {
            row_buf.extend_from_slice(&val.to_le_bytes());
        }
        writer.write_all(&row_buf)?;
    }
    Ok(())
}

/// Publish the mmap artifact at `path` through a uniquely named sibling
/// staging file, `sync_all`, and an atomic `rename`.
///
/// The previous shape — `File::create(path)` then `BufWriter::flush` — had two
/// holes. `File::create` truncates the existing generation at open time, so a
/// crash anywhere inside `fill` leaves the canonical file zero-length or
/// half-written; and `flush` reaches the kernel page cache only, which is not
/// durability. Losing this file is not losing rows: it is also the sole
/// carrier of the generation identity trailer, so a torn write silently swaps
/// query embedding into a different vector space than the tier sidecars hold.
///
/// `rename` within one filesystem is atomic, so a concurrent reader — including
/// a live `MmapEmbeddingIndex` — sees either the whole old file or the whole
/// new one. The staging name is unique per call: a fixed `.tmp` name would let
/// two concurrent index runs (or a canonical file and its tier twin) interleave
/// into each other's bytes.
fn write_atomic(
    path: &Path,
    fill: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<(), MmapError>,
) -> Result<(), MmapError> {
    let staging = staging_path(path)?;

    let result = (|| -> Result<(), MmapError> {
        let mut writer =
            std::io::BufWriter::with_capacity(1024 * 1024, std::fs::File::create(&staging)?);
        fill(&mut writer)?;
        writer.flush()?;
        // Page cache is not durability: sync the staging file before it takes
        // the canonical name, or a crash right after the rename publishes a
        // generation whose bytes never reached the device.
        writer.get_ref().sync_all()?;
        drop(writer);
        std::fs::rename(&staging, path)?;
        sync_parent_dir(path);
        Ok(())
    })();

    if result.is_err() {
        // Never leave droppings behind on a failed index.
        let _ = std::fs::remove_file(&staging);
    }
    result
}

/// The unique sibling a [`write_atomic`] caller stages into, with its parent
/// directory created so the open below cannot fail on a missing generation dir.
fn staging_path(path: &Path) -> Result<std::path::PathBuf, MmapError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MmapError::Corrupt(format!(
                "embedding path {} has no file name",
                path.display()
            ))
        })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    static STAGING_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(path.with_file_name(format!("{file_name}.tmp-{}-{seq}", std::process::id())))
}

/// Make a completed `rename` durable, best effort.
///
/// Some filesystems reject a directory fsync, and losing only rename-durability
/// is acceptable for a rebuildable index artifact — hence no `?`.
fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
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
/// Optional identity trailer: u32 LE byte length followed by UTF-8 identity.
/// ```
pub fn write_mmap_embeddings(
    path: &Path,
    embeddings: &[(String, Vec<f32>)],
) -> Result<(), MmapError> {
    write_mmap_embeddings_with_identity(path, embeddings, None)
}

/// Write embeddings to a binary file in the mmap format, optionally recording
/// the generation identity in an in-band trailer.
pub fn write_mmap_embeddings_with_identity(
    path: &Path,
    embeddings: &[(String, Vec<f32>)],
    identity: Option<&str>,
) -> Result<(), MmapError> {
    let identity_len = identity.map(identity_len_u32).transpose()?;
    let node_count = embeddings.len() as u32;
    let dimension = if embeddings.is_empty() {
        0u32
    } else {
        embeddings[0].1.len() as u32
    };

    let (id_bytes, id_offsets, id_lengths) = build_id_tables(embeddings);

    // Compute section sizes.
    let header_size = MmapHeader::SIZE;
    let offsets_table_size = embeddings.len() * 8;
    let lengths_table_size = embeddings.len() * 4;
    let ids_end = header_size + offsets_table_size + lengths_table_size + id_bytes.len();
    let embedding_matrix_offset = (ids_end + 3) & !3; // align to 4 bytes
    let padding_len = embedding_matrix_offset - ids_end;

    // Stream the file through a BufWriter instead of materializing the whole
    // output as one heap buffer: the neural matrix alone is ~43 MB and the
    // previous `vec![0u8; total_size]` doubled the transient footprint of
    // every persist (RAM-spike trim). The byte layout is unchanged.
    write_atomic(path, |writer| {
        write_header_and_ids(
            writer,
            MmapHeader {
                magic: MMAP_MAGIC,
                version: MMAP_VERSION,
                node_count,
                dimension,
            },
            &id_offsets,
            &id_lengths,
        )?;

        // ID strings + alignment padding
        writer.write_all(&id_bytes)?;
        if padding_len > 0 {
            writer.write_all(&vec![0u8; padding_len])?;
        }

        // Embedding matrix — one row (dim × 4 bytes) at a time.
        write_matrix_rows(writer, embeddings, dimension as usize)?;

        if let (Some(identity), Some(identity_len)) = (identity, identity_len) {
            writer.write_all(&identity_len.to_le_bytes())?;
            writer.write_all(identity.as_bytes())?;
        }
        Ok(())
    })
}

/// Byte length of a generation identity as the on-disk `u32` trailer length.
fn identity_len_u32(value: &str) -> Result<u32, MmapError> {
    u32::try_from(value.len())
        .map_err(|_| MmapError::Corrupt("generation identity byte length exceeds u32::MAX".into()))
}

/// Write a zero-row neural mmap that still carries an identity and a dimension.
///
/// [`write_mmap_embeddings_with_identity`] hardcodes `dimension = 0` for an
/// empty slice, which is right for the generic writer but wrong for a
/// *tier-only* neural generation: every row was sliced to a sub-native tier, so
/// no canonical rows exist, yet the header is the only artifact raw-read for
/// the persisted provider identity and native dimension (see
/// `query_remote_embedder`, which seeds the query embedder from
/// `dimension()`). A deleted canonical file therefore makes a tier-only
/// generation look absent, and query embedding then silently falls back to a
/// different vector space than the sidecars hold (Codex P1).
///
/// The file is a pure header: zero rows, the native dimension, the identity
/// trailer. Readers see `len() == 0` (so nothing is restored from it) while
/// still resolving identity and dimension.
pub fn write_empty_mmap_embeddings_with_identity(
    path: &Path,
    dimension: u32,
    identity: Option<&str>,
) -> Result<(), MmapError> {
    let identity_len = identity.map(identity_len_u32).transpose()?;

    let (id_bytes, id_offsets, id_lengths): (Vec<u8>, Vec<u64>, Vec<u32>) =
        (Vec::new(), Vec::new(), Vec::new());
    let header_size = MmapHeader::SIZE;
    let ids_end = header_size + id_bytes.len();
    let embedding_matrix_offset = (ids_end + 3) & !3; // align to 4 bytes
    let padding_len = embedding_matrix_offset - ids_end;

    write_atomic(path, |writer| {
        write_header_and_ids(
            writer,
            MmapHeader {
                magic: MMAP_MAGIC,
                version: MMAP_VERSION,
                node_count: 0,
                dimension,
            },
            &id_offsets,
            &id_lengths,
        )?;

        writer.write_all(&id_bytes)?;
        if padding_len > 0 {
            writer.write_all(&vec![0u8; padding_len])?;
        }

        if let (Some(identity), Some(identity_len)) = (identity, identity_len) {
            writer.write_all(&identity_len.to_le_bytes())?;
            writer.write_all(identity.as_bytes())?;
        }
        Ok(())
    })
}

/// Write the fixed header followed by the offset and length tables.
fn write_header_and_ids(
    writer: &mut impl std::io::Write,
    header: MmapHeader,
    id_offsets: &[u64],
    id_lengths: &[u32],
) -> std::io::Result<()> {
    let mut header_buf = [0u8; MmapHeader::SIZE];
    header.write_to(&mut header_buf);
    writer.write_all(&header_buf)?;
    write_id_tables(writer, id_offsets, id_lengths)
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
    fn test_mmap_identity_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.bin");
        let embeddings = vec![("node".to_string(), vec![0.5, 0.25])];

        write_mmap_embeddings_with_identity(
            &path,
            &embeddings,
            Some("remote|custom|http://127.0.0.1:8080/v1||mtest-model|2"),
        )
        .unwrap();
        let index = MmapEmbeddingIndex::open(&path).unwrap();
        assert_eq!(
            index.identity(),
            Some("remote|custom|http://127.0.0.1:8080/v1||mtest-model|2")
        );
    }

    #[test]
    fn test_mmap_identity_none_preserves_legacy_layout_and_ignores_arbitrary_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.bin");
        let embeddings = vec![("node".to_string(), vec![0.5, 0.25])];

        write_mmap_embeddings(&path, &embeddings).unwrap();
        let legacy = std::fs::read(&path).unwrap();
        let matrix_end = MmapEmbeddingIndex::open(&path)
            .unwrap()
            .embedding_matrix_offset
            + 2 * 4;
        assert_eq!(legacy.len(), matrix_end);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"arbitrary trailing bytes").unwrap();
        let index = MmapEmbeddingIndex::open(&path).unwrap();
        assert_eq!(index.identity(), None);
    }

    #[test]
    fn test_mmap_empty_index_identity_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty-identity.bin");

        write_mmap_embeddings_with_identity(&path, &[], Some("local|onnx/model")).unwrap();
        let index = MmapEmbeddingIndex::open(&path).unwrap();
        assert!(index.is_empty());
        assert_eq!(index.identity(), Some("local|onnx/model"));
    }

    /// A tier-only generation has no canonical rows but MUST still advertise its
    /// native dimension and provider identity: `query_remote_embedder` seeds the
    /// query embedder from both, so losing them silently swaps the query into a
    /// different vector space than the MRL sidecars hold (Codex P1). The generic
    /// writer hardcodes dimension 0 for an empty slice, which is exactly the bug
    /// this separate entry point exists to avoid.
    #[test]
    fn test_mmap_empty_index_keeps_dimension_and_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tier-only.bin");

        write_empty_mmap_embeddings_with_identity(&path, 768, Some("remote|custom|x|m")).unwrap();
        let index = MmapEmbeddingIndex::open(&path).unwrap();
        assert_eq!(index.len(), 0);
        assert_eq!(index.dimension(), 768);
        assert_eq!(index.identity(), Some("remote|custom|x|m"));

        // No identity: still a valid, openable header carrying the dimension.
        write_empty_mmap_embeddings_with_identity(&path, 768, None).unwrap();
        let index = MmapEmbeddingIndex::open(&path).unwrap();
        assert_eq!(index.len(), 0);
        assert_eq!(index.dimension(), 768);
        assert_eq!(index.identity(), None);
    }

    /// The canonical neural file is read by live mmaps (`MmapEmbeddingIndex`)
    /// and is the sole carrier of the generation identity trailer, so a
    /// truncate-then-rewrite publish leaves any concurrent reader holding a
    /// half-written image — and a crash inside that window destroys the file
    /// outright. Prove the replace is atomic from a reader's point of view: a
    /// handle opened before the second write must keep seeing the first
    /// generation's bytes, while the path itself resolves to the new one.
    #[test]
    fn test_write_mmap_embeddings_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.bin");

        let two_rows: Vec<(String, Vec<f32>)> = vec![
            ("func_a".to_string(), vec![1.0, 0.0, 0.0]),
            ("func_b".to_string(), vec![0.0, 1.0, 0.0]),
        ];
        write_mmap_embeddings_with_identity(&path, &two_rows, Some("local|model|3")).unwrap();
        let published = std::fs::read(&path).unwrap();

        // A reader that opened the first generation before the second publish.
        use std::io::Read;
        let mut reader = std::fs::File::open(&path).unwrap();

        write_mmap_embeddings_with_identity(
            &path,
            &[("func_a".to_string(), vec![1.0, 0.0, 0.0])],
            Some("local|model|3"),
        )
        .unwrap();

        // The path resolves to exactly one row...
        let index = MmapEmbeddingIndex::open(&path).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index.dimension(), 3);
        assert_eq!(index.identity(), Some("local|model|3"));

        // ...and the pre-existing reader still sees the complete first
        // generation, not a truncated or half-overwritten file.
        let mut observed = Vec::new();
        reader.read_to_end(&mut observed).unwrap();
        assert_eq!(
            observed, published,
            "an in-place rewrite truncates the file under live readers"
        );
    }

    #[test]
    fn test_empty_mmap_publish_preserves_open_reader_and_failed_write_preserves_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.bin");
        write_mmap_embeddings_with_identity(
            &path,
            &[("node".to_string(), vec![1.0, 0.0])],
            Some("old"),
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut reader = std::fs::File::open(&path).unwrap();
        write_empty_mmap_embeddings_with_identity(&path, 2, Some("new")).unwrap();
        let mut observed = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut observed).unwrap();
        assert_eq!(observed, original);
        let empty = MmapEmbeddingIndex::open(&path).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.dimension(), 2);
        assert_eq!(empty.identity(), Some("new"));

        let published = std::fs::read(&path).unwrap();
        let result = write_atomic(&path, |writer| {
            writer.write_all(b"incomplete header")?;
            Err(MmapError::Corrupt("injected write failure".into()))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), published);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn test_write_mmap_embeddings_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("embeddings.bin");

        write_mmap_embeddings_with_identity(
            &path,
            &[("func_a".to_string(), vec![1.0, 0.0])],
            Some("local|model|2"),
        )
        .unwrap();
        write_empty_mmap_embeddings_with_identity(&path, 2, Some("local|model|2")).unwrap();

        let mut leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "embeddings.bin")
            .collect();
        leftovers.sort();
        assert!(
            leftovers.is_empty(),
            "atomic publish must not leave staging files behind: {leftovers:?}"
        );
    }

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

    #[test]
    fn test_mmap_entries_reports_invalid_utf8_node_id() {
        use std::io::{Seek, SeekFrom, Write};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid-id.bin");
        write_mmap_embeddings(&path, &[("a".to_string(), vec![1.0])]).unwrap();

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start((MmapHeader::SIZE + 8 + 4) as u64))
            .unwrap();
        file.write_all(&[0xff]).unwrap();
        file.flush().unwrap();

        let index = MmapEmbeddingIndex::open(&path).unwrap();
        let error = index.entries().unwrap_err().to_string();
        assert!(error.contains("node ID row 0 is not UTF-8"), "{error}");
    }

    #[test]
    fn test_mmap_rejects_unsupported_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unsupported.bin");
        let mut header = *b"LIEE\0\0\0\0\0\0\0\0\0\0\0\0";
        header[4..8].copy_from_slice(&(MMAP_VERSION + 1).to_le_bytes());
        std::fs::write(&path, header).unwrap();

        let error = MmapEmbeddingIndex::open(&path).unwrap_err().to_string();
        assert!(error.contains("unsupported version 2"), "{error}");
    }

    #[test]
    fn test_mmap_rejects_missing_offset_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-offsets.bin");
        let mut header = *b"LIEE\0\0\0\0\0\0\0\0\0\0\0\0";
        header[4..8].copy_from_slice(&MMAP_VERSION.to_le_bytes());
        header[8..12].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(&path, header).unwrap();

        let error = MmapEmbeddingIndex::open(&path).unwrap_err().to_string();
        assert!(
            error.contains("offset table exceeds file size: table size 8, file size 16"),
            "{error}"
        );
    }

    #[test]
    fn test_embedding_matrix_size_overflow_is_checked() {
        let error = checked_embedding_matrix_size(u32::MAX, u32::MAX)
            .unwrap_err()
            .to_string();
        assert!(error.contains("embedding matrix size overflow"), "{error}");
    }
}
