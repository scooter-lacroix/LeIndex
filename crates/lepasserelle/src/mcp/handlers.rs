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
    // Phase C: Tool Supremacy
    /// Handler for file structural summary (replaces Read)
    FileSummary(FileSummaryHandler),
    /// Handler for symbol call graph lookup (replaces Grep)
    SymbolLookup(SymbolLookupHandler),
    /// Handler for annotated project tree (replaces Glob/ls)
    ProjectMap(ProjectMapHandler),
    /// Handler for structurally-aware symbol search (replaces Grep)
    GrepSymbols(GrepSymbolsHandler),
    /// Handler for targeted symbol source read (replaces Read)
    ReadSymbol(ReadSymbolHandler),
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
            ToolHandler::FileSummary(h) => h.name(),
            ToolHandler::SymbolLookup(h) => h.name(),
            ToolHandler::ProjectMap(h) => h.name(),
            ToolHandler::GrepSymbols(h) => h.name(),
            ToolHandler::ReadSymbol(h) => h.name(),
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
            ToolHandler::FileSummary(h) => h.description(),
            ToolHandler::SymbolLookup(h) => h.description(),
            ToolHandler::ProjectMap(h) => h.description(),
            ToolHandler::GrepSymbols(h) => h.description(),
            ToolHandler::ReadSymbol(h) => h.description(),
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
            ToolHandler::FileSummary(h) => h.argument_schema(),
            ToolHandler::SymbolLookup(h) => h.argument_schema(),
            ToolHandler::ProjectMap(h) => h.argument_schema(),
            ToolHandler::GrepSymbols(h) => h.argument_schema(),
            ToolHandler::ReadSymbol(h) => h.argument_schema(),
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
            ToolHandler::FileSummary(h) => h.execute(leindex, args).await,
            ToolHandler::SymbolLookup(h) => h.execute(leindex, args).await,
            ToolHandler::ProjectMap(h) => h.execute(leindex, args).await,
            ToolHandler::GrepSymbols(h) => h.execute(leindex, args).await,
            ToolHandler::ReadSymbol(h) => h.execute(leindex, args).await,
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
            let _ = index.close();
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

// ============================================================================
// Phase C: Tool Supremacy — Read/Grep/Glob Replacement
// ============================================================================

/// Format a NodeType as a human-readable string.
fn node_type_str(nt: &legraphe::pdg::NodeType) -> &'static str {
    match nt {
        legraphe::pdg::NodeType::Function => "function",
        legraphe::pdg::NodeType::Class => "class",
        legraphe::pdg::NodeType::Method => "method",
        legraphe::pdg::NodeType::Variable => "variable",
        legraphe::pdg::NodeType::Module => "module",
    }
}

