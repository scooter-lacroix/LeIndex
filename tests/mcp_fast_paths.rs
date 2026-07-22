#![cfg(feature = "cli")]

use leindex::cli::mcp::handlers::all_tool_handlers;
use leindex::cli::mcp::protocol::JsonRpcRequest;
use leindex::cli::mcp::request_meta::{
    collect_request_timings, current_request_timing_sink, record_hydrate_ms, record_neural_ms,
    record_neural_ms_to, record_pdg_ms, reset_path_counters, PhaseTimings, WorkBudget,
    NEURAL_REQUESTS, PDG_LOADS, PROJECT_HYDRATIONS,
};
use leindex::cli::mcp::server::handle_tool_call;
use leindex::cli::ProjectRegistry;
use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

struct GitFixture {
    temp: TempDir,
}

impl GitFixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("create git fixture");
        fs::create_dir_all(temp.path().join("src")).expect("create source directory");
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn fast_path_marker() -> &'static str { \"FAST_PATH_MARKER\" }\n",
        )
        .expect("write source fixture");

        for args in [
            ["init"].as_slice(),
            ["config", "user.email", "leindex@example.test"].as_slice(),
            ["config", "user.name", "LeIndex Test"].as_slice(),
            ["add", "src/lib.rs"].as_slice(),
            ["commit", "-m", "fixture"].as_slice(),
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(temp.path())
                .output()
                .expect("run git for fixture");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
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

fn tool_request(tool_name: &str, arguments: Value) -> JsonRpcRequest {
    serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool_name, "arguments": arguments }
    }))
    .expect("build tool request")
}

async fn counters_after_call(tool_name: &str, arguments: Value) -> (u64, u64, u64) {
    reset_path_counters();
    let registry = Arc::new(ProjectRegistry::new(2));
    let request = tool_request(tool_name, arguments);
    let result = handle_tool_call(&registry, &all_tool_handlers(), &request).await;
    assert!(
        result.is_ok(),
        "{tool_name} must return an MCP response: {result:?}"
    );

    (
        PROJECT_HYDRATIONS.load(Ordering::Relaxed),
        PDG_LOADS.load(Ordering::Relaxed),
        NEURAL_REQUESTS.load(Ordering::Relaxed),
    )
}

async fn current_fast_path_counters(fixture: &GitFixture) -> Vec<(&'static str, (u64, u64, u64))> {
    let project_path = fixture.project_path();
    vec![
        (
            "git-status",
            counters_after_call(
                "leindex.git-status",
                json!({ "project_path": project_path }),
            )
            .await,
        ),
        (
            "read-file",
            counters_after_call(
                "leindex.read-file",
                json!({
                    "project_path": project_path,
                    "file_path": fixture.source_path(),
                    "include_symbol_map": false,
                }),
            )
            .await,
        ),
        (
            "exact-text-search",
            counters_after_call(
                "leindex.text-search",
                json!({
                    "project_path": project_path,
                    "query": "FAST_PATH_MARKER",
                    "case_sensitive": true,
                    "context_lines": 0,
                }),
            )
            .await,
        ),
    ]
}

#[test]
fn path_metadata_primitives_are_deterministic() {
    PROJECT_HYDRATIONS.store(7, Ordering::Relaxed);
    PDG_LOADS.store(11, Ordering::Relaxed);
    NEURAL_REQUESTS.store(13, Ordering::Relaxed);
    reset_path_counters();
    assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(PDG_LOADS.load(Ordering::Relaxed), 0);
    assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);

    assert!(WorkBudget {
        max_latency_ms: 0,
        allow_partial: true,
    }
    .elapsed(Instant::now()));
    assert!(!WorkBudget {
        max_latency_ms: u64::MAX,
        allow_partial: true,
    }
    .elapsed(Instant::now()));
    assert!(!WorkBudget {
        max_latency_ms: 0,
        allow_partial: false,
    }
    .elapsed(Instant::now()));

    let metadata = serde_json::to_value(PhaseTimings {
        handler_ms: 4,
        transport_queue_ms: 2,
        total_ms: 6,
        ..PhaseTimings::default()
    })
    .expect("serialize phase metadata");
    assert_eq!(metadata["handler_ms"], 4);
    assert_eq!(metadata["transport_queue_ms"], 2);
    assert_eq!(metadata["total_ms"], 6);
    assert_eq!(metadata["hydrate_ms"], json!(0));
    assert_eq!(metadata["neural_ms"], json!(0));
}

#[tokio::test]
async fn request_timing_collector_keeps_causal_phase_measurements() {
    let (_, timings) = collect_request_timings(async {
        record_hydrate_ms(7);
        record_pdg_ms(11);
        record_neural_ms(13);
    })
    .await;

    assert_eq!(timings.hydrate_ms, 7);
    assert_eq!(timings.pdg_ms, 11);
    assert_eq!(timings.neural_ms, 13);
}

#[tokio::test]
async fn request_timing_collector_accepts_neural_worker_measurements() {
    let (_, timings) = collect_request_timings(async {
        let timing_sink = current_request_timing_sink().expect("request timing sink");
        std::thread::spawn(move || record_neural_ms_to(&timing_sink, 17))
            .join()
            .expect("timing worker");
    })
    .await;

    assert_eq!(timings.neural_ms, 17);
}

#[tokio::test]
// Task 2/3 unblock: route fast paths before hydration and supply an indexed no-neural fixture.
#[ignore = "requires Task 2/3 unblock"]
async fn current_fast_paths_hydrate_the_project_before_serving_live_data() {
    let fixture = GitFixture::new();

    for (tool, (hydrations, pdg_loads, neural_requests)) in
        current_fast_path_counters(&fixture).await
    {
        assert!(
            hydrations > 0,
            "{tool} currently hydrates a project: {hydrations}"
        );
        assert_eq!(pdg_loads, 0, "{tool} fixture has no persisted PDG to load");
        assert_eq!(neural_requests, 0, "{tool} must not need neural search");
    }
}

#[tokio::test]
// Task 2/3 unblock: route fast paths before hydration and supply an indexed no-neural fixture.
#[ignore = "requires Task 2/3 unblock"]
async fn live_fast_paths_must_not_hydrate_or_load_search_dependencies() {
    let fixture = GitFixture::new();
    let observed = current_fast_path_counters(&fixture).await;

    assert_eq!(
        observed,
        vec![
            ("git-status", (0, 0, 0)),
            ("read-file", (0, 0, 0)),
            ("exact-text-search", (0, 0, 0)),
        ],
        "Task 2 must move these live paths ahead of ProjectRegistry::get_or_create"
    );
}

#[tokio::test]
// Task 2/3 unblock: route exact grep before search setup and supply an indexed no-neural fixture.
#[ignore = "requires Task 2/3 unblock"]
async fn exact_grep_must_not_request_neural_search() {
    let fixture = GitFixture::new();
    let project_path = fixture.project_path();
    let mut index = leindex::cli::leindex::LeIndex::new(fixture.temp.path()).expect("create index");
    index.index_project(true).expect("index exact-grep fixture");
    let registry = Arc::new(ProjectRegistry::with_initial_project(2, index));

    reset_path_counters();
    let request = tool_request(
        "leindex.grep-symbols",
        json!({
            "project_path": project_path,
            "pattern": "fast_path_marker",
            "mode": "exact",
        }),
    );
    let result = handle_tool_call(&registry, &all_tool_handlers(), &request).await;
    assert!(
        result.is_ok(),
        "exact grep must return an MCP response: {result:?}"
    );
    assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(PDG_LOADS.load(Ordering::Relaxed), 0);
    assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);
}
