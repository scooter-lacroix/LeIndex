use super::helpers::{extract_bool, extract_string, extract_usize, wrap_with_meta};
use super::protocol::JsonRpcError;
use super::request_meta::WorkBudget;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex [deep_analyze
///
/// Performs deep analysis with PDG-based context expansion.
#[derive(Clone)]
pub struct DeepAnalyzeHandler;

impl DeepAnalyzeHandler {
    /// Returns the name of this MCP tool (MCP-compliant: ASCII letters, digits, underscore, hyphen, dot only)
    pub fn name(&self) -> &str {
        "leindex.deep-analyze"
    }

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "LeIndex [Deep Analyze]"
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
                },
                "task_context": {
                    "type": "string",
                    "description": "Optional bounded review/task context used only for this query",
                    "maxLength": 2000
                },
                "max_latency_ms": {
                    "type": "integer",
                    "description": "PDG context-expansion budget; configured neural startup/inference is not cancelled (default: 1500)",
                    "default": 1500,
                    "minimum": 0,
                    "maximum": 60000
                },
                "allow_partial": {
                    "type": "boolean",
                    "description": "Return partial PDG context when the budget is reached",
                    "default": true
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
        let task_context = args
            .get("task_context")
            .and_then(Value::as_str)
            .map(|context| context.chars().take(2000).collect::<String>());
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

        if guard.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        let effective_query = task_context.as_deref().map_or_else(
            || query.clone(),
            |context| format!("{}\nTask context: {}", query, context),
        );
        let result = if task_context.is_some() {
            guard.analyze_ephemeral(&effective_query, token_budget)
        } else {
            guard.analyze(&effective_query, token_budget)
        }
        .map_err(|e| JsonRpcError::internal_error(format!("Analysis error: {}", e)))?;

        serde_json::to_value(result)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
            .map(|mut v| {
                if let Some(object) = v.as_object_mut() {
                    object.insert(
                        "retrieval".to_string(),
                        serde_json::json!({
                            "tfidf_status": "fresh",
                            "pdg_status": if budget.elapsed(started) { "partial" } else { "fresh" },
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
