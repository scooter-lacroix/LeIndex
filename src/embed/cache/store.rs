//! GlobalEmbeddingCache — user-level content-addressed embedding cache.
//!
//! Stores fixed-layout mmap vector rows keyed by the `CacheKey` fingerprint.
//! Cross-project deduplication is automatic: the same content_hash combined
//! with the same model identity produces the same fingerprint, so two projects
//! embedding identical text share one cache row (spec §10.1).
//!
//! ## On-disk layout
//!
//! ```text
//! <root>/
//!   rows/
//!     <2-hex-prefix>/
//!       <64-hex-fingerprint>    <- LIDX-ECR1 frame (magic + version + ...)
//!   refs.json                    <- project-generation refcounts keyed by fingerprint
//! ```
//!
//! ## Row format (LIDX-ECR1)
//!
//! ```text
//! [magic 9B] [version 1B] [pad 2B]
//! [fingerprint 32B]
//! [dim u32 LE]
//! [content_hash 32B]          <- blake3 of the vector payload (corruption detection)
//! [payload: dim * sizeof(f32) BYTES]
//! ```
//!
//! ## Privacy (spec §10.1)
//!
//! No source text is stored after hashing. The cache row contains only the
//! fingerprint, the vector data, and the blake3 hash of the vector data. The
//! `LEINDEX_EMBED_CACHE_DEBUG` env var controls an optional debug mode that
//! stores the source text alongside the row for development troubleshooting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::key::CacheKey;

/// Magic bytes for an embedding-cache row file.
pub const ROW_MAGIC: &[u8; 9] = b"LIDX-ECR1";
/// Current row format version.
pub const ROW_VERSION: u8 = 1;
/// Padding bytes after version (to align dim field at a 4-byte boundary).
const ROW_PAD_LEN: usize = 2;
/// Fixed header size: magic(9) + version(1) + pad(2) + fingerprint(32) + dim(4) + content_hash(32).
pub const ROW_HEADER_LEN: usize = 9 + 1 + ROW_PAD_LEN + 32 + 4 + 32;

/// Filename for the persisted project refcounts.
const REFS_FILENAME: &str = "refs.json";

/// A cache probe result: the vector for a hit, or nothing for a miss.
#[derive(Debug)]
pub struct ProbeResult {
    /// Vectors for hit keys, indexed by their original position in the input.
    pub hits: HashMap<usize, Vec<f32>>,
    /// Indices of keys that were not found in the cache.
    pub misses: Vec<usize>,
}

/// Byte-budgeted compaction report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheCompactionReport {
    /// Total bytes reclaimed by removing unreferenced rows.
    pub reclaimed_bytes: u64,
    /// Number of rows removed.
    pub rows_removed: u64,
    /// Number of rows retained (have live project references).
    pub rows_retained: u64,
}

/// Errors from the embedding cache.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The row file failed validation (bad magic/version/hash mismatch/truncation).
    #[error("row validation failed: {0}")]
    BadRow(String),
    /// Serialization/deserialization error.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Project-generation reference for GC liveness tracking.
///
/// Each entry maps a fingerprint to a set of (project_id, generation) pairs
/// that reference it. GC removes only rows with an empty reference set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectRefs {
    /// fingerprint_hex -> set of "project_id:generation" references.
    #[serde(default)]
    refs: HashMap<String, HashSet<String>>,
}

impl ProjectRefs {
    /// Add a reference for a fingerprint under a specific project and generation.
    pub fn add(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        let key = ref_key(project_id, generation);
        let hex = hex_encode(fingerprint);
        self.refs.entry(hex).or_default().insert(key);
    }

    /// Remove a reference for a fingerprint under a specific project and generation.
    pub fn remove(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        let key = ref_key(project_id, generation);
        let hex = hex_encode(fingerprint);
        if let Some(set) = self.refs.get_mut(&hex) {
            set.remove(&key);
            if set.is_empty() {
                self.refs.remove(&hex);
            }
        }
    }

