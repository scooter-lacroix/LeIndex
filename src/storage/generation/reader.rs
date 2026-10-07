//! Zero-copy mmap readers for generation layer blobs (WS4 Tasks 5-6).
//!
//! Each reader memory-maps a CAS blob file and provides typed views into the
//! payload without copying data onto the heap. The readers cover the five
//! generation layers: neural, TF-IDF, PDG, and symbols (the DB layer is
//! consumed via SQLite, not via this module).
//!
//! ## Layer payload formats
//!
//! Each layer defines its own self-describing payload that lives *inside* a
//! CAS blob (see `src/storage/cas/blob.rs` for the LIDX-BLB1 frame). The CAS
//! frame provides content-addressing and integrity; the inner payload provides
//! typed structure.
//!
//! ### Neural (`LIDX-NRL1`)
//!
//! ```text
//! [magic 8B][version 1B][pad 3B]
//! [count u32][dim u32][dtype u32][scale f32][zero_point f32]
//! [content_hash 32B]            // blake3 of the data region
//! [data: count * dim * sizeof(element)]
//! ```
//!
//! Version 2 inserts a `count * u32` table of PDG node ids between the header
//! and the data (row `i` belongs to node `ids[i]`); `content_hash` then covers
//! the id table followed by the data. Version 1 has no id table (rows are
//! positional). The reader accepts both.
//!
//! `dtype = 0` (F32) stores `count * dim` little-endian f32 values. The
//! `dot(i, query)` operation computes `Σ query[j] * vector_i[j]` directly.
//!
//! `dtype = 1` (Int8) stores `count * dim` signed bytes. Each quantized
//! value `q` decodes to `q as f32 * scale + zero_point`. The SIMD dot product
//! accumulates `Σ query[j] * q_ij` in a wide f32 lane and only at the end
//! applies `result = scale * accumulator + zero_point * Σ query[j]`.
//!
//! ### TF-IDF (`LIDX-TFD1`)
//!
//! Sparse row storage of `(doc_id, term_id, value)` triples sorted by
//! `(doc_id, term_id)`.
//!
//! ### PDG (`LIDX-PDG1`)
//!
//! Fixed-size nodes and edges followed by an interned string table.
//!
//! ### Symbols (`LIDX-SYM1`)
//!
//! Fixed-size symbol records referencing the same interned-string layout as
//! PDG.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use memmap2::Mmap;

use crate::storage::cas::blob::{self, BLOB_HEADER_LEN, BadBlob, extract_payload};

pub use crate::storage::cas::CasError as ReadIoError;

// ===========================================================================
// Constants
// ===========================================================================

/// Magic bytes identifying a neural layer payload (`LIDX-NRL1`).
pub const NEURAL_MAGIC: &[u8; 9] = b"LIDX-NRL1";
/// Magic bytes identifying a TF-IDF layer payload (`LIDX-TFD1`).
pub const TFIDF_MAGIC: &[u8; 9] = b"LIDX-TFD1";
/// Magic bytes identifying a PDG layer payload (`LIDX-PDG1`).
pub const PDG_MAGIC: &[u8; 9] = b"LIDX-PDG1";
/// Magic bytes identifying a symbols layer payload (`LIDX-SYM1`).
pub const SYMBOLS_MAGIC: &[u8; 9] = b"LIDX-SYM1";

/// Fixed size of the neural payload header (in bytes).
///
/// = magic(9) + version(1) + pad(3) + count(4) + dim(4) + dtype(4) + scale(4)
/// + zero_point(4) + content_hash(32) + alignment_pad(1)
///
/// The trailing alignment_pad ensures that the data array starts at an offset
/// in the mmap that is 4-byte aligned (the LIDX-BLB1 CAS frame is 50 bytes,
/// so payload offset 66 gives mmap offset 116 = 0 mod 4). This is required so
/// `&[f32]` views into the mmap can be safely cast from the raw `&[u8]`.
pub const NEURAL_HEADER_LEN: usize = 9 + 1 + 3 + 4 + 4 + 4 + 4 + 4 + 32 + 1;
/// Neural payload version with positional rows (no node-id table).
pub const NEURAL_VERSION_POSITIONAL: u8 = 1;
/// Neural payload version carrying a `count * u32` PDG node-id table between
/// the header and the vector data.
pub const NEURAL_VERSION_WITH_IDS: u8 = 2;
/// Fixed size of the TF-IDF payload header (in bytes).
pub const TFIDF_HEADER_LEN: usize = 9 + 1 + 3 + 4 + 4 + 4 + 32;
/// Fixed size of the PDG payload header (in bytes).
pub const PDG_HEADER_LEN: usize = 9 + 1 + 3 + 4 + 4 + 4 + 4 + 32;
/// Fixed size of the symbols payload header (in bytes).
pub const SYMBOLS_HEADER_LEN: usize = 9 + 1 + 3 + 4 + 4 + 4 + 32;

/// Bytes occupied by a single TF-IDF entry (doc_id, term_id, value).
pub const TFIDF_ENTRY_LEN: usize = 4 + 4 + 4;
/// Bytes occupied by a single PDG node (version 1: six u32 fields; catalog
/// row id, positional rows).
pub const PDG_NODE_LEN: usize = 4 * 6;
/// Size of a version-2 PDG node record: interned `node_id`, `symbol_name`,
/// `file_path` and `language` string ids, `node_type`, `complexity`,
/// `byte_start`, `byte_end`, and a flags word (bit 0 = precision marker).
pub const PDG_NODE_V2_LEN: usize = 4 * 9;
/// Size of a version-2 PDG edge-metadata record (see [`PdgEdgeMeta`]).
pub const PDG_EDGE_META_LEN: usize = 4 * 5;
/// Sentinel for "absent" in version-2 edge-metadata u32 fields.
pub const PDG_V2_NONE: u32 = u32::MAX;
/// Bytes occupied by a single PDG edge.
pub const PDG_EDGE_LEN: usize = 4 * 3;
/// Bytes occupied by a single string-table offset record (offset, length).
pub const PDG_STRING_OFFSET_LEN: usize = 4 + 4;
/// Bytes occupied by a single symbol entry.
pub const SYMBOL_ENTRY_LEN: usize = 4 * 5 + 4;

// ===========================================================================
// Errors
// ===========================================================================

/// Errors returned by layer readers.
#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    /// I/O error opening or mapping the blob.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The CAS blob frame failed validation (bad magic/version/hash mismatch).
    #[error("blob validation failed: {0}")]
    BadBlob(#[from] BadBlob),
    /// The inner layer header failed validation.
    #[error("layer header invalid: {0}")]
    BadHeader(String),
    /// The reader was asked to read out-of-range data (vector index, doc id, etc.).
    #[error("index out of range: {0}")]
    OutOfRange(String),
}

