#![cfg(feature = "cli")]

use leindex::cli::mcp::handlers::all_tool_handlers;
use leindex::cli::mcp::handlers::{GrepSymbolsHandler, ReadSymbolHandler};
use leindex::cli::mcp::protocol::JsonRpcRequest;
use leindex::cli::mcp::request_meta::{
    collect_request_timings, current_request_timing_sink, record_hydrate_ms, record_neural_ms,
    record_neural_ms_to, record_pdg_ms, reset_path_counters, PhaseTimings, WorkBudget,
    NEURAL_REQUESTS, PDG_LOADS, PROJECT_HYDRATIONS,
};
use leindex::cli::mcp::server::handle_tool_call;
use leindex::cli::ProjectRegistry;
use leindex::search::query_route::{classify, QueryRoute, RequestedMode};
use leindex::storage::nodes::NodeType;
use leindex::storage::{NodeRecord, NodeStore, Storage, UniqueProjectId};
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

#[test]
fn exact_route_classifier_is_deterministic() {
    assert_eq!(
        classify("Askpass::new", RequestedMode::Auto),
        QueryRoute::ExactSymbol
    );
    assert_eq!(
        classify("run_installation", RequestedMode::Auto),
        QueryRoute::ExactSymbol
    );
    assert_eq!(
        classify("registry_record", RequestedMode::Exact),
        QueryRoute::ExactText
    );
    assert_eq!(
        classify("how are sudo credentials propagated", RequestedMode::Auto),
        QueryRoute::Semantic
    );
    assert_eq!(
        classify("sudo credential flow", RequestedMode::Deep),
        QueryRoute::DeepPdg
    );
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
async fn exact_grep_must_not_request_neural_search() {
    let (_temp, project_path) = catalog_fixture();
    let registry = Arc::new(ProjectRegistry::new(2));

    reset_path_counters();
    let request = tool_request(
        "leindex.grep-symbols",
        json!({
            "project_path": project_path,
            "pattern": "Askpass",
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

#[tokio::test]
async fn git_status_does_not_hydrate_when_no_generation_is_resident() {
    let fixture = GitFixture::new();
    let counters = counters_after_call(
        "leindex.git-status",
        json!({ "project_path": fixture.project_path() }),
    )
    .await;

    assert_eq!(counters, (0, 0, 0));
}

fn catalog_fixture() -> (TempDir, std::path::PathBuf) {
    let temp = TempDir::new().expect("create catalog fixture");
    let project = temp.path().join("project");
    let source = project.join("src/lib.rs");
    fs::create_dir_all(source.parent().unwrap()).expect("create catalog source directory");
    fs::write(&source, "pub struct Askpass;\n").expect("write catalog source");
    fs::write(temp.path().join("outside.rs"), "pub struct Outside;\n")
        .expect("write outside fixture");

    let canonical_project = project.canonicalize().expect("canonical project");
    let canonical_source = source.canonicalize().expect("canonical source");
    let storage_dir = project.join(".leindex");
    fs::create_dir_all(&storage_dir).expect("create catalog storage directory");
    let db_path = storage_dir.join("leindex.db");
    let mut storage = Storage::open(&db_path).expect("open catalog storage");
    let metadata_id = UniqueProjectId::generate(&canonical_project, &[]);
    storage
        .store_project_metadata(&metadata_id, &canonical_project)
        .expect("store catalog metadata");
    let project_id = "project".to_string();
    NodeStore::new(&mut storage)
        .insert(&NodeRecord {
            id: None,
            project_id: project_id.clone(),
            file_path: canonical_source.display().to_string(),
            node_id: "src/lib.rs:Askpass".to_string(),
            symbol_name: "Askpass".to_string(),
            qualified_name: "Askpass".to_string(),
            language: "rust".to_string(),
            node_type: NodeType::Class,
            signature: None,
            complexity: Some(1),
            content_hash: "catalog-fixture".to_string(),
            embedding: None,
            byte_range_start: Some(0),
            byte_range_end: Some(19),
            embedding_format: None,
        })
        .expect("store catalog symbol");
    storage
        .conn()
        .execute(
            "INSERT INTO indexed_files (file_path, project_id, file_hash, last_indexed) VALUES (?1, ?2, ?3, 0)",
            rusqlite::params![
                canonical_source.display().to_string(),
                project_id,
                blake3::hash(&fs::read(&canonical_source).expect("read fixture source")).to_hex().to_string(),
            ],
        )
        .expect("store catalog file hash");

    (temp, canonical_project)
}

#[tokio::test]
async fn canonical_path_read_symbol_uses_the_same_catalog_key() {
    let (_temp, project) = catalog_fixture();
    let registry = Arc::new(ProjectRegistry::new(2));
    let absolute_path = project.join("src/lib.rs");
    reset_path_counters();

    for file_path in [
        absolute_path.display().to_string(),
        "src/lib.rs".to_string(),
        "src/../src/lib.rs".to_string(),
    ] {
        let response = ReadSymbolHandler
            .execute(
                &registry,
                json!({
                    "project_path": project,
                    "file_path": file_path,
                    "symbol": "Askpass",
                }),
            )
            .await
            .expect("catalog read-symbol response");
        assert_eq!(response["symbol"], "Askpass");
        assert_eq!(response["symbol_index_miss"], false);
        assert_eq!(response["source_freshness"], "live");
        assert_eq!(response["pdg_status"], "not_loaded");
    }
    assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn default_project_exact_catalog_grep_does_not_hydrate() {
    let (_temp, project) = catalog_fixture();
    let registry = Arc::new(ProjectRegistry::new(2));
    registry.set_default_path(project).await;
    reset_path_counters();

    let response = GrepSymbolsHandler
        .execute(
            &registry,
            json!({
                "pattern": "Askpass",
                "mode": "exact",
                "type_filter": "class",
                "scope": "src",
                "max_results": 1,
                "offset": 0,
            }),
        )
        .await
        .expect("default project exact catalog grep response");

    assert_eq!(response["results"][0]["name"], "Askpass");
    assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(PDG_LOADS.load(Ordering::Relaxed), 0);
    assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn canonical_path_read_symbol_rejects_files_outside_the_project() {
    let (_temp, project) = catalog_fixture();
    let registry = Arc::new(ProjectRegistry::new(2));
    let error = ReadSymbolHandler
        .execute(
            &registry,
            json!({
                "project_path": project,
                "file_path": "../outside.rs",
                "symbol": "Outside",
            }),
        )
        .await
        .expect_err("outside path must be rejected");
    assert!(error.to_string().contains("outside the project boundary"));
}

#[tokio::test]
async fn catalog_rows_cannot_escape_the_live_project_root() {
    let (_temp, project) = catalog_fixture();
    let outside = project.parent().unwrap().join("outside.rs");
    let db_path = project.join(".leindex/leindex.db");
    let storage = Storage::open(&db_path).expect("open catalog storage");
    let project_id: String = storage
        .conn()
        .query_row(
            "SELECT unique_project_id FROM project_metadata",
            [],
            |row| row.get(0),
        )
        .expect("catalog project id");
    storage
        .conn()
        .execute(
            "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, content_hash, byte_range_start, byte_range_end, created_at, updated_at) VALUES (?1, ?2, 'outside:Escape', 'Escape', 'Escape', 'rust', 'class', 'outside', 0, 19, 0, 0)",
            rusqlite::params![project_id, outside.display().to_string()],
        )
        .expect("store malicious catalog row");

    let error = ReadSymbolHandler
        .execute(
            &Arc::new(ProjectRegistry::new(2)),
            json!({ "project_path": project, "symbol": "Escape" }),
        )
        .await
        .expect_err("catalog path outside root must be rejected");
    assert!(error.to_string().contains("outside the project boundary"));
}

#[tokio::test]
async fn default_project_catalog_reads_do_not_hydrate() {
    let (_temp, project) = catalog_fixture();
    let registry = Arc::new(ProjectRegistry::new(2));
    registry.set_default_path(project).await;
    reset_path_counters();
    let response = ReadSymbolHandler
        .execute(
            &registry,
            json!({ "file_path": "src/lib.rs", "symbol": "Askpass" }),
        )
        .await
        .expect("default project catalog response");
    assert_eq!(response["symbol"], "Askpass");
    assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(PDG_LOADS.load(Ordering::Relaxed), 0);
    assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn stale_catalog_symbol_uses_live_parser_without_hydration() {
    let (_temp, project) = catalog_fixture();
    fs::write(
        project.join("src/lib.rs"),
        "pub struct Askpass;\npub fn live_marker() {}\n",
    )
    .expect("make catalog source stale");
    reset_path_counters();
    let response = ReadSymbolHandler
        .execute(
            &Arc::new(ProjectRegistry::new(2)),
            json!({
                "project_path": project,
                "file_path": "src/lib.rs",
                "symbol": "live_marker",
            }),
        )
        .await
        .expect("live parser response");
    assert_eq!(response["symbol"], "live_marker");
    assert_eq!(response["symbol_index_miss"], true);
    assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(PDG_LOADS.load(Ordering::Relaxed), 0);
    assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn stale_unscoped_catalog_symbol_uses_its_validated_candidate() {
    let (_temp, project) = catalog_fixture();
    fs::write(
        project.join("src/lib.rs"),
        "pub struct Askpass;\npub fn live_marker() {}\n",
    )
    .expect("make catalog source stale");
    reset_path_counters();
    let response = ReadSymbolHandler
        .execute(
            &Arc::new(ProjectRegistry::new(2)),
            json!({ "project_path": project, "symbol": "Askpass" }),
        )
        .await
        .expect("unscoped live parser response");
    assert_eq!(response["symbol"], "Askpass");
    assert_eq!(response["symbol_index_miss"], true);
    assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn exact_grep_catalog_miss_uses_live_parser_without_hydration() {
    let (_temp, project) = catalog_fixture();
    fs::write(
        project.join("src/lib.rs"),
        "pub struct Askpass;\npub fn live_marker() {}\n",
    )
    .expect("make catalog source stale");
    reset_path_counters();
    let response = GrepSymbolsHandler
        .execute(
            &Arc::new(ProjectRegistry::new(2)),
            json!({
                "project_path": project,
                "pattern": "live_marker",
                "mode": "exact",
            }),
        )
        .await
        .expect("live exact grep response");
    assert_eq!(response["results"][0]["name"], "live_marker");
    assert_eq!(PROJECT_HYDRATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(PDG_LOADS.load(Ordering::Relaxed), 0);
    assert_eq!(NEURAL_REQUESTS.load(Ordering::Relaxed), 0);
}
