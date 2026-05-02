//! Edit Preview Performance Benchmarks
//!
//! Measures the cost of creating ResolvedEditChange objects for edit preview
//! operations. The original code created O(N) identical validation objects;
//! the optimized code creates a single object.
//!
//! # Running
//!
//! ```bash
//! cargo bench --bench edit_preview_bench
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use leindex::edit::ResolvedEditChange;
use std::path::PathBuf;

/// Simulate the OLD (redundant) approach: create N identical ResolvedEditChange objects.
fn create_redundant_changes_old(
    n: usize,
    file_path: &PathBuf,
    original: &str,
    modified: &str,
) -> Vec<ResolvedEditChange> {
    (0..n)
        .map(|_| {
            ResolvedEditChange::new(
                file_path.clone(),
                original.to_string(),
                modified.to_string(),
            )
        })
        .collect()
}

/// Simulate the NEW (optimized) approach: create a single ResolvedEditChange.
fn create_single_change_optimized(
    file_path: &PathBuf,
    original: &str,
    modified: &str,
) -> ResolvedEditChange {
    ResolvedEditChange::new(
        file_path.clone(),
        original.to_string(),
        modified.to_string(),
    )
}

/// Benchmark: redundant creation of N identical ResolvedEditChange objects
fn bench_redundant_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolved_edit_change_creation");

    let file_path = PathBuf::from("src/cli/mcp/edit_preview_handler.rs");
    let original = "fn old_function() { println!(\"hello\"); }\n".repeat(50);
    let modified = "fn new_function() { println!(\"world\"); }\n".repeat(50);

    let change_counts = [1, 5, 10, 50, 100, 500];

    for &n in &change_counts {
        group.throughput(Throughput::Elements(n as u64));

        // Old approach: N objects
        group.bench_with_input(BenchmarkId::new("redundant_O(N)", n), &n, |b, &n| {
            b.iter(|| {
                let changes = create_redundant_changes_old(
                    black_box(n),
                    black_box(&file_path),
                    black_box(&original),
                    black_box(&modified),
                );
                black_box(changes);
            });
        });

        // New approach: 1 object
        group.bench_with_input(BenchmarkId::new("optimized_O(1)", n), &n, |b, &_n| {
            b.iter(|| {
                let change = create_single_change_optimized(
                    black_box(&file_path),
                    black_box(&original),
                    black_box(&modified),
                );
                black_box(change);
            });
        });
    }

    group.finish();
}

/// Benchmark: allocation cost for varying content sizes
fn bench_allocation_by_content_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolved_edit_change_by_size");

    let file_path = PathBuf::from("src/main.rs");
    let content_sizes = [100, 1_000, 10_000, 100_000]; // bytes

    for &size in &content_sizes {
        let original = "x".repeat(size);
        let modified = "y".repeat(size);

        group.throughput(Throughput::Bytes(size as u64));

        // Old: 10 identical objects
        group.bench_with_input(BenchmarkId::new("redundant_10x", size), &size, |b, _| {
            b.iter(|| {
                let changes: Vec<ResolvedEditChange> = (0..10)
                    .map(|_| {
                        ResolvedEditChange::new(
                            file_path.clone(),
                            original.clone(),
                            modified.clone(),
                        )
                    })
                    .collect();
                black_box(changes);
            });
        });

        // New: 1 object
        group.bench_with_input(BenchmarkId::new("optimized_1x", size), &size, |b, _| {
            b.iter(|| {
                let change =
                    ResolvedEditChange::new(file_path.clone(), original.clone(), modified.clone());
                black_box(change);
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_redundant_creation,
    bench_allocation_by_content_size
);
criterion_main!(benches);
