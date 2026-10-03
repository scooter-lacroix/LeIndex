use super::helpers::wrap_with_meta;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex \[Diagnostics\]
///
/// Returns diagnostic information about the indexed project.
#[derive(Clone)]
pub struct DiagnosticsHandler;

impl DiagnosticsHandler {
    /// Returns the name of this MCP tool (MCP-compliant: ASCII letters, digits, underscore, hyphen, dot only)
    pub fn name(&self) -> &str {
        "leindex_diagnostics"
    }

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "LeIndex [Diagnostics]"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Get diagnostic information about the indexed project, including memory usage, index statistics, and system health."
    }

    /// Returns the JSON schema for the arguments of this RPC method
    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Project directory (omit to use current project)"
                }
            },
            "required": []
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        // Use get_or_load (no auto-index): diagnostics must report the current
        // state even when indexing itself is failing, otherwise a failed
        // persist phase turns the tool into an error instead of a status
        // report.
        let handle = registry.get_or_load(project_path).await?;
        let guard = handle.read().await;

        let diagnostics = live_diagnostics(&guard);

        // MCP diagnostics reads the persisted health snapshot and one live
        // Git status. It must not hash/stat every indexed file on the hot
        // response path; the CLI retains `is_stale_fast` for compatibility.
        let health = crate::cli::index_freshness::load_health(guard.storage_path());
        // Staleness reflects whether the persisted index health says the index
        // is degraded (stale/partial/failed) — the authoritative post-index
        // state. Git dirt must NOT drive this flag: the indexer indexes the
        // working tree, so files modified-vs-HEAD are fully indexed, and
        // counting them as "stale" produced the self-contradiction of
        // "Stale: true" next to "Freshness: status=fresh" in one payload. The
        // git-dirty lists remain available as informational metadata
        // (`uncommitted_git_files`).
        let stale_bool = health.as_ref().is_some_and(|health| {
            matches!(
                health.status,
                crate::cli::leindex::ComponentStatus::Stale
                    | crate::cli::leindex::ComponentStatus::Partial
                    | crate::cli::leindex::ComponentStatus::Failed
            )
        });
        let (changed, deleted) = git_delta(&guard);
        let storage_path = guard.storage_path().display().to_string();
        let db_size = std::fs::metadata(guard.storage_path().join("leindex.db"))
            .map(|m| m.len())
            .unwrap_or(0);
        let coverage = coverage_json(health.as_ref());

        // Extract values from diagnostics before it's consumed by serde.
        // Every live value falls back to the persisted health snapshot (or a
        // safe zero) so a degraded diagnostics path still returns partial data.
        let values = diag_values(health.as_ref(), diagnostics.as_ref());

        // Real process RSS, not the index-size estimate. Both this and
        // `index_size_mb` previously derived from `memory_usage_bytes` (an
        // index-heap estimate), so "Memory RSS" always equaled "Index size"
        // exactly — a copy bug, not a measurement.
        let process_rss_bytes = crate::cli::memory_report::current_rss_bytes();
        let memory_rss_mb = (process_rss_bytes as f64 / 1024.0 / 1024.0 * 100.0).round() / 100.0;
        // "Index size" must mean DISK, or readers assume a bug: after the
        // heap estimate was made honest it legitimately tracks RSS, and the
        // audit flagged the identical values as a conflation. Report the
        // real on-disk store size, and keep the estimate under its own name.
        let store_disk_bytes = storage_dir_size_bytes(std::path::Path::new(&storage_path));
        let size_mb = (store_disk_bytes as f64 / 1024.0 / 1024.0 * 100.0).round() / 100.0;
        let heap_estimate_mb =
            (values.memory_usage_bytes as f64 / 1024.0 / 1024.0 * 100.0).round() / 100.0;

        let mut diag_json = match diagnostics {
            Some(diagnostics) => serde_json::to_value(diagnostics)
                .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?,
            None => serde_json::json!({}),
        };

        // ORT diagnostics: share the exact same collection used by the
        // `leindex diagnostics` CLI so MCP output has parity (ort_path,
        // ort_version, execution_provider, execution_provider_active).
        let (ort_path, ort_version, execution_provider, execution_provider_active) =
            crate::cli::cli::collect_ort_diagnostics();

        if let Value::Object(ref mut map) = diag_json {
            insert_ort_fields(
                map,
                &storage_path,
                db_size,
                (
                    ort_path,
                    ort_version,
                    execution_provider,
                    execution_provider_active,
                ),
            );
            map.insert(
                "memory_rss_mb".to_string(),
                serde_json::json!(memory_rss_mb),
            );
            map.insert("engram".to_string(), engram_json());

            // Flat fields expected by trim_diagnostics / render_diagnostics
            map.insert(
                "indexed_files".to_string(),
                serde_json::json!(values.indexed_files),
            );
            map.insert(
                "symbol_count".to_string(),
                serde_json::json!(values.symbol_count),
            );
            map.insert("index_size_mb".to_string(), serde_json::json!(size_mb));
            map.insert(
                "index_heap_estimate_mb".to_string(),
                serde_json::json!(heap_estimate_mb),
            );
            map.insert("stale".to_string(), serde_json::json!(stale_bool));

            insert_system_health(map, &values, guard.storage_path());

            map.insert(
                "issues".to_string(),
                serde_json::json!(issues_json(&values, stale_bool)),
            );

            map.insert(
                "freshness".to_string(),
                staleness_json(stale_bool, &changed, &deleted),
            );
            if let Some(cov) = coverage {
                map.insert("coverage".to_string(), cov);
            }
        }

        Ok(wrap_with_meta(diag_json, &guard))
    }
}