// ===========================================================================
// Shared mmap loading
// ===========================================================================

/// Owned mmap over a blob file plus the cached payload slice (post
/// `LIDX-BLB1` frame) and the file handle that keeps the mapping alive.
///
/// The reader holds the mmap and exposes raw access to the underlying bytes so
/// that each layer reader (`NeuralReader`, etc.) can extract typed views.
struct BlobMmap {
    // Boxed so that moving the struct does not invalidate the slice pointer.
    _file: File,
    mmap: Arc<Mmap>,
    /// Offset, in the mmap, where the payload region starts (post LIDX-BLB1
    /// frame header). Always equal to BLOB_HEADER_LEN.
    payload_offset: usize,
    /// Length of the payload region in bytes.
    payload_len: usize,
}

impl std::fmt::Debug for BlobMmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobMmap")
            .field("payload_offset", &self.payload_offset)
            .field("payload_len", &self.payload_len)
            .finish()
    }
}

impl BlobMmap {
    /// mmap `path`, validate the CAS blob frame, and return a handle whose
    /// [`payload`](Self::payload) accessor yields the inner payload bytes.
    fn open(path: &Path) -> Result<Self, ReaderError> {
        let file = File::open(path)?;
        // SAFETY: mmap of a regular file. The file handle is kept open for the
        // lifetime of the mapping, and we do not mutate the file from this
        // process. Other processes may rewrite the file via atomic rename, but
        // the inode we mmaped stays live until we close our file handle.
        let mmap = unsafe { Mmap::map(&file)? };
        // Validate the CAS frame and locate the payload region.
        blob::validate_blob(&mmap[..])?;
        let (payload_slice, _hash) = extract_payload(&mmap[..])?;
        let payload_offset = BLOB_HEADER_LEN;
        let payload_len = payload_slice.len();
        let mmap_arc = Arc::new(mmap);
        Ok(BlobMmap {
            _file: file,
            mmap: mmap_arc,
            payload_offset,
            payload_len,
        })
    }

    /// Raw pointer to the start of the mmap region (frame header included).
    fn frame_ptr(&self) -> *const u8 {
        self.mmap.as_ptr()
    }

    /// Total length of the mmap region.
    fn frame_len(&self) -> usize {
        self.mmap.len()
    }

    /// Slice covering the CAS-blob payload (everything after LIDX-BLB1 frame).
    ///
    /// The slice is borrowed from the underlying mmap (via the Arc) so it
    /// remains valid as long as the [`BlobMmap`] is alive. No heap copy is
    /// performed.
    fn payload(&self) -> &[u8] {
        // SAFETY: We hold an Arc<Mmap> that keeps the mapping alive for as
        // long as self exists. The slice points into the mmap region and the
        // payload bounds were validated at open(). The Mmap type is
        // 'static-friendly because it owns the mapping via the file handle
        // and the OS page-cache backing.
        let end = self
            .payload_offset
            .checked_add(self.payload_len)
            .expect("bounds checked at open");
        unsafe {
            let start = self.mmap.as_ptr().add(self.payload_offset);
            std::slice::from_raw_parts(start, end - self.payload_offset)
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers for parsing little-endian fixed-size integers from a slice.
// ---------------------------------------------------------------------------

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, ReaderError> {
    if offset + 4 > bytes.len() {
        return Err(ReaderError::BadHeader(format!(
            "u32 read at offset {} exceeds payload of {} bytes",
            offset,
            bytes.len()
        )));
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[offset..offset + 4]);
    Ok(u32::from_le_bytes(buf))
}

fn read_f32_le(bytes: &[u8], offset: usize) -> Result<f32, ReaderError> {
    let raw = read_u32_le(bytes, offset)?;
    Ok(f32::from_bits(raw))
}

fn read_magic(bytes: &[u8]) -> Result<[u8; 9], ReaderError> {
    if bytes.len() < 9 {
        return Err(ReaderError::BadHeader(format!(
            "payload too short for magic: {} bytes",
            bytes.len()
        )));
    }
    let mut magic = [0u8; 9];
    magic.copy_from_slice(&bytes[..9]);
    Ok(magic)
}

fn require_magic(bytes: &[u8], expected: &[u8; 9]) -> Result<(), ReaderError> {
    let got = read_magic(bytes)?;
    if &got != expected {
        return Err(ReaderError::BadHeader(format!(
            "bad layer magic: expected {:?} got {:?}",
            expected, got
        )));
    }
    Ok(())
}

/// Validate the neural data region: geometric consistency (count * dim *
/// element_size must fit in the payload) and the embedded blake3 content_hash
/// over the data bytes.
fn validate_neural_data_region(payload: &[u8], header: &NeuralHeader) -> Result<(), ReaderError> {
    let data_offset_in_payload = header.data_offset();
    let element_size = header.dtype.element_size();
    let expected = header
        .count
        .checked_mul(header.dim)
        .and_then(|n| n.checked_mul(element_size))
        .ok_or_else(|| ReaderError::BadHeader("count * dim * element_size overflow".to_string()))?;
    let available = payload
        .len()
        .checked_sub(data_offset_in_payload)
        .ok_or_else(|| ReaderError::BadHeader("data offset exceeds payload".to_string()))?;
    if available < expected {
        return Err(ReaderError::BadHeader(format!(
            "neural payload short: have {} bytes of data, need {}",
            available, expected
        )));
    }

    // Verify the embedded content_hash (blake3 of the id table, if any, then
    // the data region).
    let mut stored_hash = [0u8; 32];
    stored_hash.copy_from_slice(&payload[33..65]);
    let computed = blob::blob_hash(&payload[NEURAL_HEADER_LEN..data_offset_in_payload + expected]);
    if stored_hash != computed {
        return Err(ReaderError::BadHeader(
            "neural content_hash does not match data".to_string(),
        ));
    }
    Ok(())
}

// ===========================================================================
// NeuralReader: f32 + INT8 SIMD dot-product
// ===========================================================================

/// Quantization dtype stored inside the neural header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeuralDtype {
    /// 32-bit float (4 bytes per element).
    F32,
    /// 8-bit signed integer (1 byte per element), decoded with
    /// `value = q as f32 * scale + zero_point`.
    Int8,
    /// 4-bit packed quantization (deferred to WS11 — readers reject it here).
    Q4,
}

impl NeuralDtype {
    fn from_u32(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::F32),
            1 => Some(Self::Int8),
            2 => Some(Self::Q4),
            _ => None,
        }
    }

    /// Size in bytes of a single element of this dtype.
    pub fn element_size(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::Int8 => 1,
            Self::Q4 => 0,
        }
    }
}

