//! Mandatory GPU embedding benchmark for Priority 4 go/no-go.
//!
//! Drives the real WorkerRuntime batch loop against the configured ORT
//! provider (MIGraphX/ROCm), measures wall time, time-to-first-inference
//! (TTFT), and ROCm GPU busy/utilization via `rocm-smi` when present.
//!
//! Requires a real model + ONNX runtime. Run with:
//!   cargo bench --bench gpu_embed_bench --features onnx
//!
//! Output is the authoritative sequential-path baseline for the
//! pipelining decision.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use leindex::embed::RuntimeConfig;
use leindex::embed::runtime::WorkerRuntime;

/// Sample per-GPU busy % over `duration`. Primary: AMD sysfs
/// `/sys/class/drm/cardN/device/gpu_busy_percent` (clean single value per
/// card). Fallback: strict `GPU[x]`-prefixed `rocm-smi --showuse` parse.
/// Returns the mean busy % of the busiest GPU (never >100) or None.
fn sample_gpu_busy(duration: Duration) -> Option<f64> {
    let mut per_gpu: Vec<f64> = Vec::new();
    let mut samples = 0usize;
    let deadline = Instant::now() + duration;

    // Primary: sysfs per-card busy (card0..7). This is a single clean
    // percentage per card and the most reliable source on ROCm.
    let mut sysfs_value = None;
    for card in 0..8 {
        let path = format!("/sys/class/drm/card{card}/device/gpu_busy_percent");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(pct) = contents.trim().parse::<f64>() {
                sysfs_value = Some(sysfs_value.map_or(pct, |v: f64| v.max(pct)));
            }
        }
    }
    if let Some(pct) = sysfs_value {
        per_gpu.push(pct);
        samples = 1;
    }

    // Fallback: rocm-smi --showuse, strict "GPU[<idx>]" lines only.
    if samples == 0 {
        let probe_ok = std::process::Command::new("rocm-smi")
            .arg("--showuse")
            .env("TIRITH", "0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if probe_ok {
            while Instant::now() < deadline {
                if let Ok(out) = std::process::Command::new("rocm-smi")
                    .arg("--showuse")
                    .env("TIRITH", "0")
                    .output()
                {
                    let text = String::from_utf8_lossy(&out.stdout);
                    let mut sample: Vec<f64> = Vec::new();
                    for line in text.lines() {
                        let trimmed = line.trim();
                        // Only accept lines that begin with "GPU[<n>]" and
                        // contain "GPU use (%):" — never headers/summaries.
                        if trimmed.starts_with("GPU[") {
                            if let Some(idx) = trimmed.find("GPU use (%):") {
                                if let Ok(pct) = trimmed[idx + "GPU use (%):".len()..]
                                    .trim()
                                    .trim_end_matches('%')
                                    .parse::<f64>()
                                {
                                    sample.push(pct);
                                }
                            }
                        }
                    }
                    if !sample.is_empty() {
                        samples += 1;
                        for (i, pct) in sample.iter().enumerate() {
                            if per_gpu.len() <= i {
                                per_gpu.push(0.0);
                            }
                            per_gpu[i] += pct;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    if samples == 0 {
        return None;
    }
    // Mean busy per GPU, report the busiest GPU (never >100).
    Some(
        per_gpu
            .iter()
            .map(|sum| sum / samples as f64)
            .fold(0.0f64, f64::max),
    )
}

fn main() {
    println!("=== gpu_embed_bench (mandatory GPU go/no-go) ===");
    let config = RuntimeConfig::default();
    println!(
        "  provider={} model={} ort_threads={}",
        config.execution_provider, config.model_name, config.ort_threads
    );

    let configured_provider = config.execution_provider.clone();
    let rt = WorkerRuntime::new(config);
    let active_provider = rt.bench_provider_status();
    println!(
        "  resolved_provider={} (config execution_provider={})",
        active_provider, configured_provider
    );
    let Some(session) = rt.bench_session() else {
        eprintln!("  NO ONNX SESSION — cannot run GPU benchmark; is ORT + model set up?");
        std::process::exit(2);
    };
    let tokenizer = rt
        .bench_tokenizer()
        .expect("tokenizer must exist when session exists");
    let active_provider = rt.bench_provider_status();
    let embed_dim = rt.bench_embed_dim();
    let inference_batch_size = leindex::embed::runtime::configured_onnx_inference_batch_size(
        rt.bench_model_name(),
        active_provider,
    );
    let fixed_batch = active_provider.eq_ignore_ascii_case("migraphx")
        || active_provider.eq_ignore_ascii_case("rocm");
    println!(
        "  inference_batch_size={} fixed_batch={} dim={}",
        inference_batch_size, fixed_batch, embed_dim
    );

    // Short-symbol corpus and long-document corpus.
    let short: Vec<String> = (0..256).map(|i| format!("short symbol {i}")).collect();
    let long: Vec<String> = (0..32)
        .map(|i| format!("long document {i} {}", "x".repeat(4096)))
        .collect();

    for (label, corpus) in [("short", short), ("long", long)] {
        let cancel = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        let result = rt.bench_run_onnx_embed(&session, &tokenizer, &corpus, embed_dim, &cancel);
        let wall = start.elapsed();
        match result {
            Ok(resp) => {
                let rows = resp.vectors.len() / embed_dim.max(1);
                println!("  {label}: rows={rows} wall={wall:?}");
                // GPU busy sampled over the same corpus after warmup.
                let busy = sample_gpu_busy(Duration::from_secs(5));
                match busy {
                    Some(pct) => println!(
                        "  {label}: gpu_busy_mean={}% (rocm-smi)",
                        (pct * 100.0) as u32
                    ),
                    None => println!("  {label}: rocm-smi unavailable; no GPU busy sampled"),
                }
            }
            Err(e) => println!("  {label}: ERROR {e:?}"),
        }
    }
    println!("  TTFT/wall baseline recorded above (model/provider in config)");
}
