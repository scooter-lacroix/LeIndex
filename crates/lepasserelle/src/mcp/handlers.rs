// MCP Tool Handlers
//
// This module implements the handlers for each MCP tool that the server exposes.

use super::protocol::JsonRpcError;
use crate::leindex::LeIndex;
use lephase::{run_phase_analysis, DocsMode, FormatMode, PhaseOptions, PhaseSelection};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Enum of all tool handlers
///
/// Instead of using trait objects (which don't work well with async),
/// we use an enum to dispatch to the appropriate handler.
#[derive(Clone)]
pub enum ToolHandler {
    /// Handler for project indexing
    Index(IndexHandler),
    /// Handler for semantic search
    Search(SearchHandler),
    /// Handler for deep code analysis
    DeepAnalyze(DeepAnalyzeHandler),
    /// Handler for code context expansion
    Context(ContextHandler),
    /// Handler for system diagnostics
    Diagnostics(DiagnosticsHandler),
    /// Handler for additive 5-phase analysis
    PhaseAnalysis(PhaseAnalysisHandler),
    /// Optional compatibility alias for phase analysis
    PhaseAnalysisAlias(PhaseAnalysisAliasHandler),
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
            ToolHandler::PhaseAnalysis(h) => h.name(),
            ToolHandler::PhaseAnalysisAlias(h) => h.name(),
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
            ToolHandler::PhaseAnalysis(h) => h.description(),
            ToolHandler::PhaseAnalysisAlias(h) => h.description(),
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
            ToolHandler::PhaseAnalysis(h) => h.argument_schema(),
            ToolHandler::PhaseAnalysisAlias(h) => h.argument_schema(),
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
            ToolHandler::PhaseAnalysis(h) => h.execute(leindex, args).await,
            ToolHandler::PhaseAnalysisAlias(h) => h.execute(leindex, args).await,
        }
    }
}

/// Helper to extract required string argument
fn extract_string(args: &Value, key: &str) -> Result<String, JsonRpcError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                format!("Missing required argument: {}", key),
                format!("Add \"{}\": \"<value>\" to arguments", key),
            )
        })
}

/// Helper to extract usize argument with default
fn extract_usize(args: &Value, key: &str, default: usize) -> Result<usize, JsonRpcError> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .or(Some(default))
        .ok_or_else(|| JsonRpcError::invalid_params(format!("Invalid usize argument: {}", key)))
}

/// Helper to extract bool argument with default.
fn extract_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

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
        "Index a project for code search and analysis. Parses all source files, builds the Program Dependence Graph, and creates the semantic search index."
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
                    "description": "If true, re-index even if already indexed (default: false)",
                    "default": false
                }
            },
            "required": ["project_path"]
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let project_path = extract_string(&args, "project_path")?;
        let force_reindex = args
            .get("force_reindex")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Check if already indexed and we're not forcing reindex
        {
            let index = leindex.lock().await;
            if index.is_indexed() && !force_reindex {
                return serde_json::to_value(index.get_stats()).map_err(|e| {
                    JsonRpcError::internal_error(format!("Serialization error: {}", e))
                });
            }
        }

        // Create new LeIndex instance and index the project in a blocking task
        let project_path_clone = project_path.clone();
        let stats = tokio::task::spawn_blocking(move || {
            let mut temp_leindex = LeIndex::new(&project_path_clone).map_err(|e| {
                JsonRpcError::indexing_failed(format!("Failed to create LeIndex: {}", e))
            })?;

            temp_leindex
                .index_project(force_reindex)
                .map_err(|e| JsonRpcError::indexing_failed(format!("Indexing failed: {}", e)))
        })
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))??;

        // Update shared state by loading the newly indexed project from storage
        let mut index = leindex.lock().await;

        // Ensure path matches (canonicalize to be safe)
        let path = std::path::Path::new(&project_path)
            .canonicalize()
            .map_err(|e| {
                JsonRpcError::internal_error(format!("Failed to canonicalize path: {}", e))
            })?;

        if index.project_path() != path {
            *index = LeIndex::new(&path).map_err(|e| {
                JsonRpcError::indexing_failed(format!("Failed to re-initialize LeIndex: {}", e))
            })?;
        }

        index.load_from_storage().map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to load indexed data: {}", e))
        })?;

        serde_json::to_value(stats)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
    }
}

/// Handler for leindex_search
///
/// Performs semantic search on the indexed code.
#[derive(Clone)]
pub struct SearchHandler;

impl SearchHandler {
    /// Returns the name of this RPC method
    pub fn name(&self) -> &str {
        "leindex_search"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Search indexed code using semantic search. Returns the most relevant code snippets matching your query."
    }

    /// Returns the JSON schema for the arguments of this RPC method
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

    /// Executes the RPC method
    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let top_k = extract_usize(&args, "top_k", 10)?;

        let mut index = leindex.lock().await;