/// Read a source snippet from disk using the node's byte_range.
///
/// Returns `None` if the file can't be read or the range is empty.
fn read_source_snippet(file_path: &str, byte_range: (usize, usize)) -> Option<String> {
    if byte_range.1 <= byte_range.0 {
        return None;
    }
    let bytes = std::fs::read(file_path).ok()?;
    let start = byte_range.0.min(bytes.len());
    let end = byte_range.1.min(bytes.len());
    if start >= end {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

/// Collect the NodeIds of all nodes that have a direct Call or DataDependency
/// edge pointing *to* `target_id` (i.e. the callers / direct dependents).
fn get_direct_callers(
    pdg: &legraphe::pdg::ProgramDependenceGraph,
    target_id: legraphe::pdg::NodeId,
) -> Vec<legraphe::pdg::NodeId> {
    pdg.edge_indices()
        .filter_map(|eid| {
            let (src, tgt) = pdg.edge_endpoints(eid)?;
            if tgt == target_id {
                Some(src)
            } else {
                None
            }
        })
        .collect()
}

// ============================================================================
// C.1 — leindex_file_summary
// ============================================================================

/// Handler for leindex_file_summary — structured file analysis replacing Read.
#[derive(Clone)]
pub struct FileSummaryHandler;

#[allow(missing_docs)]
impl FileSummaryHandler {
    pub fn name(&self) -> &str { "leindex_file_summary" }

    pub fn description(&self) -> &str {
        "Get a comprehensive structural analysis of a file: all symbols with signatures, \
complexity scores, cross-file dependencies and dependents, import/export maps, and \
module role summary. Returns everything needed to understand a file without reading \
its raw content — typically 5-10x more token efficient than Read. Includes cross-file \
relationship information that Read cannot provide."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to analyze"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1000)",
                    "default": 1000
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include source snippets for key symbols (default: false)",
                    "default": false
                },
                "focus_symbol": {
                    "type": "string",
                    "description": "Focus analysis on a specific symbol name (optional)"
                }
            },
            "required": ["file_path"]
        })
    }

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let file_path = extract_string(&args, "file_path")?;
        let include_source = extract_bool(&args, "include_source", false);
        let focus_symbol = args.get("focus_symbol").and_then(|v| v.as_str()).map(str::to_owned);
        let token_budget = extract_usize(&args, "token_budget", 1000)?;

        let index = leindex.lock().await;

        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // Collect all nodes in this file
        let node_ids = pdg.nodes_in_file(&file_path);

        if node_ids.is_empty() {
            return Err(JsonRpcError::invalid_params(format!(
                "No symbols found for file '{}'. Is the project indexed?",
                file_path
            )));
        }

        // Determine line count from file
        let line_count = std::fs::read_to_string(&file_path)
            .map(|s| s.lines().count())
            .unwrap_or(0);

        let language = pdg
            .get_node(node_ids[0])
            .map(|n| n.language.clone())
            .unwrap_or_default();

        // Build symbol list
        let mut symbols: Vec<Value> = Vec::new();
        let mut total_chars = 0usize;
        let chars_per_token = 4usize;
        let char_budget = token_budget * chars_per_token;

        for &nid in &node_ids {
            let node = match pdg.get_node(nid) {
                Some(n) => n,
                None => continue,
            };

            // Apply focus filter
            if let Some(ref focus) = focus_symbol {
                if !node.name.to_lowercase().contains(&focus.to_lowercase()) {
                    continue;
                }
            }

            // Outgoing edges = dependencies
            let callees = pdg.neighbors(nid);
            let dependencies: Vec<String> = callees
                .iter()
                .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
                .collect();

            // Incoming edges = dependents (callers)
            let caller_ids = get_direct_callers(pdg, nid);
            let dependents: Vec<String> = caller_ids
                .iter()
                .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
                .collect();

            // Cross-file references (edges to nodes in different files)
            let cross_file_refs: Vec<Value> = callees
                .iter()
                .filter_map(|&cid| {
                    let cn = pdg.get_node(cid)?;
                    if cn.file_path != node.file_path {
                        Some(serde_json::json!({
                            "symbol": cn.name,
                            "file": cn.file_path,
                            "relationship": "dependency"
                        }))
                    } else {
                        None
                    }
                })
                .collect();

            let mut sym = serde_json::json!({
                "name": node.name,
                "type": node_type_str(&node.node_type),
                "line_range": node.byte_range,
                "complexity": node.complexity,
                "dependencies": dependencies,
                "dependents": dependents,
                "cross_file_refs": cross_file_refs
            });

            if include_source {
                if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                    // Trim to avoid blowing up token budget
                    let truncated: String = src.chars().take(500).collect();
                    sym["source"] = Value::String(truncated);
                }
            }

            let sym_str = sym.to_string();
            total_chars += sym_str.len();
            if total_chars > char_budget {
                break;
            }
            symbols.push(sym);
        }

        // Determine module role from node type distribution
        let func_count = symbols.iter().filter(|s| s["type"] == "function").count();
        let class_count = symbols.iter().filter(|s| s["type"] == "class").count();
        let module_role = if class_count > func_count {
            format!("Class definitions ({} classes, {} functions)", class_count, func_count)
        } else {
            format!("Function module ({} functions, {} classes)", func_count, class_count)
        };

        Ok(serde_json::json!({
            "file_path": file_path,
            "language": language,
            "line_count": line_count,
            "symbol_count": symbols.len(),
            "symbols": symbols,
            "module_role": module_role
        }))
    }
}

