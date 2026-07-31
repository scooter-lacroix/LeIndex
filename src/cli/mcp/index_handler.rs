use super::helpers::{extract_bool, extract_string};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex [index
///
/// Indexes a project by parsing all source files and building the search index.
#[derive(Clone)]
pub struct IndexHandler;

impl IndexHandler {
    /// Returns the name of this MCP tool (MCP-compliant: ASCII letters, digits, underscore, hyphen, dot only)
    pub fn name(&self) -> &str {
        "leindex.index"
    }

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "LeIndex [Index]"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Start or poll a registry-owned project index job. Returns immediately by default; \
use wait=true only when an interactive caller explicitly wants to wait. Core PDG and TF-IDF \
results publish first, then the configured neural worker is actively evaluated for hybrid rows."
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
                },
                "wait": {
                    "type": "boolean",
                    "description": "Wait for completion instead of returning a pollable job snapshot (default: false)",
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
        let wait = extract_bool(&args, "wait", false);
        let snapshot = registry
            .start_index_job(Some(&project_path), force_reindex, wait)
            .await?;
        serde_json::to_value(snapshot)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
    }
}
