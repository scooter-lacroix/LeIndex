//! Text Search Byte Offset Calculation Benchmarks
//!
//! Measures performance of `find_normalised_whitespace` with various file sizes
//! to validate O(N²) → O(N) optimization in byte offset calculation.
//!
//! Both the old (O(N²)) and new (O(N)) implementations are included to allow
//! direct comparison. The benchmark uses self-contained implementations so no
//! `pub(crate)` access is needed.
//!
//! # Running
//!
//! ```bash
//! # Save baseline before optimization
//! cargo bench --bench text_search_bench -- --save-baseline before
//!
//! # Compare after optimization
//! cargo bench --bench text_search_bench -- --baseline before
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

/// Generate a haystack with the specified number of lines.
/// Each line is ~50 chars of code-like text.
fn generate_haystack(num_lines: usize) -> String {
    (0..num_lines)
        .map(|i| format!("    let value_{} = some_function(arg1, arg2) + other_call();", i))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Generate a needle that appears near the end of the haystack (worst case for search).
fn generate_needle_for_position(haystack: &str, target_line: usize) -> String {
    let lines: Vec<&str> = haystack.lines().collect();
    let line_idx = target_line.min(lines.len() - 1);
    let line = lines[line_idx];
    if let Some(idx) = line.find("some_function") {
        let portion = &line[idx..line.len().min(idx + 30)];
        portion.replace("(", " ( ").replace(",", " , ").replace(")", " ) ")
    } else {
        line.to_string()
    }
}

// ---------------------------------------------------------------------------
// Old O(N²) implementation (baseline for comparison)
// ---------------------------------------------------------------------------

fn find_normalised_whitespace_old(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let norm_needle = normalise_ws(needle);
    if norm_needle.is_empty() {
        return None;
    }
    let lines: Vec<&str> = haystack.lines().collect();
    for start_line in 0..lines.len() {
        let mut window = String::new();
        let mut raw_start_byte: Option<usize> = None;
        for end_line in start_line..lines.len().min(start_line + needle.lines().count() + 5) {
            if !window.is_empty() {
                window.push(' ');
            }
            window.push_str(lines[end_line].trim());
            let norm_window = normalise_ws(&window);
            if norm_window.find(&norm_needle).is_some() {
                let byte_start = if let Some(s) = raw_start_byte {
                    s
                } else {
                    let mut offset = 0;
                    for l in 0..start_line {
                        offset += lines[l].len() + 1;
                    }
                    offset
                };
                let mut byte_end = byte_start;
                for l in start_line..=end_line {
                    byte_end += lines[l].len() + 1;
                }
                return Some((byte_start, byte_end.min(haystack.len()) - byte_start));
            }
            if raw_start_byte.is_none() {
                let mut offset = 0;
                for l in 0..start_line {
                    offset += lines[l].len() + 1;
                }
                raw_start_byte = Some(offset);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// New O(N) implementation (pre-computed cumulative byte offsets)
// ---------------------------------------------------------------------------

fn find_normalised_whitespace_new(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let norm_needle = normalise_ws(needle);
    if norm_needle.is_empty() {
        return None;
    }
    let lines: Vec<&str> = haystack.lines().collect();

    // Pre-compute cumulative byte offsets for O(1) line-to-byte lookup.
    let mut line_offsets: Vec<usize> = Vec::with_capacity(lines.len());
    let mut cumulative: usize = 0;
    for line in &lines {
        line_offsets.push(cumulative);
        cumulative += line.len() + 1;
    }

    let max_window = needle.lines().count() + 5;
    for start_line in 0..lines.len() {
        let mut window = String::new();
        let window_end = lines.len().min(start_line + max_window);
        for end_line in start_line..window_end {
            if !window.is_empty() {
                window.push(' ');
            }
            window.push_str(lines[end_line].trim());
            let norm_window = normalise_ws(&window);
            if norm_window.find(&norm_needle).is_some() {
                let byte_start = line_offsets[start_line];
                let byte_end = line_offsets[end_line] + lines[end_line].len() + 1;
                return Some((byte_start, byte_end.min(haystack.len()) - byte_start));
            }
        }
    }
    None
}

fn normalise_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = true;
        } else {
            in_ws = false;
            out.push(ch);
        }
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

/// Benchmark the optimized (O(N)) implementation at various file sizes.
fn bench_optimized(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_normalised_whitespace");

    let sizes = vec![100, 1_000, 10_000, 100_000];

    for &num_lines in &sizes {
        let haystack = generate_haystack(num_lines);
        let needle = generate_needle_for_position(&haystack, num_lines - 1);

        group.throughput(Throughput::Elements(num_lines as u64));
        group.bench_with_input(
            BenchmarkId::new("byte_offset_search", num_lines),
            &(&haystack, &needle),
            |b, &(haystack, needle)| {
                b.iter(|| {
                    black_box(find_normalised_whitespace_new(black_box(haystack), black_box(needle)));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark the old O(N²) implementation for comparison.
fn bench_old(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_normalised_whitespace_old");

    let sizes = vec![100, 1_000, 10_000, 100_000];

    for &num_lines in &sizes {
        let haystack = generate_haystack(num_lines);
        let needle = generate_needle_for_position(&haystack, num_lines - 1);

        group.throughput(Throughput::Elements(num_lines as u64));
        group.bench_with_input(
            BenchmarkId::new("byte_offset_search", num_lines),
            &(&haystack, &needle),
            |b, &(haystack, needle)| {
                b.iter(|| {
                    black_box(find_normalised_whitespace_old(black_box(haystack), black_box(needle)));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark small file performance to ensure no regression.
fn bench_small_file_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("small_file_performance");
    group.sample_size(100);

    let sizes = vec![10, 50, 100];

    for &num_lines in &sizes {
        let haystack = generate_haystack(num_lines);
        let needle = generate_needle_for_position(&haystack, num_lines / 2);

        group.throughput(Throughput::Elements(num_lines as u64));
        group.bench_with_input(
            BenchmarkId::new("small_file", num_lines),
            &(&haystack, &needle),
            |b, &(haystack, needle)| {
                b.iter(|| {
                    black_box(find_normalised_whitespace_new(black_box(haystack), black_box(needle)));
                });
            },
        );
    }

    group.finish();
}

// Configure criterion
criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(3))
        .sample_size(50);
    targets = bench_optimized, bench_old, bench_small_file_performance
}

criterion_main!(benches);
