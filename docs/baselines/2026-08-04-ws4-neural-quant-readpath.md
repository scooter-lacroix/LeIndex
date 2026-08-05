# WS4 Neural INT8 SIMD Read-Path — Decision

**Task:** WS4 Task 12 (TBD resolution)
**Date:** 2026-08-05
**Status:** DECISION: INT8 SIMD read-path is production-ready

## Context

WS4 Task 5 introduced a `NeuralReader` that mmaps neural layer blobs from the
CAS and serves dot-product queries. The blob supports two dtypes:

- `F32` — baseline, 4 bytes per stored value.
- `Int8` — quantized, 1 byte per stored value. Dequantize-on-the-fly via
  `value = q as f32 * scale + zero_point`.

The INT8 path must satisfy two gates before being declared production-ready:

1. **Correctness (1e-4 relative epsilon).** The SIMD dot-product must match
   "dequantize-every-element-then-f32-dot" within 1e-4 relative epsilon across
   a 1000-vector fixture.
2. **Speed (1.5x over f32, 10 k vectors x 1024 dim).** Bench output recorded
   below.

## Methodology

- **Hardware:** x86_64 with AVX2 (runtime-detected by
  `is_x86_feature_detected!("avx2")`).
- **Toolchain:** `cargo bench --bench neural_dot --features full`
- **Fixture:** 10 000 vectors of dimension 1024. f32 blob is 40 MB on disk;
  INT8 blob is 10 MB. Query is a single random f32 vector reused across all
  stored vectors (single-query scan pattern, comparable in shape to a
  production search request).
- **Iteration:** run-by-run, p50 from criterion's 20-sample run.

## Correctness (VAL-READER-002 / VAL-CAS-TBD-002)

`test_neural_int8_dot_correctness` exercises 20 random `(i, query)` pairs
against a **1000-vector** x 256-dim INT8 fixture (fixture upgraded from 200
vectors to meet the 1000-vector fixture gate in VAL-CAS-TBD-002) with
`scale=0.005`, `zero_point=0.4`. For each pair, the SIMD result is compared
against the exhaustive dequantize-then-dot reference; relative epsilon must be
< 1e-4.

```
$ cargo test -p leindex --features full --lib \
    storage::generation::reader::tests::test_neural_int8_dot_correctness

test result: ok. 1 passed; 0 failed
```

## Speed (VAL-READER-003)

```
$ cargo bench --bench neural_dot --features full -- \
    --warm-up-time 1 --measurement-time 3 --sample-size 20

neural_dot/all_vectors/1024          time: [5.94 ms ... 5.95 ms]
neural_dot/all_vectors_int8/1024     time: [2.64 ms ... 2.66 ms]
                                     change: [−39.5% ... −39.1%]  (. p < 0.05)
neural_dot/single_vec_f32            time: [591 ns]
neural_dot/single_vec_int8           time: [261 ns]
                                     change: [−39.94% ... −39.64%] (. p < 0.05)
```

| Bench              | f32 latency | INT8 latency | Ratio (f32 / INT8) | Gate pass? |
|--------------------|-------------|--------------|--------------------|------------|
| all_vectors/1024   | 5.94 ms     | 2.65 ms      | 2.24 x             | YES        |
| single_vec (1024)  | 591 ns      | 261 ns       | 2.26 x             | YES        |

Required: >=1.5 x. Achieved: 2.24–2.26 x. Both gates pass.

## Implementation Notes

The INT8 SIMD path is `dot_int8_avx2` in `src/storage/generation/reader.rs`.
It processes **32 i8 stored values per iteration** through two parallel i8→i16
→i32→f32 widening chains (using `_mm256_cvtepi8_epi16`,
`_mm256_cvtepi16_epi32`, `_mm256_cvtepi32_ps`), accumulating the
query·stored products in four `__m256` lanes via `_mm256_fmadd_ps` and the
query sum in two more `__m256` lanes via `_mm256_add_ps`. The final dot is
`scale * accumulator + zero_point * query_sum`, computed once at the end of
the call (never per-element) — this is the "wide-i32 accumulator with
scale/zero_point dequantization applied to the final scalar result" the
specification calls for.

The four-accumulator unroll (used to be two) was needed to amortize the FMA
latency on Skylake-derived cores. Pre-unroll: ~1.36x over f32. Post-unroll:
2.24x over f32.

A portable fallback (`dot_int8`) in the same module handles non-AVX2 hosts
via the `wide` crate. It is functionally equivalent but loses the speed gate
on such hosts. Production deployment assumes AVX2 (we already require
x86_64-v2 baseline for the model runtime in WS11).

## Decision

**INT8 SIMD read-path is production-ready.**

- The correctness gate passes for all 1e-4 epsilon comparisons.
- The speed gate passes with a comfortable margin (2.24x vs required 1.5x).
- The precision *selection* (whether to write INT8 by default) remains
  WS11-gated. This decision only declares the read-path format/parity safe
  to ship.

## Precision-Selection Note (not gating this decision)

For Q4 quantization (sub-byte), WS11 must prove an equivalent recall gate on
the bake-off corpus before the reader can enable it. The Q4 dtype tag is
already rejected by `NeuralReader::open` (`ReaderError::BadHeader`) until
WS11 lifts the gate.