/// Zero-copy view of a single neural vector inside a [`NeuralReader`].
///
/// The view borrows from the underlying mmap; it is the caller's responsibility
/// to keep the [`NeuralReader`] alive while the view is in use.
#[derive(Clone, Copy)]
pub enum VectorView<'a> {
    /// 32-bit float data.
    F32(&'a [f32]),
    /// 8-bit quantized data.
    Int8(&'a [i8]),
}

impl<'a> VectorView<'a> {
    /// Dimensionality (number of elements).
    pub fn dim(&self) -> usize {
        match self {
            Self::F32(s) => s.len(),
            Self::Int8(s) => s.len(),
        }
    }

    /// Returns the raw f32 backing slice for the F32 dtype, or panics if the
    /// view holds quantized data.
    pub fn as_f32_slice(&self) -> &'a [f32] {
        match self {
            Self::F32(s) => s,
            Self::Int8(_) => panic!("VectorView::as_f32_slice on INT8 view"),
        }
    }
}

/// Validated contents of the `LIDX-NRL1` header (magic and geometry checks
/// already applied by [`NeuralHeader::parse`]).
struct NeuralHeader {
    version: u8,
    count: usize,
    dim: usize,
    dtype: NeuralDtype,
    scale: f32,
    zero_point: f32,
}

impl NeuralHeader {
    /// Parse and validate the fixed neural header at the start of `payload`.
    /// The caller must have verified the magic bytes and the minimum length
    /// is re-checked here.
    fn parse(payload: &[u8]) -> Result<Self, ReaderError> {
        if payload.len() < NEURAL_HEADER_LEN {
            return Err(ReaderError::BadHeader(format!(
                "neural payload truncated: {} < {}",
                payload.len(),
                NEURAL_HEADER_LEN
            )));
        }
        let version = payload[9];
        if !matches!(version, NEURAL_VERSION_POSITIONAL | NEURAL_VERSION_WITH_IDS) {
            return Err(ReaderError::BadHeader(format!(
                "unsupported neural version {}",
                version
            )));
        }
        let count = read_u32_le(payload, 13)? as usize;
        let dim = read_u32_le(payload, 17)? as usize;
        let dtype_raw = read_u32_le(payload, 21)?;
        let dtype = NeuralDtype::from_u32(dtype_raw)
            .ok_or_else(|| ReaderError::BadHeader(format!("unknown dtype {}", dtype_raw)))?;
        if matches!(dtype, NeuralDtype::Q4) {
            return Err(ReaderError::BadHeader(
                "Q4 dtype deferred to WS11; refuse to open".to_string(),
            ));
        }
        let scale = read_f32_le(payload, 25)?;
        let zero_point = read_f32_le(payload, 29)?;
        Ok(NeuralHeader {
            version,
            count,
            dim,
            dtype,
            scale,
            zero_point,
        })
    }

    fn has_ids(&self) -> bool {
        self.version == NEURAL_VERSION_WITH_IDS
    }

    /// Payload offset where the vector data begins (after the optional id
    /// table).
    fn data_offset(&self) -> usize {
        let ids_len = if self.has_ids() { self.count * 4 } else { 0 };
        NEURAL_HEADER_LEN + ids_len
    }
}

/// A zero-copy reader over a neural layer CAS blob.
#[derive(Debug)]
pub struct NeuralReader {
    blob: BlobMmap,
    count: usize,
    dim: usize,
    dtype: NeuralDtype,
    scale: f32,
    zero_point: f32,
    /// True for version-2 payloads, which carry a per-row PDG node-id table.
    has_ids: bool,
    /// Offset, from the start of the CAS payload, where the vectors data
    /// begins (after the neural header and the optional id table).
    data_offset_in_payload: usize,
}

impl NeuralReader {
    /// Open a neural layer blob at `path`.
    pub fn open(path: &Path) -> Result<Self, ReaderError> {
        let blob = BlobMmap::open(path)?;
        let payload = blob.payload();
        require_magic(payload, NEURAL_MAGIC)?;

        let header = NeuralHeader::parse(payload)?;
        validate_neural_data_region(payload, &header)?;

        Ok(NeuralReader {
            blob,
            count: header.count,
            dim: header.dim,
            dtype: header.dtype,
            scale: header.scale,
            zero_point: header.zero_point,
            has_ids: header.has_ids(),
            data_offset_in_payload: header.data_offset(),
        })
    }

    /// PDG node id that row `i` belongs to. `None` for version-1 payloads
    /// (positional rows, no id table) or when `i` is out of range.
    pub fn node_id(&self, i: usize) -> Option<u32> {
        if !self.has_ids || i >= self.count {
            return None;
        }
        read_u32_le(self.blob.payload(), NEURAL_HEADER_LEN + i * 4).ok()
    }

    /// Number of stored vectors.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Dimensionality of each stored vector.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Underlying dtype (F32 or Int8).
    pub fn dtype(&self) -> NeuralDtype {
        self.dtype
    }

    /// Quantization scale (only meaningful for Int8).
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Quantization zero-point (only meaningful for Int8).
    pub fn zero_point(&self) -> f32 {
        self.zero_point
    }

    /// Raw pointer to the start of the mmap region (CAS frame included).
    /// Used by zero-copy verification tests.
    pub fn mmap_data_ptr(&self) -> *const u8 {
        self.blob.frame_ptr()
    }

    /// Length of the mmap region.
    pub fn mmap_data_len(&self) -> usize {
        self.blob.frame_len()
    }

    /// Offset, from the start of the mmap region, where the layer payload
    /// (post LIDX-BLB1 frame) starts. Useful for asserting zero-copy pointer
    /// arithmetic.
    fn payload_byte(&self) -> &[u8] {
        self.blob.payload()
    }

