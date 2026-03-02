// MCP Tool Handlers
//
// This module implements the handlers for each MCP tool that the server exposes.

use super::protocol::JsonRpcError;
#[cfg(test)]
use crate::leindex::LeIndex;
use crate::registry::ProjectRegistry;
use leedit::EditChange;
use lephase::{run_phase_analysis, DocsMode, FormatMode, PhaseOptions, PhaseSelection};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

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
    // Phase D: Context-Aware Editing
    /// Handler for edit preview with impact analysis
    EditPreview(EditPreviewHandler),
    /// Handler for applying edits to files
    EditApply(EditApplyHandler),
    /// Handler for cross-file symbol rename
    RenameSymbol(RenameSymbolHandler),
    /// Handler for transitive dependency impact analysis
    ImpactAnalysis(ImpactAnalysisHandler),
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
            ToolHandler::EditPreview(h) => h.name(),
            ToolHandler::EditApply(h) => h.name(),
            ToolHandler::RenameSymbol(h) => h.name(),
            ToolHandler::ImpactAnalysis(h) => h.name(),
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
            ToolHandler::EditPreview(h) => h.description(),
            ToolHandler::EditApply(h) => h.description(),
            ToolHandler::RenameSymbol(h) => h.description(),
            ToolHandler::ImpactAnalysis(h) => h.description(),
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
            ToolHandler::EditPreview(h) => h.argument_schema(),
            ToolHandler::EditApply(h) => h.argument_schema(),
            ToolHandler::RenameSymbol(h) => h.argument_schema(),
            ToolHandler::ImpactAnalysis(h) => h.argument_schema(),
        }
    }

    /// Execute the tool
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        match self {
            ToolHandler::Index(h) => h.execute(registry, args).await,
            ToolHandler::Search(h) => h.execute(registry, args).await,
            ToolHandler::DeepAnalyze(h) => h.execute(registry, args).await,
            ToolHandler::Context(h) => h.execute(registry, args).await,
            ToolHandler::Diagnostics(h) => h.execute(registry, args).await,
            ToolHandler::PhaseAnalysis(h) => h.execute(registry, args).await,
            ToolHandler::PhaseAnalysisAlias(h) => h.execute(registry, args).await,
            ToolHandler::FileSummary(h) => h.execute(registry, args).await,
            ToolHandler::SymbolLookup(h) => h.execute(registry, args).await,
            ToolHandler::ProjectMap(h) => h.execute(registry, args).await,
            ToolHandler::GrepSymbols(h) => h.execute(registry, args).await,
            ToolHandler::ReadSymbol(h) => h.execute(registry, args).await,
            ToolHandler::EditPreview(h) => h.execute(registry, args).await,
            ToolHandler::EditApply(h) => h.execute(registry, args).await,
            ToolHandler::RenameSymbol(h) => h.execute(registry, args).await,
            ToolHandler::ImpactAnalysis(h) => h.execute(registry, args).await,
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

/// Helper to extract usize argument with default.
///
/// Accepts native JSON numbers **and** string representations (`"10"`, `"200"`)
/// for robustness against LLM clients that serialise integers as strings.
fn extract_usize(args: &Value, key: &str, default: usize) -> Result<usize, JsonRpcError> {
    match args.get(key) {
        Some(Value::Number(n)) => Ok(n.as_u64().map(|v| v as usize).unwrap_or(default)),
        Some(Value::String(s)) => s.trim().parse::<usize>().or(Ok(default)),
        _ => Ok(default),
    }
}

/// Helper to extract bool argument with default.
///
/// Accepts both native JSON booleans (`true`/`false`) and string representations
/// (`"true"`, `"false"`, `"1"`, `"0"`, `"yes"`, `"no"`).  This makes the API
/// robust against LLM clients that serialize booleans as strings.
fn extract_bool(args: &Value, key: &str, default: bool) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => default,
        },
        Some(Value::Number(n)) => n.as_u64().map(|v| v != 0).unwrap_or(default),
        _ => default,
    }
}

// `ensure_project_ready` is replaced by `ProjectRegistry::get_or_create()`.