/// Live diagnostics are best-effort; a failure degrades to the persisted
/// health snapshot rather than a null/error response.
fn live_diagnostics(
    index: &crate::cli::leindex::LeIndex,
) -> Option<crate::cli::leindex::Diagnostics> {
    match index.get_diagnostics() {
        Ok(diagnostics) => Some(diagnostics),
        Err(error) => {
            tracing::warn!(
                project = %index.project_path().display(),
                "Live diagnostics unavailable; returning persisted health only: {error}"
            );
            None
        }
    }
}

/// Working-tree files that differ from git HEAD: absolute paths for changed
/// files, display strings for deleted ones. Absent git or a failing probe
/// degrades to empty lists.
fn git_delta(index: &crate::cli::leindex::LeIndex) -> (Vec<std::path::PathBuf>, Vec<String>) {
    crate::cli::git::status(index.project_path())
        .ok()
        .map(|status| {
            let changed = status
                .modified
                .into_iter()
                .chain(status.staged)
                .chain(status.untracked)
                .map(|path| index.project_path().join(path))
                .collect::<Vec<_>>();
            let deleted = status
                .deleted
                .into_iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            (changed, deleted)
        })
        .unwrap_or_else(|| (Vec::new(), Vec::new()))
}

/// Coverage is a persisted index-time fact here. Re-running the full source
/// inventory on every diagnostics call defeats the live fast path and was a
/// primary source of multi-second responses on large worktrees. Git status
/// supplies the current delta.
fn coverage_json(health: Option<&crate::cli::leindex::IndexHealth>) -> Option<Value> {
    health.map(|snapshot| {
        let total = snapshot
            .indexed_file_count
            .saturating_add(snapshot.changed_unindexed_count);
        serde_json::json!({
            "total_source_files": total,
            "indexed_files": snapshot.indexed_file_count,
            "missing_files": [],
            "orphaned_entries": [],
            "coverage_pct": if total == 0 { 100.0 } else {
                snapshot.indexed_file_count as f64 / total as f64 * 100.0
            },
            "source": "persisted_health",
        })
    })
}

/// Values lifted from the (optional) live diagnostics before it is consumed by
/// serde. Every live value falls back to the persisted health snapshot (or a
/// safe zero) so a degraded diagnostics path still returns partial data.
struct DiagValues {
    /// `health.indexed_file_count`, else `stats.files_parsed`, else 0.
    indexed_files: usize,
    /// Index-time node count under `symbol_count`.
    symbol_count: usize,
    /// Index-heap estimate (not process RSS).
    memory_usage_bytes: usize,
    /// Files that failed to parse during the last run.
    failed_parses: usize,
    /// `healthy`, `stale`, `empty`, or `unknown` without live diagnostics.
    index_health: String,
    /// Live PDG node count from the in-memory graph.
    pdg_nodes: usize,
    /// Live PDG edge count from the in-memory graph.
    pdg_edges: usize,
    /// Embedding model status.
    embedding_model: String,
    /// Whether a PDG is resident.
    pdg_loaded: bool,
    /// Number of nodes in the search engine index.
    search_index_nodes: usize,
    /// Signatures extracted in the last run.
    total_signatures: usize,
    /// Whether `total_signatures` covers the whole project or just the delta.
    signature_scope: String,
    /// Index-time persisted node count.
    indexed_nodes: usize,
    /// Files encountered during the last run.
    files_parsed: usize,
    /// Duration of the last run.
    indexing_time_ms: u64,
    /// Whether the live diagnostics read succeeded at all.
    available: bool,
}

