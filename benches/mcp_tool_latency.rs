use criterion::{black_box, criterion_group, criterion_main, Criterion};
use leindex::cli::mcp::handlers::all_tool_handlers;
use leindex::cli::mcp::protocol::JsonRpcRequest;
use leindex::cli::mcp::server::{handle_tool_call, index_with_progress};
use leindex::cli::memory_report::{current_rss_bytes, MemoryReportTracker};
use leindex::cli::ProjectRegistry;
use serde_json::{json, Value};
use std::fs;
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
        let temp = TempDir::new().expect("create git fixture");
        fs::create_dir_all(temp.path().join("src")).expect("create source directory");
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn benchmark_marker() -> &'static str { \"MCP_BENCHMARK_MARKER\" }\n",
        )
        .expect("write source fixture");
        for source_file in 1..source_files {
            fs::write(
                temp.path().join(format!("src/module_{source_file}.rs")),
                format!("pub fn benchmark_marker_{source_file}() -> usize {{ {source_file} }}\n"),
            )
            .expect("write indexing fixture source");
        }

        for args in [
            ["init"].as_slice(),
            ["config", "user.email", "leindex@example.test"].as_slice(),
            ["config", "user.name", "LeIndex Benchmark"].as_slice(),
            ["add", "src"].as_slice(),
            ["commit", "-m", "fixture"].as_slice(),
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(temp.path())
                .output()
                .expect("run git for fixture");
            assert!(output.status.success(), "git {:?} failed", args);
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
