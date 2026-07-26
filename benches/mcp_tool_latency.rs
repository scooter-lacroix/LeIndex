use criterion::{black_box, criterion_group, criterion_main, Criterion};
use leindex::cli::mcp::handlers::all_tool_handlers;
use leindex::cli::mcp::protocol::JsonRpcRequest;
use leindex::cli::mcp::server::{handle_tool_call, index_with_progress};
use leindex::cli::memory_report::{current_rss_bytes, MemoryReportTracker};
use leindex::cli::ProjectRegistry;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

struct GitFixture {
    temp: TempDir,
}

impl GitFixture {
    fn new(source_files: usize) -> Self {
        Self::with_ignored(source_files, 0)
    }

    fn with_ignored(source_files: usize, ignored_files: usize) -> Self {
        let temp = TempDir::new().expect("create git fixture");
        fs::create_dir_all(temp.path().join("src")).expect("create source directory");
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn benchmark_marker() -> &'static str { \"MCP_BENCHMARK_MARKER\" }\n",
        )
        .expect("write source fixture");
        let committed_source_files = if ignored_files > 0 {
            source_files.saturating_sub(5)
        } else {
            source_files
        };
        for source_file in 1..committed_source_files {
            let directory = temp
                .path()
                .join("src")
                .join(format!("dir_{}", source_file % 200));
            fs::create_dir_all(&directory).expect("create source shard");
            fs::write(
                directory.join(format!("module_{source_file}.rs")),
                format!("pub fn benchmark_marker_{source_file}() -> usize {{ {source_file} }}\n"),
            )
            .expect("write indexing fixture source");
        }

        if ignored_files > 0 {
            fs::write(temp.path().join(".gitignore"), "build-local/\n.leindex/\n")
                .expect("write ignored build rules");
            fs::create_dir_all(temp.path().join("build-local")).expect("create ignored tree");
            for file in 0..ignored_files {
                fs::write(
                    temp.path().join(format!("build-local/generated_{file}.rs")),
                    "ignored build output\n",
                )
                .expect("write ignored build fixture");
            }
        }

        for args in [
            ["init"].as_slice(),
            ["config", "user.email", "leindex@example.test"].as_slice(),
            ["config", "user.name", "LeIndex Benchmark"].as_slice(),
            ["add", "."].as_slice(),
            ["commit", "-m", "fixture"].as_slice(),
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(temp.path())
                .output()
                .expect("run git for fixture");
            assert!(output.status.success(), "git {:?} failed", args);
        }

        if ignored_files > 0 {
            for source_file in 1..=5 {
                let path = temp
                    .path()
                    .join("src")
                    .join(format!("dir_{}", source_file % 200))
                    .join(format!("module_{source_file}.rs"));
                fs::OpenOptions::new()
                    .append(true)
                    .open(path)
                    .expect("open tracked change")
                    .write_all(b"// tracked modification\n")
                    .expect("write tracked change");
            }
            for source_file in 0..5 {
                fs::write(
                    temp.path().join(format!("src/untracked_{source_file}.rs")),
                    "pub fn untracked_benchmark_marker() {}\n",
                )
                .expect("write untracked source");
            }
        }

        Self { temp }
    }

    fn project_path(&self) -> String {
        self.temp.path().display().to_string()
    }

    fn source_path(&self) -> String {
        self.temp.path().join("src/lib.rs").display().to_string()
    }
}

#[derive(Debug, Serialize)]
struct PerfMetric {
    p50_us: u128,
    p95_us: u128,
    max_us: u128,
    rss_delta_bytes: u64,
    hydration_delta: u64,
    pdg_load_delta: u64,
    neural_request_delta: u64,
}

#[derive(Debug, Serialize)]
struct PerfReport {
    fixture_source_files: usize,
    fixture_ignored_files: usize,
    warm_samples: usize,
    cold_samples: usize,
    metrics: BTreeMap<String, PerfMetric>,
}

fn percentile(samples: &[Duration], percentile: usize) -> u128 {
    assert!(!samples.is_empty(), "benchmark needs at least one sample");
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() * percentile).div_ceil(100).max(1) - 1;
    sorted[rank].as_micros()
}

