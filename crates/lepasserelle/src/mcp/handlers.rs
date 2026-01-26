// MCP Tool Handlers
//
// This module implements the handlers for each MCP tool that the server exposes.

use super::protocol::JsonRpcError;
use crate::leindex::{AnalysisResult, Diagnostics, IndexStats, LeIndex};
use lerecherche::search::SearchResult;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Enum of all tool handlers
///
/// Instead of using trait objects (which don't work well with async),
/// we use an enum to dispatch to the appropriate handler.
#[derive(Clone)]
pub enum ToolHandler {
    Index(IndexHandler),
    Search(SearchHandler),
    DeepAnalyze(DeepAnalyzeHandler),
    Context(ContextHandler),
    Diagnostics(DiagnosticsHandler),
}

impl ToolHandler {
    /// Get the tool name
    pub fn name(&self) -> &str {
        match self {
            ToolHandler::Index(h) => h.name(),
            ToolHandler::Search(h) => h.name(),
            ToolHandler::DeepAnalyze(h) => h.name(),
            ToolHandler::Context(h) => h.name(),
            ToolHandler::Diagnostics(h) => h.name(),
        }
    }

    /// Get the tool description
    pub fn description(&self) -> &str {
        match self {
            ToolHandler::Index(h) => h.description(),
            ToolHandler::Search(h) => h.description(),
            ToolHandler::DeepAnalyze(h) => h.description(),
            ToolHandler::Context(h) => h.description(),
            ToolHandler::Diagnostics(h) => h.description(),
        }
    }

    /// Get the tool argument schema
    pub fn argument_schema(&self) -> Value {
        match self {
            ToolHandler::Index(h) => h.argument_schema(),
            ToolHandler::Search(h) => h.argument_schema(),
            ToolHandler::DeepAnalyze(h) => h.argument_schema(),
            ToolHandler::Context(h) => h.argument_schema(),
            ToolHandler::Diagnostics(h) => h.argument_schema(),
        }
    }

    /// Execute the tool
    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        match self {
            ToolHandler::Index(h) => h.execute(leindex, args).await,
            ToolHandler::Search(h) => h.execute(leindex, args).await,
            ToolHandler::DeepAnalyze(h) => h.execute(leindex, args).await,
            ToolHandler::Context(h) => h.execute(leindex, args).await,
            ToolHandler::Diagnostics(h) => h.execute(leindex, args).await,
        }
    }
}

/// Helper to extract required string argument
fn extract_string(args: &Value, key: &str) -> Result<String, JsonRpcError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| JsonRpcError::invalid_params_with_suggestion(
            format!("Missing required argument: {}", key),
            format!("Add \"{}\": \"<value>\" to arguments", key)
        ))
}

/// Helper to extract optional string argument
fn extract_optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Helper to extract usize argument with default
fn extract_usize(args: &Value, key: &str, default: usize) -> Result<usize, JsonRpcError> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .or(Some(default))
        .ok_or_else(|| JsonRpcError::invalid_params(format!("Invalid usize argument: {}", key)))
}

/// Handler for leindex_index
///
/// Indexes a project by parsing all source files and building the search index.
#[derive(Clone)]
pub struct IndexHandler;

impl IndexHandler {
    pub fn name(&self) -> &str {
        "leindex_index"
    }

    pub fn description(&self) -> &str {
        "Index a project for code search and analysis. Parses all source files, builds the Program Dependence Graph, and creates the semantic search index."
    }

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
                    "description": "If true, re-index even if already indexed (default: false)",
                    "default": false
                }
            },
            "required": ["project_path"]
        })
    }

    pub async fn execute(
        &self,
        _leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let project_path = extract_string(&args, "project_path")?;
        let _force_reindex = args.get("force_reindex")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Create new LeIndex instance and index the project
        let project_path_clone = project_path.clone();
        let stats = tokio::task::spawn_blocking(move || {
            let mut leindex = LeIndex::new(&project_path_clone)
                .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to create LeIndex: {}", e)))?;

            leindex.index_project()
                .map_err(|e| JsonRpcError::indexing_failed(format!("Indexing failed: {}", e)))
        }).await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))??;

        Ok(serde_json::to_value(stats)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?)
    }
}

/// Handler for leindex_search
///
/// Performs semantic search on the indexed code.
#[derive(Clone)]
pub struct SearchHandler;

impl SearchHandler {
    pub fn name(&self) -> &str {
        "leindex_search"
    }