        if index.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                index.project_path().display().to_string(),
            ));
        }

        let results = index
            .search(&query, top_k)
            .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;

        serde_json::to_value(results)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
    }
}

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
        "Perform deep code analysis with context expansion. Uses semantic search combined with Program Dependence Graph traversal to provide comprehensive understanding."
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
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let token_budget = extract_usize(&args, "token_budget", 2000)?;

        let mut writer = leindex.lock().await;

        if writer.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                writer.project_path().display().to_string(),
            ));
        }

        let result = writer
            .analyze(&query, token_budget)
            .map_err(|e| JsonRpcError::internal_error(format!("Analysis error: {}", e)))?;

        serde_json::to_value(result)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
    }
}

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
        "Expand context around a specific code node using Program Dependence Graph traversal. Useful for understanding code relationships."
    }

    /// Returns the JSON schema for the arguments of this RPC method
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

    /// Executes the RPC method
    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let node_id = extract_string(&args, "node_id")?;
        let token_budget = extract_usize(&args, "token_budget", 2000)?;

        let reader = leindex.lock().await;

        if !reader.is_indexed() {
            return Err(JsonRpcError::project_not_indexed(
                reader.project_path().display().to_string(),
            ));
        }

        let result = reader
            .expand_node_context(&node_id, token_budget)
            .map_err(|e| JsonRpcError::internal_error(format!("Context expansion error: {}", e)))?;

        serde_json::to_value(result)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
    }
}

/// Handler for leindex_phase_analysis.
#[derive(Clone)]
pub struct PhaseAnalysisHandler;

impl PhaseAnalysisHandler {
    /// Returns the name of this RPC method.
    pub fn name(&self) -> &str {
        "leindex_phase_analysis"
    }

    /// Returns the description of this RPC method.
    pub fn description(&self) -> &str {
        "Run additive 5-phase analysis with freshness-aware incremental execution. Defaults to all 5 phases when `phase` is omitted."
    }

    /// Returns the JSON schema for the arguments of this RPC method.
    pub fn argument_schema(&self) -> Value {
        phase_analysis_schema()
    }

    /// Executes the RPC method.
    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        execute_phase_analysis(leindex, args).await
    }
}

/// Optional compatibility alias for phase analysis.
#[derive(Clone)]
pub struct PhaseAnalysisAliasHandler;

impl PhaseAnalysisAliasHandler {
    /// Returns the alias name.
    pub fn name(&self) -> &str {
        "phase_analysis"
    }

    /// Returns description.
    pub fn description(&self) -> &str {
        "Alias for leindex_phase_analysis"
    }

    /// Returns argument schema.
    pub fn argument_schema(&self) -> Value {
        phase_analysis_schema()
    }

    /// Executes the alias method.
    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        execute_phase_analysis(leindex, args).await
    }
}

fn phase_analysis_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "phase": {
                "oneOf": [
                    { "type": "integer", "minimum": 1, "maximum": 5 },
                    { "type": "string", "enum": ["all", "1", "2", "3", "4", "5"] }
                ],
                "default": "all"
            },
            "mode": {
                "type": "string",
                "enum": ["ultra", "balanced", "verbose"],
                "default": "balanced"
            },
            "path": {
                "type": "string"
            },
            "max_files": {
                "type": "integer",
                "default": 2000
            },
            "max_focus_files": {
                "type": "integer",
                "default": 20
            },
            "top_n": {
                "type": "integer",
                "default": 10
            },
            "max_chars": {
                "type": "integer",
                "default": 12000
            },
            "include_docs": {
                "type": "boolean",
                "default": false
            },
            "docs_mode": {
                "type": "string",
                "enum": ["off", "markdown", "text", "all"],
                "default": "off"
            }
        },
        "required": []
    })
}