fn counter_snapshot() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering;
    (
        leindex::cli::mcp::request_meta::PROJECT_HYDRATIONS.load(Ordering::Relaxed),
        leindex::cli::mcp::request_meta::PDG_LOADS.load(Ordering::Relaxed),
        leindex::cli::mcp::request_meta::NEURAL_REQUESTS.load(Ordering::Relaxed),
    )
}

fn metric(
    samples: Vec<Duration>,
    before_rss: u64,
    after_rss: u64,
    before: (u64, u64, u64),
    after: (u64, u64, u64),
) -> PerfMetric {
    PerfMetric {
        p50_us: percentile(&samples, 50),
        p95_us: percentile(&samples, 95),
        max_us: samples
            .iter()
            .map(Duration::as_micros)
            .max()
            .unwrap_or_default(),
        rss_delta_bytes: after_rss.saturating_sub(before_rss),
        hydration_delta: after.0.saturating_sub(before.0),
        pdg_load_delta: after.1.saturating_sub(before.1),
        neural_request_delta: after.2.saturating_sub(before.2),
    }
}

fn enforce_limit(name: &str, actual_us: u128, limit_us: u128) {
    assert!(
        actual_us <= limit_us,
        "{name} p95 {actual_us}us exceeds {limit_us}us"
    );
}