    pub fn description(&self) -> &str {
        "Search indexed code using semantic search. Returns the most relevant code snippets matching your query."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (e.g., 'authentication', 'database connection')"
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 10)",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 100
                }
            },
            "required": ["query"]
        })
    }

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let top_k = extract_usize(&args, "top_k", 10)?;

        let reader = leindex.lock().await;

        if reader.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                reader.project_path().display().to_string()
            ));
        }

        let results = reader.search(&query, top_k).await
            .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;

        Ok(serde_json::to_value(results)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?)
    }
}

/// Handler for leindex_deep_analyze
///
/// Performs deep analysis with PDG-based context expansion.
#[derive(Clone)]
pub struct DeepAnalyzeHandler;

impl DeepAnalyzeHandler {
    pub fn name(&self) -> &str {
        "leindex_deep_analyze"
    }

    pub fn description(&self) -> &str {
        "Perform deep code analysis with context expansion. Uses semantic search combined with Program Dependence Graph traversal to provide comprehensive understanding."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Analysis query (e.g., 'How does authentication work?', 'Where is user data stored?')"
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

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let token_budget = extract_usize(&args, "token_budget", 2000)?;

        let mut writer = leindex.lock().await;

        if writer.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                writer.project_path().display().to_string()
            ));
        }

        let result = writer.analyze(&query, token_budget).await
            .map_err(|e| JsonRpcError::internal_error(format!("Analysis error: {}", e)))?;

        Ok(serde_json::to_value(result)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?)
    }
}

/// Handler for leindex_context
///
/// Expands context around a specific node using PDG traversal.
#[derive(Clone)]
pub struct ContextHandler;

impl ContextHandler {
    pub fn name(&self) -> &str {
        "leindex_context"
    }

    pub fn description(&self) -> &str {
        "Expand context around a specific code node using Program Dependence Graph traversal. Useful for understanding code relationships."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "Node ID to expand context around"
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

    pub async fn execute(
        &self,
        _leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let node_id = extract_string(&args, "node_id")?;
        let _token_budget = extract_usize(&args, "token_budget", 2000)?;

        // Placeholder implementation
        Ok(serde_json::json!({
            "node_id": node_id,
            "context": format!("/* Context expansion for node {} */\n// Note: Full PDG traversal not yet implemented", node_id),
            "tokens_used": 0,
            "related_nodes": []
        }))
    }
}

/// Handler for leindex_diagnostics
///
/// Returns diagnostic information about the indexed project.
#[derive(Clone)]
pub struct DiagnosticsHandler;

impl DiagnosticsHandler {
    pub fn name(&self) -> &str {
        "leindex_diagnostics"
    }

    pub fn description(&self) -> &str {
        "Get diagnostic information about the indexed project, including memory usage, index statistics, and system health."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        _args: Value,
    ) -> Result<Value, JsonRpcError> {
        let reader = leindex.lock().await;

        let diagnostics = reader.get_diagnostics()
            .map_err(|e| JsonRpcError::internal_error(format!("Failed to get diagnostics: {}", e)))?;

        Ok(serde_json::to_value(diagnostics)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_string() {
        let args = serde_json::json!({"query": "test"});
        assert_eq!(extract_string(&args, "query").unwrap(), "test");
        assert!(extract_string(&args, "missing").is_err());
    }

    #[test]
    fn test_extract_usize() {
        let args = serde_json::json!({"top_k": 20});
        assert_eq!(extract_usize(&args, "top_k", 10).unwrap(), 20);
        assert_eq!(extract_usize(&args, "missing", 10).unwrap(), 10);
    }

    #[test]
    fn test_handler_names() {
        assert_eq!(IndexHandler.name(), "leindex_index");
        assert_eq!(SearchHandler.name(), "leindex_search");
        assert_eq!(DeepAnalyzeHandler.name(), "leindex_deep_analyze");
        assert_eq!(ContextHandler.name(), "leindex_context");
        assert_eq!(DiagnosticsHandler.name(), "leindex_diagnostics");
    }

    #[test]
    fn test_argument_schemas() {
        let schemas = vec![
            IndexHandler.argument_schema(),
            SearchHandler.argument_schema(),
            DeepAnalyzeHandler.argument_schema(),
        ];

        for schema in schemas {
            assert!(schema.is_object());
            assert!(schema.get("type").is_some());
        }
    }
}