async fn execute_phase_analysis(
    leindex: &Arc<Mutex<LeIndex>>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let selection = match args.get("phase") {
        None => PhaseSelection::All,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("all") => PhaseSelection::All,
        Some(Value::String(s)) => {
            let parsed = s.parse::<u8>().map_err(|_| {
                JsonRpcError::invalid_params(
                    "phase must be 1..5, \"1\"..\"5\", or 'all'".to_string(),
                )
            })?;
            PhaseSelection::from_number(parsed).ok_or_else(|| {
                JsonRpcError::invalid_params("phase must be in range 1..5".to_string())
            })?
        }
        Some(Value::Number(n)) => {
            let Some(p) = n.as_u64().map(|v| v as u8) else {
                return Err(JsonRpcError::invalid_params(
                    "phase must be 1..5 or 'all'".to_string(),
                ));
            };
            PhaseSelection::from_number(p).ok_or_else(|| {
                JsonRpcError::invalid_params("phase must be in range 1..5".to_string())
            })?
        }
        _ => {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                "Invalid 'phase'".to_string(),
                "Use phase: 1..5, phase: \"1\"..\"5\", or phase: \"all\" (default)"
                    .to_string(),
            ));
        }
    };

    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("balanced");
    let parsed_mode = FormatMode::parse(mode).ok_or_else(|| {
        JsonRpcError::invalid_params("mode must be one of ultra|balanced|verbose".to_string())
    })?;

    let docs_mode_raw = args
        .get("docs_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("off");
    let parsed_docs_mode = DocsMode::parse(docs_mode_raw).ok_or_else(|| {
        JsonRpcError::invalid_params("docs_mode must be one of off|markdown|text|all".to_string())
    })?;

    let include_docs = extract_bool(&args, "include_docs", false);

    let base_project_root = {
        let reader = leindex.lock().await;
        reader.project_path().to_path_buf()
    };

    let canonical_target = match args.get("path").and_then(|v| v.as_str()) {
        Some(path) => PathBuf::from(path).canonicalize().map_err(|e| {
            JsonRpcError::invalid_params(format!("path must exist and be accessible: {}", e))
        })?,
        None => base_project_root.clone(),
    };

    let (root, focus_files) = if canonical_target.is_file() {
        let file_root = canonical_target
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| JsonRpcError::invalid_params("file path has no parent".to_string()))?;
        (file_root, vec![canonical_target.clone()])
    } else {
        (canonical_target, Vec::new())
    };

    let default_max_files = if focus_files.is_empty() { 2000 } else { 1 };
    let mut max_files = extract_usize(&args, "max_files", default_max_files)?;
    if !focus_files.is_empty() {
        max_files = max_files.max(1);
    }

    let max_focus_files = extract_usize(&args, "max_focus_files", 20)?;
    let top_n = extract_usize(&args, "top_n", 10)?;
    let max_output_chars = extract_usize(&args, "max_chars", 12000)?;

    let options = PhaseOptions {
        root,
        focus_files,
        mode: parsed_mode,
        max_files,
        max_focus_files,
        top_n,
        max_output_chars,
        use_incremental_refresh: true,
        include_docs,
        docs_mode: parsed_docs_mode,
        hotspot_keywords: PhaseOptions::default().hotspot_keywords,
    };

    let report = tokio::task::spawn_blocking(move || run_phase_analysis(options, selection))
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))?
        .map_err(|e| JsonRpcError::internal_error(format!("Phase analysis failed: {}", e)))?;

    serde_json::to_value(report)
        .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
}

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
            "properties": {},
            "required": []
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        _args: Value,
    ) -> Result<Value, JsonRpcError> {
        let reader = leindex.lock().await;

        let diagnostics = reader.get_diagnostics().map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to get diagnostics: {}", e))
        })?;

        serde_json::to_value(diagnostics)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        assert_eq!(PhaseAnalysisHandler.name(), "leindex_phase_analysis");
        assert_eq!(PhaseAnalysisAliasHandler.name(), "phase_analysis");
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

    #[test]
    fn test_phase_schema_phase_and_path_are_optional() {
        let schema = phase_analysis_schema();
        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required array");

        assert!(
            required.is_empty(),
            "phase analysis schema should not require explicit phase or path"
        );
    }

    #[test]
    fn test_phase_schema_defaults_phase_to_all() {
        let schema = phase_analysis_schema();
        let phase = schema
            .get("properties")
            .and_then(|v| v.get("phase"))
            .expect("phase schema");

        assert_eq!(phase.get("default").and_then(|v| v.as_str()), Some("all"));
    }

    #[tokio::test]
    async fn test_phase_analysis_defaults_to_all_when_phase_missing() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
        std::fs::write(&src, "pub fn ping()->bool{true}\n").expect("write source");

        let leindex = Arc::new(Mutex::new(LeIndex::new(dir.path()).expect("leindex")));
        let args = serde_json::json!({
            "path": src.display().to_string(),
            "mode": "balanced",
            "max_files": 1
        });

        let value = execute_phase_analysis(&leindex, args)
            .await
            .expect("phase analysis");
        let phases = value
            .get("executed_phases")
            .and_then(|v| v.as_array())
            .expect("executed phases");

        let as_u8 = phases
            .iter()
            .filter_map(|v| v.as_u64())
            .map(|v| v as u8)
            .collect::<Vec<_>>();
        assert_eq!(as_u8, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn test_phase_analysis_accepts_string_phase_number() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
        std::fs::write(&src, "pub fn ping()->bool{true}\n").expect("write source");

        let leindex = Arc::new(Mutex::new(LeIndex::new(dir.path()).expect("leindex")));
        let args = serde_json::json!({
            "path": src.display().to_string(),
            "phase": "1",
            "mode": "balanced",
            "max_files": 1
        });

        let value = execute_phase_analysis(&leindex, args)
            .await
            .expect("phase analysis");
        let phases = value
            .get("executed_phases")
            .and_then(|v| v.as_array())
            .expect("executed phases");

        let as_u8 = phases
            .iter()
            .filter_map(|v| v.as_u64())
            .map(|v| v as u8)
            .collect::<Vec<_>>();
        assert_eq!(as_u8, vec![1]);
    }
}