fn bench_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn run_performance_gate() {
    let source_files = bench_env_usize("LEINDEX_PERF_SOURCE_FILES", 20_000);
    let ignored_files = bench_env_usize("LEINDEX_PERF_IGNORED_FILES", 20_000);
    let warm_samples = bench_env_usize("LEINDEX_PERF_WARM_SAMPLES", 100);
    let cold_samples = bench_env_usize("LEINDEX_PERF_COLD_SAMPLES", 10);

    let fixture = GitFixture::with_ignored(source_files, ignored_files);
    let project_path = fixture.project_path();
    let source_path = fixture.source_path();
    let runtime = Runtime::new().expect("create tokio runtime");
    let warm_registry = Arc::new(ProjectRegistry::new(2));
    runtime
        .block_on(warm_registry.index_project(Some(&project_path), true))
        .expect("build completed lexical/PDG generation");

    let mut metrics = BTreeMap::new();

    let before = counter_snapshot();
    let before_rss = current_rss_bytes();
    let mut warm_status = Vec::with_capacity(warm_samples);
    for _ in 0..warm_samples {
        let started = Instant::now();
        black_box(call(
            &runtime,
            &warm_registry,
            "leindex.git-status",
            json!({ "project_path": project_path }),
        ));
        warm_status.push(started.elapsed());
    }
    let warm_status_metric = metric(
        warm_status,
        before_rss,
        current_rss_bytes(),
        before,
        counter_snapshot(),
    );
    metrics.insert("git_status_warm".to_string(), warm_status_metric);

    let before = counter_snapshot();
    let before_rss = current_rss_bytes();
    let mut cold_status = Vec::with_capacity(cold_samples);
    for _ in 0..cold_samples {
        let cold_registry = Arc::new(ProjectRegistry::new(2));
        let started = Instant::now();
        black_box(call(
            &runtime,
            &cold_registry,
            "leindex.git-status",
            json!({ "project_path": project_path }),
        ));
        cold_status.push(started.elapsed());
    }
    let cold_status_metric = metric(
        cold_status,
        before_rss,
        current_rss_bytes(),
        before,
        counter_snapshot(),
    );
    metrics.insert("git_status_cold".to_string(), cold_status_metric);

    let before = counter_snapshot();
    let before_rss = current_rss_bytes();
    let mut exact = Vec::with_capacity(warm_samples);
    for _ in 0..warm_samples {
        let started = Instant::now();
        black_box(call(
            &runtime,
            &warm_registry,
            "leindex.grep-symbols",
            json!({ "project_path": project_path, "pattern": "benchmark_marker_19999", "mode": "exact" }),
        ));
        exact.push(started.elapsed());
    }
    let exact_metric = metric(
        exact,
        before_rss,
        current_rss_bytes(),
        before,
        counter_snapshot(),
    );
    metrics.insert("exact_symbol".to_string(), exact_metric);

    let before = counter_snapshot();
    let before_rss = current_rss_bytes();
    let mut text = Vec::with_capacity(warm_samples);
    for _ in 0..warm_samples {
        let started = Instant::now();
        black_box(call(
            &runtime,
            &warm_registry,
            "leindex.text-search",
            json!({ "project_path": project_path, "query": "MCP_BENCHMARK_MARKER", "scope": source_path, "max_results": 10, "allow_partial": false }),
        ));
        text.push(started.elapsed());
    }
    let text_metric = metric(
        text,
        before_rss,
        current_rss_bytes(),
        before,
        counter_snapshot(),
    );
    metrics.insert("exact_text".to_string(), text_metric);

    let before = counter_snapshot();
    let before_rss = current_rss_bytes();
    let mut semantic = Vec::with_capacity(warm_samples);
    for _ in 0..warm_samples {
        let started = Instant::now();
        black_box(call(
            &runtime,
            &warm_registry,
            "leindex.search",
            json!({ "project_path": project_path, "query": "benchmark marker", "top_k": 10, "search_mode": "semantic" }),
        ));
        semantic.push(started.elapsed());
    }
    let semantic_metric = metric(
        semantic,
        before_rss,
        current_rss_bytes(),
        before,
        counter_snapshot(),
    );
    metrics.insert("tfidf_semantic".to_string(), semantic_metric);

    let report = PerfReport {
        fixture_source_files: source_files,
        fixture_ignored_files: ignored_files,
        warm_samples,
        cold_samples,
        metrics,
    };
    let output = std::env::var("LEINDEX_PERF_OUTPUT")
        .unwrap_or_else(|_| "target/leindex-performance.json".to_string());
    let output_path = std::path::Path::new(&output);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).expect("create performance report directory");
    }
    fs::write(
        output_path,
        serde_json::to_vec_pretty(&report).expect("serialize report"),
    )
    .expect("write performance report");

    if std::env::var_os("LEINDEX_ENFORCE_PERF").is_some() {
        enforce_limit(
            "clean git status cold",
            report.metrics["git_status_cold"].p95_us,
            500_000,
        );
        enforce_limit(
            "clean git status warm",
            report.metrics["git_status_warm"].p95_us,
            150_000,
        );
        enforce_limit(
            "exact symbol",
            report.metrics["exact_symbol"].p95_us,
            100_000,
        );
        enforce_limit("exact text", report.metrics["exact_text"].p95_us, 100_000);
        enforce_limit(
            "TF-IDF semantic",
            report.metrics["tfidf_semantic"].p95_us,
            500_000,
        );
        assert_eq!(
            report.metrics["exact_symbol"].neural_request_delta, 0,
            "exact symbol path invoked neural embedding"
        );
        assert_eq!(
            report.metrics["exact_text"].neural_request_delta, 0,
            "exact text path invoked neural embedding"
        );
        for name in [
            "git_status_warm",
            "git_status_cold",
            "exact_symbol",
            "exact_text",
        ] {
            assert_eq!(
                report.metrics[name].hydration_delta, 0,
                "{name} unexpectedly hydrated a project"
            );
            assert_eq!(
                report.metrics[name].pdg_load_delta, 0,
                "{name} unexpectedly loaded PDG"
            );
            assert_eq!(
                report.metrics[name].neural_request_delta, 0,
                "{name} unexpectedly requested neural work"
            );
        }
        assert!(
            report.metrics["tfidf_semantic"].hydration_delta <= 1,
            "semantic path hydrated more than once"
        );
        assert_eq!(
            report.metrics["tfidf_semantic"].pdg_load_delta, 0,
            "semantic TF-IDF path unexpectedly loaded PDG"
        );
        assert!(
            report.metrics["exact_symbol"].rss_delta_bytes <= 150 * 1024 * 1024,
            "exact symbol RSS delta exceeds 150 MiB"
        );
    }

    println!(
        "LeIndex performance report: {} (status warm p95={}us, cold p95={}us)",
        output, report.metrics["git_status_warm"].p95_us, report.metrics["git_status_cold"].p95_us
    );
}

fn call(
    runtime: &Runtime,
    registry: &Arc<ProjectRegistry>,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let request: JsonRpcRequest = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool_name, "arguments": arguments }
    }))
    .expect("build tool request");
    runtime
        .block_on(handle_tool_call(registry, &all_tool_handlers(), &request))
        .expect("MCP tool response")
}

