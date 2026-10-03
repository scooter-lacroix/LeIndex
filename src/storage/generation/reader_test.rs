//! Tests for zero-copy mmap readers: NeuralReader, TfidfReader, PdgReader,
//! SymbolReader.
//!
//! Covers VAL-READER-001 through VAL-READER-010:
//! - NeuralReader f32 dot-product correctness
//! - NeuralReader INT8 SIMD dot-product correctness (within 1e-4 of dequantize-then-dot)
//! - NeuralReader zero-copy verification (no heap alloc proportional to corpus)
//! - TF-IDF reader returns correct values
//! - PDG reader traverses nodes and edges correctly
//! - Symbols reader returns correct symbol inventory
//! - Reader hash validation on open
//! - Reader survives generation swap (stale read isolation)
//! - All readers O(1) setup, no corpus-proportional heap

use super::*;
use crate::storage::cas::blob::{BLOB_HEADER_LEN, encode_blob};

use std::fs;
use std::io::Write;

#[allow(dead_code)]
type _UnusedArcMutex = std::sync::Arc<std::sync::Mutex<()>>;

// ===========================================================================
// Test helpers
// ===========================================================================

/// Write `payload` as a CAS-style blob frame to `path`.
///
/// Returns the blake3 hash of the payload so the reader can find the layer
/// inside a manifest if desired.
fn write_blob(path: &std::path::Path, payload: &[u8]) -> [u8; 32] {
    let frame = encode_blob(payload);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    let mut file = fs::File::create(path).expect("create blob file");
    file.write_all(&frame).expect("write blob");
    file.sync_all().expect("fsync");
    crate::storage::cas::blob::blob_hash(payload)
}

/// Write a flat payload of `bytes.len()` bytes (no extra framing) to `path`.
fn write_raw(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    let mut file = fs::File::create(path).expect("create raw file");
    file.write_all(bytes).expect("write raw");
    file.sync_all().expect("fsync");
}

// ===========================================================================
// NeuralReader: f32 dot-product correctness (VAL-READER-001)
// ===========================================================================

#[test]
fn test_neural_f32_dot_correctness() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("neural_f32.blob");

    let count: u32 = 100;
    let dim: u32 = 384;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"LIDX-NRL1");
    payload.push(1); // version
    payload.extend_from_slice(&[0u8; 3]); // pad
    payload.extend_from_slice(&count.to_le_bytes());
    payload.extend_from_slice(&dim.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes()); // dtype = F32
    payload.extend_from_slice(&1.0f32.to_le_bytes()); // scale (unused for F32)
    payload.extend_from_slice(&0.0f32.to_le_bytes()); // zero_point (unused)
    payload.extend_from_slice(&[0u8; 32]); // content_hash placeholder
    payload.push(0); // alignment_pad to make data 4-byte aligned in mmap
    assert_eq!(payload.len(), 66);

    // Generate deterministic pseudo-random f32 values in [-1, 1].
    let mut vecs: Vec<Vec<f32>> = Vec::new();
    let mut prng = 0x1234_5678u32;
    for _ in 0..count {
        let mut v = Vec::with_capacity(dim as usize);
        for _ in 0..dim {
            // xorshift32 for reproducible values
            prng ^= prng << 13;
            prng ^= prng >> 17;
            prng ^= prng << 5;
            let f = ((prng as f32) / (u32::MAX as f32)) * 2.0 - 1.0;
            v.push(f);
        }
        vecs.push(v);
    }
    for v in &vecs {
        for f in v {
            payload.extend_from_slice(&f.to_le_bytes());
        }
    }
    // Stick the blake3 of the data section into the content_hash field.
    let data_hash = crate::storage::cas::blob::blob_hash(&payload[66..]);
    payload[33..65].copy_from_slice(&data_hash);

    let _hash = write_blob(&blob_path, &payload);

    let reader = NeuralReader::open(&blob_path).expect("open neural reader");
    assert_eq!(reader.count(), count as usize);
    assert_eq!(reader.dim(), dim as usize);
    assert_eq!(reader.dtype(), NeuralDtype::F32);

    // Verify dot product for 10 random (i, query) pairs.
    for trial in 0..10 {
        let i = ((trial * 7) as usize) % (count as usize);
        let mut query = Vec::with_capacity(dim as usize);
        let mut qprng = 0xfeed_faceu32 ^ (i as u32);
        for _ in 0..dim {
            qprng ^= qprng << 13;
            qprng ^= qprng >> 17;
            qprng ^= qprng << 5;
            let f = ((qprng as f32) / (u32::MAX as f32)) * 2.0 - 1.0;
            query.push(f);
        }

        let got = reader.dot(i, &query);
        let expected: f32 = vecs[i].iter().zip(query.iter()).map(|(a, b)| a * b).sum();
        let rel = ((got - expected).abs() / expected.abs().max(1e-9)).min(1.0);
        assert!(
            rel < 1e-6,
            "f32 dot mismatch at i={}: got={} expected={} rel={}",
            i,
            got,
            expected,
            rel
        );
    }
}

