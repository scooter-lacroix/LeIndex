use super::helpers::{extract_bool, extract_string, extract_usize, wrap_with_meta};
use super::protocol::JsonRpcError;
use super::request_meta::WorkBudget;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex [context
///
/// Expands context around a specific node using PDG traversal.
#[derive(Clone)]
pub struct ContextHandler;

impl ContextHandler {
    /// Returns the name of this MCP tool (MCP-compliant: ASCII letters, digits, underscore, hyphen, dot only)
    pub fn name(&self) -> &str {
        "leindex.context"
    }

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "LeIndex [Context]"
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
                },
                "max_latency_ms": {
                    "type": "integer",
                    "description": "Optional enrichment budget; elapsed work returns partial PDG context (default: 1500)",
                    "default": 1500,
                    "minimum": 0,
                    "maximum": 60000
                },
                "allow_partial": {
                    "type": "boolean",
                    "description": "Allow bounded PDG context when the enrichment budget is reached",
                    "default": true
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
        let budget = WorkBudget {
            max_latency_ms: extract_usize(&args, "max_latency_ms", 1500)? as u64,
            allow_partial: extract_bool(&args, "allow_partial", true),
        };
        let started = std::time::Instant::now();

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        guard
            .ensure_analysis_context_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        if !guard.is_indexed() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        let result = guard
            .expand_node_context(&node_id, token_budget)
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("not found") {
                    JsonRpcError::invalid_params(msg)
                } else {
                    JsonRpcError::internal_error(format!("Context expansion error: {}", e))
                }
            })?;

        let partial = budget.elapsed(started);
        serde_json::to_value(result)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
            .map(|mut v| {
                if let Some(object) = v.as_object_mut() {
                    object.insert(
                        "retrieval".to_string(),
                        serde_json::json!({
                            "tfidf_status": "fresh",
                            "pdg_status": if partial { "partial" } else { "fresh" },
                            "max_latency_ms": budget.max_latency_ms,
                            "allow_partial": budget.allow_partial,
                            "neural_status": guard.neural_status()
                        }),
                    );
                }
                wrap_with_meta(v, &guard)
            })
    }
}
