use super::helpers::{extract_string, extract_usize, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_deep_analyze
///
/// Performs deep analysis with PDG-based context expansion.
#[derive(Clone)]
pub struct DeepAnalyzeHandler;

impl DeepAnalyzeHandler {
    /// Returns the name of this RPC method
    pub fn name(&self) -> &str {
        "leindex_deep_analyze"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Deep analysis: semantic search + PDG traversal for definition, callers, callees, \
data flow, and impact radius. Use for broad codebase understanding queries."
    }

    /// Returns the JSON schema for the arguments of this RPC method
    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Analysis query (e.g., 'How does authentication work?', 'Where is user data stored?')"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Maximum tokens for context expansion (default: 2000)",
                    "default": 2000,
                    "minimum": 100,
                    "maximum": 100000
                }
            },
            "required": ["query"]
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let token_budget = extract_usize(&args, "token_budget", 2000)?;

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        guard
            .ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        if guard.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        let result = guard
            .analyze(&query, token_budget)
            .map_err(|e| JsonRpcError::internal_error(format!("Analysis error: {}", e)))?;

        serde_json::to_value(result)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
            .map(|v| wrap_with_meta(v, &guard))
    }
}