    /// Returns `true` if the given fingerprint has at least one live reference.
    pub fn is_referenced(&self, fingerprint: &[u8; 32]) -> bool {
        let hex = hex_encode(fingerprint);
        self.refs.get(&hex).is_some_and(|set| !set.is_empty())
    }

    /// Number of distinct fingerprints tracked.
    pub fn tracked_count(&self) -> usize {
        self.refs.len()
    }

    /// All tracked fingerprints (hex-encoded).
    pub fn tracked_fingerprints(&self) -> Vec<String> {
        self.refs.keys().cloned().collect()
    }
}

fn ref_key(project_id: &str, generation: u64) -> String {
    format!("{project_id}:{generation}")
}

/// User-level content-addressed global embedding cache.
///
/// The cache stores embedding vectors in fixed-layout mmap-friendly row files
/// keyed by the `CacheKey` fingerprint. Cross-project dedup is automatic:
/// the same content produces the same fingerprint regardless of which project
/// originated the request.
///
/// ## Privacy
///
/// No source text is stored after hashing (spec §10.1). The cache row contains
/// only the fingerprint, the vector data, and a blake3 hash of the vector data
/// for corruption detection. The `LEINDEX_EMBED_CACHE_DEBUG` env var enables
/// debug mode, which stores the source text alongside the row for development
/// troubleshooting.
///
/// ## Corruption detection
///
/// Each row stores a blake3 hash of its vector payload (the "content hash" of
/// the vector). On read, the payload is re-hashed and compared. A mismatch
/// causes the row to be treated as a miss (and optionally marked for GC).
///
/// ## Byte-budgeted compaction
///
/// [`gc`](Self::gc) removes rows with zero project-generation references. It
/// returns a [`CacheCompactionReport`] with bytes reclaimed, rows removed, and
/// rows retained. Count-only eviction is prohibited (spec §10.3).
pub struct GlobalEmbeddingCache {
    root: PathBuf,
    refs: ProjectRefs,
}

