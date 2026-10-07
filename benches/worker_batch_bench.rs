//! Hermetic worker batch-loop benchmark (no GPU, no model).
//!
//! Priority 4 go/no-go measurement stage. This bench cannot authorize the
//! pipeline by itself — it only characterizes the sequential batch loop's
//! wall time and the synthetic split between batching (tokenization) and
//! per-batch work (inference) so we know whether overlap has real headroom.
//! The authoritative decision requires the GPU benchmark (gpu_embed_bench.rs).
//!
//! Run with: cargo bench --bench worker_batch_bench

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use leindex::embed::RuntimeConfig;
use leindex::embed::runtime::WorkerRuntime;

/// Stand-in for `tokenizers::Encoding`: the batch loop only consumes
/// `encodings.len()`, so a unit struct with the derived `Clone` is enough.
#[derive(Clone)]
struct FakeEncoding;

/// `tokenize` closure cost (tunable per invocation) that simulates the real
/// tokenizer's per-sub-batch cost without depending on tokenizers.
fn fake_tokenize(
    sub_texts: &[String],
    per_text_cost: Duration,
) -> Result<Vec<FakeEncoding>, leindex::embed::protocol::WorkerError> {
    std::thread::sleep(per_text_cost.saturating_mul(sub_texts.len() as u32));
    Ok(vec![FakeEncoding; sub_texts.len()])
}

/// `run_sub_batch` closure cost: sleep to emulate ORT inference wall time,
/// then return flat row-major vectors of `rows * dim`.
fn fake_infer(
    encodings: &[FakeEncoding],
    dim: usize,
    per_batch_cost: Duration,
    batch_cost: Duration,
) -> Result<Vec<f32>, leindex::embed::protocol::WorkerError> {
    std::thread::sleep(per_batch_cost.saturating_add(batch_cost));
    Ok(vec![0.0f32; encodings.len() * dim])
}

/// Run the batch loop with synthetic closures and report wall time plus the
/// synthetic tokenize/infer component split.
fn bench_seq(
    texts: Vec<String>,
    dim: usize,
    batch_size: usize,
    fixed: bool,
    tokenize_cost: Duration,
    infer_cost: Duration,
) -> (Duration, usize, usize) {
    let rt = WorkerRuntime::new(RuntimeConfig::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let mut tokenize_calls = 0usize;
    let mut infer_calls = 0usize;
    let start = Instant::now();
    rt.run_onnx_embed_text_batch_loop(
        &texts,
        batch_size,
        fixed,
        dim,
        &cancel,
        |sub_texts| {
            tokenize_calls += 1;
            fake_tokenize(sub_texts, tokenize_cost)
        },
        |encodings, dim| {
            infer_calls += 1;
            fake_infer(encodings, dim, tokenize_cost, infer_cost)
        },
    )
    .expect("synthetic batch loop must succeed");
    (start.elapsed(), tokenize_calls, infer_calls)
}

fn report(name: &str, elapsed: Duration, tokenize_calls: usize, infer_calls: usize) {
    let total_ms = elapsed.as_secs_f64() * 1e3;
    let per_batch_ms = total_ms / infer_calls.max(1) as f64;
    println!(
        "  {name}: total={total_ms:.2}ms tokenize_calls={tokenize_calls} infer_calls={infer_calls} per_batch={per_batch_ms:.3}ms"
    );
}

fn main() {
    println!("=== worker_batch_bench (hermetic, no GPU) ===");
    // Dynamic provider path (e.g. CPU/CUDA) — variable batch sizes.
    let short_dynamic: Vec<String> = (0..64).map(|i| format!("short-{i}")).collect();
    let (t, tc, ic) = bench_seq(
        short_dynamic.clone(),
        768,
        8,
        false,
        Duration::from_micros(200),
        Duration::from_micros(2000),
    );
    report("dynamic short 64x768", t, tc, ic);

    // Fixed provider path (MIGraphX/ROCm) — constant batch size 8 with a
    // final partial batch padded.
    let short_fixed: Vec<String> = (0..64).map(|i| format!("short-fixed-{i}")).collect();
    let (t, tc, ic) = bench_seq(
        short_fixed,
        768,
        8,
        true,
        Duration::from_micros(200),
        Duration::from_micros(2000),
    );
    report("fixed short 64x768", t, tc, ic);

    // Few maximum-length texts → fewer, larger batches.
    let long: Vec<String> = (0..8)
        .map(|i| format!("long-doc-{i}-{}", "x".repeat(4096)))
        .collect();
    let (t, tc, ic) = bench_seq(
        long,
        768,
        8,
        false,
        Duration::from_micros(1500),
        Duration::from_micros(15000),
    );
    report("dynamic long 8x768", t, tc, ic);

    println!("  (hermetic only: use gpu_embed_bench for the decision)");
}