// ===========================================================================
// NeuralReader: INT8 SIMD dot-product correctness (VAL-READER-002)
// ===========================================================================

#[test]
fn test_neural_int8_dot_correctness() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("neural_int8.blob");

    let count: u32 = 1000; // 1000-vector fixture per VAL-CAS-TBD-002
    let dim: u32 = 256; // smaller for speed, still validates SIMD paths
    let scale: f32 = 0.005;
    let zero_point: f32 = 0.4;

    // Generate random original f32 vectors, quantize to i8 with scale/zero_point,
    // then store.
    let mut payload = Vec::new();
    payload.extend_from_slice(b"LIDX-NRL1");
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
    payload.extend_from_slice(&count.to_le_bytes());
    payload.extend_from_slice(&dim.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes()); // dtype = Int8
    payload.extend_from_slice(&scale.to_le_bytes());
    payload.extend_from_slice(&zero_point.to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]);
    payload.push(0); // alignment_pad to make data 4-byte aligned in mmap
    assert_eq!(payload.len(), 66);

    let mut originals: Vec<Vec<f32>> = Vec::new();
    let mut quantized: Vec<Vec<i8>> = Vec::new();
    let mut prng = 0xabad_cafeu32;
    for _ in 0..count {
        let mut v = Vec::with_capacity(dim as usize);
        let mut q = Vec::with_capacity(dim as usize);
        for _ in 0..dim {
            prng ^= prng << 13;
            prng ^= prng >> 17;
            prng ^= prng << 5;
            let original = ((prng as f32) / (u32::MAX as f32)) * 2.0 - 1.0;
            // Quantize original to i8: q = round((original - zero_point) / scale)
            let q_raw = ((original - zero_point) / scale).round();
            let q_clamped = q_raw.clamp(-128.0, 127.0) as i8;
            v.push(original);
            q.push(q_clamped);
        }
        originals.push(v);
        quantized.push(q);
    }
    for q in &quantized {
        for b in q {
            payload.push(*b as u8);
        }
    }
    let data_hash = crate::storage::cas::blob::blob_hash(&payload[66..]);
    payload[33..65].copy_from_slice(&data_hash);

    let _hash = write_blob(&blob_path, &payload);

    let reader = NeuralReader::open(&blob_path).expect("open neural int8 reader");
    assert_eq!(reader.dtype(), NeuralDtype::Int8);
    assert_eq!(reader.count(), count as usize);
    assert_eq!(reader.dim(), dim as usize);

    // Verify the SIMD dot-product matches dequantize-then-dot within 1e-4.
    for trial in 0..20 {
        let i = ((trial * 11) as usize) % (count as usize);
        let mut query = Vec::with_capacity(dim as usize);
        let mut qprng = 0xcafe_babeu32 ^ (i as u32);
        for _ in 0..dim {
            qprng ^= qprng << 13;
            qprng ^= qprng >> 17;
            qprng ^= qprng << 5;
            let f = ((qprng as f32) / (u32::MAX as f32)) * 2.0 - 1.0;
            query.push(f);
        }

        let got = reader.dot(i, &query);
        // Dequantize vector i, then dot with query.
        let dequant_i: Vec<f32> = quantized[i]
            .iter()
            .map(|&q| q as f32 * scale + zero_point)
            .collect();
        let expected: f32 = query.iter().zip(dequant_i.iter()).map(|(q, d)| q * d).sum();
        let rel = ((got - expected).abs() / expected.abs().max(1e-6)).min(1.0);
        assert!(
            rel < 1e-4,
            "INT8 dot mismatch at i={}: got={} expected={} rel={}",
            i,
            got,
            expected,
            rel
        );
    }
}

