use super::helpers::{extract_string, extract_usize, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_context
///
/// Expands context around a specific node using PDG traversal.
#[derive(Clone)]
pub struct ContextHandler;

impl ContextHandler {
    /// Returns the name of this RPC method
    pub fn name(&self) -> &str {
        "leindex_context"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Expand context around a code node via PDG: callers, callees, data dependencies, and \
sibling nodes. Supersedes Read for understanding how a function fits into its module \
without reading the entire file. Accepts project_path to auto-switch between projects."
    }

    /// Returns the JSON schema for the arguments of this RPC method
    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "Node ID to expand context around (short name like 'my_func' or full ID like 'file.py:Class.method')"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Maximum tokens for context (default: 2000)",
                    "default": 2000,
                    "minimum": 100,
                    "maximum": 100000
                }
            },
            "required": ["node_id"]
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let node_id = extract_string(&args, "node_id")?;
        let token_budget = extract_usize(&args, "token_budget", 2000)?;

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        // Best-effort PDG load
        let _ = guard.ensure_pdg_loaded();

        if !guard.is_indexed() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        let result = guard
            .expand_node_context(&node_id, token_budget)
            .map_err(|e| JsonRpcError::internal_error(format!("Context expansion error: {}", e)))?;

        serde_json::to_value(result)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
            .map(|v| wrap_with_meta(v, &*guard))
    }
}
