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
        let mut guard = handle.write().await;

        let diagnostics = guard.get_diagnostics().map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to get diagnostics: {}", e))
        })?;

        // Use is_stale_fast() for the boolean staleness check (fast: mtime +
        // count comparison). Only run the expensive check_freshness() (which
        // hashes ALL source files) when the index is actually stale, to
        // provide detailed changed/deleted file lists.
        let stale_fast = guard.is_stale_fast();
        let (changed, deleted) = if stale_fast {
            guard.check_freshness().unwrap_or_else(|_| (vec![], vec![]))
        } else {
            (vec![], vec![])
        };
        let storage_path = guard.storage_path().display().to_string();
        let db_size = std::fs::metadata(guard.storage_path().join("leindex.db"))
            .map(|m| m.len())
            .unwrap_or(0);
        let coverage = guard.coverage_report().ok();

        // Extract values from diagnostics before it's consumed by serde
        let indexed_files_ct = coverage
            .as_ref()
            .map(|c| c.indexed_files)
            .unwrap_or(diagnostics.stats.files_parsed);
        let symbol_count = diagnostics.stats.indexed_nodes;
        let memory_rss_mb =
            (diagnostics.memory_usage_bytes as f64 / 1024.0 / 1024.0 * 100.0).round() / 100.0;
        let size_mb = diagnostics.memory_usage_bytes as f64 / 1024.0 / 1024.0;
        let failed_parses = diagnostics.stats.failed_parses;
        let index_health = diagnostics.index_health.clone();
        let is_stale = !changed.is_empty() || !deleted.is_empty();
        // When is_stale_fast() reported stale, we ran check_freshness() which
        // is authoritative (hash-based). If check_freshness found no changes,
        // the is_stale_fast positive was a false positive (e.g., same-second
        // mtime) and the index is actually fresh.
        let stale_bool = if stale_fast { is_stale } else { false };
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

        if let Value::Object(ref mut map) = diag_json {
            map.insert("storage_path".to_string(), serde_json::json!(storage_path));
            map.insert("db_size_bytes".to_string(), serde_json::json!(db_size));
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
                map.insert(
                    "coverage".to_string(),
                    serde_json::to_value(cov).unwrap_or(Value::Null),
                );
            }
        }

        Ok(wrap_with_meta(diag_json, &guard))
    }
}