fn mcp_tool_latency(c: &mut Criterion) {
    if std::env::var_os("LEINDEX_ENFORCE_PERF").is_some() {
        run_performance_gate();
        return;
    }
    let fixture = GitFixture::new(1);
    let project_path = fixture.project_path();
    let source_path = fixture.source_path();
    let runtime = Runtime::new().expect("create tokio runtime");
    let mut memory =
        MemoryReportTracker::new(fixture.temp.path().join("mcp_tool_latency_rss.json"));
    memory.record_phase("before", current_rss_bytes(), 1);

    let mut group = c.benchmark_group("mcp_tool_latency");
    group.bench_function("git-status/cold", |b| {
        b.iter(|| {
            // Each cold sample owns a new project directory and therefore a
            // new in-project `.leindex` cache/storage directory.
            let cold_fixture = GitFixture::new(1);
            let registry = Arc::new(ProjectRegistry::new(2));
            black_box(call(
                &runtime,
                &registry,
                "leindex.git-status",
                json!({ "project_path": cold_fixture.project_path() }),
            ));
        });
    });

    let warm_registry = Arc::new(ProjectRegistry::new(2));
    black_box(call(
        &runtime,
        &warm_registry,
        "leindex.git-status",
        json!({ "project_path": project_path }),
    ));
    group.bench_function("git-status/warm", |b| {
        b.iter(|| {
            black_box(call(
                &runtime,
                &warm_registry,
                "leindex.git-status",
                json!({ "project_path": project_path }),
            ));
        });
    });

    group.bench_function("status-during-indexing", |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;

            for _ in 0..iterations {
                let indexing_fixture = GitFixture::new(256);
                let indexing_path = indexing_fixture.project_path();
                let indexing_registry = Arc::new(ProjectRegistry::new(2));

                // Prime the old index outside timing so git-status can read it
                // while the forced replacement index builds in the background.
                runtime
                    .block_on(indexing_registry.index_project(Some(&indexing_path), true))
                    .expect("prime indexing fixture");

                let (progress_tx, mut progress_rx) = mpsc::channel(2);
                let index_registry = Arc::clone(&indexing_registry);
                let index_path = indexing_path.clone();
                let index_task = runtime.spawn(async move {
                    index_with_progress(&index_registry, &index_path, true, progress_tx).await
                });

                // `consolidating` is emitted immediately before the registry
                // starts the forced index; wait without a timeout or cancellation.
                runtime.block_on(async {
                    loop {
                        match progress_rx.recv().await {
                            Some(event) if event.stage == "consolidating" => break,
                            Some(_) => {}
                            None => panic!("index job ended before consolidation"),
                        }
                    }
                });
                runtime.block_on(tokio::task::yield_now());
                assert!(
                    !index_task.is_finished(),
                    "index job ended before git-status began"
                );

                let status_started = Instant::now();
                let status = black_box(call(
                    &runtime,
                    &indexing_registry,
                    "leindex.git-status",
                    json!({ "project_path": indexing_path }),
                ));
                elapsed += status_started.elapsed();
                black_box(status);

                runtime
                    .block_on(index_task)
                    .expect("join index job")
                    .expect("complete forced index");
            }

            elapsed
        });
    });

    group.bench_function("grep-symbols/exact", |b| {
        b.iter(|| {
            black_box(call(
                &runtime,
                &warm_registry,
                "leindex.grep-symbols",
                json!({ "project_path": project_path, "pattern": "benchmark_marker", "mode": "exact" }),
            ));
        });
    });
    group.bench_function("file-summary", |b| {
        b.iter(|| {
            black_box(call(
                &runtime,
                &warm_registry,
                "leindex.file-summary",
                json!({ "project_path": project_path, "file_path": source_path }),
            ));
        });
    });
    group.bench_function("search/semantic", |b| {
        b.iter(|| {
            black_box(call(
                &runtime,
                &warm_registry,
                "leindex.search",
                json!({ "project_path": project_path, "query": "benchmark marker" }),
            ));
        });
    });
    group.finish();

    memory.record_phase("after", current_rss_bytes(), 1);
    let _ = memory.write_report();
}

criterion_group!(benches, mcp_tool_latency);
criterion_main!(benches);