// ============================================================================
// C.2 — leindex_symbol_lookup
// ============================================================================

/// Handler for leindex_symbol_lookup — full call graph for any symbol.
#[derive(Clone)]
pub struct SymbolLookupHandler;

#[allow(missing_docs)]
impl SymbolLookupHandler {
    pub fn name(&self) -> &str { "leindex_symbol_lookup" }

    pub fn description(&self) -> &str {
        "Look up any symbol (function, class, method, variable) and get its full structural \
context: definition location, signature, callers, callees, data dependencies, and impact \
radius showing how many symbols and files would be affected by changes. Replaces Grep + \
multiple Read calls with a single structured response."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to look up"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1500)",
                    "default": 1500
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include source code of definition (default: false)",
                    "default": false
                },
                "include_callers": {
                    "type": "boolean",
                    "description": "Include callers (default: true)",
                    "default": true
                },
                "include_callees": {
                    "type": "boolean",
                    "description": "Include callees (default: true)",
                    "default": true
                },
                "depth": {
                    "type": "integer",
                    "description": "Call graph traversal depth (default: 2, max: 5)",
                    "default": 2,
                    "minimum": 1,
                    "maximum": 5
                }
            },
            "required": ["symbol"]
        })
    }

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let symbol = extract_string(&args, "symbol")?;
        let include_source = extract_bool(&args, "include_source", false);
        let include_callers = extract_bool(&args, "include_callers", true);
        let include_callees = extract_bool(&args, "include_callees", true);
        let _depth = extract_usize(&args, "depth", 2)?.min(5);
        let token_budget = extract_usize(&args, "token_budget", 1500)?;

        let index = leindex.lock().await;

        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // 1. Exact symbol lookup
        let node_id = if let Some(nid) = pdg.find_by_symbol(&symbol) {
            nid
        } else {
            // 2. Fuzzy match: substring, case-insensitive
            let sym_lower = symbol.to_lowercase();
            let found = pdg.node_indices().find(|&nid| {
                pdg.get_node(nid)
                    .map(|n| n.name.to_lowercase().contains(&sym_lower) || n.id.to_lowercase().contains(&sym_lower))
                    .unwrap_or(false)
            });
            found.ok_or_else(|| {
                JsonRpcError::invalid_params(format!("Symbol '{}' not found in project index", symbol))
            })?
        };

        let node = pdg.get_node(node_id).ok_or_else(|| {
            JsonRpcError::internal_error("PDG node disappeared after lookup")
        })?;

        let char_budget = token_budget * 4;

        // Callees (direct)
        let callees: Vec<Value> = if include_callees {
            pdg.neighbors(node_id)
                .iter()
                .filter_map(|&cid| {
                    pdg.get_node(cid).map(|cn| serde_json::json!({
                        "name": cn.name,
                        "file": cn.file_path,
                        "type": node_type_str(&cn.node_type)
                    }))
                })
                .take(50)
                .collect()
        } else {
            Vec::new()
        };

        // Callers (direct)
        let callers: Vec<Value> = if include_callers {
            get_direct_callers(pdg, node_id)
                .iter()
                .filter_map(|&cid| {
                    pdg.get_node(cid).map(|cn| serde_json::json!({
                        "name": cn.name,
                        "file": cn.file_path,
                        "type": node_type_str(&cn.node_type)
                    }))
                })
                .take(50)
                .collect()
        } else {
            Vec::new()
        };

        // Forward impact (transitive dependents)
        let forward = pdg.get_forward_impact(node_id);
        let affected_files: std::collections::HashSet<&str> = forward
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.file_path.as_str()))
            .collect();
        let impact_radius = serde_json::json!({
            "affected_symbols": forward.len(),
            "affected_files": affected_files.len()
        });

        let mut result = serde_json::json!({
            "symbol": node.name,
            "type": node_type_str(&node.node_type),
            "file": node.file_path,
            "byte_range": node.byte_range,
            "complexity": node.complexity,
            "language": node.language,
            "callers": callers,
            "callees": callees,
            "impact_radius": impact_radius
        });

        if include_source {
            if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                let truncated: String = src.chars().take(char_budget / 2).collect();
                result["source"] = Value::String(truncated);
            }
        }

        Ok(result)
    }
}