/// Validate that a file path resides within the project root.
///
/// Returns `Ok(canonical_path)` if the file is within bounds, or an error
/// describing the boundary violation.
fn validate_file_within_project(
    file_path: &str,
    project_root: &std::path::Path,
) -> Result<PathBuf, JsonRpcError> {
    let canonical = std::path::Path::new(file_path)
        .canonicalize()
        .map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot resolve file path '{}': {}", file_path, e))
        })?;
    if !canonical.starts_with(project_root) {
        return Err(JsonRpcError::invalid_params(format!(
            "File '{}' is outside the project boundary '{}'",
            file_path,
            project_root.display()
        )));
    }
    Ok(canonical)
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
            let index = handle.lock().await;
            if index.is_indexed() {
                return serde_json::to_value(index.get_stats()).map_err(|e| {
                    JsonRpcError::internal_error(format!("Serialization error: {}", e))
                });
            }
        }

        let stats = registry
            .index_project(Some(&project_path), force_reindex)
            .await?;

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
        "Semantic code search. Finds symbols by meaning, not just name. Returns ranked \
results with composite scores (semantic + text + structural). Accepts project_path \
to auto-switch/auto-index projects."
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
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 10)",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 100
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
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
        let top_k = extract_usize(&args, "top_k", 10)?;
        let offset = extract_usize(&args, "offset", 0)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.lock().await;

        if index.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                index.project_path().display().to_string(),
            ));
        }

        // Fetch top_k + offset results, then slice for pagination
        let all_results = index
            .search(&query, top_k + offset)
            .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;

        let page: Vec<_> = all_results.into_iter().skip(offset).collect();
        let total_returned = page.len();

        Ok(serde_json::json!({
            "results": serde_json::to_value(&page).map_err(|e|
                JsonRpcError::internal_error(format!("Serialization error: {}", e)))?,
            "offset": offset,
            "count": total_returned,
            "has_more": total_returned == top_k,
            "scoring": "Each result has a composite score (0.0–1.0): 0.5×semantic (TF-IDF cosine similarity) + 0.3×text_match (token overlap) + 0.2×structural (PDG centrality)."
        }))
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
        "Deep analysis: semantic search + PDG traversal for definition, callers, callees, \
data flow, and impact radius. Supersedes Grep + multiple Read. Accepts project_path \
to auto-switch/auto-index projects."
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
        let mut writer = handle.lock().await;

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
        let reader = handle.lock().await;

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
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        execute_phase_analysis(registry, args).await
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
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        execute_phase_analysis(registry, args).await
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
            "project_path": {
                "type": "string",
                "description": "Project directory (auto-indexes on first use; omit to use current project)"
            },
            "mode": {
                "type": "string",
                "enum": ["ultra", "balanced", "verbose"],
                "default": "balanced"
            },
            "path": {
                "type": "string",
                "description": "File or directory to analyze (defaults to project root)"
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
                "description": "Include Markdown/Text docs in phase analysis (default: false). \
    Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
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
    registry: &Arc<ProjectRegistry>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let project_path = args.get("project_path").and_then(|v| v.as_str());
    let handle = registry.get_or_create(project_path).await?;

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
                "Use phase: 1..5, phase: \"1\"..\"5\", or phase: \"all\" (default)".to_string(),
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
        let reader = handle.lock().await;
        reader.project_path().to_path_buf()
    };

    let canonical_target = match args.get("path").and_then(|v| v.as_str()) {
        Some(path) => PathBuf::from(path).canonicalize().map_err(|e| {
            JsonRpcError::invalid_params(format!("path must exist and be accessible: {}", e))
        })?,
        None => base_project_root.clone(),
    };

    // Keep a clone for the C.7 single-file deep-dive enrichment below.
    let single_file_target: Option<PathBuf> = if canonical_target.is_file() {
        Some(canonical_target.clone())
    } else {
        None
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

    // ── Single-file deep dive (Task C.7) ────────────────────────────────────
    // When `path` is a file, augment the phase report with per-symbol PDG data:
    // signature, line range, complexity, caller_count, cross-file deps.
    let file_symbols_json: Option<Vec<serde_json::Value>> =
        if let Some(ref file_path) = single_file_target {
            // Read the file for byte→line conversion (single file, cheap).
            let file_content = std::fs::read_to_string(file_path).unwrap_or_default();

            // Build line-start offsets for O(log N) byte→line lookups.
            let mut line_starts = vec![0usize];
            for (i, &b) in file_content.as_bytes().iter().enumerate() {
                if b == b'\n' {
                    line_starts.push(i + 1);
                }
            }
            let byte_to_line = |byte: usize| -> usize {
                // Returns 1-indexed line number
                line_starts.partition_point(|&s| s <= byte)
            };

            let file_path_str = file_path.to_string_lossy().to_string();
            let reader = handle.lock().await;
            if let Some(pdg) = reader.pdg() {
                let node_ids = pdg.nodes_in_file(&file_path_str);
                let mut symbols: Vec<serde_json::Value> = node_ids
                    .iter()
                    .filter_map(|&node_idx| {
                        let node = pdg.get_node(node_idx)?;
                        let (start_byte, end_byte) = node.byte_range;
                        let line_start = byte_to_line(start_byte);
                        let line_end = byte_to_line(end_byte.saturating_sub(1));

                        // Signature: first non-empty line at the node's byte offset.
                        let signature: Option<String> = if start_byte < file_content.len() {
                            file_content[start_byte..]
                                .lines()
                                .next()
                                .map(|l| l.trim().to_string())
                                .filter(|l| !l.is_empty() && !l.starts_with("// ["))
                        } else {
                            None
                        };

                        // Cross-file outgoing dependencies (calls/imports to other files).
                        let cross_file_deps: Vec<serde_json::Value> = pdg
                            .neighbors(node_idx)
                            .iter()
                            .filter_map(|&dep_idx| pdg.get_node(dep_idx))
                            .filter(|dep| dep.file_path != node.file_path)
                            .map(|dep| {
                                serde_json::json!({
                                    "name": dep.name,
                                    "file": dep.file_path,
                                    "type": format!("{:?}", dep.node_type).to_lowercase(),
                                })
                            })
                            .collect();

                        let symbol_type = format!("{:?}", node.node_type).to_lowercase();

                        Some(serde_json::json!({
                            "name": node.name,
                            "symbol_type": symbol_type,
                            "signature": signature,
                            "line_start": line_start,
                            "line_end": line_end,
                            "complexity": node.complexity,
                            "caller_count": pdg.predecessor_count(node_idx),
                            "dependency_count": pdg.neighbors(node_idx).len(),
                            "cross_file_deps": cross_file_deps,
                        }))
                    })
                    .collect();

                // Sort by line_start so the LLM reads top-to-bottom.
                symbols.sort_by_key(|s| s["line_start"].as_u64().unwrap_or(0));
                Some(symbols)
            } else {
                // PDG not available (project not yet indexed) — skip enrichment.
                None
            }
        } else {
            None
        };

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

    let mut report_value = serde_json::to_value(report)
        .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?;

    // Merge per-symbol data into the response when available.
    if let Some(symbols) = file_symbols_json {
        if let serde_json::Value::Object(ref mut map) = report_value {
            map.insert("file_symbols".to_string(), serde_json::json!(symbols));
        }
    }

    Ok(report_value)
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
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let reader = handle.lock().await;

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

/// Collect the NodeIds of all nodes that have a direct edge pointing *to*
/// `target_id` (i.e. the callers / direct dependents).
///
/// Uses petgraph's `neighbors_directed(Incoming)` for O(degree) performance
/// instead of the previous O(E) full-edge scan.
fn get_direct_callers(
    pdg: &legraphe::pdg::ProgramDependenceGraph,
    target_id: legraphe::pdg::NodeId,
) -> Vec<legraphe::pdg::NodeId> {
    pdg.predecessors(target_id)
}

// ============================================================================
// C.1 — leindex_file_summary
// ============================================================================

/// Handler for leindex_file_summary — structured file analysis replacing Read.
#[derive(Clone)]
pub struct FileSummaryHandler;

#[allow(missing_docs)]
impl FileSummaryHandler {
    pub fn name(&self) -> &str {
        "leindex_file_summary"
    }

    pub fn description(&self) -> &str {
        "Structural analysis of a file: all symbols, signatures, complexity scores, \
cross-file deps/dependents, and module role. 5-10x more token efficient than Read — \
returns what you need to understand a file without reading raw content. Includes \
cross-file relationships that Read cannot provide."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to analyze"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1000)",
                    "default": 1000
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include source snippets for key symbols (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
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
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let file_path = extract_string(&args, "file_path")?;
        let include_source = extract_bool(&args, "include_source", false);
        let focus_symbol = args
            .get("focus_symbol")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let token_budget = extract_usize(&args, "token_budget", 1000)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;

        // Enforce project boundary
        let _ = validate_file_within_project(&file_path, index.project_path())?;

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
                "byte_range": node.byte_range,
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
            format!(
                "Class definitions ({} classes, {} functions)",
                class_count, func_count
            )
        } else {
            format!(
                "Function module ({} functions, {} classes)",
                func_count, class_count
            )
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
    pub fn name(&self) -> &str {
        "leindex_symbol_lookup"
    }

    pub fn description(&self) -> &str {
        "Look up a symbol and get its full structural context: definition, signature, callers, \
callees, data dependencies, and impact radius. Replaces Grep + multiple Read calls with \
a single structured response including cross-file relationships."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to look up (single symbol)"
                },
                "symbols": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Batch mode: look up multiple symbols in one call (max 20)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1500)",
                    "default": 1500
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include source code of definition (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "include_callers": {
                    "type": "boolean",
                    "description": "Include callers (default: true). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": true
                },
                "include_callees": {
                    "type": "boolean",
                    "description": "Include callees (default: true). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
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
            "required": []
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let include_source = extract_bool(&args, "include_source", false);
        let include_callers = extract_bool(&args, "include_callers", true);
        let include_callees = extract_bool(&args, "include_callees", true);
        let depth = extract_usize(&args, "depth", 2)?.min(5);
        let token_budget = extract_usize(&args, "token_budget", 1500)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        // Determine symbol list: single "symbol" or batch "symbols"
        let symbols: Vec<String> = if let Some(arr) = args.get("symbols").and_then(|v| v.as_array())
        {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .take(20)
                .collect()
        } else if let Ok(sym) = extract_string(&args, "symbol") {
            vec![sym]
        } else {
            return Err(JsonRpcError::invalid_params(
                "Provide either 'symbol' (string) or 'symbols' (array of strings)".to_string(),
            ));
        };

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;

        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // For batch mode, collect results for each symbol
        if symbols.len() > 1 {
            let char_budget = token_budget * 4;
            let per_symbol_budget = char_budget / symbols.len();
            let mut results: Vec<Value> = Vec::new();

            for symbol in &symbols {
                match self.lookup_single_symbol(
                    pdg,
                    symbol,
                    include_source,
                    include_callers,
                    include_callees,
                    depth,
                    per_symbol_budget,
                ) {
                    Ok(val) => results.push(val),
                    Err(e) => results.push(serde_json::json!({
                        "symbol": symbol,
                        "error": format!("{}", e)
                    })),
                }
            }

            return Ok(serde_json::json!({
                "batch": true,
                "count": results.len(),
                "results": results
            }));
        }

        // Single symbol mode
        let char_budget = token_budget * 4;
        self.lookup_single_symbol(
            pdg,
            &symbols[0],
            include_source,
            include_callers,
            include_callees,
            depth,
            char_budget,
        )
    }

    /// Resolve and return full structural context for a single symbol.
    fn lookup_single_symbol(
        &self,
        pdg: &legraphe::pdg::ProgramDependenceGraph,
        symbol: &str,
        include_source: bool,
        include_callers: bool,
        include_callees: bool,
        depth: usize,
        char_budget: usize,
    ) -> Result<Value, JsonRpcError> {
        // 1. Exact symbol lookup (by node.id in symbol_index)
        let node_id = if let Some(nid) = pdg.find_by_symbol(symbol) {
            nid
        // 2. Exact name lookup (by node.name in name_index) — prefer non-module nodes
        } else if let Some(nid) = {
            let candidates = pdg.find_all_by_name(symbol);
            // Prefer class/function/method over module nodes
            candidates
                .iter()
                .copied()
                .find(|&nid| {
                    pdg.get_node(nid)
                        .map(|n| n.node_type != legraphe::pdg::NodeType::Module)
                        .unwrap_or(false)
                })
                .or_else(|| candidates.first().copied())
        } {
            nid
        } else {
            // 3. Fuzzy match: substring, case-insensitive — prefer non-module nodes
            let sym_lower = symbol.to_lowercase();
            let mut best: Option<legraphe::pdg::NodeId> = None;
            let mut best_is_module = true;
            for nid in pdg.node_indices() {
                let Some(n) = pdg.get_node(nid) else { continue };
                let matches = n.name.to_lowercase().contains(&sym_lower)
                    || n.id.to_lowercase().contains(&sym_lower);
                if !matches {
                    continue;
                }
                let is_module = n.node_type == legraphe::pdg::NodeType::Module;
                // Always prefer non-module; only accept module if it's the first match
                if best.is_none() || (best_is_module && !is_module) {
                    best = Some(nid);
                    best_is_module = is_module;
                    if !is_module {
                        break;
                    } // non-module is best, stop early
                }
            }
            best.ok_or_else(|| {
                JsonRpcError::invalid_params_with_suggestion(
                    format!("Symbol '{}' not found in project index", symbol),
                    "Check spelling, or use leindex_grep_symbols to search for partial matches",
                )
            })?
        };

        let node = pdg
            .get_node(node_id)
            .ok_or_else(|| JsonRpcError::internal_error("PDG node disappeared after lookup"))?;

        // Callees (direct)
        let callees: Vec<Value> = if include_callees {
            pdg.neighbors(node_id)
                .iter()
                .filter_map(|&cid| {
                    pdg.get_node(cid).map(|cn| {
                        serde_json::json!({
                            "name": cn.name,
                            "file": cn.file_path,
                            "type": node_type_str(&cn.node_type)
                        })
                    })
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
                    pdg.get_node(cid).map(|cn| {
                        serde_json::json!({
                            "name": cn.name,
                            "file": cn.file_path,
                            "type": node_type_str(&cn.node_type)
                        })
                    })
                })
                .take(50)
                .collect()
        } else {
            Vec::new()
        };

        // Forward impact (depth-bounded transitive dependents)
        let forward = pdg.get_forward_impact_bounded(node_id, depth);
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
    pub fn name(&self) -> &str {
        "leindex_project_map"
    }

    pub fn description(&self) -> &str {
        "Annotated project structure map: files, directories, symbol counts, complexity \
hotspots, and inter-module dependency arrows. Unlike Glob's flat file lists, shows \
architecture — which modules depend on which and where complexity lives. 5x more \
token efficient than Glob + directory reads."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Subdirectory to scope to (default: project root)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
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
                    "description": "Include top symbols per file (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N files for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of files to return (default: unlimited, subject to token_budget)",
                    "minimum": 1
                }
            },
            "required": []
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let sort_by = args
            .get("sort_by")
            .and_then(|v| v.as_str())
            .unwrap_or("complexity")
            .to_owned();
        let depth = extract_usize(&args, "depth", 3)?.min(10);
        let token_budget = extract_usize(&args, "token_budget", 2000)?;
        let include_symbols = extract_bool(&args, "include_symbols", false);
        let offset = extract_usize(&args, "offset", 0)?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;
        let project_root = index.project_path().to_path_buf();

        let scope_path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => {
                // Canonicalize if possible, fall back to raw path
                PathBuf::from(p)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(p))
            }
            None => project_root.clone(),
        };

        // Ensure scope_path string ends with separator for proper prefix matching
        // This prevents "/path/src" from matching "/path/src_backup/file.py"
        let scope_str = {
            let s = scope_path.to_string_lossy().to_string();
            if s.ends_with(std::path::MAIN_SEPARATOR) || s.ends_with('/') {
                s
            } else {
                format!("{}{}", s, std::path::MAIN_SEPARATOR)
            }
        };

        let pdg = index
            .pdg()
            .ok_or_else(|| JsonRpcError::project_not_indexed(project_root.display().to_string()))?;

        // Build file info from PDG nodes
        let mut file_map: std::collections::HashMap<String, (usize, u32, Vec<String>)> =
            std::collections::HashMap::new(); // file → (node_count, total_complexity, symbol_names)

        for nid in pdg.node_indices() {
            if let Some(node) = pdg.get_node(nid) {
                let entry = file_map
                    .entry(node.file_path.clone())
                    .or_insert((0, 0, Vec::new()));
                entry.0 += 1;
                entry.1 += node.complexity;
                if entry.2.len() < 5 {
                    entry.2.push(node.name.clone());
                }
            }
        }

        // Filter to scope path and respect depth.
        // Files must either be exactly in the scope directory or in a subdirectory.
        let mut files: Vec<Value> = file_map
            .iter()
            .filter(|(fp, _)| {
                // File is in scope if its path starts with scope_str (directory prefix)
                // or if the file IS the scope path (exact match for single-file scope)
                fp.starts_with(&scope_str) || fp.as_str() == scope_path.to_str().unwrap_or("")
            })
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
                    entry["top_symbols"] =
                        Value::Array(syms.iter().map(|s| Value::String(s.clone())).collect());
                }
                Some(entry)
            })
            .collect();

        // Sort
        match sort_by.as_str() {
            "complexity" => files.sort_by(|a, b| {
                b["total_complexity"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&a["total_complexity"].as_u64().unwrap_or(0))
            }),
            "name" => files.sort_by(|a, b| {
                a["relative_path"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["relative_path"].as_str().unwrap_or(""))
            }),
            "dependencies" | "size" => files.sort_by(|a, b| {
                b["symbol_count"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&a["symbol_count"].as_u64().unwrap_or(0))
            }),
            _ => {}
        }

        // Apply pagination: offset + limit
        let total_before_pagination = files.len();
        let files: Vec<Value> = files.into_iter().skip(offset).collect();
        let files: Vec<Value> = if let Some(lim) = limit {
            files.into_iter().take(lim).collect()
        } else {
            files
        };

        // Truncate to token budget
        let char_budget = token_budget * 4;
        let mut total_chars = 0;
        let mut truncated_files: Vec<Value> = Vec::new();
        for f in files {
            let s = f.to_string();
            total_chars += s.len();
            if total_chars > char_budget {
                break;
            }
            truncated_files.push(f);
        }

        Ok(serde_json::json!({
            "project_root": project_root.display().to_string(),
            "scope": scope_path.display().to_string(),
            "total_files_in_scope": total_before_pagination,
            "offset": offset,
            "count": truncated_files.len(),
            "has_more": offset + truncated_files.len() < total_before_pagination,
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
    pub fn name(&self) -> &str {
        "leindex_grep_symbols"
    }

    pub fn description(&self) -> &str {
        "Search for symbols across the indexed codebase with structural awareness. Unlike \
text-based grep, results include each match's type (function/class/method) and its role \
in the dependency graph. Supports exact match and substring search."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Symbol name or substring to search for"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
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
                    "description": "Maximum results (default: 20, max: 200)",
                    "default": 20,
                    "minimum": 1,
                    "maximum": 200
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                }
            },
            "required": ["pattern"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let pattern = extract_string(&args, "pattern")?;
        let scope_raw = args
            .get("scope")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let type_filter = args
            .get("type_filter")
            .and_then(|v| v.as_str())
            .unwrap_or("all")
            .to_owned();
        let token_budget = extract_usize(&args, "token_budget", 1500)?;
        let max_results = extract_usize(&args, "max_results", 20)?.min(200);
        let context_lines = extract_usize(&args, "include_context_lines", 0)?.min(10);
        let offset = extract_usize(&args, "offset", 0)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.lock().await;

        // Validate and canonicalise scope path if provided
        let scope = if let Some(raw) = scope_raw {
            let p = std::path::Path::new(&raw);
            let canonical = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
            let s = canonical.to_string_lossy().to_string();
            // Ensure directory scopes end with separator for correct prefix matching
            if canonical.is_dir() && !s.ends_with(std::path::MAIN_SEPARATOR) && !s.ends_with('/') {
                Some(format!("{}{}", s, std::path::MAIN_SEPARATOR))
            } else {
                Some(s)
            }
        } else {
            None
        };

        // Use indexed search to pre-filter candidates (avoids full O(N) PDG scans).
        let candidate_limit = (max_results + offset).saturating_mul(5).max(50);
        let candidate_results = index
            .search(&pattern, candidate_limit)
            .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;

        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        let pattern_lower = pattern.to_lowercase();
        let char_budget = token_budget * 4;

        // Collect matches from pre-filtered candidates
        // Fetch max_results + offset matches, then paginate
        let fetch_limit = max_results + offset;
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut all_matches: Vec<Value> = Vec::new();

        for sr in candidate_results {
            if all_matches.len() >= fetch_limit {
                break;
            }

            let Some(nid) = pdg.find_by_id(&sr.node_id) else {
                continue;
            };
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

            // Scope filter — file path must start with the (canonical, separator-terminated) scope
            if let Some(ref s) = scope {
                if !node.file_path.starts_with(s.as_str())
                    && node.file_path != s.trim_end_matches(std::path::MAIN_SEPARATOR)
                {
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
                    let snippet: String = src
                        .lines()
                        .take(context_lines)
                        .collect::<Vec<_>>()
                        .join("\n");
                    entry["context"] = Value::String(snippet);
                }
            }

            all_matches.push(entry);
        }

        // Apply pagination
        let total_matched = all_matches.len();
        let page: Vec<Value> = all_matches.into_iter().skip(offset).collect();

        // Truncate to token budget
        let mut results: Vec<Value> = Vec::new();
        let mut total_chars = 0usize;
        for entry in page {
            let s = entry.to_string();
            total_chars += s.len();
            if total_chars > char_budget {
                break;
            }
            results.push(entry);
        }

        Ok(serde_json::json!({
            "pattern": pattern,
            "offset": offset,
            "count": results.len(),
            "total_matched": total_matched,
            "has_more": offset + results.len() < total_matched,
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
    pub fn name(&self) -> &str {
        "leindex_read_symbol"
    }

    pub fn description(&self) -> &str {
        "Read the source code of a specific symbol with its doc comment and the signatures \
of its dependencies and dependents. Reads exactly what you need — far more token efficient \
than reading an entire file. Supersedes targeted Read."
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
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "include_dependencies": {
                    "type": "boolean",
                    "description": "Include dependency signatures (default: true). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
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
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let symbol = extract_string(&args, "symbol")?;
        let file_path_hint = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let include_dependencies = extract_bool(&args, "include_dependencies", true);
        let token_budget = extract_usize(&args, "token_budget", 2000)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;
        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // Find symbol node (with optional file path disambiguation)
        let symbol_lower = symbol.to_lowercase();
        let node_id = if let Some(ref fp_hint) = file_path_hint {
            // Find by name within the specific file
            pdg.nodes_in_file(fp_hint).into_iter().find(|&nid| {
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

        let node = pdg
            .get_node(node_id)
            .ok_or_else(|| JsonRpcError::internal_error("PDG node disappeared after lookup"))?;

        let char_budget = token_budget * 4;

        // Read source code
        let source = read_source_snippet(&node.file_path, node.byte_range)
            .map(|s| s.chars().take(char_budget / 2).collect::<String>());

        // Extract doc comment: read lines above byte_range and look for `///`, `//!`,
        // or proper `/** ... */` blocks only.
        let doc_comment = (|| {
            let file_bytes = std::fs::read(&node.file_path).ok()?;
            // Use byte slicing (not char counting) to correctly handle multi-byte UTF-8
            let end = node.byte_range.0.min(file_bytes.len());
            let up_to_def = String::from_utf8_lossy(&file_bytes[..end]).into_owned();
            let mut comment_lines: Vec<&str> = Vec::new();
            let mut in_doc_block = false;
            let mut saw_doc_start = false;

            for line in up_to_def.lines().rev().take(20) {
                let t = line.trim_start();

                if t.starts_with("///") || t.starts_with("//!") {
                    comment_lines.push(line);
                    continue;
                }

                if t.starts_with("*/") {
                    in_doc_block = true;
                    comment_lines.push(line);
                    continue;
                }

                if in_doc_block {
                    if t.starts_with("/**") {
                        saw_doc_start = true;
                        comment_lines.push(line);
                        in_doc_block = false;
                        continue;
                    }

                    // Keep only canonical inner doc-block lines.
                    if t.starts_with('*') || t.is_empty() {
                        comment_lines.push(line);
                        continue;
                    }

                    // Non-doc block comment (`/*`) or code => reject block capture.
                    comment_lines.clear();
                    break;
                }

                if t.is_empty() && !comment_lines.is_empty() {
                    comment_lines.push(line);
                    continue;
                }

                break;
            }

            if in_doc_block
                || (!saw_doc_start
                    && comment_lines
                        .iter()
                        .any(|l| l.trim_start().starts_with("*/")))
            {
                comment_lines.clear();
            }

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
// Phase D: Tool Supremacy — Context-Aware Editing
// ============================================================================

/// Parse a JSON `changes` array into a Vec<EditChange>.
///
/// Supports two modes for `replace_text`:
/// - **Byte-offset mode**: When `start_byte` and `end_byte` are provided, uses exact offsets.
/// - **Text-search mode**: When `old_text` is provided without explicit offsets, searches
///   for `old_text` in the file content and replaces at the found position.
///
/// The `content` parameter enables text-search mode resolution.
fn parse_edit_changes(
    changes_val: &Value,
    content: Option<&str>,
) -> Result<Vec<EditChange>, JsonRpcError> {
    let arr = changes_val
        .as_array()
        .ok_or_else(|| JsonRpcError::invalid_params("'changes' must be an array"))?;

    let mut result = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let change_type = item.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
            JsonRpcError::invalid_params(format!("changes[{}]: missing 'type'", i))
        })?;

        let change = match change_type {
            "replace_text" => {
                let old_text = item.get("old_text").and_then(|v| v.as_str()).unwrap_or("");
                let new_text = item
                    .get("new_text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        JsonRpcError::invalid_params(format!("changes[{}]: missing 'new_text'", i))
                    })?;

                let has_explicit_start = item.get("start_byte").is_some();
                let has_explicit_end = item.get("end_byte").is_some();

                if has_explicit_start || has_explicit_end {
                    // Byte-offset mode: use exact offsets
                    let start =
                        item.get("start_byte").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let end = item
                        .get("end_byte")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as usize)
                        .unwrap_or(start + old_text.len());
                    EditChange::ReplaceText {
                        start,
                        end,
                        new_text: new_text.to_owned(),
                    }
                } else if !old_text.is_empty() {
                    // Text-search mode: find old_text in content and use its position.
                    // Falls back to whitespace-normalised matching if exact match fails.
                    if let Some(content) = content {
                        if let Some(pos) = content.find(old_text) {
                            EditChange::ReplaceText {
                                start: pos,
                                end: pos + old_text.len(),
                                new_text: new_text.to_owned(),
                            }
                        } else if let Some((pos, matched_len)) =
                            find_normalised_whitespace(content, old_text)
                        {
                            // Whitespace-normalised match: the old_text matched after
                            // collapsing runs of whitespace to single spaces.  Use the
                            // original content's byte range so the replacement is exact.
                            EditChange::ReplaceText {
                                start: pos,
                                end: pos + matched_len,
                                new_text: new_text.to_owned(),
                            }
                        } else {
                            let preview = if old_text.len() > 60 {
                                format!("{}...", &old_text[..60])
                            } else {
                                old_text.to_string()
                            };
                            return Err(JsonRpcError::invalid_params_with_suggestion(
                                format!("changes[{}]: old_text not found in file content: '{}'", i, preview),
                                "Ensure old_text exactly matches the source. Whitespace-normalised matching \
                                 is attempted automatically. You can also use start_byte/end_byte for precise offsets.",
                            ));
                        }
                    } else {
                        // No content available — fall back to zero-based offset (legacy)
                        let start = 0usize;
                        let end = old_text.len();
                        EditChange::ReplaceText {
                            start,
                            end,
                            new_text: new_text.to_owned(),
                        }
                    }
                } else {
                    return Err(JsonRpcError::invalid_params(format!(
                        "changes[{}]: replace_text requires either 'start_byte'/'end_byte' or non-empty 'old_text'", i
                    )));
                }
            }
            "rename_symbol" => {
                let old_name = item
                    .get("old_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        JsonRpcError::invalid_params(format!("changes[{}]: missing 'old_name'", i))
                    })?;
                let new_name = item
                    .get("new_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        JsonRpcError::invalid_params(format!("changes[{}]: missing 'new_name'", i))
                    })?;
                EditChange::RenameSymbol {
                    old_name: old_name.to_owned(),
                    new_name: new_name.to_owned(),
                }
            }
            other => {
                return Err(JsonRpcError::invalid_params(format!(
                    "changes[{}]: unknown type '{}' (use replace_text or rename_symbol)",
                    i, other
                )));
            }
        };
        result.push(change);
    }
    Ok(result)
}

/// Apply a Vec<EditChange> to content in memory and return the modified string.
///
/// ReplaceText changes are sorted in reverse order by start offset so that
/// earlier changes don't invalidate later changes' byte positions.
fn apply_changes_in_memory(content: &str, changes: &[EditChange]) -> Result<String, JsonRpcError> {
    // Separate replace_text (byte-offset–sensitive) from other change types
    let mut replace_changes: Vec<&EditChange> = Vec::new();
    let mut other_changes: Vec<&EditChange> = Vec::new();
    for change in changes {
        match change {
            EditChange::ReplaceText { .. } => replace_changes.push(change),
            _ => other_changes.push(change),
        }
    }

    // Sort byte-range replacements in reverse order so earlier edits don't
    // shift later offsets.
    replace_changes.sort_by(|a, b| {
        let a_start = match a {
            EditChange::ReplaceText { start, .. } => *start,
            _ => 0,
        };
        let b_start = match b {
            EditChange::ReplaceText { start, .. } => *start,
            _ => 0,
        };
        b_start.cmp(&a_start)
    });

    let mut modified = content.to_owned();

    // Apply byte-range replacements first (in reverse order)
    for change in &replace_changes {
        if let EditChange::ReplaceText {
            start,
            end,
            new_text,
        } = change
        {
            let bytes = modified.as_bytes();
            let s = (*start).min(bytes.len());
            let e = (*end).min(bytes.len());
            modified = format!("{}{}{}", &modified[..s], new_text, &modified[e..]);
        }
    }

    // Then apply rename / other changes
    for change in &other_changes {
        modified = match change {
            EditChange::RenameSymbol { old_name, new_name } => {
                replace_whole_word(&modified, old_name, new_name)
            }
            _ => modified,
        };
    }

    Ok(modified)
}

/// Check if a byte is a word character (alphanumeric or underscore).
/// Replace `old` with `new` only at word boundaries.
///
/// A match at position `pos` is a whole-word match if:
/// - The character before `pos` is not a word character (or `pos == 0`)
/// - The character after `pos + old.len()` is not a word character (or at end)
fn replace_whole_word(content: &str, old: &str, new: &str) -> String {
    if old.is_empty() {
        return content.to_owned();
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    let mut result = String::with_capacity(content.len());
    let mut last_match_end = 0usize;

    for (start, matched) in content.match_indices(old) {
        let end = start + matched.len();
        let before_ok = start == 0
            || content[..start]
                .chars()
                .last()
                .map(|c| !is_word_char(c))
                .unwrap_or(true);
        let after_ok = end == content.len()
            || content[end..]
                .chars()
                .next()
                .map(|c| !is_word_char(c))
                .unwrap_or(true);

        if before_ok && after_ok {
            result.push_str(&content[last_match_end..start]);
            result.push_str(new);
            last_match_end = end;
        }
    }

    result.push_str(&content[last_match_end..]);
    result
}

/// Normalise whitespace in a string: collapse runs of spaces/tabs/newlines into a
/// single space, then trim.  Used to match old_text against file content when the
/// LLM inserts slightly different indentation or line breaks.
fn normalise_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = true;
        } else {
            in_ws = false;
            out.push(ch);
        }
    }
    out.trim_end().to_string()
}

/// Find `needle` in `haystack` using whitespace-normalised matching.
///
/// Returns `Some((start_byte, matched_byte_len))` where start_byte and
/// matched_byte_len refer to byte offsets in the **original** (un-normalised)
/// haystack.  Returns `None` if no match is found.
fn find_normalised_whitespace(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let norm_needle = normalise_ws(needle);
    if norm_needle.is_empty() {
        return None;
    }

    // Sliding window over lines in haystack.
    // We check progressively wider windows to find a contiguous span whose
    // normalised form contains the normalised needle.
    let lines: Vec<&str> = haystack.lines().collect();
    for start_line in 0..lines.len() {
        let mut window = String::new();
        let mut raw_start_byte: Option<usize> = None;
        for end_line in start_line..lines.len().min(start_line + needle.lines().count() + 5) {
            if !window.is_empty() {
                window.push(' ');
            }
            window.push_str(lines[end_line].trim());

            let norm_window = normalise_ws(&window);
            if let Some(_pos) = norm_window.find(&norm_needle) {
                // Found it.  Compute byte positions in the original haystack.
                let byte_start = if raw_start_byte.is_none() {
                    let mut offset = 0;
                    for l in 0..start_line {
                        offset += lines[l].len() + 1; // +1 for newline
                    }
                    offset
                } else {
                    raw_start_byte.unwrap()
                };
                let mut byte_end = byte_start;
                for l in start_line..=end_line {
                    byte_end += lines[l].len() + 1;
                }
                // Trim trailing newline
                byte_end = byte_end.min(haystack.len());
                return Some((byte_start, byte_end - byte_start));
            }

            if raw_start_byte.is_none() {
                let mut offset = 0;
                for l in 0..start_line {
                    offset += lines[l].len() + 1;
                }
                raw_start_byte = Some(offset);
            }
        }
    }
    None
}

/// Generate a unified diff between two strings.
fn make_diff(original: &str, modified: &str, file_path: &str) -> String {
    let patch = diffy::create_patch(original, modified);
    let patch_str = patch.to_string();
    if patch_str.is_empty() {
        format!("--- {}\n+++ {}\n(no changes)\n", file_path, file_path)
    } else {
        format!("--- {}\n+++ {}\n{}", file_path, file_path, patch_str)
    }
}

// ============================================================================
// D.2 — leindex_edit_preview
// ============================================================================

/// Handler for leindex_edit_preview — impact analysis + diff before any edit.
#[derive(Clone)]
pub struct EditPreviewHandler;

/// Handler for leindex_edit_apply — apply edits to files.
#[derive(Clone)]
pub struct EditApplyHandler;

/// Handler for leindex_rename_symbol — rename a symbol across all files.
#[derive(Clone)]
pub struct RenameSymbolHandler;

/// Handler for leindex_impact_analysis — transitive dependency impact.
#[derive(Clone)]
pub struct ImpactAnalysisHandler;

#[allow(missing_docs)]
impl EditPreviewHandler {
    pub fn name(&self) -> &str {
        "leindex_edit_preview"
    }

    pub fn description(&self) -> &str {
        "Preview a code edit: unified diff, affected symbols/files, breaking changes, and risk \
level — all before touching the filesystem. No equivalent in standard tools. Run before \
leindex_edit_apply to understand the blast radius of your change."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to edit"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "changes": {
                    "type": "array",
                    "description": "List of changes to preview. Each change has 'type' (replace_text/rename_symbol) and type-specific fields.",
                    "items": { "type": "object" }
                }
            },
            "required": ["file_path", "changes"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let file_path = extract_string(&args, "file_path")?;
        let changes_val = args
            .get("changes")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;

        // Enforce project boundary
        let _ = validate_file_within_project(&file_path, index.project_path())?;

        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // Read file content
        let original = std::fs::read_to_string(&file_path).map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        // Parse changes with content available for text-search resolution
        let changes = parse_edit_changes(&changes_val, Some(&original))?;

        // Apply changes in memory
        let modified = apply_changes_in_memory(&original, &changes)?;

        // Generate diff
        let diff = make_diff(&original, &modified, &file_path);

        // Compute impact from PDG
        let mut affected_nodes: Vec<String> = Vec::new();
        let mut affected_files: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        affected_files.insert(file_path.clone());
        let mut breaking_changes: Vec<String> = Vec::new();

        for change in &changes {
            if let EditChange::RenameSymbol {
                old_name,
                new_name: _,
            } = change
            {
                // Try name-based lookup for PDG impact analysis
                let found_id = pdg
                    .find_by_symbol(old_name)
                    .or_else(|| pdg.find_by_name(old_name))
                    .or_else(|| pdg.find_by_name_in_file(old_name, None));
                if let Some(node_id) = found_id {
                    let forward = pdg.get_forward_impact(node_id);
                    for dep_id in &forward {
                        if let Some(dn) = pdg.get_node(*dep_id) {
                            affected_nodes.push(dn.name.clone());
                            affected_files.insert(dn.file_path.clone());
                        }
                    }
                    let backward = pdg.get_backward_impact(node_id);
                    if !backward.is_empty() {
                        breaking_changes.push(format!(
                            "Renaming '{}' may break {} caller(s)",
                            old_name,
                            backward.len()
                        ));
                    }
                }
            }
        }

        let risk = if affected_nodes.len() > 5 || affected_files.len() > 3 {
            "high"
        } else if affected_nodes.len() > 1 || affected_files.len() > 1 {
            "medium"
        } else {
            "low"
        };

        Ok(serde_json::json!({
            "diff": diff,
            "affected_symbols": affected_nodes,
            "affected_files": affected_files.into_iter().collect::<Vec<_>>(),
            "breaking_changes": breaking_changes,
            "risk_level": risk,
            "change_count": changes.len()
        }))
    }
}

#[allow(missing_docs)]
impl EditApplyHandler {
    pub fn name(&self) -> &str {
        "leindex_edit_apply"
    }

    pub fn description(&self) -> &str {
        "Apply code edits to files with optional dry-run mode and impact reporting. \
Always run leindex_edit_preview first to understand the impact. With dry_run=true, \
returns the preview without modifying any files."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to edit"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "changes": {
                    "type": "array",
                    "description": "List of changes to apply",
                    "items": { "type": "object" }
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, return preview without modifying files (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                }
            },
            "required": ["file_path", "changes"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let dry_run = extract_bool(&args, "dry_run", false);

        if dry_run {
            // Delegate to preview
            return EditPreviewHandler.execute(registry, args).await;
        }

        let file_path = extract_string(&args, "file_path")?;
        let changes_val = args
            .get("changes")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;

        // Enforce project boundary
        {
            let index = handle.lock().await;
            let _ = validate_file_within_project(&file_path, index.project_path())?;
        }

        // Read → parse (with content for text-search) → apply → write
        let original = std::fs::read_to_string(&file_path).map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        let changes = parse_edit_changes(&changes_val, Some(&original))?;
        let modified = apply_changes_in_memory(&original, &changes)?;

        if modified == original {
            return Ok(serde_json::json!({
                "success": true,
                "changes_applied": 0,
                "files_modified": [],
                "message": "No changes needed — content already matches"
            }));
        }

        std::fs::write(&file_path, modified.as_bytes()).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to write '{}': {}", file_path, e))
        })?;

        Ok(serde_json::json!({
            "success": true,
            "changes_applied": changes.len(),
            "files_modified": [file_path]
        }))
    }
}

#[allow(missing_docs)]
impl RenameSymbolHandler {
    pub fn name(&self) -> &str {
        "leindex_rename_symbol"
    }

    pub fn description(&self) -> &str {
        "Rename a symbol across all files using PDG to find all reference sites. Generates a \
unified multi-file diff (preview_only=true by default for safety). Replaces manual \
Grep + multi-file Edit with a single atomic operation."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "old_name": {
                    "type": "string",
                    "description": "Current symbol name"
                },
                "new_name": {
                    "type": "string",
                    "description": "New symbol name"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "scope": {
                    "type": "string",
                    "description": "Limit rename to a file or directory path (optional)"
                },
                "preview_only": {
                    "type": "boolean",
                    "description": "If true, return diff without applying changes (default: true). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": true
                }
            },
            "required": ["old_name", "new_name"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let old_name = extract_string(&args, "old_name")?;
        let new_name = extract_string(&args, "new_name")?;
        let scope = args
            .get("scope")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let preview_only = extract_bool(&args, "preview_only", true);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;
        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // Collect all files containing references to old_name
        let mut ref_files: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Resolve old_name to PDG node using multiple strategies:
        // 1. Exact ID match ("file_path:qualified_name")
        // 2. Name-based match ("health_check")
        // 3. Fuzzy case-insensitive substring match
        let node_id = pdg
            .find_by_symbol(&old_name)
            .or_else(|| pdg.find_by_name(&old_name))
            .or_else(|| pdg.find_by_name_in_file(&old_name, None));

        if let Some(node_id) = node_id {
            // The definition file
            if let Some(n) = pdg.get_node(node_id) {
                ref_files.insert(n.file_path.clone());
            }
            // Include all known incoming references, not just direct call edges.
            // This captures call, data, and transitive usage relationships.
            for ref_id in pdg.get_backward_impact_bounded(node_id, 5) {
                if let Some(dn) = pdg.get_node(ref_id) {
                    ref_files.insert(dn.file_path.clone());
                }
            }
            // Also include files where the old_name appears in other symbols' IDs
            // (e.g., imports or references that aren't captured as direct callers)
            for nid in pdg.find_all_by_name(&old_name) {
                if let Some(n) = pdg.get_node(nid) {
                    ref_files.insert(n.file_path.clone());
                }
            }
        } else {
            return Err(JsonRpcError::invalid_params(format!(
                "Symbol '{}' not found in project index. The index uses short symbol names \
                (e.g., 'health_check', not 'ClassName.health_check'). \
                Try leindex_grep_symbols to find the exact name.",
                old_name
            )));
        }

        // Apply scope filter
        let filtered_files: Vec<String> = ref_files
            .into_iter()
            .filter(|f| {
                scope
                    .as_ref()
                    .map(|s| f.starts_with(s.as_str()))
                    .unwrap_or(true)
            })
            .collect();

        // Generate per-file diffs
        let mut diffs: Vec<Value> = Vec::new();
        let mut files_to_modify: Vec<String> = Vec::new();

        for file_path in &filtered_files {
            let Ok(original) = std::fs::read_to_string(file_path) else {
                continue;
            };
            let modified = replace_whole_word(&original, &old_name, &new_name);
            if modified != original {
                let diff = make_diff(&original, &modified, file_path);
                diffs.push(serde_json::json!({ "file": file_path, "diff": diff }));
                files_to_modify.push(file_path.clone());
            }
        }

        if !preview_only {
            // Apply changes to all files
            for file_path in &files_to_modify {
                if let Ok(original) = std::fs::read_to_string(file_path) {
                    let modified = replace_whole_word(&original, &old_name, &new_name);
                    let _ = std::fs::write(file_path, modified.as_bytes());
                }
            }
        }

        Ok(serde_json::json!({
            "old_name": old_name,
            "new_name": new_name,
            "files_affected": files_to_modify.len(),
            "preview_only": preview_only,
            "diffs": diffs,
            "applied": !preview_only
        }))
    }
}

#[allow(missing_docs)]
impl ImpactAnalysisHandler {
    pub fn name(&self) -> &str {
        "leindex_impact_analysis"
    }

    pub fn description(&self) -> &str {
        "Analyze the transitive impact of changing a symbol: shows all symbols and files \
affected at each dependency depth level, with a risk assessment. Use before refactoring \
to understand the blast radius of your change. No equivalent in standard tools."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol to analyze impact for"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "change_type": {
                    "type": "string",
                    "enum": ["modify", "remove", "rename", "change_signature"],
                    "description": "Type of change to analyze (default: modify)",
                    "default": "modify"
                },
                "depth": {
                    "type": "integer",
                    "description": "Traversal depth (default: 3, max: 5)",
                    "default": 3,
                    "minimum": 1,
                    "maximum": 5
                }
            },
            "required": ["symbol"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let symbol = extract_string(&args, "symbol")?;
        let change_type = args
            .get("change_type")
            .and_then(|v| v.as_str())
            .unwrap_or("modify")
            .to_owned();
        let depth = extract_usize(&args, "depth", 3)?.min(5);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;
        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        let node_id = if let Some(nid) = pdg.find_by_symbol(&symbol) {
            nid
        } else {
            let sym_lower = symbol.to_lowercase();
            pdg.node_indices()
                .find(|&nid| {
                    pdg.get_node(nid)
                        .map(|n| n.name.to_lowercase() == sym_lower)
                        .unwrap_or(false)
                })
                .ok_or_else(|| {
                    JsonRpcError::invalid_params(format!(
                        "Symbol '{}' not found in project index",
                        symbol
                    ))
                })?
        };

        let node = pdg
            .get_node(node_id)
            .ok_or_else(|| JsonRpcError::internal_error("PDG node disappeared"))?;

        // Direct callers (depth 1)
        let direct_callers: Vec<String> = get_direct_callers(pdg, node_id)
            .iter()
            .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
            .collect();

        // Depth-bounded transitive forward impact
        let forward = pdg.get_forward_impact_bounded(node_id, depth);
        let affected_symbols: Vec<String> = forward
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.name.clone()))
            .take(50)
            .collect();
        let affected_files: std::collections::HashSet<&str> = forward
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.file_path.as_str()))
            .collect();

        // Depth-bounded transitive backward impact
        let backward = pdg.get_backward_impact_bounded(node_id, depth);

        let risk = match change_type.as_str() {
            "remove" | "change_signature" => {
                if forward.len() > 5 || affected_files.len() > 3 {
                    "high"
                } else if !forward.is_empty() {
                    "medium"
                } else {
                    "low"
                }
            }
            _ => {
                if affected_files.len() > 3 {
                    "high"
                } else if affected_files.len() > 1 {
                    "medium"
                } else {
                    "low"
                }
            }
        };

        Ok(serde_json::json!({
            "symbol": node.name,
            "file": node.file_path,
            "change_type": change_type,
            "direct_callers": direct_callers,
            "transitive_affected_symbols": affected_symbols,
            "transitive_affected_files": affected_files.len(),
            "transitive_callers": backward.len(),
            "risk_level": risk,
            "summary": format!(
                "Changing '{}' directly affects {} symbols in {} files (risk: {})",
                node.name, forward.len(), affected_files.len(), risk
            )
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

    fn test_registry_for(path: &std::path::Path) -> Arc<ProjectRegistry> {
        let leindex = LeIndex::new(path).expect("leindex");
        Arc::new(ProjectRegistry::with_initial_project(5, leindex))
    }

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
        // Phase D handlers
        assert_eq!(EditPreviewHandler.name(), "leindex_edit_preview");
        assert_eq!(EditApplyHandler.name(), "leindex_edit_apply");
        assert_eq!(RenameSymbolHandler.name(), "leindex_rename_symbol");
        assert_eq!(ImpactAnalysisHandler.name(), "leindex_impact_analysis");
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

        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({
            "path": src.display().to_string(),
            "mode": "balanced",
            "max_files": 1
        });

        let value = execute_phase_analysis(&registry, args)
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

        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({
            "path": src.display().to_string(),
            "phase": "1",
            "mode": "balanced",
            "max_files": 1
        });

        let value = execute_phase_analysis(&registry, args)
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
        assert_eq!(
            node_type_str(&legraphe::pdg::NodeType::Function),
            "function"
        );
        assert_eq!(node_type_str(&legraphe::pdg::NodeType::Class), "class");
        assert_eq!(node_type_str(&legraphe::pdg::NodeType::Method), "method");
        assert_eq!(
            node_type_str(&legraphe::pdg::NodeType::Variable),
            "variable"
        );
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
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "file_path": "/some/file.rs" });
        let result = FileSummaryHandler.execute(&registry, args).await;
        // Should return project_not_indexed error since no PDG
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_symbol_lookup_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbol": "my_func" });
        let result = SymbolLookupHandler.execute(&registry, args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_project_map_auto_indexes_empty_project() {
        // With auto-indexing, an empty project returns an empty file list (not an error)
        let dir = tempdir().unwrap();
        // Create a minimal source file so indexing has something to find
        let src = dir.path().join("main.rs");
        std::fs::write(&src, "fn main() {}\n").unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({});
        let result = ProjectMapHandler.execute(&registry, args).await;
        assert!(result.is_ok(), "auto-indexing should succeed");
    }

    #[tokio::test]
    async fn test_grep_symbols_auto_indexes_returns_empty() {
        // With auto-indexing, a project with no matching symbols returns empty results
        let dir = tempdir().unwrap();
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "pub fn greet() {}\n").unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "pattern": "nonexistent" });
        let result = GrepSymbolsHandler.execute(&registry, args).await;
        // Should succeed (auto-index happens) but with 0 matches
        assert!(result.is_ok(), "auto-indexing should succeed");
        let val = result.unwrap();
        assert_eq!(val["count"].as_u64().unwrap_or(0), 0);
    }

    #[tokio::test]
    async fn test_read_symbol_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbol": "my_func" });
        let result = ReadSymbolHandler.execute(&registry, args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_phase_c_handler_schemas() {
        // All Phase C schemas should be valid JSON objects with required fields
        let schemas = vec![
            (FileSummaryHandler.argument_schema(), vec!["file_path"]),
            // SymbolLookupHandler has no required fields (symbol or symbols accepted)
            (SymbolLookupHandler.argument_schema(), vec![]),
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

    // =========================================================================
    // Remediation tests — validate fixes for evaluation report issues
    // =========================================================================

    #[test]
    fn test_parse_edit_changes_text_search_mode() {
        // When old_text is provided without start_byte, should find text in content
        let content = "fn hello() {\n    println!(\"Hello\");\n}";
        let changes_json = serde_json::json!([{
            "type": "replace_text",
            "old_text": "println!(\"Hello\")",
            "new_text": "println!(\"Goodbye\")"
        }]);
        let changes = parse_edit_changes(&changes_json, Some(content)).unwrap();
        assert_eq!(changes.len(), 1);
        if let EditChange::ReplaceText {
            start,
            end,
            new_text,
        } = &changes[0]
        {
            assert_eq!(*start, content.find("println!(\"Hello\")").unwrap());
            assert_eq!(*end, *start + "println!(\"Hello\")".len());
            assert_eq!(new_text, "println!(\"Goodbye\")");
        } else {
            panic!("Expected ReplaceText");
        }
    }

    #[test]
    fn test_parse_edit_changes_explicit_byte_offsets() {
        // When start_byte and end_byte are provided, use them directly
        let changes_json = serde_json::json!([{
            "type": "replace_text",
            "start_byte": 10,
            "end_byte": 20,
            "new_text": "replacement"
        }]);
        let changes = parse_edit_changes(&changes_json, Some("any content")).unwrap();
        assert_eq!(changes.len(), 1);
        if let EditChange::ReplaceText { start, end, .. } = &changes[0] {
            assert_eq!(*start, 10);
            assert_eq!(*end, 20);
        } else {
            panic!("Expected ReplaceText");
        }
    }

    #[test]
    fn test_parse_edit_changes_text_not_found_returns_error() {
        let changes_json = serde_json::json!([{
            "type": "replace_text",
            "old_text": "nonexistent text",
            "new_text": "replacement"
        }]);
        let result = parse_edit_changes(&changes_json, Some("actual file content"));
        assert!(
            result.is_err(),
            "Should error when old_text not found in content"
        );
    }

    #[test]
    fn test_apply_changes_in_memory_text_search_integration() {
        let content = "def health_check(self):\n    return True\n\ndef other():\n    pass";
        let changes_json = serde_json::json!([{
            "type": "replace_text",
            "old_text": "def health_check(self):\n    return True",
            "new_text": "def health_status(self):\n    return True"
        }]);
        let changes = parse_edit_changes(&changes_json, Some(content)).unwrap();
        let modified = apply_changes_in_memory(content, &changes).unwrap();
        assert!(
            modified.contains("def health_status(self):"),
            "Replacement should be applied"
        );
        assert!(
            modified.contains("def other():"),
            "Other content should be preserved"
        );
    }

    #[test]
    fn test_replace_whole_word_basic() {
        assert_eq!(
            replace_whole_word("foo bar baz", "bar", "qux"),
            "foo qux baz"
        );
        assert_eq!(replace_whole_word("foobar baz", "bar", "qux"), "foobar baz");
        assert_eq!(replace_whole_word("bar_foo", "bar", "qux"), "bar_foo");
        assert_eq!(replace_whole_word("bar", "bar", "qux"), "qux");
    }

    #[test]
    fn test_pdg_find_by_name() {
        let mut pdg = legraphe::pdg::ProgramDependenceGraph::new();
        let n1 = pdg.add_node(legraphe::pdg::Node {
            id: "file.py:MyClass.health_check".into(),
            node_type: legraphe::pdg::NodeType::Method,
            name: "health_check".into(),
            file_path: "file.py".into(),
            byte_range: (0, 50),
            complexity: 2,
            language: "python".into(),
            embedding: None,
        });

        // find_by_symbol with full ID works
        assert_eq!(pdg.find_by_symbol("file.py:MyClass.health_check"), Some(n1));

        // find_by_name with short name works
        assert_eq!(pdg.find_by_name("health_check"), Some(n1));

        // find_by_symbol with short name does NOT work (that's the old bug)
        assert_eq!(pdg.find_by_symbol("health_check"), None);
    }

    #[test]
    fn test_pdg_find_by_name_in_file() {
        let mut pdg = legraphe::pdg::ProgramDependenceGraph::new();
        let n1 = pdg.add_node(legraphe::pdg::Node {
            id: "a.py:run".into(),
            node_type: legraphe::pdg::NodeType::Function,
            name: "run".into(),
            file_path: "a.py".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".into(),
            embedding: None,
        });
        let n2 = pdg.add_node(legraphe::pdg::Node {
            id: "b.py:run".into(),
            node_type: legraphe::pdg::NodeType::Function,
            name: "run".into(),
            file_path: "b.py".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".into(),
            embedding: None,
        });

        // Without file hint, returns first match
        assert!(pdg.find_by_name("run").is_some());

        // With file hint, returns correct one
        assert_eq!(pdg.find_by_name_in_file("run", Some("a.py")), Some(n1));
        assert_eq!(pdg.find_by_name_in_file("run", Some("b.py")), Some(n2));
    }

    #[test]
    fn test_make_diff_generates_unified_diff() {
        let original = "line1\nline2\nline3\n";
        let modified = "line1\nmodified\nline3\n";
        let diff = make_diff(original, modified, "test.rs");
        assert!(
            diff.contains("--- test.rs"),
            "Should have original file header"
        );
        assert!(
            diff.contains("+++ test.rs"),
            "Should have modified file header"
        );
        assert!(diff.contains("-line2"), "Should show removed line");
        assert!(diff.contains("+modified"), "Should show added line");
    }

    // =========================================================================
    // Boolean coercion & multi-project tests
    // =========================================================================

    #[test]
    fn test_extract_bool_native_bool() {
        let args = serde_json::json!({"flag": true, "off": false});
        assert_eq!(extract_bool(&args, "flag", false), true);
        assert_eq!(extract_bool(&args, "off", true), false);
    }

    #[test]
    fn test_extract_bool_string_coercion() {
        let args = serde_json::json!({
            "a": "true", "b": "false", "c": "1", "d": "0", "e": "yes", "f": "no",
            "g": "TRUE", "h": "False"
        });
        assert_eq!(extract_bool(&args, "a", false), true);
        assert_eq!(extract_bool(&args, "b", true), false);
        assert_eq!(extract_bool(&args, "c", false), true);
        assert_eq!(extract_bool(&args, "d", true), false);
        assert_eq!(extract_bool(&args, "e", false), true);
        assert_eq!(extract_bool(&args, "f", true), false);
        assert_eq!(extract_bool(&args, "g", false), true);
        assert_eq!(extract_bool(&args, "h", true), false);
    }

    #[test]
    fn test_extract_bool_number_coercion() {
        let args = serde_json::json!({"one": 1, "zero": 0, "big": 42});
        assert_eq!(extract_bool(&args, "one", false), true);
        assert_eq!(extract_bool(&args, "zero", true), false);
        assert_eq!(extract_bool(&args, "big", false), true);
    }

    #[test]
    fn test_extract_bool_missing_uses_default() {
        let args = serde_json::json!({});
        assert_eq!(extract_bool(&args, "absent", true), true);
        assert_eq!(extract_bool(&args, "absent", false), false);
    }

    #[test]
    fn test_validate_file_within_project_ok() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();
        let result = validate_file_within_project(file.to_str().unwrap(), dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_file_outside_project_fails() {
        let dir = tempdir().unwrap();
        // /tmp is always outside our tempdir
        let result = validate_file_within_project("/etc/passwd", dir.path());
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("outside the project boundary"));
    }

    #[test]
    fn test_symbol_lookup_schema_supports_batch() {
        let schema = SymbolLookupHandler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("symbol").is_some(), "should have 'symbol'");
        assert!(
            props.get("symbols").is_some(),
            "should have 'symbols' for batch"
        );

        // Required should be empty (either symbol or symbols accepted)
        let required = schema.get("required").and_then(|v| v.as_array());
        assert!(
            required.map(|r| r.is_empty()).unwrap_or(true),
            "required should be empty since symbol/symbols are alternatives"
        );
    }

    #[test]
    fn test_search_schema_has_pagination() {
        let schema = SearchHandler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(
            props.get("offset").is_some(),
            "should have 'offset' for pagination"
        );
        assert!(
            props.get("project_path").is_some(),
            "should have 'project_path'"
        );
    }

    #[test]
    fn test_grep_symbols_schema_has_pagination() {
        let schema = GrepSymbolsHandler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(
            props.get("offset").is_some(),
            "should have 'offset' for pagination"
        );
        assert!(
            props.get("project_path").is_some(),
            "should have 'project_path'"
        );
    }

    #[test]
    fn test_project_map_schema_has_pagination() {
        let schema = ProjectMapHandler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("offset").is_some(), "should have 'offset'");
        assert!(props.get("limit").is_some(), "should have 'limit'");
        assert!(
            props.get("project_path").is_some(),
            "should have 'project_path'"
        );
    }

    #[test]
    fn test_all_tools_have_project_path_except_index_diagnostics() {
        // Tools that need indexed data should all accept project_path
        let tools_with_project_path = vec![
            SearchHandler.argument_schema(),
            DeepAnalyzeHandler.argument_schema(),
            ContextHandler.argument_schema(),
            FileSummaryHandler.argument_schema(),
            SymbolLookupHandler.argument_schema(),
            ProjectMapHandler.argument_schema(),
            GrepSymbolsHandler.argument_schema(),
            ReadSymbolHandler.argument_schema(),
            EditPreviewHandler.argument_schema(),
            EditApplyHandler.argument_schema(),
            RenameSymbolHandler.argument_schema(),
            ImpactAnalysisHandler.argument_schema(),
        ];
        for schema in tools_with_project_path {
            let props = schema.get("properties").unwrap();
            assert!(
                props.get("project_path").is_some(),
                "All query tools must accept project_path"
            );
        }
    }
}