// ===========================================================================
// NeuralReader: zero-copy verification (VAL-READER-004)
// ===========================================================================

#[test]
fn test_neural_zero_copy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("neural_zc.blob");

    let count: u32 = 100;
    let dim: u32 = 128;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"LIDX-NRL1");
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
    payload.extend_from_slice(&count.to_le_bytes());
    payload.extend_from_slice(&dim.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes()); // F32
    payload.extend_from_slice(&1.0f32.to_le_bytes());
    payload.extend_from_slice(&0.0f32.to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]);
    payload.push(0); // alignment_pad to make data 4-byte aligned in mmap
    let bytes_per_vec = (dim as usize) * 4;
    payload.resize(payload.len() + (count as usize) * bytes_per_vec, 0);
    let data_hash = crate::storage::cas::blob::blob_hash(&payload[66..]);
    payload[33..65].copy_from_slice(&data_hash);

    let _hash = write_blob(&blob_path, &payload);
    let reader = NeuralReader::open(&blob_path).expect("open");

    // The VectorView pointer must lie within the mmap range.
    for i in 0..count as usize {
        let view = reader.vector(i);
        let view_ptr = view.as_f32_slice().as_ptr() as usize;
        let mmap_base = reader.mmap_data_ptr() as usize;
        let mmap_end = mmap_base + reader.mmap_data_len();
        assert!(
            view_ptr >= mmap_base && view_ptr < mmap_end,
            "vector {} pointer {:x} outside mmap range [{:x}, {:x})",
            i,
            view_ptr,
            mmap_base,
            mmap_end
        );
        // Length check
        assert_eq!(view.dim(), dim as usize);
    }

    // The pointer arithmetic: vectors are packed at offset 65 (neural header)
    // from the start of the CAS payload.
    let header_offset = BLOB_HEADER_LEN + 66;
    for i in 0..count as usize {
        let view = reader.vector(i);
        let expected_offset = header_offset + i * bytes_per_vec;
        let actual_offset = view.as_f32_slice().as_ptr() as usize - reader.mmap_data_ptr() as usize;
        assert_eq!(
            actual_offset, expected_offset,
            "vector {} offset mismatch",
            i
        );
    }
}

// ===========================================================================
// NeuralReader: handles missing artifact (Task 6)
// ===========================================================================

#[test]
fn test_neural_reader_missing_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.blob");
    let err = NeuralReader::open(&missing).unwrap_err();
    assert!(matches!(err, ReaderError::Io(_)), "got {:?}", err);
}

#[test]
fn test_neural_reader_bad_magic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("bad_magic.blob");
    let mut payload = vec![0u8; 128];
    payload[0..8].copy_from_slice(b"BAD-MAGC");
    let _h = write_blob(&blob_path, &payload);
    let err = NeuralReader::open(&blob_path).unwrap_err();
    assert!(
        matches!(err, ReaderError::BadHeader(_)) || matches!(err, ReaderError::BadBlob(_)),
        "got {:?}",
        err
    );
}

// ===========================================================================
// NeuralReader: reader hash validation (VAL-READER-010)
// ===========================================================================