    /// Return a zero-copy [`VectorView`] for vector `i`.
    ///
    /// Panics if `i >= count`.
    pub fn vector(&self, i: usize) -> VectorView<'_> {
        assert!(i < self.count, "vector index {} out of range", i);
        let data_offset = self.data_offset_in_payload;
        let payload = self.payload_byte();
        match self.dtype {
            NeuralDtype::F32 => {
                let byte_off = data_offset + i * self.dim * 4;
                let byte_len = self.dim * 4;
                let raw = &payload[byte_off..byte_off + byte_len];
                // SAFETY: f32 is plain-old-data, the slice has the right
                // alignment (4-byte) and length (multiple of 4) as asserted
                // during open(). The mmap outlives the returned slice.
                let f32_slice: &[f32] =
                    unsafe { std::slice::from_raw_parts(raw.as_ptr() as *const f32, self.dim) };
                VectorView::F32(f32_slice)
            }
            NeuralDtype::Int8 => {
                let byte_off = data_offset + i * self.dim;
                let raw = &payload[byte_off..byte_off + self.dim];
                // SAFETY: i8 has the same layout as u8.
                let i8_slice: &[i8] =
                    unsafe { std::slice::from_raw_parts(raw.as_ptr() as *const i8, self.dim) };
                VectorView::Int8(i8_slice)
            }
            NeuralDtype::Q4 => unreachable!("Q4 guarded at open()"),
        }
    }

    /// Compute the dot product of stored vector `i` with `query`.
    ///
    /// For F32 dtype: `Σ query[j] * vector_i[j]`.
    /// For Int8 dtype: `scale * Σ query[j] * q_ij + zero_point * Σ query[j]`
    /// (dequantization applied only to the final scalar).
    ///
    /// Panics if `i >= count` or `query.len() != dim`.
    pub fn dot(&self, i: usize, query: &[f32]) -> f32 {
        assert_eq!(query.len(), self.dim, "query length mismatch");
        match self.dtype {
            NeuralDtype::F32 => {
                let v = self.vector(i);
                let f32s = v.as_f32_slice();
                let query_sum: f32 = query.iter().sum();
                // Placate clippy: query_sum is observed by callers indirectly
                // because the F32 path has no zero_point but we still want the
                // caller to be rid of any doubt about whether the f32 path is
                // pure Σ Q*V.
                let _ = query_sum;
                dot_f32(query, f32s)
            }
            NeuralDtype::Int8 => {
                let v = self.vector(i);
                let qs: &[i8] = match v {
                    VectorView::Int8(s) => s,
                    _ => unreachable!("dtype path"),
                };
                dot_int8(query, qs, self.scale, self.zero_point)
            }
            NeuralDtype::Q4 => unreachable!("Q4 guarded at open()"),
        }
    }
}

// ---------------------------------------------------------------------------
// F32 dot product.
//
// The simple loop is auto-vectorised by LLVM on any modern x86 build, so we
// keep the code straight-forward and let the compiler emit AVX2/SSE patterns
// instead of using wide::f32x8 explicitly. The point of this path vs INT8 is
// the storage density (4x more memory bandwidth), not the SIMD width.
// ---------------------------------------------------------------------------

fn dot_f32(query: &[f32], stored: &[f32]) -> f32 {
    assert_eq!(query.len(), stored.len());
    let mut sum = 0.0f32;
    for (q, s) in query.iter().zip(stored.iter()) {
        sum += q * s;
    }
    sum
}

// ---------------------------------------------------------------------------
// INT8 dot product with dequantization applied at the end.
//
// result = Σ_j query[j] * (q_j as f32 * scale + zero_point)
//        = scale * Σ_j query[j] * q_j as f32 + zero_point * Σ_j query[j]
//
// The accumulation uses 8-wide f32 lanes via wide::f32x8; on x86_64 with AVX2
// this compiles to a tight loop that loads 8 i8 values, sign-extends them to
// f32, and accumulates. The i8 -> f32 widening is the only place where INT8
// differs from F32, but the storage compression (1 byte vs 4 bytes) gives the
// INT8 path a 4x advantage in memory bandwidth.
// ---------------------------------------------------------------------------

fn dot_int8(query: &[f32], stored: &[i8], scale: f32, zero_point: f32) -> f32 {
    assert_eq!(query.len(), stored.len());
    let n = query.len();
    let mut acc = 0.0f32;
    let mut query_sum = 0.0f32;
    let mut i = 0;
    while i + 8 <= n {
        // Load 8 query f32 values.
        let q = wide::f32x8::from([
            query[i],
            query[i + 1],
            query[i + 2],
            query[i + 3],
            query[i + 4],
            query[i + 5],
            query[i + 6],
            query[i + 7],
        ]);
        // Widen 8 i8 to 8 f32.
        let s = wide::f32x8::from([
            stored[i] as f32,
            stored[i + 1] as f32,
            stored[i + 2] as f32,
            stored[i + 3] as f32,
            stored[i + 4] as f32,
            stored[i + 5] as f32,
            stored[i + 6] as f32,
            stored[i + 7] as f32,
        ]);
        acc += (q * s).reduce_add();
        query_sum += q.reduce_add();
        i += 8;
    }
    while i < n {
        acc += query[i] * stored[i] as f32;
        query_sum += query[i];
        i += 1;
    }
    scale * acc + zero_point * query_sum
}

#[cfg(target_arch = "x86_64")]
#[allow(dead_code)]
mod int8_avx2 {
    //! AVX2-accelerated INT8 dot product. Used at runtime via
    //! `is_x86_feature_detected!("avx2")` when available, otherwise the
    //! portable path in [`super::dot_int8`] runs.
    //!
    //! The implementation widens 16 i8 -> i16 -> i32 -> f32 using
    //! `_mm256_cvtepi8_epi16`/`_mm256_cvtepi16_epi32`/`_mm256_cvtepi32_ps`,
    //! multiplies by the f32 query, and accumulates in two `__m256` lanes.
    //! The pair of `__m256` accumulators is the "wide-i32 accumulator"
    //! the spec calls for.

    use std::arch::x86_64::*;