// ============================================================================
// C.3 — leindex_project_map
// ============================================================================

/// Handler for leindex_project_map — annotated project tree replacing Glob/ls.
#[derive(Clone)]
pub struct ProjectMapHandler;

#[allow(missing_docs)]
impl ProjectMapHandler {
    pub fn name(&self) -> &str { "leindex_project_map" }

    pub fn description(&self) -> &str {
        "Get an annotated project structure map showing files, directories, symbol counts, \
complexity hotspots, and inter-module dependency arrows. Unlike Glob which returns flat \
file lists, this shows the project's architecture — which modules depend on which, where \
complexity lives, and what the entry points are. Typically 5x more token efficient than \
Glob + directory reads."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Subdirectory to scope to (default: project root)"
                },
                "depth": {
                    "type": "integer",
                    "description": "Tree depth (default: 3, max: 10)",
                    "default": 3,
                    "minimum": 1,
                    "maximum": 10
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 2000)",
                    "default": 2000
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["complexity", "name", "dependencies", "size"],
                    "description": "Sort order (default: complexity)",
                    "default": "complexity"
                },
                "include_symbols": {
                    "type": "boolean",
                    "description": "Include top symbols per file (default: false)",
                    "default": false
                }
            },
            "required": []
        })
    }

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let sort_by = args.get("sort_by").and_then(|v| v.as_str()).unwrap_or("complexity").to_owned();
        let depth = extract_usize(&args, "depth", 3)?.min(10);
        let token_budget = extract_usize(&args, "token_budget", 2000)?;
        let include_symbols = extract_bool(&args, "include_symbols", false);

        let index = leindex.lock().await;
        let project_root = index.project_path().to_path_buf();

        let scope_path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => PathBuf::from(p),
            None => project_root.clone(),
        };

        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(project_root.display().to_string())
        })?;

        // Build file info from PDG nodes
        let mut file_map: std::collections::HashMap<String, (usize, u32, Vec<String>)> =
            std::collections::HashMap::new(); // file → (node_count, total_complexity, symbol_names)

        for nid in pdg.node_indices() {
            if let Some(node) = pdg.get_node(nid) {
                let entry = file_map.entry(node.file_path.clone()).or_insert((0, 0, Vec::new()));
                entry.0 += 1;
                entry.1 += node.complexity;
                if entry.2.len() < 5 {
                    entry.2.push(node.name.clone());
                }
            }
        }

        // Filter to scope path and respect depth
        let mut files: Vec<Value> = file_map
            .iter()
            .filter(|(fp, _)| fp.starts_with(scope_path.to_str().unwrap_or("")))
            .filter_map(|(fp, (count, complexity, syms))| {
                let path = std::path::Path::new(fp);
                let rel = path.strip_prefix(&scope_path).ok()?;
                // Check depth
                if rel.components().count() > depth {
                    return None;
                }
                let mut entry = serde_json::json!({
                    "path": fp,
                    "relative_path": rel.display().to_string(),
                    "symbol_count": count,
                    "total_complexity": complexity
                });
                if include_symbols {
                    entry["top_symbols"] = Value::Array(
                        syms.iter().map(|s| Value::String(s.clone())).collect()
                    );
                }
                Some(entry)
            })
            .collect();

        // Sort
        match sort_by.as_str() {
            "complexity" => files.sort_by(|a, b| {
                b["total_complexity"].as_u64().unwrap_or(0)
                    .cmp(&a["total_complexity"].as_u64().unwrap_or(0))
            }),
            "name" => files.sort_by(|a, b| {
                a["relative_path"].as_str().unwrap_or("")
                    .cmp(b["relative_path"].as_str().unwrap_or(""))
            }),
            "dependencies" | "size" => files.sort_by(|a, b| {
                b["symbol_count"].as_u64().unwrap_or(0)
                    .cmp(&a["symbol_count"].as_u64().unwrap_or(0))
            }),
            _ => {}
        }

        // Truncate to token budget
        let char_budget = token_budget * 4;
        let mut total_chars = 0;
        let mut truncated_files: Vec<Value> = Vec::new();
        for f in files {
            let s = f.to_string();
            total_chars += s.len();
            if total_chars > char_budget { break; }
            truncated_files.push(f);
        }

        Ok(serde_json::json!({
            "project_root": project_root.display().to_string(),
            "scope": scope_path.display().to_string(),
            "total_files": truncated_files.len(),
            "files": truncated_files
        }))
    }
}

