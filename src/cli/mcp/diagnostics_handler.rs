use super::helpers::{wrap_with_meta, HandlerContext};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_diagnostics
///
/// Returns diagnostic information about the indexed project.
#[derive(Clone)]
pub struct DiagnosticsHandler;

impl DiagnosticsHandler {
    /// Returns the name of this RPC method
    pub fn name(&self) -> &str {
        "leindex_diagnostics"
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
        let mut ctx = HandlerContext::new_index_only(registry, &args).await?;

        let diagnostics = ctx.index_mut().get_diagnostics().map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to get diagnostics: {}", e))
        })?;

        let (changed, deleted) = ctx
            .index_mut()
            .check_freshness()
            .unwrap_or_else(|_| (vec![], vec![]));
        let storage_path = ctx.storage_path().display().to_string();
        let db_size = std::fs::metadata(ctx.storage_path().join("leindex.db"))
            .map(|m| m.len())
            .unwrap_or(0);
        let coverage = ctx.index_mut().coverage_report().ok();

        let mut diag_json = serde_json::to_value(diagnostics)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?;

        if let Value::Object(ref mut map) = diag_json {
            map.insert("storage_path".to_string(), serde_json::json!(storage_path));
            map.insert("db_size_bytes".to_string(), serde_json::json!(db_size));

            let staleness = if changed.is_empty() && deleted.is_empty() {
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
                    "suggestion": "Call leindex_index with force_reindex=true to refresh",
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

        Ok(wrap_with_meta(diag_json, ctx.index()))
    }
}