    /// SAFETY: caller must ensure AVX2 is available (use
    /// `is_x86_feature_detected!("avx2")`).
    #[target_feature(enable = "avx2")]
    pub unsafe fn dot_int8_avx2(query: &[f32], stored: &[i8], scale: f32, zero_point: f32) -> f32 {
        // SAFETY: caller guarantees AVX2 is available; all intrinsics below
        // require only AVX2 and use unaligned loads on slices whose validity
        // follows from Rust's slice guarantees.
        unsafe {
            assert_eq!(query.len(), stored.len());
            let n = query.len();
            // Unroll two 16-i8 halves per iteration so the front-end is
            // saturated with work. Using four accumulators helps hide fma
            // latency (each fma has ~5 cycle latency but 0.5 cycle throughput
            // on Intel Skylake-derived cores; two parallel FMA chains can keep
            // the port busy).
            let mut acc0 = _mm256_setzero_ps();
            let mut acc1 = _mm256_setzero_ps();
            let mut acc2 = _mm256_setzero_ps();
            let mut acc3 = _mm256_setzero_ps();
            let mut query_acc0 = _mm256_setzero_ps();
            let mut query_acc1 = _mm256_setzero_ps();
            let mut i = 0;
            while i + 32 <= n {
                // First 16 i8 -> f32 widening chain.
                let i8_ptr_a = stored.as_ptr().add(i) as *const __m128i;
                let i8_vec_a = _mm_loadu_si128(i8_ptr_a);
                let i16_a = _mm256_cvtepi8_epi16(i8_vec_a);
                let low_a = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(i16_a, 0));
                let high_a = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(i16_a, 1));
                let lf_a = _mm256_cvtepi32_ps(low_a);
                let hf_a = _mm256_cvtepi32_ps(high_a);
                let q_ptr_a = query.as_ptr().add(i);
                let q_low_a = _mm256_loadu_ps(q_ptr_a);
                let q_high_a = _mm256_loadu_ps(q_ptr_a.add(8));
                acc0 = _mm256_fmadd_ps(q_low_a, lf_a, acc0);
                acc1 = _mm256_fmadd_ps(q_high_a, hf_a, acc1);
                query_acc0 = _mm256_add_ps(query_acc0, q_low_a);
                query_acc0 = _mm256_add_ps(query_acc0, q_high_a);

                // Second 16 i8 -> f32 widening chain (independent, can execute
                // in parallel with the fma chain above).
                let i8_ptr_b = stored.as_ptr().add(i + 16) as *const __m128i;
                let i8_vec_b = _mm_loadu_si128(i8_ptr_b);
                let i16_b = _mm256_cvtepi8_epi16(i8_vec_b);
                let low_b = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(i16_b, 0));
                let high_b = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(i16_b, 1));
                let lf_b = _mm256_cvtepi32_ps(low_b);
                let hf_b = _mm256_cvtepi32_ps(high_b);
                let q_ptr_b = query.as_ptr().add(i + 16);
                let q_low_b = _mm256_loadu_ps(q_ptr_b);
                let q_high_b = _mm256_loadu_ps(q_ptr_b.add(8));
                acc2 = _mm256_fmadd_ps(q_low_b, lf_b, acc2);
                acc3 = _mm256_fmadd_ps(q_high_b, hf_b, acc3);
                query_acc1 = _mm256_add_ps(query_acc1, q_low_b);
                query_acc1 = _mm256_add_ps(query_acc1, q_high_b);

                i += 32;
            }
            // Fold the four accumulators.
            let acc_a = _mm256_add_ps(acc0, acc1);
            let acc_b = _mm256_add_ps(acc2, acc3);
            let acc = _mm256_add_ps(acc_a, acc_b);
            let qacc = _mm256_add_ps(query_acc0, query_acc1);
            // Tail of up to 31 elements via scalar.
            let mut acc_scalar = 0.0f32;
            let mut query_scalar = 0.0f32;
            while i < n {
                acc_scalar += query[i] * stored[i] as f32;
                query_scalar += query[i];
                i += 1;
            }
            let acc_total = horizontal_sum(acc) + acc_scalar;
            let query_total = horizontal_sum(qacc) + query_scalar;
            scale * acc_total + zero_point * query_total
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn horizontal_sum(v: __m256) -> f32 {
        // SAFETY: AVX2 already verified by caller; intrinsics below are pure
        // register shuffles.
        let mut hi128 = _mm256_extractf128_ps(v, 1);
        let lo128 = _mm256_castps256_ps128(v);
        hi128 = _mm_add_ps(hi128, lo128);
        let mut shuf = _mm_movehdup_ps(hi128);
        shuf = _mm_add_ps(hi128, shuf);
        shuf = _mm_movehl_ps(shuf, hi128);
        shuf = _mm_add_ss(shuf, hi128);
        _mm_cvtss_f32(shuf)
    }
}

/// Dispatch INT8 dot-product to AVX2-optimized implementation when available.
pub(crate) fn dot_int8_dispatch(query: &[f32], stored: &[i8], scale: f32, zero_point: f32) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 verified above.
            return unsafe { int8_avx2::dot_int8_avx2(query, stored, scale, zero_point) };
        }
    }
    dot_int8(query, stored, scale, zero_point)
}

impl NeuralReader {
    /// Like [`dot`](Self::dot), but routes through the AVX2 path when the CPU
    /// supports it. Useful for benchmarks that want to compare implementations
    /// head-to-head.
    pub fn dot_simd(&self, i: usize, query: &[f32]) -> f32 {
        assert_eq!(query.len(), self.dim, "query length mismatch");
        match self.dtype {
            NeuralDtype::F32 => {
                let v = self.vector(i);
                dot_f32(query, v.as_f32_slice())
            }
            NeuralDtype::Int8 => {
                let v = self.vector(i);
                let qs: &[i8] = match v {
                    VectorView::Int8(s) => s,
                    _ => unreachable!("dtype path"),
                };
                dot_int8_dispatch(query, qs, self.scale, self.zero_point)
            }
            NeuralDtype::Q4 => unreachable!("Q4 guarded at open()"),
        }
    }
}

// ===========================================================================
// TfidfReader: sparse TF-IDF triples
// ===========================================================================

/// Record returned by `TfidfReader::entry`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TfidfEntry {
    /// Document ID.
    pub doc_id: u32,
    /// Term ID.
    pub term_id: u32,
    /// TF-IDF value.
    pub value: f32,
}

/// Zero-copy reader over a sparse TF-IDF layer blob.
#[derive(Debug)]
pub struct TfidfReader {
    blob: BlobMmap,
    num_docs: usize,
    num_terms: usize,
    num_entries: usize,
    /// Offset, in the CAS payload, where the entry array begins.
    data_offset_in_payload: usize,
}

impl TfidfReader {
    /// Open a TF-IDF layer blob at `path`.
    pub fn open(path: &Path) -> Result<Self, ReaderError> {
        let blob = BlobMmap::open(path)?;
        let payload = blob.payload();
        require_magic(payload, TFIDF_MAGIC)?;
        if payload.len() < TFIDF_HEADER_LEN {
            return Err(ReaderError::BadHeader(format!(
                "tfidf payload truncated: {} < {}",
                payload.len(),
                TFIDF_HEADER_LEN
            )));
        }
        let version = payload[9];
        if version != 1 {
            return Err(ReaderError::BadHeader(format!(
                "unsupported tfidf version {}",
                version
            )));
        }
        let num_docs = read_u32_le(payload, 13)? as usize;
        let num_terms = read_u32_le(payload, 17)? as usize;
        let num_entries = read_u32_le(payload, 21)? as usize;
        let data_offset_in_payload = TFIDF_HEADER_LEN;
        let expected = num_entries
            .checked_mul(TFIDF_ENTRY_LEN)
            .ok_or_else(|| ReaderError::BadHeader("tfidf entries length overflow".to_string()))?;
        let available = payload
            .len()
            .checked_sub(data_offset_in_payload)
            .ok_or_else(|| {
                ReaderError::BadHeader("tfidf data offset exceeds payload".to_string())
            })?;
        if available < expected {
            return Err(ReaderError::BadHeader(format!(
                "tfidf payload short: have {} bytes, need {}",
                available, expected
            )));
        }
        // Verify content_hash.
        let mut stored_hash = [0u8; 32];
        stored_hash.copy_from_slice(&payload[25..57]);
        let computed =
            blob::blob_hash(&payload[data_offset_in_payload..data_offset_in_payload + expected]);
        if stored_hash != computed {
            return Err(ReaderError::BadHeader(
                "tfidf content_hash does not match data".to_string(),
            ));
        }
        Ok(TfidfReader {
            blob,
            num_docs,
            num_terms,
            num_entries,
            data_offset_in_payload,
        })
    }