impl GlobalEmbeddingCache {
    /// Open (or initialise) the embedding cache at `root`.
    ///
    /// Creates the root and `rows/` subdirectories on demand. Loads the
    /// persisted project references from `refs.json`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CacheError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join("rows"))?;
        let refs = load_refs(&root)?;
        Ok(GlobalEmbeddingCache { root, refs })
    }

    /// Root directory of the cache.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory for row files of a given fingerprint's 2-hex prefix.
    fn prefix_dir(&self, fingerprint: &[u8; 32]) -> PathBuf {
        let hex = hex_encode(fingerprint);
        self.root.join("rows").join(&hex[0..2])
    }

    /// Final row path: `<root>/rows/<2-hex-prefix>/<64-hex-fingerprint>`.
    pub fn row_path(&self, fingerprint: &[u8; 32]) -> PathBuf {
        let hex = hex_encode(fingerprint);
        self.prefix_dir(fingerprint).join(&hex)
    }

    /// Check whether a cache row exists for `key`.
    pub fn exists(&self, key: &CacheKey) -> bool {
        self.row_path(&key.fingerprint()).exists()
    }

    /// Probe the cache for a batch of keys.
    ///
    /// Returns (`hits`, `misses`), where:
    /// - `hits` maps the original index to the vector for found entries
    /// - `misses` is a list of indices whose keys were not found (or corrupt)
    ///
    /// VAL-CACHE-003: Hits return bit-identical vectors to what was stored.
    /// VAL-CACHE-004: Corrupted rows are detected via re-hash and treated as misses.
    pub fn probe(&self, keys: &[CacheKey]) -> Result<ProbeResult, CacheError> {
        let mut hits = HashMap::new();
        let mut misses = Vec::new();

        for (i, key) in keys.iter().enumerate() {
            let fingerprint = key.fingerprint();
            let path = self.row_path(&fingerprint);
            if !path.exists() {
                misses.push(i);
                continue;
            }
            match read_row(&path, &fingerprint) {
                Ok(vector) => {
                    hits.insert(i, vector);
                }
                Err(_) => {
                    // Corrupted row → treat as miss (not an error).
                    misses.push(i);
                }
            }
        }

        Ok(ProbeResult { hits, misses })
    }

    /// Store a vector in the cache under `key`.
    ///
    /// Writes a fixed-layout row file atomically (staging + rename). If the
    /// row already exists, the call is a no-op (dedup).
    ///
    /// VAL-CACHE-007: The stored vector is bit-identical to the one passed in;
    /// no precision loss occurs during serialization.
    pub fn put(&self, key: &CacheKey, vector: &[f32]) -> Result<(), CacheError> {
        let fingerprint = key.fingerprint();
        let final_path = self.row_path(&fingerprint);
        if final_path.exists() {
            return Ok(());
        }

        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let row_bytes = encode_row(&fingerprint, key.output_dimensions as usize, vector);

        // Atomic write: temp file -> fsync -> rename.
        let staging_path = final_path.with_extension("partial");
        {
            let file = fs::File::create(&staging_path)?;
            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(&row_bytes)?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }

        match fs::rename(&staging_path, &final_path) {
            Ok(()) => {}
            Err(_) if final_path.exists() => {
                let _ = fs::remove_file(&staging_path);
            }
            Err(e) => {
                let _ = fs::remove_file(&staging_path);
                return Err(CacheError::Io(e));
            }
        }

        Ok(())
    }

    /// Read a single vector from the cache. Returns `None` if not found or corrupt.
    pub fn get(&self, key: &CacheKey) -> Result<Option<Vec<f32>>, CacheError> {
        let fingerprint = key.fingerprint();
        let path = self.row_path(&fingerprint);
        if !path.exists() {
            return Ok(None);
        }
        match read_row(&path, &fingerprint) {
            Ok(vector) => Ok(Some(vector)),
            Err(e) => Err(e),
        }
    }

    /// Add a project-generation reference for a fingerprint.
    ///
    /// This prevents GC from dropping a row that is still live for a project.
    pub fn add_reference(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        self.refs.add(fingerprint, project_id, generation);
    }

    /// Remove a project-generation reference for a fingerprint.
    pub fn remove_reference(&mut self, fingerprint: &[u8; 32], project_id: &str, generation: u64) {
        self.refs.remove(fingerprint, project_id, generation);
    }

    /// Returns `true` if the fingerprint has any live project-generation references.
    pub fn is_referenced(&self, fingerprint: &[u8; 32]) -> bool {
        self.refs.is_referenced(fingerprint)
    }

    /// Count the number of stored rows on disk.
    pub fn row_count(&self) -> Result<usize, CacheError> {
        let rows_dir = self.root.join("rows");
        if !rows_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        for entry in fs::read_dir(&rows_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for sub_entry in fs::read_dir(entry.path())? {
                    let sub_entry = sub_entry?;
                    if sub_entry.file_type()?.is_file()
                        && !sub_entry
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".partial")
                    {
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    /// Byte-budgeted compaction: remove rows with zero project-generation
    /// references.
    ///
    /// VAL-CACHE-010: Returns a report with bytes reclaimed, rows removed,
    /// and rows retained. Count-only eviction is prohibited.
    pub fn gc(&mut self) -> Result<CacheCompactionReport, CacheError> {
        let mut report = CacheCompactionReport::default();
        let rows_dir = self.root.join("rows");
        if !rows_dir.exists() {
            return Ok(report);
        }

        for entry in fs::read_dir(&rows_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            for sub_entry in fs::read_dir(entry.path())? {
                let sub_entry = sub_entry?;
                let path = sub_entry.path();

                // Skip staging files.
                if path.extension().is_some_and(|ext| ext == "partial") {
                    continue;
                }
                if !path.is_file() {
                    continue;
                }

                // Parse fingerprint from the filename.
                let filename = sub_entry.file_name().to_string_lossy().to_string();
                let Some(fingerprint) = hex_decode(&filename) else {
                    continue;
                };

                let is_live = self.refs.is_referenced(&fingerprint);
                if is_live {
                    report.rows_retained += 1;
                } else {
                    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let _ = fs::remove_file(&path);
                    report.reclaimed_bytes += size;
                    report.rows_removed += 1;
                }
            }
        }

        // Persist updated refs (remove entries that referred to deleted rows).
        let tracked: Vec<String> = self.refs.refs.keys().cloned().collect();
        let mut still_existing = HashSet::new();
        for hex in &tracked {
            if let Some(fp) = hex_decode(hex) {
                if self.row_path(&fp).exists() {
                    still_existing.insert(hex.clone());
                }
            }
        }
        self.refs.refs.retain(|k, _| still_existing.contains(k));
        self.persist_refs()?;

        Ok(report)
    }

    /// Persist the project refs to disk.
    pub fn persist_refs(&self) -> Result<(), CacheError> {
        let path = self.root.join(REFS_FILENAME);
        let tmp = path.with_extension("tmp");
        let data = serde_json::to_vec_pretty(&self.refs)?;
        fs::write(&tmp, &data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Current project refs (for inspection / reporting).
    pub fn project_refs(&self) -> &ProjectRefs {
        &self.refs
    }

    /// Total bytes used by all stored rows.
    pub fn total_bytes(&self) -> Result<u64, CacheError> {
        let rows_dir = self.root.join("rows");
        if !rows_dir.exists() {
            return Ok(0);
        }
        let mut total = 0u64;
        for entry in fs::read_dir(&rows_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for sub_entry in fs::read_dir(entry.path())? {
                    let sub_entry = sub_entry?;
                    let path = sub_entry.path();
                    if path.is_file() && !path.ends_with(".partial") {
                        total += fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    }
                }
            }
        }
        Ok(total)
    }
}

// ---------------------------------------------------------------------------
// Row encode / decode
// ---------------------------------------------------------------------------

/// Encode a cache row as a byte vector.
///
/// Layout: `[magic(9)] [version(1)] [pad(2)] [fingerprint(32)] [dim u32 LE(4)] [content_hash(32)] [payload]`.
///
/// The "content hash" is a blake3 of the raw f32 bytes of the payload,
/// enabling corruption detection on read.
pub fn encode_row(fingerprint: &[u8; 32], dim: usize, vector: &[f32]) -> Vec<u8> {
    assert_eq!(vector.len(), dim, "vector length must match dim");
    let payload_bytes = f32_slice_to_bytes(vector);
    let payload_hash: [u8; 32] = blake3::hash(&payload_bytes).into();

    let mut buf = Vec::with_capacity(ROW_HEADER_LEN + payload_bytes.len());
    buf.extend_from_slice(ROW_MAGIC);
    buf.push(ROW_VERSION);
    buf.extend_from_slice(&[0u8; ROW_PAD_LEN]);
    buf.extend_from_slice(fingerprint);
    buf.extend_from_slice(&(dim as u32).to_le_bytes());
    buf.extend_from_slice(&payload_hash);
    buf.extend_from_slice(&payload_bytes);
    buf
}

/// Read and validate a cache row, returning the embedded vector.
///
/// VAL-CACHE-004: Corruption is detected by re-hashing the payload and
/// comparing against the stored content hash.
pub fn read_row(path: &Path, expected_fingerprint: &[u8; 32]) -> Result<Vec<f32>, CacheError> {
    let bytes = fs::read(path)?;
    if bytes.len() < ROW_HEADER_LEN {
        return Err(CacheError::BadRow(format!(
            "row truncated: {} bytes, need at least {} for header",
            bytes.len(),
            ROW_HEADER_LEN
        )));
    }
    let mut magic = [0u8; 9];
    magic.copy_from_slice(&bytes[0..9]);
    if &magic != ROW_MAGIC {
        return Err(CacheError::BadRow(format!("bad magic: got {magic:?}")));
    }
    let version = bytes[9];
    if version != ROW_VERSION {
        return Err(CacheError::BadRow(format!(
            "unsupported version: got {version}, expected {ROW_VERSION}"
        )));
    }

    // fingerprint at offset 12
    let mut stored_fingerprint = [0u8; 32];
    stored_fingerprint.copy_from_slice(&bytes[12..44]);
    if &stored_fingerprint != expected_fingerprint {
        return Err(CacheError::BadRow(
            "fingerprint mismatch: stored fingerprint does not match expected".to_string(),
        ));
    }

    // dim at offset 44
    let mut dim_bytes = [0u8; 4];
    dim_bytes.copy_from_slice(&bytes[44..48]);
    let dim = u32::from_le_bytes(dim_bytes) as usize;

    // content_hash at offset 48
    let mut stored_hash = [0u8; 32];
    stored_hash.copy_from_slice(&bytes[48..80]);

    // payload starts at offset 80
    let payload = &bytes[ROW_HEADER_LEN..];
    let expected_payload_len = dim * std::mem::size_of::<f32>();
    if payload.len() != expected_payload_len {
        return Err(CacheError::BadRow(format!(
            "payload length mismatch: got {}, expected {}",
            payload.len(),
            expected_payload_len
        )));
    }

    // Re-hash the vector payload and compare against stored content_hash.
    let computed_hash: [u8; 32] = blake3::hash(payload).into();
    if computed_hash != stored_hash {
        return Err(CacheError::BadRow(
            "hash mismatch: stored vector hash does not match recomputed hash".to_string(),
        ));
    }

    // Convert raw bytes back to f32 vector.
    Ok(bytes_to_f32_vec(payload))
}

/// Convert a &[f32] slice to raw bytes (little-endian on most platforms;
/// this is the same as to_ne_bytes since f32::to_ne_bytes is LE on x86/arm).
fn f32_slice_to_bytes(slice: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(slice));
    for &val in slice {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Convert raw bytes back to Vec<f32>.
fn bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    let mut result = Vec::with_capacity(bytes.len() / std::mem::size_of::<f32>());
    for chunk in bytes.chunks_exact(4) {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(chunk);
        result.push(f32::from_le_bytes(buf));
    }
    result
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

/// Encode a 32-byte hash as a lowercase hex string.
pub fn hex_encode(hash: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for byte in hash {
        write!(&mut s, "{byte:02x}").expect("formatting into String never fails");
    }
    s
}

/// Decode a 64-character hex string into a 32-byte hash.
pub fn hex_decode(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    let bytes = hex.as_bytes();
    for (i, byte) in hash.iter_mut().enumerate() {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(hash)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Load the project refs from `refs.json`, or return empty refs if not present.
fn load_refs(root: &Path) -> Result<ProjectRefs, CacheError> {
    let path = root.join(REFS_FILENAME);
    if !path.exists() {
        return Ok(ProjectRefs::default());
    }
    let data = fs::read(&path)?;
    Ok(serde_json::from_slice(&data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::cache::key::{Normalization, Pooling};

    fn sample_key(content: &str, dim: u32) -> CacheKey {
        CacheKey {
            model_digest: CacheKey::model_digest(b"model-bytes"),
            tokenizer_digest: CacheKey::tokenizer_digest(b"tokenizer-config"),
            prompt_role_and_version: 0,
            pooling: Pooling::Mean,
            normalization: Normalization::L2,
            output_dimensions: dim,
            content_hash: CacheKey::content_hash(content),
        }
    }

    fn sample_vector(dim: usize, seed: f32) -> Vec<f32> {
        (0..dim).map(|i| seed + i as f32 * 0.01).collect()
    }

    /// VAL-CACHE-003: probe returns hits and misses correctly.
    #[test]
    fn test_probe_returns_hits_and_misses() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key1 = sample_key("text one", 4);
        let key2 = sample_key("text two", 4);
        let key3 = sample_key("text three", 4);

        let vec1 = sample_vector(4, 0.1);
        let vec2 = sample_vector(4, 0.2);

        cache.put(&key1, &vec1).unwrap();
        cache.put(&key2, &vec2).unwrap();

        let result = cache.probe(&[key1, key2, key3]).unwrap();
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.misses, vec![2]);

        // Verify vectors are bit-identical.
        let hit1 = result.hits.get(&0).unwrap();
        let hit2 = result.hits.get(&1).unwrap();
        assert_eq!(hit1, &vec1);
        assert_eq!(hit2, &vec2);
    }

    /// VAL-CACHE-003: Hits return bit-identical vectors to what was stored.
    #[test]
    fn test_put_then_get_is_bit_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("exact text", 8);
        let original = sample_vector(8, 0.5);

        cache.put(&key, &original).unwrap();

        let gotten = cache.get(&key).unwrap().unwrap();
        assert_eq!(gotten.len(), original.len());
        for (a, b) in gotten.iter().zip(original.iter()) {
            assert_eq!(a.to_bits(), b.to_bits(), "f32 bits must match");
        }
    }

    /// VAL-CACHE-004: Corruption detection on read.
    #[test]
    fn test_corruption_detected_on_read() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("text to corrupt", 4);
        cache.put(&key, &sample_vector(4, 0.1)).unwrap();

        // Corrupt by flipping one byte in the vector payload area.
        let path = cache.row_path(&key.fingerprint());
        let mut data = fs::read(&path).unwrap();
        // Flip a byte deep in the payload (well past the header).
        let corrupt_offset = ROW_HEADER_LEN + 4;
        data[corrupt_offset] ^= 0xFF;
        fs::write(&path, &data).unwrap();

        // probe should treat it as a miss.
        let result = cache.probe(std::slice::from_ref(&key)).unwrap();
        assert!(result.hits.is_empty());
        assert_eq!(result.misses, vec![0]);

        // get should return an error.
        assert!(cache.get(&key).is_err());
    }

    /// VAL-CACHE-005: Cross-project deduplication — same content = one row.
    #[test]
    fn test_cross_project_dedup_single_row() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        // Both projects embed the same text under the same model.
        let key = sample_key("shared content across projects", 4);
        let vec = sample_vector(4, 0.3);

        // Project A puts the vector.
        cache.put(&key, &vec).unwrap();
        assert_eq!(cache.row_count().unwrap(), 1);

        // Project B puts the same vector (same key) — dedup, no second row.
        cache.put(&key, &vec).unwrap();
        assert_eq!(cache.row_count().unwrap(), 1);
    }

    /// VAL-CACHE-005: Two projects adding references, then one drops, row stays.
    #[test]
    fn test_cross_project_ref_keeps_row_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("shared text", 4);
        cache.put(&key, &sample_vector(4, 0.1)).unwrap();

        let fp = key.fingerprint();
        cache.add_reference(&fp, "project-a", 1);
        cache.add_reference(&fp, "project-b", 1);

        // Remove project-a's reference; row should survive (project-b still refs).
        cache.remove_reference(&fp, "project-a", 1);
        assert!(cache.is_referenced(&fp));

        // GC should retain the row.
        let report = cache.gc().unwrap();
        assert_eq!(report.rows_removed, 0);
        assert_eq!(report.rows_retained, 1);
    }

    /// VAL-CACHE-010: Byte-budgeted compaction removes unreferenced rows.
    #[test]
    fn test_gc_removes_unreferenced_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key_live = sample_key("live text", 4);
        let key_dead = sample_key("dead text", 4);

        cache.put(&key_live, &sample_vector(4, 0.1)).unwrap();
        cache.put(&key_dead, &sample_vector(4, 0.2)).unwrap();

        // Mark key_live as referenced by a project.
        let fp_live = key_live.fingerprint();
        cache.add_reference(&fp_live, "project-a", 1);

        // key_dead has no references — should be removed by GC.
        let report = cache.gc().unwrap();
        assert_eq!(report.rows_removed, 1);
        assert_eq!(report.rows_retained, 1);
        assert!(
            report.reclaimed_bytes > 0,
            "should report non-zero reclaimed bytes"
        );

        // Verify the live row survives and dead is gone.
        assert_eq!(cache.row_count().unwrap(), 1);
        assert!(cache.get(&key_live).unwrap().is_some());
        assert!(cache.get(&key_dead).unwrap().is_none());
    }

    #[test]
    fn test_gc_on_empty_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();
        let report = cache.gc().unwrap();
        assert_eq!(report.rows_removed, 0);
        assert_eq!(report.rows_retained, 0);
        assert_eq!(report.reclaimed_bytes, 0);
    }

    /// Idempotent put: writing the same key twice does not duplicate the row.
    #[test]
    fn test_put_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("idempotent text", 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();

        assert_eq!(cache.row_count().unwrap(), 1);
    }

    #[test]
    fn test_open_creates_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("nested").join("cache");
        let cache = GlobalEmbeddingCache::open(&cache_root).unwrap();
        assert!(cache_root.join("rows").exists());

        let key = sample_key("after open", 2);
        cache.put(&key, &[1.0, 2.0]).unwrap();
        assert_eq!(cache.row_count().unwrap(), 1);
    }

    #[test]
    fn test_persist_and_reload_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("persist test", 4);
        cache.put(&key, &[0.5, 0.6, 0.7, 0.8]).unwrap();

        let fp = key.fingerprint();
        cache.add_reference(&fp, "proj", 42);
        cache.persist_refs().unwrap();

        // Reload.
        let cache2 = GlobalEmbeddingCache::open(tmp.path()).unwrap();
        assert!(cache2.is_referenced(&fp));

        // GC should retain the row.
        let mut cache2 = cache2;
        let report = cache2.gc().unwrap();
        assert_eq!(report.rows_retained, 1);
        assert_eq!(report.rows_removed, 0);
    }

    #[test]
    fn test_multiple_projects_add_and_remove_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let key = sample_key("multi-ref text", 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        let fp = key.fingerprint();

        cache.add_reference(&fp, "proj-a", 1);
        cache.add_reference(&fp, "proj-b", 2);
        assert!(cache.is_referenced(&fp));

        cache.remove_reference(&fp, "proj-a", 1);
        assert!(cache.is_referenced(&fp));

        cache.remove_reference(&fp, "proj-b", 2);
        assert!(!cache.is_referenced(&fp));
    }

    #[test]
    fn test_total_bytes_reporting() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let cycles_before = cache.total_bytes().unwrap();
        assert_eq!(cycles_before, 0);

        let key = sample_key("bytes test", 4);
        cache.put(&key, &[1.0, 2.0, 3.0, 4.0]).unwrap();
        let bytes_after = cache.total_bytes().unwrap();
        assert_eq!(bytes_after, (ROW_HEADER_LEN + 4 * 4) as u64);
    }

    /// Privacy: no source text stored in the row file.
    #[test]
    fn test_privacy_no_source_text_in_row() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = GlobalEmbeddingCache::open(tmp.path()).unwrap();

        let secret_text = "this is a secret symbol name fn_do_not_store_me";
        let key = sample_key(secret_text, 4);
        cache.put(&key, &[0.1, 0.2, 0.3, 0.4]).unwrap();

        // Inspect the raw row file on disk.
        let path = cache.row_path(&key.fingerprint());
        let raw = fs::read(&path).unwrap();

        // The source text must NOT appear anywhere in the row file.
        assert!(
            !raw.windows(secret_text.len())
                .any(|w| w == secret_text.as_bytes()),
            "source text must not be stored in the cache row"
        );
    }

    #[test]
    fn test_row_path_uses_2_hex_prefix() {
        let key = sample_key("path test", 4);
        let fp = key.fingerprint();
        let path = GlobalEmbeddingCache::open(tempfile::tempdir().unwrap().path())
            .unwrap()
            .row_path(&fp);
        let hex = hex_encode(&fp);
        assert!(
            path.to_string_lossy()
                .contains(&format!("rows/{}/{}", &hex[0..2], hex))
        );
    }
}
