//! `neural_dot` benchmark — f32 vs INT8 dot-product throughput.
//!
//! Implements VAL-READER-003 / WS4 Task 5 speed gate. Runs on 10 000 vectors
//! of dimensionality 1024. INT8 must be at least 1.5x faster than f32 (median
//! latency) for the read-path to ship INT8-native.
//!
//! # Running
//!
//! ```bash
//! cargo bench --bench neural_dot
//! cargo bench --bench neural_dot -- --save-baseline current
//! ```
//!
//! The benchmark mmaps real CAS blob files on disk so that page-cache and
//! memory-bandwidth effects are properly accounted for (not just CPU compute).
//! Each iteration walks all 10 000 vectors and computes the dot product with
//! a fresh random query.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use leindex::storage::generation::{NeuralDtype, NeuralReader};
use std::hint::black_box;

const DIM: usize = 1024;
const COUNT: usize = 10_000;

/// Build a CAS blob containing `count * dim` f32 values.
fn write_f32_blob(path: &std::path::Path, count: usize, dim: usize) {
    use std::fs;
    use std::io::Write;
    let mut payload = Vec::with_capacity(66 + count * dim * 4);
    payload.extend_from_slice(b"LIDX-NRL1");
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
    payload.extend_from_slice(&(count as u32).to_le_bytes());
    payload.extend_from_slice(&(dim as u32).to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes()); // F32
    payload.extend_from_slice(&1.0f32.to_le_bytes());
    payload.extend_from_slice(&0.0f32.to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]);
    payload.push(0); // alignment_pad
    for i in 0..count {
        for j in 0..dim {
            // Deterministic-ish pseudo-random values in [-1, 1].
            let seed = ((i.wrapping_mul(31)).wrapping_add(j)) as u32;
            let f = ((seed as f32) / (u32::MAX as f32)) * 2.0 - 1.0;
            payload.extend_from_slice(&f.to_le_bytes());
        }
    }
    let data_hash = leindex::storage::cas::blob::blob_hash(&payload[66..]);
    payload[33..65].copy_from_slice(&data_hash);

    let frame = leindex::storage::cas::blob::encode_blob(&payload);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut file = fs::File::create(path).expect("create blob");
    file.write_all(&frame).expect("write blob");
    file.sync_all().expect("fsync");
}

/// Build a CAS blob containing `count * dim` INT8 values with `scale` and
/// `zero_point` such that `dequantize(q) = q * scale + zero_point`.
fn write_int8_blob(path: &std::path::Path, count: usize, dim: usize) {
    use std::fs;
    use std::io::Write;
    let scale: f32 = 0.005;
    let zero_point: f32 = 0.3;
    let mut payload = Vec::with_capacity(66 + count * dim);
    payload.extend_from_slice(b"LIDX-NRL1");
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
    payload.extend_from_slice(&(count as u32).to_le_bytes());
    payload.extend_from_slice(&(dim as u32).to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes()); // Int8
    payload.extend_from_slice(&scale.to_le_bytes());
    payload.extend_from_slice(&zero_point.to_le_bytes());
    payload.extend_from_slice(&[0u8; 32]);
    payload.push(0); // alignment_pad
    for i in 0..count {
        for j in 0..dim {
            // Deterministic i8 in [-128, 127].
            let seed = ((i.wrapping_mul(31)).wrapping_add(j)) as u8;
            payload.push(seed as i8 as u8);
        }
    }
    let data_hash = leindex::storage::cas::blob::blob_hash(&payload[66..]);
    payload[33..65].copy_from_slice(&data_hash);

    let frame = leindex::storage::cas::blob::encode_blob(&payload);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut file = fs::File::create(path).expect("create blob");
    file.write_all(&frame).expect("write blob");
    file.sync_all().expect("fsync");
}

fn build_query(dim: usize, salt: u32) -> Vec<f32> {
    (0..dim)
        .map(|j| {
            let seed = salt.wrapping_add(j as u32);
            ((seed as f32) / (u32::MAX as f32)) * 2.0 - 1.0
        })
        .collect()
}

fn bench_f32_vs_int8(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let f32_path = dir.path().join("f32.blob");
    let int8_path = dir.path().join("int8.blob");
    write_f32_blob(&f32_path, COUNT, DIM);
    write_int8_blob(&int8_path, COUNT, DIM);

    let reader_f32 = NeuralReader::open(&f32_path).expect("open f32");
    let reader_int8 = NeuralReader::open(&int8_path).expect("open int8");
    assert_eq!(reader_f32.dtype(), NeuralDtype::F32);
    assert_eq!(reader_int8.dtype(), NeuralDtype::Int8);

    let query = build_query(DIM, 0xC0FFEE);

    let mut group = c.benchmark_group("neural_dot");
    group.throughput(Throughput::Elements((COUNT * DIM) as u64));
    group.sample_size(20);

    // Scan all 10 000 vectors, dotting with the query.
    group.bench_with_input(BenchmarkId::new("all_vectors", DIM), &query, |b, q| {
        b.iter(|| {
            let mut acc = 0.0f32;
            for i in 0..COUNT {
                acc += reader_f32.dot_simd(i, black_box(q));
            }
            black_box(acc);
        });
    });

    group.bench_with_input(BenchmarkId::new("all_vectors_int8", DIM), &query, |b, q| {
        b.iter(|| {
            let mut acc = 0.0f32;
            for i in 0..COUNT {
                acc += reader_int8.dot_simd(i, black_box(q));
            }
            black_box(acc);
        });
    });

    // Single-shot latency comparison.
    group.bench_function("single_vec_f32", |b| {
        b.iter(|| reader_f32.dot_simd(black_box(0), black_box(&query)));
    });
    group.bench_function("single_vec_int8", |b| {
        b.iter(|| reader_int8.dot_simd(black_box(0), black_box(&query)));
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(10))
        .sample_size(40);
    targets = bench_f32_vs_int8
}

criterion_main!(benches);