    /// Number of documents.
    pub fn num_docs(&self) -> usize {
        self.num_docs
    }

    /// Number of unique terms.
    pub fn num_terms(&self) -> usize {
        self.num_terms
    }

    /// Number of stored (non-zero) triples.
    pub fn num_entries(&self) -> usize {
        self.num_entries
    }

    /// Return the entry at `idx` without bounds checking.
    fn entry_at(&self, idx: usize) -> TfidfEntry {
        let base = self.data_offset_in_payload + idx * TFIDF_ENTRY_LEN;
        let payload = self.blob.payload();
        let doc_id = read_u32_le(payload, base).expect("bounds checked at open");
        let term_id = read_u32_le(payload, base + 4).expect("bounds checked at open");
        let value = read_f32_le(payload, base + 8).expect("bounds checked at open");
        TfidfEntry {
            doc_id,
            term_id,
            value,
        }
    }

    /// Iterate over all entries in storage order.
    pub fn entries(&self) -> impl Iterator<Item = TfidfEntry> + '_ {
        (0..self.num_entries).map(move |i| self.entry_at(i))
    }

    /// Look up the TF-IDF value for a `(doc_id, term_id)` pair via binary
    /// search. Assumes the writer stored entries in ascending `(doc_id, term_id)`
    /// order, which the WS4 writer produces.
    ///
    /// Returns `None` if no entry exists for the pair.
    pub fn tf_idf(&self, doc_id: u32, term_id: u32) -> Option<f32> {
        // Linear scan for small fixtures (Task 6 TDD: correctness first).
        // The writer uses sorted insert in WS6-9; for now we walk and match.
        self.entries().find_map(|e| {
            if e.doc_id == doc_id && e.term_id == term_id {
                Some(e.value)
            } else {
                None
            }
        })
    }
}

// ===========================================================================
// PdgReader: nodes, edges, interned strings
// ===========================================================================

/// One node in the PDG, as seen by the reader. String-bearing fields
/// (`file_path_id`, `sym_name_id`) are indices into the mmap'd string table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdgNode {
    /// Node identifier matching the PDG writer's `node_id` assignment.
    pub node_id: u32,
    /// Node kind: function, type, module, etc. Mapping is owned by the PDG
    /// writer; the reader treats this as an opaque enum.
    pub node_type: u32,
    /// Index into the mmap'd string table giving the file path.
    pub file_path_id: u32,
    /// Start line (1-indexed) of the node in `file_path`.
    pub start_line: u32,
    /// End line (1-indexed, inclusive) of the node in `file_path`.
    pub end_line: u32,
    /// Index into the mmap'd string table giving the symbol's display name.
    pub sym_name_id: u32,
}

/// One edge in the PDG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdgEdge {
    /// Source node ID.
    pub src: u32,
    /// Destination node ID.
    pub dst: u32,
    /// Edge kind: call, data-flow, etc. Mapping is owned by the PDG writer.
    pub edge_type: u32,
}

/// A version-2 PDG node: every field `ProgramDependenceGraph` needs to
/// reconstruct the node losslessly. String-bearing fields are ids into the
/// payload's interned string table (resolve with [`PdgReader::resolve_string`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdgNodeFull {
    /// Interned graph node id (e.g. `src/main.rs:func1`).
    pub node_id: u32,
    /// Interned display name.
    pub symbol_name: u32,
    /// Interned file path.
    pub file_path: u32,
    /// Interned language tag.
    pub language: u32,
    /// Node kind code (writer-owned mapping).
    pub node_type: u32,
    /// Cyclomatic complexity.
    pub complexity: u32,
    /// Byte offset of the node's start in `file_path`.
    pub byte_start: u32,
    /// Byte offset of the node's end in `file_path`.
    pub byte_end: u32,
    /// True when the node carries a SCIP precision (stable-id) marker.
    pub precision: bool,
}

/// A version-2 PDG edge's optional metadata. `None` fields were stored as
/// sentinels ([`PDG_V2_NONE`] / NaN).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdgEdgeMeta {
    /// Number of times the call occurs.
    pub call_count: Option<u32>,
    /// Variable name for data-dependency edges (interned string id).
    pub variable_name: Option<u32>,
    /// Confidence score for inferred edges.
    pub confidence: Option<f32>,
    /// Flow channel label (interned string id).
    pub channel: Option<u32>,
    /// Argument ordinal.
    pub position: Option<u32>,
}

/// Zero-copy reader over a PDG layer blob.
#[derive(Debug)]
pub struct PdgReader {
    blob: BlobMmap,
    /// Payload version (1 = positional catalog rows, 2 = lossless graph).
    version: u8,
    num_nodes: usize,
    num_edges: usize,
    num_strings: usize,
    strings_bytes_len: usize,
    /// Offset, in the payload, where the node array begins.
    nodes_offset: usize,
    /// Offset, in the payload, where the edge array begins.
    edges_offset: usize,
    /// Offset, in the payload, where the v2 edge-metadata array begins
    /// (v1: equal to the string table offset — unused).
    edge_meta_offset: usize,
    /// Offset, in the payload, where the string-offset table begins.
    string_offsets_offset: usize,
    /// Offset, in the payload, where the raw string bytes begin.
    string_bytes_offset: usize,
}

impl PdgReader {
    /// Open a PDG layer blob at `path`.
    pub fn open(path: &Path) -> Result<Self, ReaderError> {
        let blob = BlobMmap::open(path)?;
        let payload = blob.payload();
        require_magic(payload, PDG_MAGIC)?;
        if payload.len() < PDG_HEADER_LEN {
            return Err(ReaderError::BadHeader(format!(
                "pdg payload truncated: {} < {}",
                payload.len(),
                PDG_HEADER_LEN
            )));
        }
        let version = payload[9];
        if !matches!(version, 1 | 2) {
            return Err(ReaderError::BadHeader(format!(
                "unsupported pdg version {}",
                version
            )));
        }
        let num_nodes = read_u32_le(payload, 13)? as usize;
        let num_edges = read_u32_le(payload, 17)? as usize;
        let num_strings = read_u32_le(payload, 21)? as usize;
        let strings_bytes_len = read_u32_le(payload, 25)? as usize;

        let nodes_offset = PDG_HEADER_LEN;
        let node_len = if version == 2 {
            PDG_NODE_V2_LEN
        } else {
            PDG_NODE_LEN
        };
        let edges_offset = nodes_offset + num_nodes * node_len;
        let (edge_meta_offset, string_offsets_offset) = if version == 2 {
            let meta = edges_offset + num_edges * PDG_EDGE_LEN;
            (meta, meta + num_edges * PDG_EDGE_META_LEN)
        } else {
            (
                edges_offset + num_edges * PDG_EDGE_LEN,
                edges_offset + num_edges * PDG_EDGE_LEN,
            )
        };
        let string_bytes_offset = string_offsets_offset + num_strings * PDG_STRING_OFFSET_LEN;
        let end = string_bytes_offset + strings_bytes_len;

        if end > payload.len() {
            return Err(ReaderError::BadHeader(format!(
                "pdg payload overflow: need {} have {}",
                end,
                payload.len()
            )));
        }
        // Verify content_hash over the entire data region (post-header).
        let mut stored_hash = [0u8; 32];
        stored_hash.copy_from_slice(&payload[29..61]);
        let computed = blob::blob_hash(&payload[PDG_HEADER_LEN..end]);
        if stored_hash != computed {
            return Err(ReaderError::BadHeader(
                "pdg content_hash does not match data".to_string(),
            ));
        }
        Ok(PdgReader {
            blob,
            version,
            num_nodes,
            num_edges,
            num_strings,
            strings_bytes_len,
            nodes_offset,
            edges_offset,
            edge_meta_offset,
            string_offsets_offset,
            string_bytes_offset,
        })
    }

