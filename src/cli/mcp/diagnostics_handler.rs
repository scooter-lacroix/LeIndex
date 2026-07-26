use super::helpers::wrap_with_meta;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex [Diagnostics]
///
/// Returns diagnostic information about the indexed project.
#[derive(Clone)]
pub struct DiagnosticsHandler;

impl DiagnosticsHandler {
    /// Returns the name of this MCP tool (MCP-compliant: ASCII letters, digits, underscore, hyphen, dot only)
    pub fn name(&self) -> &str {
        "leindex.diagnostics"
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
        let handle = registry.get_or_create(project_path).await?;
        let guard = handle.read().await;

        let diagnostics = guard.get_diagnostics().map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to get diagnostics: {}", e))
        })?;

        // MCP diagnostics reads the persisted health snapshot and one live
        // Git status. It must not hash/stat every indexed file on the hot
        // response path; the CLI retains `is_stale_fast` for compatibility.
        let health = crate::cli::index_freshness::load_health(guard.storage_path());
        let stale_fast = health.as_ref().is_some_and(|health| {
            matches!(
                health.status,
                crate::cli::leindex::ComponentStatus::Stale
                    | crate::cli::leindex::ComponentStatus::Partial
                    | crate::cli::leindex::ComponentStatus::Failed
            )
        });
        let (changed, deleted) = crate::cli::git::status(guard.project_path())
            .ok()
            .map(|status| {
                let changed = status
                    .modified
                    .into_iter()
                    .chain(status.staged)
                    .chain(status.untracked)
                    .map(|path| guard.project_path().join(path))
                    .collect::<Vec<_>>();
                let deleted = status
                    .deleted
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>();
                (changed, deleted)
            })
            .unwrap_or_else(|| (Vec::new(), Vec::new()));
        let storage_path = guard.storage_path().display().to_string();
        let db_size = std::fs::metadata(guard.storage_path().join("leindex.db"))
            .map(|m| m.len())
            .unwrap_or(0);
        // Coverage is a persisted index-time fact here. Re-running the full
        // source inventory on every diagnostics call defeats the live fast
        // path and was a primary source of multi-second responses on large
        // worktrees. Git status above supplies the current delta.
        let coverage = health.as_ref().map(|snapshot| {
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
        });

        // Extract values from diagnostics before it's consumed by serde
        let indexed_files_ct = health
            .as_ref()
            .map(|snapshot| snapshot.indexed_file_count)
            .unwrap_or(diagnostics.stats.files_parsed);
        let symbol_count = diagnostics.stats.indexed_nodes;
        let memory_rss_mb =
            (diagnostics.memory_usage_bytes as f64 / 1024.0 / 1024.0 * 100.0).round() / 100.0;
        let size_mb = diagnostics.memory_usage_bytes as f64 / 1024.0 / 1024.0;
        let failed_parses = diagnostics.stats.failed_parses;
        let index_health = diagnostics.index_health.clone();
        let is_stale = stale_fast || !changed.is_empty() || !deleted.is_empty();
        // When is_stale_fast() reported stale, we ran check_freshness() which
        // is authoritative (hash-based). If check_freshness found no changes,
        // the is_stale_fast positive was a false positive (e.g., same-second
        // mtime) and the index is actually fresh.
        let stale_bool = is_stale;
        // Live PDG counts from the in-memory graph (pdg.node_count() /
        // pdg.edge_count()). These reflect the current state of the loaded
        // PDG and may differ from the index-time snapshot in stats.pdg_nodes
        // / stats.pdg_edges if the PDG was partially loaded or modified.
        let pdg_nodes = diagnostics.pdg_nodes;
        let pdg_edges = diagnostics.pdg_edges;
        let embedding_model = diagnostics.embedding_model.clone();
        let pdg_loaded = diagnostics.pdg_loaded;
        let search_index_nodes = diagnostics.search_index_nodes;
        let total_signatures = diagnostics.stats.total_signatures;
        let indexed_nodes = diagnostics.stats.indexed_nodes;
        let files_parsed = diagnostics.stats.files_parsed;
        let indexing_time_ms = diagnostics.stats.indexing_time_ms;

        let mut diag_json = serde_json::to_value(diagnostics)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?;

        // ORT diagnostics: share the exact same collection used by the
        // `leindex diagnostics` CLI so MCP output has parity (ort_path,
        // ort_version, execution_provider).
        let (ort_path, ort_version, execution_provider) =
            crate::cli::cli::collect_ort_diagnostics();

        if let Value::Object(ref mut map) = diag_json {
            map.insert("storage_path".to_string(), serde_json::json!(storage_path));
            map.insert("db_size_bytes".to_string(), serde_json::json!(db_size));
            map.insert("ort_path".to_string(), serde_json::json!(ort_path));
            map.insert("ort_version".to_string(), serde_json::json!(ort_version));
            map.insert(
                "execution_provider".to_string(),
                serde_json::json!(execution_provider),
            );
            map.insert(
                "memory_rss_mb".to_string(),
                serde_json::json!(memory_rss_mb),
            );

            // Flat fields expected by trim_diagnostics / render_diagnostics
            map.insert(
                "indexed_files".to_string(),
                serde_json::json!(indexed_files_ct),
            );
            map.insert("symbol_count".to_string(), serde_json::json!(symbol_count));
            map.insert("index_size_mb".to_string(), serde_json::json!(size_mb));
            map.insert("stale".to_string(), serde_json::json!(stale_bool));

            // System health metrics: index freshness, live PDG node/edge
            // counts (from the in-memory graph), embedding model status,
            // search index size. Note: pdg_nodes/pdg_edges here are live
            // counts from the loaded PDG, while the same fields under
            // `stats` are index-time snapshots persisted to storage.
            map.insert(
                "system_health".to_string(),
                serde_json::json!({
                    "index_health": index_health,
                    "pdg_loaded": pdg_loaded,
                    "pdg_nodes": pdg_nodes,
                    "pdg_edges": pdg_edges,
                    "search_index_nodes": search_index_nodes,
                    "embedding_model": embedding_model,
                    "total_signatures": total_signatures,
                    "indexed_nodes": indexed_nodes,
                    "files_parsed": files_parsed,
                    "failed_parses": failed_parses,
                    "indexing_time_ms": indexing_time_ms,
                }),
            );

            // last_indexed_secs_ago: rough estimate from storage_path mtime
            let lm = std::fs::metadata(guard.storage_path().join("leindex.db"))
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

            // issues: collect any non-empty warning indicators
            let mut issues: Vec<Value> = Vec::new();
            if failed_parses > 0 {
                issues.push(serde_json::json!({
                    "severity": "warning",
                    "message": format!("{} files failed to parse", failed_parses),
                }));
            }
            if stale_bool {
                issues.push(serde_json::json!({
                    "severity": "warning",
                    "message": "Index may be stale. Call LeIndex [Index] with force_reindex=true for fresh results.",
                }));
            }
            map.insert("issues".to_string(), serde_json::json!(issues));

            let staleness = if !stale_bool {
                serde_json::json!({
                    "status": "fresh",
                    "changed_files": 0,
                    "deleted_files": 0,
                })
            } else {
                serde_json::json!({
                    "status": "stale",
                    "changed_files": changed.len(),
                    "deleted_files": deleted.len(),
                    "changed_sample": changed.iter().take(10).map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "deleted_sample": deleted.iter().take(10).cloned().collect::<Vec<_>>(),
                    "suggestion": "Call LeIndex [Index] with force_reindex=true to refresh",
                })
            };
            map.insert("freshness".to_string(), staleness);
            if let Some(cov) = coverage {
                map.insert("coverage".to_string(), cov);
            }
        }

        Ok(wrap_with_meta(diag_json, &guard))
    }
}
