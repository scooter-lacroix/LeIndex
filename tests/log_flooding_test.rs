//! Log-flooding regression tests.
//!
//! Covers the Log Flooding fix area (VAL-LOG-001..006):
//!
//! * VAL-LOG-001: duplicate node_id drops are logged at DEBUG, not WARN.
//! * VAL-LOG-002/004: the daemon (`src/bin/leindexd.rs`) defaults to WARN
//!   via `EnvFilter` and honors `RUST_LOG` (no static `with_max_level`).
//! * VAL-LOG-003: the embed worker defaults to WARN via its EnvFilter fallback.
//! * VAL-LOG-005: per-file "Read file once" messages are DEBUG, not INFO.
//! * VAL-LOG-006: indexing a duplicate-heavy project emits zero WARN output
//!   for the duplicate-drop path at the default (WARN) level.
//!
//! The daemon and embed worker are intentionally not started as long-running
//! services here (mission boundary); their log defaults are verified with
//! source-level assertions, which the mission AGENTS.md explicitly allows for
//! tracing-macro/log-level changes.

use std::path::Path;
use std::sync::{Arc, Mutex};

use leindex::search::{NodeInfo, SearchEngine};

/// Repo root directory (crate root, where `src/` lives).
fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("failed to read {rel}: {e}"))
}

#[test]
fn test_append_nodes_duplicate_drop_uses_debug() {
    // VAL-LOG-001: the duplicate-drop message must be emitted via
    // `tracing::debug!`, never `tracing::warn!`.
    let src = read_source("src/search/search/mod.rs");
    let marker = "append_nodes: dropped {} duplicate node_id(s)";
    let marker_idx = src
        .find(marker)
        .unwrap_or_else(|| panic!("append_nodes duplicate-drop message is missing from source"));
    // The macro invocation immediately preceding the message must be
    // `tracing::debug!`, never `tracing::warn!`.
    let just_before = &src[marker_idx.saturating_sub(120)..marker_idx];
    assert!(
        just_before.contains("tracing::debug!(") && !just_before.contains("tracing::warn!"),
        "append_nodes duplicate drop must use tracing::debug! (not warn!); context: {just_before:?}"
    );
}

#[test]
fn test_daemon_uses_env_filter_not_with_max_level() {
    // VAL-LOG-002 + VAL-LOG-004: init_logging must use EnvFilter (defaulting
    // to "warn") and must not use the static `with_max_level` ceiling.
    let src = read_source("src/bin/leindexd.rs");
    assert!(
        src.contains("EnvFilter::try_from_default_env()"),
        "daemon init_logging must read RUST_LOG via EnvFilter::try_from_default_env()"
    );
    assert!(
        src.contains("EnvFilter::new(\"warn\")"),
        "daemon EnvFilter fallback must default to WARN"
    );
    assert!(
        src.contains("with_env_filter"),
        "daemon must use with_env_filter"
    );
    assert!(
        !src.contains("with_max_level"),
        "daemon must not use the static with_max_level ceiling"
    );
}

#[test]
fn test_worker_env_filter_fallback_is_warn() {
    // VAL-LOG-003: the embed worker EnvFilter fallback must be "warn".
    let src = read_source("src/embed/worker_main.rs");
    assert!(
        src.contains("EnvFilter::new(\"warn\")"),
        "worker EnvFilter fallback must be \"warn\""
    );
    assert!(
        !src.contains("EnvFilter::new(\"info\")")
            && !src.contains("EnvFilter::new(\"informational\")"),
        "worker EnvFilter fallback must not be more verbose than WARN"
    );
}

#[test]
fn test_read_file_message_is_debug_not_info() {
    // VAL-LOG-005: per-file "Read file once" must be debug!, not info!.
    let src = read_source("src/cli/index_builder/mod.rs");
    let marker = "Read file once for hash and content";
    let marker_idx = src
        .find(marker)
        .unwrap_or_else(|| panic!("per-file read message is missing from source"));
    // The macro invocation immediately preceding the message must be `debug!`,
    // never `info!`.
    let just_before = &src[marker_idx.saturating_sub(120)..marker_idx];
    assert!(
        just_before.contains("debug!(") && !just_before.contains("info!("),
        "per-file 'Read file once' message must use debug! (not info!); context: {just_before:?}"
    );
}

/// A minimal MakeWriter that appends formatted events to a shared buffer.
#[derive(Clone)]
struct BufferWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufferWriter {
    type Writer = BufferWriter;
    fn make_writer(&'a self) -> Self::Writer {
        BufferWriter(self.0.clone())
    }
}

fn node(node_id: &str) -> NodeInfo {
    NodeInfo {
        node_id: node_id.to_string(),
        file_path: "src/lib.rs".to_string(),
        symbol_name: node_id.to_string(),
        language: "rust".to_string(),
        content: "fn sample() { let x = 1; }".to_string(),
        byte_range: (0, 40),
        tfidf_embedding: vec![],
        neural_embedding: None,
        complexity: 1,
        signature: None,
        pre_tokenized: None,
    }
}

#[test]
fn test_duplicate_append_is_warn_clean_at_default_level() {
    // VAL-LOG-006: indexing with duplicate node_ids must not emit WARN output
    // for the duplicate-drop path at the default (WARN) level.
    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(BufferWriter(buf.clone()))
        .with_ansi(false)
        .without_time()
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        let mut engine = SearchEngine::new();
        // First pass indexes the node; second pass re-supplies the same
        // node_id, exercising the duplicate-drop path in append_nodes.
        engine.index_nodes(vec![node("dup_node")]);
        engine.index_nodes(vec![node("dup_node")]);

        // Sanity: the duplicate really was dropped; only one node is kept.
        assert_eq!(engine.node_count(), 1);
    });

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        !output.contains("append_nodes")
            && !output.contains("duplicate")
            && !output.contains("dropped"),
        "duplicate-drop must not emit WARN output at default level; got:\n{output}"
    );
}

#[test]
fn test_warn_env_filter_fallback_filters_info() {
    // VAL-LOG-002/003/004: the `EnvFilter::new("warn")` fallback used by both
    // the daemon and the embed worker must let WARN+ through while filtering
    // INFO and DEBUG. This exercises the exact fallback string semantics.
    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufferWriter(buf.clone()))
        .with_ansi(false)
        .without_time()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        tracing::event!(tracing::Level::ERROR, "err-marker");
        tracing::event!(tracing::Level::WARN, "warn-marker");
        tracing::event!(tracing::Level::INFO, "info-marker");
        tracing::event!(tracing::Level::DEBUG, "debug-marker");
    });

    let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        output.contains("err-marker") && output.contains("warn-marker"),
        "warn filter must pass ERROR/WARN; got:\n{output}"
    );
    assert!(
        !output.contains("info-marker") && !output.contains("debug-marker"),
        "warn filter must suppress INFO/DEBUG; got:\n{output}"
    );
}