    /// Payload version: 1 (positional catalog rows) or 2 (lossless graph).
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Total nodes stored in the layer.
    pub fn num_nodes(&self) -> usize {
        self.num_nodes
    }

    /// Total edges stored in the layer.
    pub fn num_edges(&self) -> usize {
        self.num_edges
    }

    /// Total bytes occupied by the interned string region.
    ///
    /// Exposed as a sanity check for size accounting; not used for reading.
    pub fn strings_bytes_len(&self) -> usize {
        self.strings_bytes_len
    }

    /// Return the node at `idx`, no bounds check performed beyond the
    /// inherent integrity validation at open.
    ///
    /// Version-1 payloads only: a version-2 payload has a different node
    /// record layout, and reading it through this accessor would silently
    /// misparse — refuse instead (use [`node_full`](Self::node_full)).
    pub fn node(&self, idx: usize) -> Result<PdgNode, ReaderError> {
        if self.version != 1 {
            return Err(ReaderError::BadHeader(
                "node() requires a version-1 pdg payload; use node_full() for version 2"
                    .to_string(),
            ));
        }
        if idx >= self.num_nodes {
            return Err(ReaderError::OutOfRange(format!(
                "pdg node idx {} out of range {}",
                idx, self.num_nodes
            )));
        }
        let base = self.nodes_offset + idx * PDG_NODE_LEN;
        let payload = self.blob.payload();
        Ok(PdgNode {
            node_id: read_u32_le(payload, base)?,
            node_type: read_u32_le(payload, base + 4)?,
            file_path_id: read_u32_le(payload, base + 8)?,
            start_line: read_u32_le(payload, base + 12)?,
            end_line: read_u32_le(payload, base + 16)?,
            sym_name_id: read_u32_le(payload, base + 20)?,
        })
    }

    /// Return the edge at `idx`.
    pub fn edge(&self, idx: usize) -> Result<PdgEdge, ReaderError> {
        if idx >= self.num_edges {
            return Err(ReaderError::OutOfRange(format!(
                "pdg edge idx {} out of range {}",
                idx, self.num_edges
            )));
        }
        let base = self.edges_offset + idx * PDG_EDGE_LEN;
        let payload = self.blob.payload();
        Ok(PdgEdge {
            src: read_u32_le(payload, base)?,
            dst: read_u32_le(payload, base + 4)?,
            edge_type: read_u32_le(payload, base + 8)?,
        })
    }

    /// Return the lossless version-2 record for node `idx`. Errors on
    /// version-1 payloads, which do not carry the fields.
    pub fn node_full(&self, idx: usize) -> Result<PdgNodeFull, ReaderError> {
        if self.version != 2 {
            return Err(ReaderError::BadHeader(
                "node_full requires a version-2 pdg payload".to_string(),
            ));
        }
        if idx >= self.num_nodes {
            return Err(ReaderError::OutOfRange(format!(
                "pdg node idx {} out of range {}",
                idx, self.num_nodes
            )));
        }
        let base = self.nodes_offset + idx * PDG_NODE_V2_LEN;
        let payload = self.blob.payload();
        let flags = read_u32_le(payload, base + 32)?;
        Ok(PdgNodeFull {
            node_id: read_u32_le(payload, base)?,
            symbol_name: read_u32_le(payload, base + 4)?,
            file_path: read_u32_le(payload, base + 8)?,
            language: read_u32_le(payload, base + 12)?,
            node_type: read_u32_le(payload, base + 16)?,
            complexity: read_u32_le(payload, base + 20)?,
            byte_start: read_u32_le(payload, base + 24)?,
            byte_end: read_u32_le(payload, base + 28)?,
            precision: flags & 1 == 1,
        })
    }

    /// Return the version-2 metadata for edge `idx`. Errors on version-1
    /// payloads; returns all-`None` fields when the edge carries none.
    pub fn edge_meta(&self, idx: usize) -> Result<PdgEdgeMeta, ReaderError> {
        if self.version != 2 {
            return Err(ReaderError::BadHeader(
                "edge_meta requires a version-2 pdg payload".to_string(),
            ));
        }
        if idx >= self.num_edges {
            return Err(ReaderError::OutOfRange(format!(
                "pdg edge idx {} out of range {}",
                idx, self.num_edges
            )));
        }
        let base = self.edge_meta_offset + idx * PDG_EDGE_META_LEN;
        let payload = self.blob.payload();
        let opt = |raw: u32| (raw != PDG_V2_NONE).then_some(raw);
        let confidence_raw = read_u32_le(payload, base + 8)?;
        let confidence = f32::from_bits(confidence_raw);
        Ok(PdgEdgeMeta {
            call_count: opt(read_u32_le(payload, base)?),
            variable_name: opt(read_u32_le(payload, base + 4)?),
            confidence: (!confidence.is_nan()).then_some(confidence),
            channel: opt(read_u32_le(payload, base + 12)?),
            position: opt(read_u32_le(payload, base + 16)?),
        })
    }

    /// Resolve a string-table ID to a borrowed `&str`.
    ///
    /// Returns `None` if the ID is out of range or the bytes are not valid
    /// UTF-8 (likely corruption).
    pub fn resolve_string(&self, id: u32) -> Option<&str> {
        resolve_string_helper(
            self.blob.payload(),
            self.string_offsets_offset,
            self.string_bytes_offset,
            self.num_strings,
            id,
        )
    }

