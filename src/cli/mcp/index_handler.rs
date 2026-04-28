use super::helpers::{extract_bool, extract_string, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_index
///
/// Indexes a project by parsing all source files and building the search index.
#[derive(Clone)]
pub struct IndexHandler;

impl IndexHandler {
    /// Returns the name of this RPC method
    pub fn name(&self) -> &str {
        "leindex_index"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Index a project. Auto-indexes on first use; returns cached stats on repeat calls. \
Use force_reindex=true only to rebuild after external file changes. All other tools \
also accept project_path and auto-index, so explicit indexing is optional."
    }

    /// Returns the JSON schema for the arguments of this RPC method
    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Absolute path to the project directory to index"
                },
                "force_reindex": {
                    "type": "boolean",
                    "description": "If true, re-index even if already indexed (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                }
            },
            "required": ["project_path"]
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let project_path = extract_string(&args, "project_path")?;
        let force_reindex = extract_bool(&args, "force_reindex", false);

        if !force_reindex {
            // Auto-index-if-needed path: registry handles everything.
            let handle = registry.get_or_create(Some(&project_path)).await?;
            let index = handle.read().await;
            if index.is_indexed() {
                return serde_json::to_value(index.get_stats())
                    .map(|v| wrap_with_meta(v, &index))
                    .map_err(|e| {
                        JsonRpcError::internal_error(format!("Serialization error: {}", e))
                    });
            }
        }

        let stats = registry
            .index_project(Some(&project_path), force_reindex)
            .await?;

        let index = registry.get_or_create(Some(&project_path)).await?;
        let idx = index.read().await;
        serde_json::to_value(stats)
            .map(|v| wrap_with_meta(v, &idx))
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
    }
}