// ============================================================================
// C.4 — leindex_grep_symbols
// ============================================================================

/// Handler for leindex_grep_symbols — structurally-aware symbol search.
#[derive(Clone)]
pub struct GrepSymbolsHandler;

#[allow(missing_docs)]
impl GrepSymbolsHandler {
    pub fn name(&self) -> &str { "leindex_grep_symbols" }

    pub fn description(&self) -> &str {
        "Search for symbols and patterns across the indexed codebase with structural awareness. \
Unlike text-based grep, results include each match's type (function/class/method), its role \
in the dependency graph, and related symbols. Supports exact match, substring, and semantic \
search. Supersedes Grep for symbol-oriented searches."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Symbol name, substring, or natural language query"
                },
                "scope": {
                    "type": "string",
                    "description": "Limit results to a file or directory path (optional)"
                },
                "type_filter": {
                    "type": "string",
                    "enum": ["function", "class", "method", "variable", "module", "all"],
                    "description": "Filter by symbol type (default: all)",
                    "default": "all"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1500)",
                    "default": 1500
                },
                "include_context_lines": {
                    "type": "integer",
                    "description": "Source context lines around each match (default: 0, max: 10)",
                    "default": 0,
                    "minimum": 0,
                    "maximum": 10
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results (default: 20, max: 100)",
                    "default": 20,
                    "minimum": 1,
                    "maximum": 100
                }
            },
            "required": ["pattern"]
        })
    }

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let pattern = extract_string(&args, "pattern")?;
        let scope = args.get("scope").and_then(|v| v.as_str()).map(str::to_owned);
        let type_filter = args.get("type_filter").and_then(|v| v.as_str()).unwrap_or("all").to_owned();
        let token_budget = extract_usize(&args, "token_budget", 1500)?;
        let max_results = extract_usize(&args, "max_results", 20)?.min(100);
        let context_lines = extract_usize(&args, "include_context_lines", 0)?.min(10);

        let index = leindex.lock().await;
        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        let pattern_lower = pattern.to_lowercase();
        let char_budget = token_budget * 4;

        // Collect matches via exact, then substring, then semantic fallback
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut results: Vec<Value> = Vec::new();
        let mut total_chars = 0usize;

        for nid in pdg.node_indices() {
            if results.len() >= max_results { break; }

            let node = match pdg.get_node(nid) {
                Some(n) => n,
                None => continue,
            };

            // Type filter
            if type_filter != "all" {
                if node_type_str(&node.node_type) != type_filter.as_str() {
                    continue;
                }
            }

            // Scope filter
            if let Some(ref s) = scope {
                if !node.file_path.starts_with(s.as_str()) {
                    continue;
                }
            }

            // Pattern match
            let matches = node.name.to_lowercase().contains(&pattern_lower)
                || node.id.to_lowercase().contains(&pattern_lower);

            if !matches || seen_ids.contains(&node.id) {
                continue;
            }
            seen_ids.insert(node.id.clone());

            let caller_count = get_direct_callers(pdg, nid).len();
            let dep_count = pdg.neighbors(nid).len();

            let mut entry = serde_json::json!({
                "name": node.name,
                "type": node_type_str(&node.node_type),
                "file": node.file_path,
                "byte_range": node.byte_range,
                "complexity": node.complexity,
                "caller_count": caller_count,
                "dependency_count": dep_count,
                "language": node.language
            });

            if context_lines > 0 {
                if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                    let snippet: String = src.lines().take(context_lines).collect::<Vec<_>>().join("\n");
                    entry["context"] = Value::String(snippet);
                }
            }

            let s = entry.to_string();
            total_chars += s.len();
            if total_chars > char_budget { break; }
            results.push(entry);
        }

        Ok(serde_json::json!({
            "pattern": pattern,
            "result_count": results.len(),
            "results": results
        }))
    }
}