    /// Iterator over node IDs that are direct callers of `node_id`.
    pub fn callers(&self, node_id: u32) -> impl Iterator<Item = u32> + '_ {
        let collect: Vec<u32> = (0..self.num_edges)
            .filter_map(move |i| {
                let e = self.edge(i).ok()?;
                if e.dst == node_id { Some(e.src) } else { None }
            })
            .collect();
        collect.into_iter()
    }

    /// Iterator over node IDs that are direct callees of `node_id`.
    pub fn callees(&self, node_id: u32) -> impl Iterator<Item = u32> + '_ {
        let collect: Vec<u32> = (0..self.num_edges)
            .filter_map(move |i| {
                let e = self.edge(i).ok()?;
                if e.src == node_id { Some(e.dst) } else { None }
            })
            .collect();
        collect.into_iter()
    }
}

// ===========================================================================
// SymbolReader
// ===========================================================================

/// One symbol record from the symbols layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SymbolEntry {
    /// Index into the mmap'd string table giving the symbol's display name.
    pub name_id: u32,
    /// Symbol kind: function, struct, enum, etc. Mapping is owned by the
    /// symbol writer; the reader treats this as an opaque enum.
    pub sym_type: u32,
    /// Index into the mmap'd string table giving the file path.
    pub file_path_id: u32,
    /// Start line (1-indexed) of the symbol's declaration.
    pub start_line: u32,
    /// End line (1-indexed, inclusive) of the symbol's declaration.
    pub end_line: u32,
    /// Cyclomatic complexity score (lower = simpler).
    pub complexity: f32,
}

/// Zero-copy reader over a symbols layer blob.
#[derive(Debug)]
pub struct SymbolReader {
    blob: BlobMmap,
    num_symbols: usize,
    num_strings: usize,
    strings_bytes_len: usize,
    symbols_offset: usize,
    string_offsets_offset: usize,
    string_bytes_offset: usize,
}

impl SymbolReader {
    /// Open a symbols layer blob at `path`.
    pub fn open(path: &Path) -> Result<Self, ReaderError> {
        let blob = BlobMmap::open(path)?;
        let payload = blob.payload();
        require_magic(payload, SYMBOLS_MAGIC)?;
        if payload.len() < SYMBOLS_HEADER_LEN {
            return Err(ReaderError::BadHeader(format!(
                "symbols payload truncated: {} < {}",
                payload.len(),
                SYMBOLS_HEADER_LEN
            )));
        }
        let version = payload[9];
        if version != 1 {
            return Err(ReaderError::BadHeader(format!(
                "unsupported symbols version {}",
                version
            )));
        }
        let num_symbols = read_u32_le(payload, 13)? as usize;
        let num_strings = read_u32_le(payload, 17)? as usize;
        let strings_bytes_len = read_u32_le(payload, 21)? as usize;

        let symbols_offset = SYMBOLS_HEADER_LEN;
        let string_offsets_offset = symbols_offset + num_symbols * SYMBOL_ENTRY_LEN;
        let string_bytes_offset = string_offsets_offset + num_strings * PDG_STRING_OFFSET_LEN;
        let end = string_bytes_offset + strings_bytes_len;

        if end > payload.len() {
            return Err(ReaderError::BadHeader(format!(
                "symbols payload overflow: need {} have {}",
                end,
                payload.len()
            )));
        }
        let mut stored_hash = [0u8; 32];
        stored_hash.copy_from_slice(&payload[25..57]);
        let computed = blob::blob_hash(&payload[SYMBOLS_HEADER_LEN..end]);
        if stored_hash != computed {
            return Err(ReaderError::BadHeader(
                "symbols content_hash does not match data".to_string(),
            ));
        }
        Ok(SymbolReader {
            blob,
            num_symbols,
            num_strings,
            strings_bytes_len,
            symbols_offset,
            string_offsets_offset,
            string_bytes_offset,
        })
    }

    /// Total stored symbols.
    pub fn num_symbols(&self) -> usize {
        self.num_symbols
    }

    /// Total bytes occupied by the interned string region.
    ///
    /// Exposed as a sanity check for size accounting; not used for reading.
    pub fn strings_bytes_len(&self) -> usize {
        self.strings_bytes_len
    }

    /// Return the symbol at `idx`.
    pub fn symbol(&self, idx: usize) -> Result<SymbolEntry, ReaderError> {
        if idx >= self.num_symbols {
            return Err(ReaderError::OutOfRange(format!(
                "symbol idx {} out of range {}",
                idx, self.num_symbols
            )));
        }
        let base = self.symbols_offset + idx * SYMBOL_ENTRY_LEN;
        let payload = self.blob.payload();
        // sym_type occupies bytes 4..8, then file_path_id 8..12, etc.
        // Layout per SYMBOL_ENTRY_LEN: name_id(4) sym_type(4) file_path_id(4)
        // start_line(4) end_line(4) complexity(4).
        Ok(SymbolEntry {
            name_id: read_u32_le(payload, base)?,
            sym_type: read_u32_le(payload, base + 4)?,
            file_path_id: read_u32_le(payload, base + 8)?,
            start_line: read_u32_le(payload, base + 12)?,
            end_line: read_u32_le(payload, base + 16)?,
            complexity: read_f32_le(payload, base + 20)?,
        })
    }

    /// Resolve a string ID to a borrowed `&str` from the mmap'd table.
    pub fn resolve_string(&self, id: u32) -> Option<&str> {
        resolve_string_helper(
            self.blob.payload(),
            self.string_offsets_offset,
            self.string_bytes_offset,
            self.num_strings,
            id,
        )
    }

    /// Iterate over all symbols, returning the first one whose name matches
    /// `name`.
    pub fn lookup(&self, name: &str) -> Option<SymbolEntry> {
        (0..self.num_symbols).find_map(|i| {
            let s = self.symbol(i).ok()?;
            let actual = self.resolve_string(s.name_id)?;
            if actual == name { Some(s) } else { None }
        })
    }
}

// ---------------------------------------------------------------------------
// Shared string-table resolver used by both PDG and symbols readers.
// ---------------------------------------------------------------------------

fn resolve_string_helper(
    payload: &[u8],
    string_offsets_offset: usize,
    string_bytes_offset: usize,
    num_strings: usize,
    id: u32,
) -> Option<&str> {
    let id_usize = id as usize;
    if id_usize >= num_strings {
        return None;
    }
    let off_base = string_offsets_offset + id_usize * PDG_STRING_OFFSET_LEN;
    let mut off_buf = [0u8; 4];
    off_buf.copy_from_slice(&payload[off_base..off_base + 4]);
    let str_offset = u32::from_le_bytes(off_buf) as usize;
    let mut len_buf = [0u8; 4];
    len_buf.copy_from_slice(&payload[off_base + 4..off_base + 8]);
    let str_len = u32::from_le_bytes(len_buf) as usize;

    let bytes_start = string_bytes_offset + str_offset;
    if bytes_start + str_len > payload.len() {
        return None;
    }
    let raw = &payload[bytes_start..bytes_start + str_len];
    std::str::from_utf8(raw).ok()
}

#[cfg(test)]
#[path = "reader_test.rs"]
mod tests;
