//! SIMD Performance Benchmarks
//!
//! This benchmark suite measures the performance of SIMD-optimized distance
//! computations, comparing AVX2 (when available) against the fallback implementation.
//!
//! # Running Benchmarks
//!
//! ```bash
//! # Run all SIMD benchmarks
//! cargo bench --bench simd_benchmarks
//!
//! # Run specific benchmark
//! cargo bench --bench simd_benchmarks dot_product
//!
//! # Save baseline for comparison
//! cargo bench --bench simd_benchmarks -- --save-baseline avx2
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use leindex::search::quantization::simd::{dot_product_adc, fallback};
use leindex::search::quantization::{Int8QuantizedVector, Quantize};

/// Generate test data for benchmarks
fn generate_test_data(dim: usize) -> (Vec<f32>, Vec<f32>, Int8QuantizedVector) {
    // Deterministic test data for reproducibility
    let query: Vec<f32> = (0..dim).map(|i| ((i * 7) % 100) as f32 / 100.0).collect();
    let stored: Vec<f32> = (0..dim).map(|i| ((i * 13) % 100) as f32 / 100.0).collect();
    let quantized = stored.quantize();
    (query, stored, quantized)
}

/// Benchmark dot product computation across various dimensions
fn benchmark_dot_product(c: &mut Criterion) {
    let mut group = c.benchmark_group("dot_product_adc");

    // Comprehensive dimension coverage:
    // - Edge cases: 1, 7, 8, 9, 15, 16, 17 (remainder handling)
    // - Small: 32, 64, 96
    // - Medium: 128, 256, 384, 512
    // - Standard embeddings: 768 (BERT), 1024 (GPT), 1536 (OpenAI)
    // - Large: 2048, 4096 (modern LLMs)
    let dimensions = vec![
        1, 7, 8, 9, 15, 16, 17, // Edge cases for remainder handling
        32, 64, 96, // Small dimensions
        128, 256, 384, 512, // Medium dimensions
        768, 1024, 1536, // Standard embedding models
        2048, 4096, // Large model dimensions
    ];

    for dim in &dimensions {
        let (query, _stored, quantized) = generate_test_data(*dim);
        let query_sum: f32 = query.iter().sum();

        // Set throughput metric (elements processed)
        group.throughput(Throughput::Elements(*dim as u64));

        // Benchmark the public API (auto-selected implementation)
        group.bench_with_input(BenchmarkId::new("auto", dim), dim, |b, _| {
            b.iter(|| {
                dot_product_adc(
                    black_box(&query),
                    black_box(&quantized),
                    black_box(query_sum),
                )
            });
        });

        // Benchmark fallback implementation
        group.bench_with_input(BenchmarkId::new("fallback", dim), dim, |b, _| {
            b.iter(|| {
                fallback::dot_product_adc(
                    black_box(&query),
                    black_box(&quantized),
                    black_box(query_sum),
                )
            });
        });

        // Benchmark AVX2 if available
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                use lerecherche::quantization::simd::x86_avx2;

                group.bench_with_input(BenchmarkId::new("avx2", dim), dim, |b, _| {
                    b.iter(|| unsafe {
                        x86_avx2::dot_product_adc(
                            black_box(&query),
                            black_box(&quantized),
                            black_box(query_sum),
                        )
                    });
                });
            }
        }
    }

    group.finish();
}

/// Benchmark throughput at scale (batch processing simulation)
fn benchmark_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    group.sample_size(100);

    let dim = 768; // Typical embedding dimension
    let batch_size = 1000; // Number of distance computations

    let (query, _stored, quantized) = generate_test_data(dim);
    let query_sum: f32 = query.iter().sum();

    // Store multiple copies to simulate batch processing
    let quantized_vectors: Vec<_> = (0..batch_size).map(|_| quantized.clone()).collect();

    group.throughput(Throughput::Elements((dim * batch_size) as u64));

    // Benchmark auto-selected implementation
    group.bench_function("auto_batch_1000", |b| {
        b.iter(|| {
            let mut total = 0.0f32;
            for qvec in &quantized_vectors {
                total += dot_product_adc(black_box(&query), black_box(qvec), black_box(query_sum));
            }
            black_box(total);
        });
    });

    // Benchmark fallback
    group.bench_function("fallback_batch_1000", |b| {
        b.iter(|| {
            let mut total = 0.0f32;
            for qvec in &quantized_vectors {
                total += fallback::dot_product_adc(
                    black_box(&query),
                    black_box(qvec),
                    black_box(query_sum),
                );
            }
            black_box(total);
        });
    });

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            use lerecherche::quantization::simd::x86_avx2;

            group.bench_function("avx2_batch_1000", |b| {
                b.iter(|| {
                    let mut total = 0.0f32;
                    for qvec in &quantized_vectors {
                        total += unsafe {
                            x86_avx2::dot_product_adc(
                                black_box(&query),
                                black_box(qvec),
                                black_box(query_sum),
                            )
                        };
                    }
                    black_box(total);
                });
            });
        }
    }

    group.finish();
}