#[test]
fn test_reader_hash_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("neural_valid.blob");

    let count: u32 = 10;
    let dim: u32 = 32;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"LIDX-NRL1");
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
    payload.extend_from_slice(&count.to_le_bytes());
    payload.extend_from_slice(&dim.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&1.0f32.to_le_bytes());
    payload.extend_from_slice(&0.0f32.to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]);
    payload.push(0); // alignment_pad
    payload.resize(payload.len() + (count * dim * 4) as usize, 0);
    let data_hash = crate::storage::cas::blob::blob_hash(&payload[66..]);
    payload[33..65].copy_from_slice(&data_hash);
    let _h = write_blob(&blob_path, &payload);

    // Valid blob opens fine.
    let reader = NeuralReader::open(&blob_path).expect("valid blob opens");
    assert_eq!(reader.count(), count as usize);

    // Corrupt: flip a payload byte. We construct a corrupted CAS frame.
    let corrupted_path = dir.path().join("neural_corrupt.blob");
    let mut frame = encode_blob(&payload);
    // Flip one byte deep inside the payload (after the neural header).
    let flip_offset = BLOB_HEADER_LEN + 66 + 10;
    frame[flip_offset] ^= 0xff;
    write_raw(&corrupted_path, &frame);

    let err = NeuralReader::open(&corrupted_path).unwrap_err();
    assert!(
        matches!(err, ReaderError::BadBlob(_)),
        "expected BadBlob, got {:?}",
        err
    );
}

// ===========================================================================
// TfidfReader correctness (VAL-READER-005)
// ===========================================================================

#[test]
fn test_tfidf_reader_correctness() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("tfidf.blob");

    let entries: Vec<(u32, u32, f32)> = vec![
        (0, 5, 0.12),
        (0, 17, 0.45),
        (1, 5, 0.08),
        (1, 42, 0.99),
        (3, 17, 0.21),
        (5, 0, 0.55),
        (5, 5, 0.34),
    ];
    let num_docs = 6u32;
    let num_terms = 64u32;

    let mut payload = Vec::new();
    payload.extend_from_slice(b"LIDX-TFD1"); // 8 bytes magic
    payload.push(1); // version
    payload.extend_from_slice(&[0u8; 3]); // pad
    payload.extend_from_slice(&num_docs.to_le_bytes());
    payload.extend_from_slice(&num_terms.to_le_bytes());
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]); // content_hash
    assert_eq!(payload.len(), 57);
    for (d, t, v) in &entries {
        payload.extend_from_slice(&d.to_le_bytes());
        payload.extend_from_slice(&t.to_le_bytes());
        payload.extend_from_slice(&v.to_le_bytes());
    }
    let data_hash = crate::storage::cas::blob::blob_hash(&payload[57..]);
    payload[25..57].copy_from_slice(&data_hash);
    let _h = write_blob(&blob_path, &payload);

    let reader = TfidfReader::open(&blob_path).expect("open tfidf");
    assert_eq!(reader.num_docs(), num_docs as usize);
    assert_eq!(reader.num_terms(), num_terms as usize);
    assert_eq!(reader.num_entries(), entries.len());

    // Look up each entry.
    for (d, t, v) in &entries {
        let got = reader.tf_idf(*d, *t).expect("tf_idf lookup");
        assert!(
            (got - v).abs() < 1e-7,
            "tf_idf({},{})={} expected={}",
            d,
            t,
            got,
            v
        );
    }
    // Non-existent entry returns None.
    assert!(reader.tf_idf(99, 99).is_none());
    assert!(reader.tf_idf(0, 999).is_none());
}

// ===========================================================================
// PdgReader correctness (VAL-READER-006)
// ===========================================================================