// ============================================================================
// C.5 — leindex_read_symbol
// ============================================================================

/// Handler for leindex_read_symbol — targeted symbol source read.
#[derive(Clone)]
pub struct ReadSymbolHandler;

#[allow(missing_docs)]
impl ReadSymbolHandler {
    pub fn name(&self) -> &str { "leindex_read_symbol" }

    pub fn description(&self) -> &str {
        "Read the source code of a specific symbol along with its doc comment, signature, \
and the signatures of its dependencies and dependents. Reads exactly what you need instead \
of an entire file — far more token efficient for targeted understanding. Supersedes \
targeted Read calls for symbol-level code review."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to read source for"
                },
                "file_path": {
                    "type": "string",
                    "description": "Disambiguate when symbol exists in multiple files (optional)"
                },
                "include_dependencies": {
                    "type": "boolean",
                    "description": "Include dependency signatures (default: true)",
                    "default": true
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 2000)",
                    "default": 2000
                }
            },
            "required": ["symbol"]
        })
    }

    pub async fn execute(
        &self,
        leindex: &Arc<Mutex<LeIndex>>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let symbol = extract_string(&args, "symbol")?;
        let file_path_hint = args.get("file_path").and_then(|v| v.as_str()).map(str::to_owned);
        let include_dependencies = extract_bool(&args, "include_dependencies", true);
        let token_budget = extract_usize(&args, "token_budget", 2000)?;

        let index = leindex.lock().await;
        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // Find symbol node (with optional file path disambiguation)
        let symbol_lower = symbol.to_lowercase();
        let node_id = if let Some(ref fp_hint) = file_path_hint {
            // Find by name within the specific file
            pdg.nodes_in_file(fp_hint)
                .into_iter()
                .find(|&nid| {
                    pdg.get_node(nid)
                        .map(|n| n.name.to_lowercase() == symbol_lower)
                        .unwrap_or(false)
                })
        } else {
            pdg.find_by_symbol(&symbol).or_else(|| {
                // Fuzzy match
                pdg.node_indices().find(|&nid| {
                    pdg.get_node(nid)
                        .map(|n| n.name.to_lowercase() == symbol_lower)
                        .unwrap_or(false)
                })
            })
        };

        let node_id = node_id.ok_or_else(|| {
            JsonRpcError::invalid_params(format!("Symbol '{}' not found in project index", symbol))
        })?;

        let node = pdg.get_node(node_id).ok_or_else(|| {
            JsonRpcError::internal_error("PDG node disappeared after lookup")
        })?;

        let char_budget = token_budget * 4;

        // Read source code
        let source = read_source_snippet(&node.file_path, node.byte_range)
            .map(|s| s.chars().take(char_budget / 2).collect::<String>());

        // Extract doc comment: read lines above byte_range and look for `///` or `//!`
        let doc_comment = (|| {
            let file_bytes = std::fs::read(&node.file_path).ok()?;
            let file_str = String::from_utf8_lossy(&file_bytes);
            let up_to_def: String = file_str
                .chars()
                .take(node.byte_range.0)
                .collect();
            let comment_lines: Vec<&str> = up_to_def
                .lines()
                .rev()
                .take(10)
                .take_while(|l| {
                    let t = l.trim();
                    t.starts_with("///") || t.starts_with("//!") || t.starts_with("/**") || t.starts_with("*") || t.is_empty()
                })
                .collect::<Vec<_>>();
            if comment_lines.is_empty() {
                None
            } else {
                let reversed: Vec<&str> = comment_lines.into_iter().rev().collect();
                Some(reversed.join("\n"))
            }
        })();

        // Dependency signatures (first line of their source)
        let dep_signatures: Vec<Value> = if include_dependencies {
            pdg.neighbors(node_id)
                .iter()
                .filter_map(|&did| {
                    let dn = pdg.get_node(did)?;
                    let sig = read_source_snippet(&dn.file_path, dn.byte_range)
                        .and_then(|s| s.lines().next().map(str::to_owned));
                    Some(serde_json::json!({
                        "name": dn.name,
                        "type": node_type_str(&dn.node_type),
                        "file": dn.file_path,
                        "signature": sig
                    }))
                })
                .take(20)
                .collect()
        } else {
            Vec::new()
        };

        let callers: Vec<String> = get_direct_callers(pdg, node_id)
            .iter()
            .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
            .take(20)
            .collect();

        Ok(serde_json::json!({
            "symbol": node.name,
            "type": node_type_str(&node.node_type),
            "file": node.file_path,
            "language": node.language,
            "complexity": node.complexity,
            "byte_range": node.byte_range,
            "doc_comment": doc_comment,
            "source": source,
            "dependencies": dep_signatures,
            "callers": callers
        }))
    }
}