/// Benchmark comparison between implementations
fn benchmark_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("comparison");

    // Single dimension for clear comparison
    let dim = 768;
    let (query, _stored, quantized) = generate_test_data(dim);
    let query_sum: f32 = query.iter().sum();

    // Warm up to ensure consistent results
    for _ in 0..100 {
        let _ = dot_product_adc(&query, &quantized, query_sum);
    }

    group.bench_function("auto_768d", |b| {
        b.iter(|| {
            dot_product_adc(
                black_box(&query),
                black_box(&quantized),
                black_box(query_sum),
            )
        });
    });

    group.bench_function("fallback_768d", |b| {
        b.iter(|| {
            fallback::dot_product_adc(
                black_box(&query),
                black_box(&quantized),
                black_box(query_sum),
            )
        });
    });

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            use lerecherche::quantization::simd::x86_avx2;

            group.bench_function("avx2_768d", |b| {
                b.iter(|| unsafe {
                    x86_avx2::dot_product_adc(
                        black_box(&query),
                        black_box(&quantized),
                        black_box(query_sum),
                    )
                });
            });
        }
    }

    group.finish();
}

/// Comprehensive scaling analysis across dimension ranges
fn benchmark_scaling_analysis(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling_analysis");
    group.sample_size(50); // Faster for large dimensions

    // Test scaling from small to very large dimensions
    let dimensions = vec![
        ("tiny", 8),
        ("small", 64),
        ("medium", 512),
        ("standard", 768),
        ("large", 1536),
        ("xlarge", 4096),
    ];

    for (name, dim) in &dimensions {
        let (query, _stored, quantized) = generate_test_data(*dim);
        let query_sum: f32 = query.iter().sum();

        // Report throughput in distances/second
        group.throughput(Throughput::Elements(*dim as u64));

        group.bench_with_input(BenchmarkId::new("fallback", name), dim, |b, _| {
            b.iter(|| {
                fallback::dot_product_adc(
                    black_box(&query),
                    black_box(&quantized),
                    black_box(query_sum),
                )
            });
        });
    }

    group.finish();
}

/// Latency distribution analysis
fn benchmark_latency_distribution(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_distribution");

    // Test at standard embedding dimension
    let dim = 768;
    let (query, _stored, quantized) = generate_test_data(dim);
    let query_sum: f32 = query.iter().sum();

    // Single-shot latency (no batching)
    group.bench_function("single_shot_latency", |b| {
        b.iter(|| {
            black_box(dot_product_adc(
                black_box(&query),
                black_box(&quantized),
                black_box(query_sum),
            ))
        });
    });

    group.finish();
}

/// Generate a performance report summary
#[allow(dead_code)]
fn print_performance_summary() {
    println!("\n");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║         SIMD Performance Benchmark Summary                   ║");
    println!("╠══════════════════════════════════════════════════════════════╣");

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            println!("║  Platform: x86_64 with AVX2 support                          ║");
            println!("║  Expected: ~3-4x speedup over fallback                       ║");
        } else if is_x86_feature_detected!("sse4.1") {
            println!("║  Platform: x86_64 with SSE4.1 (no AVX2)                      ║");
            println!("║  Expected: ~2x speedup over baseline                         ║");
        } else {
            println!("║  Platform: x86_64 (no AVX2/SSE4.1)                           ║");
            println!("║  Expected: Baseline performance                              ║");
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        println!("║  Platform: AArch64                                           ║");
        println!("║  Expected: Baseline (NEON support planned)                   ║");
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        println!("║  Platform: Other                                             ║");
        println!("║  Expected: Baseline performance                              ║");
    }

    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("\n");
}

// Configure criterion and define benchmark groups
criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(5))
        .sample_size(200);
    targets = benchmark_dot_product, benchmark_throughput, benchmark_comparison,
              benchmark_scaling_analysis, benchmark_latency_distribution
}

criterion_main!(benches);