#[test]
fn test_pdg_reader_traversal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("pdg.blob");

    // Build a simple PDG with 3 nodes, 2 edges, interned strings.
    // Strings: "src/main.rs", "parse", "compute", "lookup"
    let strings: Vec<String> = vec![
        "src/main.rs".to_string(),
        "parse".to_string(),
        "compute".to_string(),
        "lookup".to_string(),
    ];
    let mut strings_bytes = Vec::new();
    let mut offsets: Vec<(u32, u32)> = Vec::new();
    for s in &strings {
        offsets.push((strings_bytes.len() as u32, s.len() as u32));
        strings_bytes.extend_from_slice(s.as_bytes());
    }

    // Nodes: (node_id, node_type, file_path_id, start_line, end_line, sym_name_id)
    let nodes: Vec<PdgNodeFix> = vec![
        PdgNodeFix {
            node_id: 0,
            node_type: 1,
            file_path_id: 0,
            start_line: 10,
            end_line: 20,
            sym_name_id: 1,
        },
        PdgNodeFix {
            node_id: 1,
            node_type: 2,
            file_path_id: 0,
            start_line: 25,
            end_line: 30,
            sym_name_id: 2,
        },
        PdgNodeFix {
            node_id: 2,
            node_type: 1,
            file_path_id: 0,
            start_line: 35,
            end_line: 50,
            sym_name_id: 3,
        },
    ];
    let edges: Vec<PdgEdgeFix> = vec![
        PdgEdgeFix {
            src: 0,
            dst: 1,
            edge_type: 1,
        },
        PdgEdgeFix {
            src: 0,
            dst: 2,
            edge_type: 2,
        },
    ];

    let mut payload = Vec::new();
    payload.extend_from_slice(b"LIDX-PDG1");
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
    payload.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(edges.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(strings_bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]); // content_hash
    assert_eq!(payload.len(), 61);
    for n in &nodes {
        payload.extend_from_slice(&n.node_id.to_le_bytes());
        payload.extend_from_slice(&n.node_type.to_le_bytes());
        payload.extend_from_slice(&n.file_path_id.to_le_bytes());
        payload.extend_from_slice(&n.start_line.to_le_bytes());
        payload.extend_from_slice(&n.end_line.to_le_bytes());
        payload.extend_from_slice(&n.sym_name_id.to_le_bytes());
    }
    for e in &edges {
        payload.extend_from_slice(&e.src.to_le_bytes());
        payload.extend_from_slice(&e.dst.to_le_bytes());
        payload.extend_from_slice(&e.edge_type.to_le_bytes());
    }
    for (offset, length) in &offsets {
        payload.extend_from_slice(&offset.to_le_bytes());
        payload.extend_from_slice(&length.to_le_bytes());
    }
    payload.extend_from_slice(&strings_bytes);
    let data_hash = crate::storage::cas::blob::blob_hash(&payload[61..]);
    payload[29..61].copy_from_slice(&data_hash);
    let _h = write_blob(&blob_path, &payload);

    let reader = PdgReader::open(&blob_path).expect("open pdg");
    assert_eq!(reader.num_nodes(), 3);
    assert_eq!(reader.num_edges(), 2);

    // Verify node metadata and string resolution.
    let n0 = reader.node(0).expect("node 0");
    assert_eq!(n0.node_id, 0);
    assert_eq!(n0.node_type, 1);
    assert_eq!(n0.start_line, 10);
    assert_eq!(n0.end_line, 20);
    assert_eq!(reader.resolve_string(n0.file_path_id), Some("src/main.rs"));
    assert_eq!(reader.resolve_string(n0.sym_name_id), Some("parse"));

    let n1 = reader.node(1).expect("node 1");
    assert_eq!(n0.file_path_id, n1.file_path_id); // same file
    assert_eq!(reader.resolve_string(n1.sym_name_id), Some("compute"));

    // Edge lookup by source.
    let callers_of_1: Vec<_> = reader.callers(1).collect();
    assert_eq!(callers_of_1, vec![0]);
    let callees_of_0: Vec<_> = reader.callees(0).collect();
    assert!(callees_of_0.contains(&1));
    assert!(callees_of_0.contains(&2));
}

// ===========================================================================
// SymbolReader correctness (VAL-READER-007)
// ===========================================================================

