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

        let (changed, deleted) = guard.check_freshness().unwrap_or_else(|_| (vec![], vec![]));
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
        let size_mb = diagnostics.memory_usage_bytes as f64 / 1024.0 / 1024.0;
        let failed_parses = diagnostics.stats.failed_parses;
        let index_health = diagnostics.index_health.clone();
        let is_stale = !changed.is_empty() || !deleted.is_empty();
        let stale_bool = is_stale || index_health == "stale" || failed_parses > 0;

        let mut diag_json = serde_json::to_value(diagnostics)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?;

        if let Value::Object(ref mut map) = diag_json {
            map.insert("storage_path".to_string(), serde_json::json!(storage_path));
            map.insert("db_size_bytes".to_string(), serde_json::json!(db_size));

            // Flat fields expected by trim_diagnostics / render_diagnostics
            map.insert("indexed_files".to_string(), serde_json::json!(indexed_files_ct));
            map.insert("symbol_count".to_string(), serde_json::json!(symbol_count));
            map.insert("index_size_mb".to_string(), serde_json::json!(size_mb));
            map.insert("stale".to_string(), serde_json::json!(stale_bool));

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

            let staleness = if !is_stale {
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