/// Extract every field the response reports from the two optional snapshots.
fn diag_values(
    health: Option<&crate::cli::leindex::IndexHealth>,
    diagnostics: Option<&crate::cli::leindex::Diagnostics>,
) -> DiagValues {
    let stats = diagnostics.map(|d| &d.stats);
    DiagValues {
        indexed_files: health
            .map(|snapshot| snapshot.indexed_file_count)
            .or_else(|| stats.map(|s| s.files_parsed))
            .unwrap_or(0),
        symbol_count: stats.map(|s| s.indexed_nodes).unwrap_or(0),
        memory_usage_bytes: diagnostics.map(|d| d.memory_usage_bytes).unwrap_or(0),
        failed_parses: stats.map(|s| s.failed_parses).unwrap_or(0),
        index_health: diagnostics
            .map(|d| d.index_health.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        pdg_nodes: diagnostics.map(|d| d.pdg_nodes).unwrap_or(0),
        pdg_edges: diagnostics.map(|d| d.pdg_edges).unwrap_or(0),
        embedding_model: diagnostics
            .map(|d| d.embedding_model.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        pdg_loaded: diagnostics.map(|d| d.pdg_loaded).unwrap_or(false),
        search_index_nodes: diagnostics.map(|d| d.search_index_nodes).unwrap_or(0),
        total_signatures: stats.map(|s| s.total_signatures).unwrap_or(0),
        signature_scope: stats
            .map(|s| s.signature_scope.clone())
            .unwrap_or_else(|| "full".to_string()),
        indexed_nodes: stats.map(|s| s.indexed_nodes).unwrap_or(0),
        files_parsed: stats.map(|s| s.files_parsed).unwrap_or(0),
        indexing_time_ms: stats.map(|s| s.indexing_time_ms).unwrap_or(0),
        available: diagnostics.is_some(),
    }
}

/// Engram query phrase-book counters plus the global embed-cache counters
/// (index-time neural reuse), so cache effectiveness is visible in one place.
/// Counters are per process.
fn engram_json() -> Value {
    let engram = serde_json::to_value(crate::search::engram::global_stats())
        .unwrap_or_else(|_| serde_json::json!({}));
    #[cfg(feature = "onnx")]
    let engram = {
        let mut engram = engram;
        if let Value::Object(ref mut engram_map) = engram {
            let (hits, misses) = crate::search::onnx::embed_cache_frontend::counters();
            engram_map.insert(
                "embed_cache".to_string(),
                serde_json::json!({ "hits": hits, "misses": misses }),
            );
        }
        engram
    };
    engram
}

/// Storage identification plus ORT diagnostics: shared with the
/// `leindex diagnostics` CLI so MCP output has parity (ort_path, ort_version,
/// execution_provider, execution_provider_active).
fn insert_ort_fields(
    map: &mut serde_json::Map<String, Value>,
    storage_path: &str,
    db_size: u64,
    ort: (Option<String>, Option<String>, String, Option<String>),
) {
    let (ort_path, ort_version, execution_provider, execution_provider_active) = ort;
    map.insert("storage_path".to_string(), serde_json::json!(storage_path));
    map.insert("db_size_bytes".to_string(), serde_json::json!(db_size));
    map.insert("ort_path".to_string(), serde_json::json!(ort_path));
    map.insert("ort_version".to_string(), serde_json::json!(ort_version));
    map.insert(
        "execution_provider".to_string(),
        serde_json::json!(execution_provider),
    );
    // The provider the embed worker actually activated (live daemon health
    // probe); `None` when no worker is running yet. Differs from
    // `execution_provider` when a requested GPU provider failed to load and
    // the worker fell back to CPU.
    map.insert(
        "execution_provider_active".to_string(),
        serde_json::json!(execution_provider_active),
    );
}

/// System health metrics: index freshness, live PDG node/edge counts (from the
/// in-memory graph), embedding model status, search index size. Note:
/// pdg_nodes/pdg_edges here are live counts from the loaded PDG, while the
/// same fields under `stats` are index-time snapshots persisted to storage.
/// Also reports `last_indexed_secs_ago`, a rough estimate from the storage
/// mtime.
fn insert_system_health(
    map: &mut serde_json::Map<String, Value>,
    values: &DiagValues,
    storage_path: &std::path::Path,
) {
    map.insert(
        "system_health".to_string(),
        serde_json::json!({
            "index_health": values.index_health,
            "pdg_loaded": values.pdg_loaded,
            "pdg_nodes": values.pdg_nodes,
            "pdg_edges": values.pdg_edges,
            "search_index_nodes": values.search_index_nodes,
            "embedding_model": values.embedding_model,
            "total_signatures": values.total_signatures,
            "signature_scope": values.signature_scope,
            "indexed_nodes": values.indexed_nodes,
            "files_parsed": values.files_parsed,
            "failed_parses": values.failed_parses,
            "indexing_time_ms": values.indexing_time_ms,
        }),
    );

    let lm = std::fs::metadata(storage_path.join("leindex.db"))
        .and_then(|m| m.modified())
        .ok();
    let secs_ago = lm.and_then(|t| {
        std::time::SystemTime::now()
            .duration_since(t)
            .ok()
            .map(|d| d.as_secs())
    });
    map.insert(
        "last_indexed_secs_ago".to_string(),
        serde_json::json!(secs_ago),
    );
}

/// Issues: collect any non-empty warning indicators.
fn issues_json(values: &DiagValues, stale_bool: bool) -> Vec<Value> {
    let mut issues: Vec<Value> = Vec::new();
    if values.failed_parses > 0 {
        issues.push(serde_json::json!({
            "severity": "warning",
            "message": format!("{} files failed to parse", values.failed_parses),
        }));
    }
    if !values.available {
        issues.push(serde_json::json!({
            "severity": "warning",
            "message": "Live diagnostics unavailable — showing persisted health snapshot only. Reindex (LeIndex [Index] with force_reindex=true) to restore full diagnostics.",
        }));
    }
    if stale_bool {
        issues.push(serde_json::json!({
            "severity": "warning",
            "message": "Index may be stale. Call leindex_manage action=index with force_reindex=true for fresh results.",
        }));
    }
    issues
}

/// Freshness block. Staleness reflects whether the persisted index health says
/// the index is degraded (stale/partial/failed) — the authoritative
/// post-index state. Git dirt must NOT drive this flag: the indexer indexes
/// the working tree, so files modified-vs-HEAD are fully indexed, and counting
/// them as "stale" produced the self-contradiction of "Stale: true" next to
/// "Freshness: status=fresh" in one payload. The git-dirty lists remain
/// available as informational metadata (`uncommitted_git_files`).
fn staleness_json(stale_bool: bool, changed: &[std::path::PathBuf], deleted: &[String]) -> Value {
    if !stale_bool {
        serde_json::json!({
            "status": "fresh",
            "changed_files": 0,
            "deleted_files": 0,
            // Informational: working-tree files that differ from the git HEAD
            // commit. They are indexed (the indexer reads the working tree);
            // the count is surfaced so agents can tell "dirty worktree" apart
            // from "stale index".
            "uncommitted_git_files": changed.len() + deleted.len(),
        })
    } else {
        serde_json::json!({
            "status": "stale",
            "changed_files": changed.len(),
            "deleted_files": deleted.len(),
            "changed_sample": changed.iter().take(10).map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "deleted_sample": deleted.iter().take(10).cloned().collect::<Vec<_>>(),
            "suggestion": "Call leindex_manage action=index with force_reindex=true to refresh",
        })
    }
}

/// On-disk size (bytes) of the project's `.leindex` store — the number a
/// reader expects under "Index size". Bounded walk of the store directory
/// (index artifacts, snapshots, generations); missing paths read as 0.
fn storage_dir_size_bytes(root: &std::path::Path) -> u64 {
    fn dir_size(dir: &std::path::Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut total = 0u64;
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                total += dir_size(&entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        total
    }
    dir_size(root)
}