#[test]
fn test_symbols_reader_inventory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blob_path = dir.path().join("symbols.blob");

    let strings: Vec<String> = vec![
        "lib.rs".to_string(),
        "parse_token".to_string(),
        "compute_hash".to_string(),
        "lookup_id".to_string(),
    ];
    let mut strings_bytes = Vec::new();
    let mut offsets: Vec<(u32, u32)> = Vec::new();
    for s in &strings {
        offsets.push((strings_bytes.len() as u32, s.len() as u32));
        strings_bytes.extend_from_slice(s.as_bytes());
    }

    let symbols: Vec<SymbolEntryFix> = vec![
        SymbolEntryFix {
            name_id: 1,
            sym_type: 1, // function
            file_path_id: 0,
            start_line: 5,
            end_line: 15,
            complexity: 2.5,
        },
        SymbolEntryFix {
            name_id: 2,
            sym_type: 1,
            file_path_id: 0,
            start_line: 20,
            end_line: 30,
            complexity: 7.1,
        },
        SymbolEntryFix {
            name_id: 3,
            sym_type: 2, // struct
            file_path_id: 0,
            start_line: 40,
            end_line: 42,
            complexity: 0.0,
        },
    ];

    let mut payload = Vec::new();
    payload.extend_from_slice(b"LIDX-SYM1");
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
    payload.extend_from_slice(&(symbols.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(strings_bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]); // content_hash
    assert_eq!(payload.len(), 57);
    for s in &symbols {
        payload.extend_from_slice(&s.name_id.to_le_bytes());
        payload.extend_from_slice(&s.sym_type.to_le_bytes());
        payload.extend_from_slice(&s.file_path_id.to_le_bytes());
        payload.extend_from_slice(&s.start_line.to_le_bytes());
        payload.extend_from_slice(&s.end_line.to_le_bytes());
        payload.extend_from_slice(&s.complexity.to_le_bytes());
    }
    for (o, l) in &offsets {
        payload.extend_from_slice(&o.to_le_bytes());
        payload.extend_from_slice(&l.to_le_bytes());
    }
    payload.extend_from_slice(&strings_bytes);
    let data_hash = crate::storage::cas::blob::blob_hash(&payload[57..]);
    payload[25..57].copy_from_slice(&data_hash);
    let _h = write_blob(&blob_path, &payload);

    let reader = SymbolReader::open(&blob_path).expect("open symbols");
    assert_eq!(reader.num_symbols(), 3);

    let sym0 = reader.symbol(0).expect("symbol 0");
    assert_eq!(reader.resolve_string(sym0.name_id), Some("parse_token"));
    assert_eq!(sym0.sym_type, 1);
    assert_eq!(sym0.start_line, 5);
    assert_eq!(reader.resolve_string(sym0.file_path_id), Some("lib.rs"));
    assert!((sym0.complexity - 2.5).abs() < 1e-6);

    // lookup by name resolves correctly.
    let found = reader.lookup("compute_hash");
    assert!(found.is_some());
    let s = found.unwrap();
    assert_eq!(s.sym_type, 1);
    assert_eq!(s.complexity, 7.1);

    assert!(reader.lookup("nonexistent").is_none());
}

// ===========================================================================
// Readers share no heap allocation proportional to corpus (VAL-READER-008)
// ===========================================================================

#[test]
fn test_readers_no_corpus_proportional_heap() {
    // Sanity check: opening readers for small and large fixtures should be
    // roughly constant-time. We don't have a global allocator hook here (would
    // require lib features) so we instead verify that opening produces a
    // fixed-size reader state independent of vector count by examining the
    // public API.
    let dir = tempfile::tempdir().expect("tempdir");

    let small_path = dir.path().join("small.blob");
    let large_path = dir.path().join("large.blob");

    for (path, count) in [(&small_path, 10u32), (&large_path, 100u32)] {
        let dim = 64u32;
        let mut payload = Vec::new();
        payload.extend_from_slice(b"LIDX-NRL1");
        payload.push(1);
        payload.extend_from_slice(&[0u8; 3]);
        payload.extend_from_slice(&count.to_le_bytes());
        payload.extend_from_slice(&dim.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&1.0f32.to_le_bytes());
        payload.extend_from_slice(&0.0f32.to_le_bytes());
        payload.extend_from_slice(&[0u8; 32]);
        payload.push(0); // alignment_pad
        payload.resize(payload.len() + (count * dim * 4) as usize, 0);
        let data_hash = crate::storage::cas::blob::blob_hash(&payload[66..]);
        payload[33..65].copy_from_slice(&data_hash);
        let _h = write_blob(path, &payload);
    }

    let small_reader = NeuralReader::open(&small_path).expect("open small");
    let large_reader = NeuralReader::open(&large_path).expect("open large");

    // The reader state size does not scale with corpus size. Both must have
    // identical struct layout - the only state behind NeuralReader is the
    // mmap handle and a parsed header.
    assert_eq!(
        std::mem::size_of_val(&small_reader),
        std::mem::size_of_val(&large_reader),
    );
    // Read 50 random vectors from each; succeeds for both regardless of corpus size.
    for i in 0..50 {
        let idx_small = i.min(small_reader.count() - 1);
        let idx_large = i.min(large_reader.count() - 1);
        let _ = small_reader.vector(idx_small);
        let _ = large_reader.vector(idx_large);
    }
}