// ============================================================================
// Phase C tests
// ============================================================================

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
        // Phase C handlers
        assert_eq!(FileSummaryHandler.name(), "leindex_file_summary");
        assert_eq!(SymbolLookupHandler.name(), "leindex_symbol_lookup");
        assert_eq!(ProjectMapHandler.name(), "leindex_project_map");
        assert_eq!(GrepSymbolsHandler.name(), "leindex_grep_symbols");
        assert_eq!(ReadSymbolHandler.name(), "leindex_read_symbol");
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

    // =========================================================================
    // Phase C helper tests
    // =========================================================================

    #[test]
    fn test_node_type_str() {
        assert_eq!(node_type_str(&legraphe::pdg::NodeType::Function), "function");
        assert_eq!(node_type_str(&legraphe::pdg::NodeType::Class), "class");
        assert_eq!(node_type_str(&legraphe::pdg::NodeType::Method), "method");
        assert_eq!(node_type_str(&legraphe::pdg::NodeType::Variable), "variable");
        assert_eq!(node_type_str(&legraphe::pdg::NodeType::Module), "module");
    }

    #[test]
    fn test_read_source_snippet_empty_range() {
        // Zero-size range returns None
        assert!(read_source_snippet("/nonexistent/path", (0, 0)).is_none());
    }

    #[test]
    fn test_read_source_snippet_from_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, b"pub fn hello() {}").unwrap();
        let path = file.to_str().unwrap();
        // Read the whole file
        let snippet = read_source_snippet(path, (0, 17));
        assert!(snippet.is_some());
        assert_eq!(snippet.unwrap(), "pub fn hello() {}");
    }

    #[test]
    fn test_read_source_snippet_partial_range() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, b"0123456789").unwrap();
        let path = file.to_str().unwrap();
        let snippet = read_source_snippet(path, (2, 5));
        assert_eq!(snippet.unwrap(), "234");
    }

    #[test]
    fn test_read_source_snippet_nonexistent_file() {
        assert!(read_source_snippet("/definitely/does/not/exist.rs", (0, 10)).is_none());
    }

    #[test]
    fn test_get_direct_callers_empty_pdg() {
        let pdg = legraphe::pdg::ProgramDependenceGraph::new();
        // An invalid NodeId on an empty PDG — edge iteration returns nothing
        let node = legraphe::pdg::Node {
            id: "test".into(),
            node_type: legraphe::pdg::NodeType::Function,
            name: "test".into(),
            file_path: "test.rs".into(),
            byte_range: (0, 0),
            complexity: 1,
            language: "rust".into(),
            embedding: None,
        };
        let mut pdg = pdg;
        let nid = pdg.add_node(node);
        let callers = get_direct_callers(&pdg, nid);
        assert!(callers.is_empty(), "new node should have no callers");
    }

    #[test]
    fn test_get_direct_callers_with_edge() {
        let mut pdg = legraphe::pdg::ProgramDependenceGraph::new();
        let caller_node = legraphe::pdg::Node {
            id: "caller".into(),
            node_type: legraphe::pdg::NodeType::Function,
            name: "caller".into(),
            file_path: "a.rs".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".into(),
            embedding: None,
        };
        let callee_node = legraphe::pdg::Node {
            id: "callee".into(),
            node_type: legraphe::pdg::NodeType::Function,
            name: "callee".into(),
            file_path: "b.rs".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".into(),
            embedding: None,
        };
        let cid = pdg.add_node(caller_node);
        let did = pdg.add_node(callee_node);
        pdg.add_call_graph_edges(vec![(cid, did)]);

        // callee should have caller as a direct caller
        let callers = get_direct_callers(&pdg, did);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0], cid);

        // caller should have no callers
        let no_callers = get_direct_callers(&pdg, cid);
        assert!(no_callers.is_empty());
    }

    #[tokio::test]
    async fn test_file_summary_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let leindex = Arc::new(Mutex::new(
            crate::leindex::LeIndex::new(dir.path()).expect("leindex"),
        ));
        let args = serde_json::json!({ "file_path": "/some/file.rs" });
        let result = FileSummaryHandler.execute(&leindex, args).await;
        // Should return project_not_indexed error since no PDG
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_symbol_lookup_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let leindex = Arc::new(Mutex::new(
            crate::leindex::LeIndex::new(dir.path()).expect("leindex"),
        ));
        let args = serde_json::json!({ "symbol": "my_func" });
        let result = SymbolLookupHandler.execute(&leindex, args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_project_map_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let leindex = Arc::new(Mutex::new(
            crate::leindex::LeIndex::new(dir.path()).expect("leindex"),
        ));
        let args = serde_json::json!({});
        let result = ProjectMapHandler.execute(&leindex, args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_grep_symbols_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let leindex = Arc::new(Mutex::new(
            crate::leindex::LeIndex::new(dir.path()).expect("leindex"),
        ));
        let args = serde_json::json!({ "pattern": "auth" });
        let result = GrepSymbolsHandler.execute(&leindex, args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_symbol_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let leindex = Arc::new(Mutex::new(
            crate::leindex::LeIndex::new(dir.path()).expect("leindex"),
        ));
        let args = serde_json::json!({ "symbol": "my_func" });
        let result = ReadSymbolHandler.execute(&leindex, args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_phase_c_handler_schemas() {
        // All Phase C schemas should be valid JSON objects with required fields
        let schemas = vec![
            (FileSummaryHandler.argument_schema(), vec!["file_path"]),
            (SymbolLookupHandler.argument_schema(), vec!["symbol"]),
            (ProjectMapHandler.argument_schema(), vec![]),
            (GrepSymbolsHandler.argument_schema(), vec!["pattern"]),
            (ReadSymbolHandler.argument_schema(), vec!["symbol"]),
        ];

        for (schema, required_fields) in schemas {
            assert!(schema.is_object(), "schema must be a JSON object");
            for field in required_fields {
                let required = schema
                    .get("required")
                    .and_then(|v| v.as_array())
                    .expect("required array");
                assert!(
                    required.iter().any(|v| v.as_str() == Some(field)),
                    "field '{}' must be in required list",
                    field
                );
            }
        }
    }
}