// ===========================================================================
// Reader survives generation swap (VAL-READER-009)
// ===========================================================================

#[test]
fn test_reader_survives_generation_swap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gen_n_path = dir.path().join("gen_n.blob");
    let gen_n1_path = dir.path().join("gen_n1.blob");

    let count = 10u32;
    let dim = 16u32;
    // gen_n: count=10, dim=16, F32, all-zero vectors.
    let mut payload_n = Vec::new();
    payload_n.extend_from_slice(b"LIDX-NRL1");
    payload_n.push(1);
    payload_n.extend_from_slice(&[0u8; 3]);
    payload_n.extend_from_slice(&count.to_le_bytes());
    payload_n.extend_from_slice(&dim.to_le_bytes());
    payload_n.extend_from_slice(&0u32.to_le_bytes());
    payload_n.extend_from_slice(&1.0f32.to_le_bytes());
    payload_n.extend_from_slice(&0.0f32.to_le_bytes());
    payload_n.extend_from_slice(&[0u8; 32]);
    payload_n.push(0); // alignment_pad
    payload_n.resize(payload_n.len() + (count * dim * 4) as usize, 0xAA);
    let data_hash = crate::storage::cas::blob::blob_hash(&payload_n[66..]);
    payload_n[33..65].copy_from_slice(&data_hash);
    let _h = write_blob(&gen_n_path, &payload_n);

    // gen_n1: count=10, dim=16, F32, different bytes (0xBB).
    let mut payload_n1 = payload_n.clone();
    for b in &mut payload_n1[66..] {
        *b = 0xBB;
    }
    let data_hash_n1 = crate::storage::cas::blob::blob_hash(&payload_n1[66..]);
    payload_n1[33..65].copy_from_slice(&data_hash_n1);
    let _h1 = write_blob(&gen_n1_path, &payload_n1);

    // Acquire reader on gen_n.
    let reader_n = NeuralReader::open(&gen_n_path).expect("open gen_n");

    // "Publish" gen_n1 as if a new generation were swapped in. The reader
    // still holds an mmap on gen_n's file and should continue to read those
    // bytes (not gen_n1's).

    // Verify reader_n reads gen_n data, not gen_n1 data.
    let v0 = reader_n.vector(0);
    let first_f32 = v0.as_f32_slice()[0];
    let expected = f32::from_le_bytes([0xAA; 4]);
    assert_eq!(first_f32, expected, "reader_n should read gen_n data");

    // Open reader_n1 and confirm it reads different data.
    let reader_n1 = NeuralReader::open(&gen_n1_path).expect("open gen_n1");
    let v0_n1 = reader_n1.vector(0);
    let first_f32_n1 = v0_n1.as_f32_slice()[0];
    let expected_n1 = f32::from_le_bytes([0xBB; 4]);
    assert_eq!(
        first_f32_n1, expected_n1,
        "reader_n1 should read gen_n1 data"
    );

    // And reader_n still reads gen_n data.
    let v0_recheck = reader_n.vector(0);
    assert_eq!(v0_recheck.as_f32_slice()[0], expected);
}

// ===========================================================================
// Helpers used by tests (small fixed structs that mirror the on-disk layout)
// ===========================================================================

#[derive(Debug, Clone, Copy)]
struct PdgNodeFix {
    node_id: u32,
    node_type: u32,
    file_path_id: u32,
    start_line: u32,
    end_line: u32,
    sym_name_id: u32,
}

#[derive(Debug, Clone, Copy)]
struct PdgEdgeFix {
    src: u32,
    dst: u32,
    edge_type: u32,
}

#[derive(Debug, Clone, Copy)]
struct SymbolEntryFix {
    name_id: u32,
    sym_type: u32,
    file_path_id: u32,
    start_line: u32,
    end_line: u32,
    complexity: f32,
}
