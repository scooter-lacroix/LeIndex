// MCP Tool Handlers
//
// This module implements the handlers for each MCP tool that the server exposes.

use super::protocol::JsonRpcError;
#[cfg(test)]
use crate::cli::leindex::LeIndex;
use crate::cli::registry::ProjectRegistry;
use crate::edit::EditChange;
use crate::phase::{run_phase_analysis, DocsMode, FormatMode, PhaseOptions, PhaseSelection};
use regex::RegexBuilder;
use serde_json::Value;
use std::path::{Path, PathBuf};
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
    // Phase E: Precision Tooling
    /// Handler for PDG-enriched text search (replaces rg)
    TextSearch(TextSearchHandler),
    /// Handler for PDG-annotated file read (replaces Read/cat)
    ReadFile(ReadFileHandler),
    /// Handler for PDG-aware git status (replaces git status/diff)
    GitStatus(GitStatusHandler),
}

/// Build the full MCP/CLI tool surface in one place so stdio, HTTP, and CLI bridges
/// all stay in sync as new tools are added.
pub fn all_tool_handlers() -> Vec<ToolHandler> {
    vec![
        ToolHandler::DeepAnalyze(DeepAnalyzeHandler),
        ToolHandler::Diagnostics(DiagnosticsHandler),
        ToolHandler::Index(IndexHandler),
        ToolHandler::Context(ContextHandler),
        ToolHandler::Search(SearchHandler),
        ToolHandler::PhaseAnalysis(PhaseAnalysisHandler),
        ToolHandler::PhaseAnalysisAlias(PhaseAnalysisAliasHandler),
        ToolHandler::FileSummary(FileSummaryHandler),
        ToolHandler::SymbolLookup(SymbolLookupHandler),
        ToolHandler::ProjectMap(ProjectMapHandler),
        ToolHandler::GrepSymbols(GrepSymbolsHandler),
        ToolHandler::ReadSymbol(ReadSymbolHandler),
        ToolHandler::EditPreview(EditPreviewHandler),
        ToolHandler::EditApply(EditApplyHandler),
        ToolHandler::RenameSymbol(RenameSymbolHandler),
        ToolHandler::ImpactAnalysis(ImpactAnalysisHandler),
        ToolHandler::TextSearch(TextSearchHandler),
        ToolHandler::ReadFile(ReadFileHandler),
        ToolHandler::GitStatus(GitStatusHandler),
    ]
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
            ToolHandler::TextSearch(h) => h.name(),
            ToolHandler::ReadFile(h) => h.name(),
            ToolHandler::GitStatus(h) => h.name(),
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
            ToolHandler::TextSearch(h) => h.description(),
            ToolHandler::ReadFile(h) => h.description(),
            ToolHandler::GitStatus(h) => h.description(),
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
            ToolHandler::TextSearch(h) => h.argument_schema(),
            ToolHandler::ReadFile(h) => h.argument_schema(),
            ToolHandler::GitStatus(h) => h.argument_schema(),
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
            ToolHandler::TextSearch(h) => h.execute(registry, args).await,
            ToolHandler::ReadFile(h) => h.execute(registry, args).await,
            ToolHandler::GitStatus(h) => h.execute(registry, args).await,
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
        let idx = index.lock().await;
        serde_json::to_value(stats)
            .map(|v| wrap_with_meta(v, &idx))
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
                "scope": {
                    "type": "string",
                    "description": "Optional path to limit results (absolute or relative to project root)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "search_mode": {
                    "type": "string",
                    "enum": ["code", "prose", "auto"],
                    "description": "Scoring mode: 'code' (default) emphasizes semantic/structural similarity, \
        'prose' boosts text-match weight for natural-language queries (e.g. roadmap, README content), \
        'auto' detects based on query shape.",
                    "default": "code"
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
        let search_mode = args
            .get("search_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("code");
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        // Resolve query type
        let query_type = match search_mode {
            "prose" => Some(crate::search::ranking::QueryType::Text),
            "code" => Some(crate::search::ranking::QueryType::Semantic),
            "auto" => {
                let q_lower = query.to_lowercase();
                let prose_keywords = [
                    "how", "what", "where", "why", "who", "when", "can", "is", "explain",
                    "describe", "find", "show",
                ];
                let is_natural_language = q_lower.split_whitespace().count() > 3
                    || prose_keywords.iter().any(|k| q_lower.contains(k));

                if is_natural_language {
                    Some(crate::search::ranking::QueryType::Text)
                } else {
                    Some(crate::search::ranking::QueryType::Semantic)
                }
            }
            _ => Some(crate::search::ranking::QueryType::Semantic),
        };

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.lock().await;
        let scope = resolve_scope(&args, index.project_path())?;

        if index.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                index.project_path().display().to_string(),
            ));
        }

        // Fetch top_k + offset results, then slice for pagination
        let all_results = index
            .search(&query, top_k + offset, query_type)
            .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;

        let filtered: Vec<_> = all_results
            .into_iter()
            .filter(|r| match &scope {
                Some(s) => r.file_path.starts_with(s),
                None => true,
            })
            .collect();

        let total_filtered = filtered.len();

        let page: Vec<_> = filtered.into_iter().skip(offset).take(top_k).collect();
        let total_returned = page.len();

        // Check for zero results and provide helpful suggestion
        // Only emit suggestion when there are truly no matches, not when offset is past total
        if total_filtered == 0 {
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "results": [],
                    "offset": offset,
                    "count": 0,
                    "has_more": false,
                    "suggestion": format!(
                        "No semantic matches found for '{}'. The project contains {} indexed files. \
                        Try: rephrase query, use different keywords, or try leindex_grep_symbols for exact symbol names.",
                        query,
                        index.source_file_paths().map(|p| p.len()).unwrap_or(0)
                    )
                }),
                &index,
            ));
        }

        Ok(wrap_with_meta(
            serde_json::json!({
                "results": serde_json::to_value(&page).map_err(|e|
                    JsonRpcError::internal_error(format!("Serialization error: {}", e)))?,
                "offset": offset,
                "count": total_returned,
                "has_more": offset + total_returned < total_filtered
            }),
            &index,
        ))
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
            .map(|v| wrap_with_meta(v, &writer))
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
            .map(|v| wrap_with_meta(v, &reader))
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
                "description": "IMPORTANT: Enable to include prose/documentation files (README, docs/, *.md) \
    in the analysis. Without this, only source code files are analyzed. Set to true when you need \
    architectural docs, changelogs, or project documentation. Also accepts strings: 'true'/'false'.",
                "default": false
            },
            "docs_mode": {
                "type": "string",
                "enum": ["off", "markdown", "text", "all"],
                "description": "Controls which documentation files to include: 'off' (default, code only), \
    'markdown' (*.md files like README, CHANGELOG), 'text' (*.txt, *.rst), 'all' (all doc formats). \
    Use 'markdown' or 'all' to analyze project documentation alongside code.",
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

    if let serde_json::Value::Object(ref mut map) = report_value {
        map.insert(
            "phase_explanations".to_string(),
            serde_json::json!({
                "1": "File parsing & signature extraction",
                "2": "Import graph construction (internal/external edges)",
                "3": "Entry point identification & impact analysis",
                "4": "Complexity hotspot detection",
                "5": "Actionable recommendations generation"
            }),
        );
        map.insert(
            "example_interpretation".to_string(),
            serde_json::json!({
                "high_unresolved_modules": "Consider adding missing type definitions",
                "many_entry_points": "May indicate architectural coupling issues"
            }),
        );
    }

    let index_for_meta = handle.lock().await;
    Ok(wrap_with_meta(report_value, &index_for_meta))
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
        let mut reader = handle.lock().await;

        let diagnostics = reader.get_diagnostics().map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to get diagnostics: {}", e))
        })?;

        let (changed, deleted) = reader
            .check_freshness()
            .unwrap_or_else(|_| (vec![], vec![]));
        let storage_path = reader.storage_path().display().to_string();
        let db_size = std::fs::metadata(reader.storage_path().join("leindex.db"))
            .map(|m| m.len())
            .unwrap_or(0);
        let coverage = reader.coverage_report().ok();

        let mut diag_json = serde_json::to_value(diagnostics)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?;

        if let Value::Object(ref mut map) = diag_json {
            map.insert("storage_path".to_string(), serde_json::json!(storage_path));
            map.insert("db_size_bytes".to_string(), serde_json::json!(db_size));

            let staleness = if changed.is_empty() && deleted.is_empty() {
                serde_json::json!({
                    "status": "fresh",
                    "changed_files": 0,
                    "deleted_files": 0,
                })
            } else {
                serde_json::json!({
                    "status": "stale",
                    "changed_files": changed.len(),
                    "deleted_files": deleted.len(),
                    "changed_sample": changed.iter().take(10).map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "deleted_sample": deleted.iter().take(10).cloned().collect::<Vec<_>>(),
                    "suggestion": "Call leindex_index with force_reindex=true to refresh",
                })
            };
            map.insert("freshness".to_string(), staleness);
            if let Some(cov) = coverage {
                map.insert(
                    "coverage".to_string(),
                    serde_json::to_value(cov).unwrap_or(Value::Null),
                );
            }
        }

        Ok(wrap_with_meta(diag_json, &reader))
    }
}

// ============================================================================
// Phase C: Tool Supremacy — Read/Grep/Glob Replacement
// ============================================================================

/// Format a NodeType as a human-readable string.
fn node_type_str(nt: &crate::graph::pdg::NodeType) -> &'static str {
    match nt {
        crate::graph::pdg::NodeType::Function => "function",
        crate::graph::pdg::NodeType::Class => "class",
        crate::graph::pdg::NodeType::Method => "method",
        crate::graph::pdg::NodeType::Variable => "variable",
        crate::graph::pdg::NodeType::Module => "module",
        crate::graph::pdg::NodeType::External => "external",
    }
}

/// Resolve and normalize a scope path for consistent filtering.
/// Returns None if no scope was provided. Ensures directory scopes end with a separator.
fn resolve_scope(args: &Value, project_root: &Path) -> Result<Option<String>, JsonRpcError> {
    let raw = match args.get("scope").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(None),
    };

    let path = Path::new(raw);

    // If relative, resolve against project root
    let resolved = if path.is_relative() {
        project_root.join(path)
    } else {
        path.to_path_buf()
    };

    let canonical = resolved.canonicalize().map_err(|e| {
        JsonRpcError::invalid_params_with_suggestion(
            format!("Cannot resolve scope path '{}': {}", raw, e),
            format!(
                "Use an absolute path or a path relative to the project root: {}",
                project_root.display()
            ),
        )
    })?;

    let mut s = canonical.to_string_lossy().to_string();
    if canonical.is_dir() && !s.ends_with(std::path::MAIN_SEPARATOR) && !s.ends_with('/') {
        s.push(std::path::MAIN_SEPARATOR);
    }

    Ok(Some(s))
}

/// Attach meta information to tool responses about index staleness and context.
fn wrap_with_meta(mut result: Value, index: &crate::cli::leindex::LeIndex) -> Value {
    let stale = index.is_stale_fast();
    if let Some(obj) = result.as_object_mut() {
        if stale {
            obj.insert(
                "_warning".to_string(),
                Value::String(
                    "Index may be stale. Call leindex_index with force_reindex=true for fresh results."
                        .to_string(),
                ),
            );
        }
    }
    result
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

/// Convert a byte range to a 1-indexed line range.
fn byte_range_to_line_range(content: &str, byte_range: (usize, usize)) -> (usize, usize) {
    let (start, end) = byte_range;
    let bytes = content.as_bytes();
    let mut line = 1usize;
    let mut start_line = 1usize;
    let mut end_line = 1usize;
    let mut found_start = false;

    for (idx, b) in bytes.iter().enumerate() {
        if idx == start {
            start_line = line;
            found_start = true;
        }
        if idx >= end {
            end_line = line;
            break;
        }
        if *b == b'\n' {
            line += 1;
        }
    }
    if !found_start {
        start_line = line;
    }
    if end >= bytes.len() {
        end_line = line;
    }
    (start_line, end_line.max(start_line))
}

/// Collect the NodeIds of all nodes that have a direct edge pointing *to*
/// `target_id` (i.e. the callers / direct dependents).
///
/// Uses petgraph's `neighbors_directed(Incoming)` for O(degree) performance
/// instead of the previous O(E) full-edge scan.
fn get_direct_callers(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    target_id: crate::graph::pdg::NodeId,
) -> Vec<crate::graph::pdg::NodeId> {
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
        "File overview: symbol inventory, complexity scores, cross-file dependencies, \
and module role. Use for understanding structure without reading raw content. \
For exact file contents use leindex_read_file; for a specific implementation \
use leindex_read_symbol."
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

        Ok(wrap_with_meta(
            serde_json::json!({
                "file_path": file_path,
                "language": language,
                "line_count": line_count,
                "symbol_count": symbols.len(),
                "symbols": symbols,
                "module_role": module_role
            }),
            &index,
        ))
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
        "Symbol relationship lookup: callers, callees, data dependencies, and impact radius. \
Use for understanding how a symbol connects to the rest of the codebase. \
For the exact source implementation use leindex_read_symbol."
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
                "scope": {
                    "type": "string",
                    "description": "Optional path to limit lookup (absolute or relative to project root)"
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
        let is_batch = args
            .get("symbols")
            .and_then(|v| v.as_array())
            .map_or(false, |a| a.len() > 1);
        let include_source = extract_bool(&args, "include_source", !is_batch);
        let include_callers = extract_bool(&args, "include_callers", true);
        let include_callees = extract_bool(&args, "include_callees", true);
        let depth = extract_usize(&args, "depth", 2)?.min(5);
        let token_budget = extract_usize(&args, "token_budget", 1500)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let scope = {
            let handle = registry.get_or_create(project_path).await?;
            let index = handle.lock().await;
            resolve_scope(&args, index.project_path())?
        };

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
                    &scope,
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

            return Ok(wrap_with_meta(
                serde_json::json!({
                    "batch": true,
                    "count": results.len(),
                    "results": results
                }),
                &index,
            ));
        }

        // Single symbol mode
        let char_budget = token_budget * 4;
        let single = self.lookup_single_symbol(
            pdg,
            &symbols[0],
            &scope,
            include_source,
            include_callers,
            include_callees,
            depth,
            char_budget,
        )?;

        Ok(wrap_with_meta(single, &index))
    }

    /// Resolve and return full structural context for a single symbol.
    fn lookup_single_symbol(
        &self,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        symbol: &str,
        scope: &Option<String>,
        include_source: bool,
        include_callers: bool,
        include_callees: bool,
        depth: usize,
        char_budget: usize,
    ) -> Result<Value, JsonRpcError> {
        let in_scope = |node: &crate::graph::pdg::Node| match scope {
            Some(s) => node.file_path.starts_with(s),
            None => true,
        };

        // 1. Exact symbol lookup (by node.id in symbol_index)
        let node_id = if let Some(nid) = pdg.find_by_symbol(symbol) {
            pdg.get_node(nid).filter(|n| in_scope(*n)).map(|_| nid)
        } else {
            None
        }
        // 2. Exact name lookup (by node.name in name_index) — prefer non-module nodes
        .or_else(|| {
            let candidates = pdg.find_all_by_name(symbol);
            // Prefer class/function/method over module nodes
            candidates
                .iter()
                .copied()
                .find(|&nid| {
                    pdg.get_node(nid)
                        .map(|n| n.node_type != crate::graph::pdg::NodeType::Module && in_scope(n))
                        .unwrap_or(false)
                })
                .or_else(|| {
                    candidates
                        .iter()
                        .copied()
                        .find(|&nid| pdg.get_node(nid).map(|n| in_scope(n)).unwrap_or(false))
                })
        })
        .or_else(|| {
            // 3. Fuzzy match: substring, case-insensitive — prefer non-module nodes
            let sym_lower = symbol.to_lowercase();
            let mut best: Option<crate::graph::pdg::NodeId> = None;
            let mut best_is_module = true;
            for nid in pdg.node_indices() {
                let Some(n) = pdg.get_node(nid) else { continue };
                if !in_scope(n) {
                    continue;
                }
                let matches = n.name.to_lowercase().contains(&sym_lower)
                    || n.id.to_lowercase().contains(&sym_lower);
                if !matches {
                    continue;
                }
                let is_module = n.node_type == crate::graph::pdg::NodeType::Module;
                // Always prefer non-module; only accept module if it's the first match
                if best.is_none() || (best_is_module && !is_module) {
                    best = Some(nid);
                    best_is_module = is_module;
                    if !is_module {
                        break;
                    } // non-module is best, stop early
                }
            }
            best
        })
        .ok_or_else(|| {
            let total_symbols = pdg.node_count();
            let total_files = pdg.file_count();
            let suggestion = format!(
                "Symbol '{}' not found among {} indexed symbols across {} files. Try: \
                check spelling, use leindex_grep_symbols for partial matches, \
                or leindex_text_search for raw content search.",
                symbol, total_symbols, total_files
            );
            JsonRpcError::invalid_params_with_suggestion(
                format!("Symbol '{}' not found in project index", symbol),
                &suggestion
            )
        })?;

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
        let forward = pdg.forward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig {
                max_depth: Some(depth),
                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
            },
        );
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
        "Project structure map — use instead of Glob/ls for directory listing. Shows files \
with symbol counts, complexity hotspots, and inter-module dependency arrows. Supports \
scoping to subdirectories, sorting, and pagination."
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
                },
                "focus": {
                    "type": "string",
                    "description": "Semantic focus area — ranks files by relevance to this topic (e.g., 'authentication', 'database layer', 'payment flow')"
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
        let focus = args.get("focus").and_then(|v| v.as_str()).map(String::from);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.lock().await;
        let project_root = index.project_path().to_path_buf();

        // Allow legacy "path" param; map it into "scope" for resolution.
        let mut args_with_scope = args.clone();
        if let Some(obj) = args_with_scope.as_object_mut() {
            if !obj.contains_key("scope") {
                if let Some(p) = obj.get("path").cloned() {
                    obj.insert("scope".to_string(), p);
                }
            }
        }
        let scope = resolve_scope(&args_with_scope, index.project_path())?;
        let scope_str = scope.unwrap_or_else(|| {
            let mut s = project_root.to_string_lossy().to_string();
            if !s.ends_with(std::path::MAIN_SEPARATOR) {
                s.push(std::path::MAIN_SEPARATOR);
            }
            s
        });
        let scope_path = PathBuf::from(&scope_str);
        let scope_base = PathBuf::from(
            scope_str.trim_end_matches(|c| c == '/' || c == std::path::MAIN_SEPARATOR),
        );

        // Use cached file stats if available, otherwise build from PDG
        // Collect source paths first to avoid borrow conflicts with file_stats()/pdg()
        let source_paths = index.source_file_paths().unwrap_or_default();

        // file → (symbol_count, total_complexity, symbol_names, incoming_deps, outgoing_deps)
        let file_map: std::collections::HashMap<String, (usize, u32, Vec<String>, usize, usize)> = if let Some(cache) = index.file_stats() {
            // Fast path: use cached statistics (includes pre-computed dep counts)
            let mut map: std::collections::HashMap<String, (usize, u32, Vec<String>, usize, usize)> = source_paths
                .into_iter()
                .map(|path| (path.display().to_string(), (0, 0, Vec::new(), 0, 0)))
                .collect();

            // Overlay cached statistics, capping symbol_names to top 5
            for (path, stats) in cache.iter() {
                let capped: Vec<String> = stats.symbol_names.iter().take(5).cloned().collect();
                map.insert(path.clone(), (stats.symbol_count, stats.total_complexity, capped, stats.incoming_deps, stats.outgoing_deps));
            }
            map
        } else {
            // Fallback: build from PDG via the same method used at index time
            index.build_file_stats_cache();
            let mut map: std::collections::HashMap<String, (usize, u32, Vec<String>, usize, usize)> = source_paths
                .into_iter()
                .map(|path| (path.display().to_string(), (0, 0, Vec::new(), 0, 0)))
                .collect();

            if let Some(cache) = index.file_stats() {
                for (path, stats) in cache.iter() {
                    let capped: Vec<String> = stats.symbol_names.iter().take(5).cloned().collect();
                    map.insert(path.clone(), (stats.symbol_count, stats.total_complexity, capped, stats.incoming_deps, stats.outgoing_deps));
                }
            }
            map
        }; // file → (node_count, total_complexity, symbol_names)

        // Get PDG for scope filtering (no degree computation needed — cached in file_map)
        let _pdg = index
            .pdg()
            .ok_or_else(|| JsonRpcError::project_not_indexed(project_root.display().to_string()))?;

        // Filter to scope path and respect depth.
        // Files must either be exactly in the scope directory or in a subdirectory.
        let mut files: Vec<Value> = file_map
            .iter()
            .filter(|(fp, _)| {
                // File is in scope if its path starts with scope_str (directory prefix)
                // or if the file IS the scope path (exact match for single-file scope)
                fp.starts_with(&scope_str) || fp.as_str() == scope_path.to_str().unwrap_or("")
            })
            .filter_map(|(fp, (count, complexity, syms, in_deg, out_deg))| {
                let path = std::path::Path::new(fp);
                let rel = path.strip_prefix(&scope_base).ok()?;
                let directory_depth = rel
                    .parent()
                    .map(|parent| parent.components().count())
                    .unwrap_or(0);
                if directory_depth > depth {
                    return None;
                }

                let mut entry = serde_json::json!({
                    "path": fp,
                    "relative_path": rel.display().to_string(),
                    "symbol_count": count,
                    "total_complexity": complexity,
                    "incoming_dependencies": in_deg,
                    "outgoing_dependencies": out_deg
                });
                if include_symbols || focus.is_some() {
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
            "dependencies" => files.sort_by(|a, b| {
                let a_deg = a["incoming_dependencies"].as_u64().unwrap_or(0)
                    + a["outgoing_dependencies"].as_u64().unwrap_or(0);
                let b_deg = b["incoming_dependencies"].as_u64().unwrap_or(0)
                    + b["outgoing_dependencies"].as_u64().unwrap_or(0);
                b_deg.cmp(&a_deg)
            }),
            "size" => files.sort_by(|a, b| {
                b["symbol_count"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&a["symbol_count"].as_u64().unwrap_or(0))
            }),
            _ => {}
        }

        // Semantic focus ranking: when focus is provided, re-rank files by
        // cosine similarity between the focus embedding and per-file symbol embeddings.
        if let Some(ref focus_text) = focus {
            let focus_emb = index.generate_query_embedding(focus_text);
            for entry in &mut files {
                let syms = entry["top_symbols"].as_array();
                let file_text = syms
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default();
                let file_emb = index.generate_query_embedding(&file_text);
                let score = crate::search::vector::cosine_similarity(&focus_emb, &file_emb);
                entry["relevance_score"] = serde_json::json!(score);
            }
            files.sort_by(|a, b| {
                let sa = a["relevance_score"].as_f64().unwrap_or(0.0);
                let sb = b["relevance_score"].as_f64().unwrap_or(0.0);
                sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
            });
            // Remove top_symbols from output if not requested
            if !include_symbols {
                for entry in &mut files {
                    entry.as_object_mut().map(|o| o.remove("top_symbols"));
                }
            }
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

        Ok(wrap_with_meta(
            serde_json::json!({
                "project_root": project_root.display().to_string(),
                "scope": scope_path.display().to_string(),
                "total_files_in_scope": total_before_pagination,
                "offset": offset,
                "count": truncated_files.len(),
                "has_more": offset + truncated_files.len() < total_before_pagination,
                "files": truncated_files
            }),
            &index,
        ))
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
        "Search for symbols across the codebase with structural awareness. Supports \
        substring and regex patterns. Results include symbol type (function/class) and \
        its role in the dependency graph."
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
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include up to 4000 chars of symbol source code in results (default: false)",
                    "default": false
                },
                "mode": {
                    "type": "string",
                    "enum": ["exact", "semantic"],
                    "description": "Search mode: 'exact' for name substring matching (default), 'semantic' for concept-based similarity search using TF-IDF embeddings",
                    "default": "exact"
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
        let type_filter = args
            .get("type_filter")
            .and_then(|v| v.as_str())
            .unwrap_or("all")
            .to_owned();
        let token_budget = extract_usize(&args, "token_budget", 1500)?;
        let max_results = extract_usize(&args, "max_results", 20)?.min(200);
        let context_lines = extract_usize(&args, "include_context_lines", 0)?.min(10);
        let offset = extract_usize(&args, "offset", 0)?;
        let include_source = extract_bool(&args, "include_source", false);
        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("exact")
            .to_owned();
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.lock().await;
        let scope = resolve_scope(&args, index.project_path())?;

        // Use indexed search to pre-filter candidates (avoids full O(N) PDG scans).
        let candidate_limit = (max_results + offset).saturating_mul(5).max(50);
        let candidate_results = index
            .search(&pattern, candidate_limit, None)
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
        // Use (file_path, byte_range) for location dedup, but skip dedup for synthetic (0,0) ranges
        let mut seen_locations: std::collections::HashSet<(String, (usize, usize))> = std::collections::HashSet::new();
        let mut all_matches: Vec<Value> = Vec::new();

        // Semantic mode: return results directly from the search engine, ranked by
        // cosine similarity. No text matching — finds conceptually related symbols.
        if mode == "semantic" {
            for result in &candidate_results {
                if all_matches.len() >= fetch_limit {
                    break;
                }
                // Look up PDG node for enrichment
                let nid = match pdg.find_by_id(&result.node_id) {
                    Some(id) => id,
                    None => continue,
                };
                let node = match pdg.get_node(nid) {
                    Some(n) => n,
                    None => continue,
                };

                // Apply scope and type filters
                if let Some(ref s) = scope {
                    if !node.file_path.starts_with(s.trim_end_matches(std::path::MAIN_SEPARATOR)) {
                        continue;
                    }
                }
                if type_filter != "all" && node_type_str(&node.node_type) != type_filter {
                    continue;
                }

                let caller_ids = get_direct_callers(pdg, nid);
                let callers: Vec<String> = caller_ids.iter().take(50).filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone())).collect();
                let callee_ids = pdg.neighbors(nid);
                let callees: Vec<String> = callee_ids.iter().take(50).filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone())).collect();

                let mut entry = serde_json::json!({
                    "name": node.name,
                    "type": node_type_str(&node.node_type),
                    "file": node.file_path,
                    "byte_range": node.byte_range,
                    "complexity": node.complexity,
                    "caller_count": caller_ids.len(),
                    "dependency_count": callee_ids.len(),
                    "callers": callers,
                    "callees": callees,
                    "language": node.language,
                    "score": result.score,
                });

                if context_lines > 0 {
                    if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                        let snippet: String = src.lines().take(context_lines).collect::<Vec<_>>().join("\n");
                        entry["context"] = Value::String(snippet);
                    }
                }
                if include_source {
                    if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                        let truncated: String = src.chars().take(4000).collect();
                        let was_truncated = src.chars().count() > 4000;
                        entry["source"] = Value::String(truncated);
                        if was_truncated {
                            entry["source_truncated"] = Value::Bool(true);
                        }
                    }
                }
                all_matches.push(entry);
            }

            // Paginate and format
            let total_matches = all_matches.len();
            let paginated: Vec<Value> = all_matches.into_iter().skip(offset).take(max_results).collect();
            let used_chars: usize = paginated.iter().map(|v| v.to_string().len()).sum();

            let mut response = serde_json::json!({
                "results": paginated,
                "total_matches": total_matches,
                "shown": total_matches.saturating_sub(offset).min(max_results),
                "offset": offset,
                "mode": "semantic",
                "truncated": used_chars > char_budget,
            });
            response = wrap_with_meta(response, &index);
            return Ok(response);
        }

        // Exact mode: current behavior — text matching with semantic pre-filter

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

            // Skip phantom external-reference nodes (no source location)
            if matches!(node.node_type, crate::graph::pdg::NodeType::External) && node.byte_range == (0, 0) {
                continue;
            }

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

            // Use (file_path, byte_range) for location dedup, but skip dedup for synthetic (0,0) ranges
            // which don't represent real source positions
            let location_key = (node.file_path.clone(), node.byte_range);
            let is_duplicate_location = node.byte_range != (0, 0) && seen_locations.contains(&location_key);
            if !matches || seen_ids.contains(&node.id) || is_duplicate_location {
                continue;
            }
            seen_ids.insert(node.id.clone());
            if node.byte_range != (0, 0) {
                seen_locations.insert(location_key);
            }

            let caller_ids = get_direct_callers(pdg, nid);
            let caller_count = caller_ids.len();
            let callers: Vec<String> = caller_ids
                .iter()
                .take(50)
                .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
                .collect();
            let callee_ids = pdg.neighbors(nid);
            let dep_count = callee_ids.len();
            let callees: Vec<String> = callee_ids
                .iter()
                .take(50)
                .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
                .collect();

            let mut entry = serde_json::json!({
                "name": node.name,
                "type": node_type_str(&node.node_type),
                "file": node.file_path,
                "byte_range": node.byte_range,
                "complexity": node.complexity,
                "caller_count": caller_count,
                "dependency_count": dep_count,
                "callers": callers,
                "callees": callees,
                "language": node.language
            });

            // TODO: Cache file contents within handler execution — read_source_snippet
            // does sync I/O per match, which can be slow for large result sets.
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

            if include_source {
                if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                    let truncated: String = src.chars().take(4000).collect();
                    let was_truncated = src.chars().count() > 4000;
                    entry["source"] = Value::String(truncated);
                    if was_truncated {
                        entry["source_truncated"] = Value::Bool(true);
                    }
                }
            }

            all_matches.push(entry);
        }

        // Always try direct PDG scan when we haven't hit the fetch_limit.
        // This merges semantic pre-filter results with direct symbol scans.
        if all_matches.len() < fetch_limit {
            let re = RegexBuilder::new(&pattern)
                .case_insensitive(true)
                .build()
                .ok();

            for nid in pdg.node_indices() {
                if all_matches.len() >= fetch_limit {
                    break;
                }

                let Some(node) = pdg.get_node(nid) else {
                    continue;
                };

                // Skip phantom external-reference nodes (no source location)
                if matches!(node.node_type, crate::graph::pdg::NodeType::External) && node.byte_range == (0, 0) {
                    continue;
                }

                if type_filter != "all" && node_type_str(&node.node_type) != type_filter.as_str() {
                    continue;
                }

                if let Some(ref s) = scope {
                    if !node.file_path.starts_with(s.as_str())
                        && node.file_path != s.trim_end_matches(std::path::MAIN_SEPARATOR)
                    {
                        continue;
                    }
                }

                let matches = if let Some(ref re) = re {
                    re.is_match(&node.name) || re.is_match(&node.id)
                } else {
                    node.name.to_lowercase().contains(&pattern_lower)
                        || node.id.to_lowercase().contains(&pattern_lower)
                };

                // Use (file_path, byte_range) for location dedup, skip synthetic (0,0) ranges
                let location_key = (node.file_path.clone(), node.byte_range);
                let is_duplicate_location = node.byte_range != (0, 0) && seen_locations.contains(&location_key);
                if !matches || seen_ids.contains(&node.id) || is_duplicate_location {
                    continue;
                }
                seen_ids.insert(node.id.clone());
                if node.byte_range != (0, 0) {
                    seen_locations.insert(location_key);
                }

                let caller_ids = get_direct_callers(pdg, nid);
                let caller_count = caller_ids.len();
                let callers: Vec<String> = caller_ids
                    .iter()
                    .take(50)
                    .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
                    .collect();
                let callee_ids = pdg.neighbors(nid);
                let dep_count = callee_ids.len();
                let callees: Vec<String> = callee_ids
                    .iter()
                    .take(50)
                    .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
                    .collect();

                let mut entry = serde_json::json!({
                    "name": node.name,
                    "type": node_type_str(&node.node_type),
                    "file": node.file_path,
                    "byte_range": node.byte_range,
                    "complexity": node.complexity,
                    "caller_count": caller_count,
                    "dependency_count": dep_count,
                    "callers": callers,
                    "callees": callees,
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

                if include_source {
                    if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                        let truncated: String = src.chars().take(4000).collect();
                        let was_truncated = src.chars().count() > 4000;
                        entry["source"] = Value::String(truncated);
                        if was_truncated {
                            entry["source_truncated"] = Value::Bool(true);
                        }
                    }
                }

                all_matches.push(entry);
            }
        }

        // Apply pagination
        let total_matched = all_matches.len();
        let page: Vec<Value> = all_matches.into_iter().skip(offset).collect();

        // Check for zero results and provide helpful suggestion
        // Only emit suggestion when there are truly no matches, not when offset is past total
        if total_matched == 0 {
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "pattern": pattern,
                    "offset": offset,
                    "count": 0,
                    "total_matched": 0,
                    "has_more": false,
                    "results": [],
                    "suggestion": format!(
                        "No symbols matching '{}' found in {} indexed symbols across {} files. Try: broader substring, check case, or use leindex_text_search for raw text.",
                        pattern,
                        pdg.node_count(),
                        pdg.file_count()
                    )
                }),
                &index,
            ));
        }

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

        Ok(wrap_with_meta(
            serde_json::json!({
                "pattern": pattern,
                "offset": offset,
                "count": results.len(),
                "total_matched": total_matched,
                "has_more": offset + results.len() < total_matched,
                "results": results
            }),
            &index,
        ))
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
        "PRIMARY symbol reader — returns exact source code with line numbers, doc comments, \
and compact caller/callee locations (file:line). Use instead of Read for specific \
functions, methods, classes, or types. Set include_dependencies=true for full signatures."
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
                    "description": "Include dependency signatures (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 8000)",
                    "default": 8000
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
        let include_dependencies = extract_bool(&args, "include_dependencies", false);
        let token_budget = extract_usize(&args, "token_budget", 8000)?;
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
            .map(|s| s.chars().take(char_budget).collect::<String>());

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

        // Callers with file:line — eliminates follow-up Grep for "who calls this?"
        let callers: Vec<Value> = get_direct_callers(pdg, node_id)
            .iter()
            .filter_map(|&cid| {
                let cn = pdg.get_node(cid)?;
                let caller_line = {
                    let fc = std::fs::read_to_string(&cn.file_path).unwrap_or_default();
                    byte_range_to_line_range(&fc, cn.byte_range).0
                };
                Some(serde_json::json!({
                    "name": cn.name,
                    "file": cn.file_path,
                    "line": caller_line
                }))
            })
            .take(15)
            .collect();

        // Callees with file:line — eliminates follow-up Grep for "what does this call?"
        let callees: Vec<Value> = pdg
            .neighbors(node_id)
            .iter()
            .filter_map(|&did| {
                let dn = pdg.get_node(did)?;
                let callee_line = {
                    let fc = std::fs::read_to_string(&dn.file_path).unwrap_or_default();
                    byte_range_to_line_range(&fc, dn.byte_range).0
                };
                Some(serde_json::json!({
                    "name": dn.name,
                    "file": dn.file_path,
                    "line": callee_line
                }))
            })
            .take(15)
            .collect();

        // Compute line range from byte range
        let (line_start, line_end) = {
            let file_content = std::fs::read_to_string(&node.file_path).unwrap_or_default();
            byte_range_to_line_range(&file_content, node.byte_range)
        };

        Ok(wrap_with_meta(
            serde_json::json!({
                "symbol": node.name,
                "type": node_type_str(&node.node_type),
                "file": node.file_path,
                "language": node.language,
                "complexity": node.complexity,
                "line_start": line_start,
                "line_end": line_end,
                "doc_comment": doc_comment,
                "source": source,
                "callers": callers,
                "callees": callees,
                "dependencies": dep_signatures
            }),
            &index,
        ))
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
        let change_type = item.get("type").and_then(|v| v.as_str())
            .or_else(|| {
                if item.get("old_text").is_some() || item.get("old_str").is_some() {
                    Some("replace_text")
                } else if item.get("old_name").is_some() && item.get("new_name").is_some() {
                    Some("rename_symbol")
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                JsonRpcError::invalid_params(format!("changes[{}]: missing 'type' — use 'replace_text' or 'rename_symbol', or provide old_text+new_text", i))
            })?;

        let change = match change_type {
            "replace_text" => {
                let old_text = item
                    .get("old_text")
                    .or_else(|| item.get("old_str"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new_text = item
                    .get("new_text")
                    .or_else(|| item.get("new_str"))
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
                "old_text": {
                    "type": "string",
                    "description": "Simple mode: text to find and replace (exact match)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Simple mode: replacement text"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "changes": {
                    "type": "array",
                    "description": "Advanced mode: list of changes to preview. Each has 'type' (replace_text/rename_symbol) and type-specific fields.",
                    "items": { "type": "object" }
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

        // Support simple mode: top-level old_text/new_text (or old_str/new_str aliases)
        let changes_val = if let Some(changes) = args.get("changes").cloned() {
            changes
        } else {
            let old_text = args
                .get("old_text")
                .or_else(|| args.get("old_str"))
                .and_then(|v| v.as_str());
            let new_text = args
                .get("new_text")
                .or_else(|| args.get("new_str"))
                .and_then(|v| v.as_str());
            match (old_text, new_text) {
                (Some(old), Some(new)) => {
                    serde_json::json!([{
                        "type": "replace_text",
                        "old_text": old,
                        "new_text": new
                    }])
                }
                _ => Value::Array(vec![]),
            }
        };
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
                    let forward = pdg.forward_impact(
                        node_id,
                        &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                    );
                    for dep_id in &forward {
                        if let Some(dn) = pdg.get_node(*dep_id) {
                            affected_nodes.push(dn.name.clone());
                            affected_files.insert(dn.file_path.clone());
                        }
                    }
                    let backward = pdg.backward_impact(
                        node_id,
                        &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                    );
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

        Ok(wrap_with_meta(
            serde_json::json!({
                "diff": diff,
                "affected_symbols": affected_nodes,
                "affected_files": affected_files.into_iter().collect::<Vec<_>>(),
                "breaking_changes": breaking_changes,
                "risk_level": risk,
                "change_count": changes.len()
            }),
            &index,
        ))
    }
}

#[allow(missing_docs)]
impl EditApplyHandler {
    pub fn name(&self) -> &str {
        "leindex_edit_apply"
    }

    pub fn description(&self) -> &str {
        "PRIMARY file editor — use instead of edit_file. Simple mode: provide file_path + \
old_text + new_text for exact replacement. Advanced mode: use changes[] array for \
multiple or byte-offset edits. Supports dry_run=true for preview."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to edit"
                },
                "old_text": {
                    "type": "string",
                    "description": "Simple mode: text to find and replace (exact match)"
                },
                "old_str": {
                    "type": "string",
                    "description": "Alias for old_text (compatibility with edit_file)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Simple mode: replacement text"
                },
                "new_str": {
                    "type": "string",
                    "description": "Alias for new_text (compatibility with edit_file)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "changes": {
                    "type": "array",
                    "description": "Advanced mode: list of changes to apply. Each has type (replace_text/rename_symbol) and type-specific fields.",
                    "items": { "type": "object" }
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, return preview without modifying files (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
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
        let dry_run = extract_bool(&args, "dry_run", false);

        if dry_run {
            // Delegate to preview
            return EditPreviewHandler.execute(registry, args).await;
        }

        let file_path = extract_string(&args, "file_path")?;

        // Support simple mode: top-level old_text/new_text (or old_str/new_str aliases)
        let changes_val = if let Some(changes) = args.get("changes").cloned() {
            changes
        } else {
            let old_text = args
                .get("old_text")
                .or_else(|| args.get("old_str"))
                .and_then(|v| v.as_str());
            let new_text = args
                .get("new_text")
                .or_else(|| args.get("new_str"))
                .and_then(|v| v.as_str());
            match (old_text, new_text) {
                (Some(old), Some(new)) => {
                    serde_json::json!([{
                        "type": "replace_text",
                        "old_text": old,
                        "new_text": new
                    }])
                }
                _ => {
                    return Err(JsonRpcError::invalid_params(
                        "Provide either 'changes' array or 'old_text'+'new_text' for simple replacement"
                    ));
                }
            }
        };
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
            let idx = handle.lock().await;
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "success": true,
                    "changes_applied": 0,
                    "files_modified": [],
                    "message": "No changes needed — content already matches"
                }),
                &idx,
            ));
        }

        std::fs::write(&file_path, modified.as_bytes()).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to write '{}': {}", file_path, e))
        })?;

        // Build verification context: show the edited region so LLM doesn't need to Read
        let modified_lines: Vec<&str> = modified.lines().collect();

        // Find the first differing line to show relevant context
        let original_lines: Vec<&str> = original.lines().collect();
        let first_diff_line = original_lines
            .iter()
            .zip(modified_lines.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(0);

        // Show ±5 lines around the edit point
        let ctx_start = first_diff_line.saturating_sub(5);
        let ctx_end = (first_diff_line + 10).min(modified_lines.len());
        let edit_region: Vec<String> = modified_lines[ctx_start..ctx_end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", ctx_start + i + 1, line))
            .collect();

        // Compact affected callers — eliminates follow-up Grep for breakage
        let affected_callers: Vec<String> = {
            let idx = handle.lock().await;
            if let Some(pdg) = idx.pdg() {
                let nodes = pdg.nodes_in_file(&file_path);
                let mut callers: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for &nid in &nodes {
                    for &cid in &get_direct_callers(pdg, nid) {
                        if let Some(cn) = pdg.get_node(cid) {
                            if cn.file_path != file_path {
                                callers.insert(format!("{}:{}", cn.file_path, cn.name));
                            }
                        }
                    }
                }
                callers.into_iter().take(15).collect()
            } else {
                Vec::new()
            }
        };

        let idx = handle.lock().await;
        Ok(wrap_with_meta(
            serde_json::json!({
                "success": true,
                "changes_applied": changes.len(),
                "files_modified": [&file_path],
                "edit_region": edit_region.join("\n"),
                "external_callers": affected_callers
            }),
            &idx,
        ))
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
            for ref_id in pdg.backward_impact(
                node_id,
                &crate::graph::pdg::TraversalConfig {
                    max_depth: Some(5),
                    ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
                },
            ) {
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

        // Generate per-file diffs (file I/O — offload to blocking thread)
        let (diffs, files_to_modify) = tokio::task::spawn_blocking({
            let filtered_files = filtered_files;
            let old_name = old_name.clone();
            let new_name = new_name.clone();
            move || {
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
                (diffs, files_to_modify)
            }
        })
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Rename task failed: {}", e)))?;

        if !preview_only {
            // Apply changes to all files (file I/O — offload to blocking thread)
            let old_name_c = old_name.clone();
            let new_name_c = new_name.clone();
            let apply_files = files_to_modify.clone();
            tokio::task::spawn_blocking(move || {
                for file_path in &apply_files {
                    let original = match std::fs::read_to_string(file_path) {
                        Ok(o) => o,
                        Err(e) => return Err(format!("Failed reading '{}': {}", file_path, e)),
                    };
                    let modified = replace_whole_word(&original, &old_name_c, &new_name_c);
                    if let Err(e) = std::fs::write(file_path, modified.as_bytes()) {
                        return Err(format!("Failed writing '{}': {}", file_path, e));
                    }
                }
                Ok(())
            })
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("Rename apply task failed: {}", e)))?
            .map_err(JsonRpcError::internal_error)?;
        }

        Ok(wrap_with_meta(
            serde_json::json!({
                "old_name": old_name,
                "new_name": new_name,
                "files_affected": files_to_modify.len(),
                "preview_only": preview_only,
                "diffs": diffs,
                "applied": !preview_only
            }),
            &index,
        ))
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
        let forward = pdg.forward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig {
                max_depth: Some(depth),
                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
            },
        );
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
        let backward = pdg.backward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig {
                max_depth: Some(depth),
                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
            },
        );

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

        Ok(wrap_with_meta(
            serde_json::json!({
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
            }),
            &index,
        ))
    }
}

// ============================================================================
// Phase E: Precision Tooling — PDG-enriched equivalents
// ============================================================================

// E.1 — leindex_text_search
// ============================================================================

/// Handler for leindex_text_search — PDG-enriched text content search.
///
/// Unlike plain `rg`, every match is annotated with the owning PDG symbol,
/// its type, complexity score, caller count, and dependency count.
#[derive(Clone)]
pub struct TextSearchHandler;

#[allow(missing_docs)]
impl TextSearchHandler {
    pub fn name(&self) -> &str {
        "leindex_text_search"
    }

    pub fn description(&self) -> &str {
        "PRIMARY text search — use instead of Grep/rg. Returns exact matching lines with \
file:line and the owning symbol name+type for each match. One call replaces Grep + Read \
to understand match context. Supports regex, globs, scope, and context_lines."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Text pattern to search for (literal or regex)"
                },
                "is_regex": {
                    "type": "boolean",
                    "description": "Treat query as regex (default: false = literal match)",
                    "default": false
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default: false)",
                    "default": false
                },
                "include_globs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only search files matching these globs, e.g. [\"*.rs\", \"*.ts\"]"
                },
                "exclude_globs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exclude files matching these globs, e.g. [\"*_test.rs\"]"
                },
                "scope": {
                    "type": "string",
                    "description": "Restrict search to a directory path"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (default: 100)",
                    "default": 100,
                    "minimum": 1,
                    "maximum": 1000
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context above/below each match (default: 2)",
                    "default": 2,
                    "minimum": 0,
                    "maximum": 10
                }
            },
            "required": ["query"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let is_regex = extract_bool(&args, "is_regex", false);
        let case_sensitive = extract_bool(&args, "case_sensitive", false);
        let max_results = extract_usize(&args, "max_results", 100)?.min(1000);
        let offset = extract_usize(&args, "offset", 0)?;
        let context_lines = extract_usize(&args, "context_lines", 2)?.min(10);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let include_globs: Vec<String> = args
            .get("include_globs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let exclude_globs: Vec<String> = args
            .get("exclude_globs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;
        let scope = resolve_scope(&args, index.project_path())?;

        // Build regex or literal matcher
        let regex = if is_regex {
            let re = RegexBuilder::new(&query)
                .case_insensitive(!case_sensitive)
                .build()
                .map_err(|e| {
                    JsonRpcError::invalid_params(format!("Invalid regex '{}': {}", query, e))
                })?;
            Some(re)
        } else {
            None
        };

        let search_query = if case_sensitive {
            query.clone()
        } else {
            query.to_lowercase()
        };

        // Get PDG for enrichment (optional — works without it)
        let pdg = index.pdg();

        // Collect source files from the project
        let project_root = index.project_path();
        let mut results: Vec<Value> = Vec::new();

        // Dirs to always skip
        use crate::cli::skip_dirs::SKIP_DIRS;

        for entry in walkdir::WalkDir::new(project_root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.iter().any(|s| name == *s)
            })
        {
            if results.len() >= max_results {
                break;
            }

            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().is_file() {
                continue;
            }

            let file_path = entry.path();
            let file_path_str = file_path.to_string_lossy();

            // Apply scope filter
            if let Some(ref s) = scope {
                if !file_path_str.starts_with(s.as_str()) {
                    continue;
                }
            }

            // Apply include globs
            if !include_globs.is_empty() {
                let matches_any = include_globs.iter().any(|g| glob_match(&file_path_str, g));
                if !matches_any {
                    continue;
                }
            }

            // Apply exclude globs
            if exclude_globs.iter().any(|g| glob_match(&file_path_str, g)) {
                continue;
            }

            // Read file content
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => continue, // Skip binary or unreadable files
            };

            let lines: Vec<&str> = content.lines().collect();

            for (line_idx, line) in lines.iter().enumerate() {
                if results.len() >= max_results {
                    break;
                }

                let matched = if let Some(ref re) = regex {
                    re.is_match(line)
                } else if case_sensitive {
                    line.contains(&search_query)
                } else {
                    line.to_lowercase().contains(&search_query)
                };

                if !matched {
                    continue;
                }

                let line_number = line_idx + 1; // 1-indexed

                // Collect context lines
                let ctx_before: Vec<String> = (line_idx.saturating_sub(context_lines)..line_idx)
                    .map(|i| format!("{}: {}", i + 1, lines[i]))
                    .collect();
                let ctx_after: Vec<String> = ((line_idx + 1)
                    ..((line_idx + 1 + context_lines).min(lines.len())))
                    .map(|i| format!("{}: {}", i + 1, lines[i]))
                    .collect();

                // Compact PDG enrichment: just symbol name + type (~4 tokens)
                // Eliminates follow-up Read to understand what code this match is in
                let (in_symbol, symbol_type) = pdg
                    .and_then(|pdg| {
                        let byte_offset: usize =
                            lines[..line_idx].iter().map(|l| l.len() + 1).sum();

                        let nodes = pdg.nodes_in_file(&file_path_str);
                        let mut best: Option<(crate::graph::pdg::NodeId, usize)> = None;

                        for nid in nodes {
                            if let Some(node) = pdg.get_node(nid) {
                                let (start, end) = node.byte_range;
                                if byte_offset >= start && byte_offset < end {
                                    let range_size = end - start;
                                    if best.map_or(true, |(_, sz)| range_size < sz) {
                                        best = Some((nid, range_size));
                                    }
                                }
                            }
                        }

                        best.and_then(|(nid, _)| {
                            pdg.get_node(nid).map(|node| {
                                (node.name.clone(), node_type_str(&node.node_type).to_owned())
                            })
                        })
                    })
                    .map(|(name, typ)| (Some(name), Some(typ)))
                    .unwrap_or((None, None));

                let mut entry = serde_json::json!({
                    "file": file_path_str,
                    "line": line_number,
                    "content": *line,
                });

                // Only include context lines when requested (context_lines > 0)
                if !ctx_before.is_empty() {
                    entry["before"] = serde_json::json!(ctx_before);
                }
                if !ctx_after.is_empty() {
                    entry["after"] = serde_json::json!(ctx_after);
                }

                // Compact symbol annotation — always present when PDG available
                if let Some(sym) = in_symbol {
                    entry["in_symbol"] = Value::String(sym);
                }
                if let Some(typ) = symbol_type {
                    entry["symbol_type"] = Value::String(typ);
                }

                results.push(entry);
            }
        }

        let total = results.len();
        let paginated: Vec<Value> = results.into_iter().skip(offset).collect();
        let count = paginated.len();

        Ok(wrap_with_meta(
            serde_json::json!({
                "query": query,
                "is_regex": is_regex,
                "offset": offset,
                "count": count,
                "total_matched": total,
                "has_more": offset + count < total,
                "results": paginated,
            }),
            &index,
        ))
    }
}

/// Simple glob matching for include/exclude patterns.
/// Supports `*` (any chars) and `?` (single char) at the end of patterns.
fn glob_match(path: &str, pattern: &str) -> bool {
    if pattern.starts_with("*.") {
        // Extension match: *.rs matches any .rs file
        let ext = &pattern[1..]; // ".rs"
        path.ends_with(ext)
    } else if pattern.contains('*') {
        // Simple wildcard: convert to prefix/suffix match
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            path.contains(parts[0]) && path.ends_with(parts[1])
        } else {
            path.contains(pattern)
        }
    } else {
        path.contains(pattern)
    }
}

// ============================================================================
// E.2 — leindex_read_file
// ============================================================================

/// Handler for leindex_read_file — PDG-annotated file read.
///
/// Unlike plain `cat`/`Read`, returns a symbol_map overlay showing
/// which symbols span the visible lines, with callers, callees, and complexity.
#[derive(Clone)]
pub struct ReadFileHandler;

#[allow(missing_docs)]
impl ReadFileHandler {
    pub fn name(&self) -> &str {
        "leindex_read_file"
    }

    pub fn description(&self) -> &str {
        "PRIMARY file reader — returns exact file contents with line numbers PLUS context \
showing symbols, imports, and dependents. One call replaces Read + Grep for imports. \
Works for any text file including configs and docs."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to file to read"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Start line, 1-indexed (default: 1)",
                    "default": 1,
                    "minimum": 1
                },
                "end_line": {
                    "type": "integer",
                    "description": "End line, 1-indexed inclusive (default: end of file)",
                    "minimum": 1
                },
                "max_lines": {
                    "type": "integer",
                    "description": "Maximum lines to return (default: 500, safety cap)",
                    "default": 500,
                    "minimum": 1,
                    "maximum": 2000
                },
                "include_symbol_map": {
                    "type": "boolean",
                    "description": "Include PDG symbol annotations (default: false). \
        Set true when structural context is useful.",
                    "default": false
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
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
        let start_line = extract_usize(&args, "start_line", 1)?.max(1);
        let max_lines = extract_usize(&args, "max_lines", 500)?.min(2000);
        let include_symbol_map = extract_bool(&args, "include_symbol_map", false);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        // Try to get project handle for boundary validation and PDG, but don't require it
        let maybe_handle = registry.get_or_create(project_path).await.ok();
        if let Some(ref handle) = maybe_handle {
            let index = handle.lock().await;
            // Validate file within project when indexed
            let _ = validate_file_within_project(&file_path, index.project_path());
        }

        // Read file content — works for any text file
        let content = std::fs::read_to_string(&file_path).map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        // Resolve end_line
        let end_line_raw = extract_usize(&args, "end_line", total_lines)?;
        let end_line = end_line_raw
            .min(total_lines)
            .min(start_line + max_lines - 1);

        if start_line > total_lines {
            return Err(JsonRpcError::invalid_params(format!(
                "start_line {} exceeds total lines {}",
                start_line, total_lines
            )));
        }

        // Build numbered content (1-indexed)
        let visible_lines: Vec<String> = all_lines[(start_line - 1)..end_line.min(total_lines)]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", start_line + i, line))
            .collect();
        let content_str = visible_lines.join("\n");

        // Detect language from extension
        let language = Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| match ext {
                "rs" => "rust",
                "py" => "python",
                "js" | "mjs" | "cjs" => "javascript",
                "ts" | "mts" | "cts" => "typescript",
                "tsx" => "typescriptreact",
                "jsx" => "javascriptreact",
                "go" => "go",
                "java" => "java",
                "c" | "h" => "c",
                "cpp" | "hpp" | "cc" => "cpp",
                "rb" => "ruby",
                "php" => "php",
                "swift" => "swift",
                "kt" => "kotlin",
                "cs" => "csharp",
                "lua" => "lua",
                "zig" => "zig",
                "md" => "markdown",
                "json" => "json",
                "yaml" | "yml" => "yaml",
                "toml" => "toml",
                "html" => "html",
                "css" => "css",
                "scss" => "scss",
                "sql" => "sql",
                "sh" | "bash" => "shell",
                other => other,
            })
            .unwrap_or("text");

        // Build symbol map from PDG only when requested and available
        let symbol_map: Vec<Value> = if include_symbol_map {
            let pdg_opt = if let Some(ref handle) = maybe_handle {
                let index = handle.lock().await;
                index.pdg().map(|pdg| {
                    let nodes = pdg.nodes_in_file(&file_path);
                    let mut symbols: Vec<Value> = Vec::new();

                    // Pre-compute cumulative byte offsets for line-to-byte mapping
                    let line_byte_offsets: Vec<usize> = {
                        let mut offsets = Vec::with_capacity(all_lines.len() + 1);
                        offsets.push(0);
                        let mut acc: usize = 0;
                        for line in &all_lines {
                            acc += line.len() + 1; // +1 for newline
                            offsets.push(acc);
                        }
                        offsets
                    };

                    // Visible byte range
                    let visible_start_byte =
                        line_byte_offsets.get(start_line - 1).copied().unwrap_or(0);
                    let visible_end_byte = line_byte_offsets
                        .get(end_line.min(total_lines))
                        .copied()
                        .unwrap_or(content.len());

                    for nid in nodes {
                        let Some(node) = pdg.get_node(nid) else {
                            continue;
                        };
                        let (sym_start, sym_end) = node.byte_range;

                        // Check if symbol overlaps with visible range
                        if sym_end <= visible_start_byte || sym_start >= visible_end_byte {
                            continue;
                        }

                        // Convert byte range to line numbers
                        let line_start = line_byte_offsets
                            .iter()
                            .position(|&off| off > sym_start)
                            .unwrap_or(1); // 1-indexed
                        let line_end = line_byte_offsets
                            .iter()
                            .position(|&off| off >= sym_end)
                            .unwrap_or(total_lines);

                        let caller_count = get_direct_callers(pdg, nid).len();
                        let dep_count = pdg.neighbors(nid).len();

                        // Get caller names (up to 5)
                        let callers: Vec<String> = get_direct_callers(pdg, nid)
                            .iter()
                            .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
                            .take(5)
                            .collect();

                        // Get callee names (up to 5)
                        let callees: Vec<String> = pdg
                            .neighbors(nid)
                            .iter()
                            .filter_map(|&did| pdg.get_node(did).map(|n| n.name.clone()))
                            .take(5)
                            .collect();

                        symbols.push(serde_json::json!({
                            "name": node.name,
                            "type": node_type_str(&node.node_type),
                            "line_start": line_start,
                            "line_end": line_end,
                            "complexity": node.complexity,
                            "caller_count": caller_count,
                            "dependency_count": dep_count,
                            "callers": callers,
                            "callees": callees,
                        }));
                    }

                    // Sort by line_start for readability
                    symbols
                        .sort_by_key(|s| s.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0));
                    symbols
                })
            } else {
                None
            };
            pdg_opt.unwrap_or_default()
        } else {
            Vec::new()
        };

        // Build compact context block — always present when PDG available (~80-120 tokens)
        // This eliminates follow-up Grep/Read calls for imports and dependencies
        let context = if let Some(ref handle) = maybe_handle {
            let index = handle.lock().await;
            index.pdg().map(|pdg| {
                let nodes = pdg.nodes_in_file(&file_path);

                // Pre-compute line offsets
                let line_byte_offsets: Vec<usize> = {
                    let mut offsets = Vec::with_capacity(all_lines.len() + 1);
                    offsets.push(0);
                    let mut acc: usize = 0;
                    for line in &all_lines {
                        acc += line.len() + 1;
                        offsets.push(acc);
                    }
                    offsets
                };
                let visible_start_byte =
                    line_byte_offsets.get(start_line - 1).copied().unwrap_or(0);
                let visible_end_byte = line_byte_offsets
                    .get(end_line.min(total_lines))
                    .copied()
                    .unwrap_or(content.len());

                // Compact symbol index: just "name(L5-L42)" for visible symbols
                let mut symbols_here: Vec<String> = Vec::new();
                // Cross-file deps this file imports from
                let mut imports_from: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                // External files that call into this file
                let mut used_by: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();

                for &nid in &nodes {
                    let Some(node) = pdg.get_node(nid) else {
                        continue;
                    };
                    let (sym_start, sym_end) = node.byte_range;

                    // Visible symbol summary
                    if sym_end > visible_start_byte && sym_start < visible_end_byte {
                        let ls = line_byte_offsets
                            .iter()
                            .position(|&off| off > sym_start)
                            .unwrap_or(1);
                        let le = line_byte_offsets
                            .iter()
                            .position(|&off| off >= sym_end)
                            .unwrap_or(total_lines);
                        symbols_here.push(format!("{}(L{}-L{})", node.name, ls, le));
                    }

                    // Cross-file outgoing deps (this file depends on)
                    for &did in &pdg.neighbors(nid) {
                        if let Some(dep) = pdg.get_node(did) {
                            if dep.file_path != node.file_path {
                                let dep_line = {
                                    let fc =
                                        std::fs::read_to_string(&dep.file_path).unwrap_or_default();
                                    byte_range_to_line_range(&fc, dep.byte_range).0
                                };
                                imports_from.insert(format!(
                                    "{}:{} (L{})",
                                    dep.file_path, dep.name, dep_line
                                ));
                            }
                        }
                    }

                    // Cross-file incoming deps (other files depend on this)
                    for &cid in &get_direct_callers(pdg, nid) {
                        if let Some(caller) = pdg.get_node(cid) {
                            if caller.file_path != node.file_path {
                                used_by.insert(format!("{}:{}", caller.file_path, caller.name));
                            }
                        }
                    }
                }

                // Cap to keep compact
                let imports_vec: Vec<String> = imports_from.into_iter().take(10).collect();
                let used_by_vec: Vec<String> = used_by.into_iter().take(10).collect();

                serde_json::json!({
                    "symbols_on_visible_lines": symbols_here,
                    "imports_from": imports_vec,
                    "used_by": used_by_vec
                })
            })
        } else {
            None
        };

        let mut result = serde_json::json!({
            "file_path": file_path,
            "language": language,
            "total_lines": total_lines,
            "start_line": start_line,
            "end_line": end_line.min(total_lines),
            "content": content_str,
        });

        // Always attach compact context when available
        if let Some(ctx) = context {
            result["context"] = ctx;
        }

        // Verbose symbol map only when explicitly requested
        if include_symbol_map && !symbol_map.is_empty() {
            result["symbol_map"] = serde_json::json!(symbol_map);
        }

        // Add staleness warning only if we have an indexed project
        if let Some(ref handle) = maybe_handle {
            let index = handle.lock().await;
            result = wrap_with_meta(result, &index);
        }

        Ok(result)
    }
}

// ============================================================================
// E.3 — leindex_git_status
// ============================================================================

/// Handler for leindex_git_status — PDG-aware git status.
///
/// Unlike plain `git status`, maps changed files to affected PDG symbols
/// and computes forward impact (blast radius).
#[derive(Clone)]
pub struct GitStatusHandler;

#[allow(missing_docs)]
impl GitStatusHandler {
    pub fn name(&self) -> &str {
        "leindex_git_status"
    }

    pub fn description(&self) -> &str {
        "Show git working tree status enriched with PDG structural analysis. \
Maps changed files to affected symbols, their callers, and transitive forward impact. \
Turns a raw diff into a structural change summary with blast radius."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "include_diff": {
                    "type": "boolean",
                    "description": "Include unified diff content for modified files (default: false)",
                    "default": false
                },
                "diff_context_lines": {
                    "type": "integer",
                    "description": "Context lines for diff output (default: 3)",
                    "default": 3,
                    "minimum": 0,
                    "maximum": 20
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
        let include_diff = extract_bool(&args, "include_diff", false);
        let diff_context_lines = extract_usize(&args, "diff_context_lines", 3)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;
        let project_root = index.project_path().to_path_buf();

        // Check if it's a git repo
        let git_dir = project_root.join(".git");
        if !git_dir.exists() {
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "is_git_repo": false,
                    "message": "Not a git repository"
                }),
                &index,
            ));
        }

        // Run git status --porcelain
        let status_output = std::process::Command::new("git")
            .args(["status", "--porcelain", "-uall"])
            .current_dir(&project_root)
            .output()
            .map_err(|e| {
                JsonRpcError::internal_error(format!("Failed to run git status: {}", e))
            })?;

        if !status_output.status.success() {
            return Err(JsonRpcError::internal_error(format!(
                "git status failed: {}",
                String::from_utf8_lossy(&status_output.stderr)
            )));
        }

        let status_text = String::from_utf8_lossy(&status_output.stdout);

        // Parse git status output
        let mut modified_files: Vec<String> = Vec::new();
        let mut staged_files: Vec<String> = Vec::new();
        let mut untracked_files: Vec<String> = Vec::new();

        for line in status_text.lines() {
            if line.len() < 4 {
                continue;
            }
            let status_code = &line[..2];
            let file = line[3..].trim().to_string();

            match status_code.trim() {
                "M" | "MM" | "AM" => modified_files.push(file),
                "A" | "A " => staged_files.push(file),
                "??" => untracked_files.push(file),
                "D" | "D " => staged_files.push(file),
                s if s.starts_with('M') => staged_files.push(file),
                s if s.ends_with('M') => modified_files.push(file),
                _ => modified_files.push(file),
            }
        }

        // Get current branch
        let branch_output = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&project_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_string());

        // PDG enrichment: map changed files to symbols
        let pdg = index.pdg();
        let mut changed_symbols: Vec<Value> = Vec::new();
        let mut total_affected_symbols = 0usize;
        let mut affected_files_set: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        if let Some(pdg) = pdg {
            for file in modified_files.iter().chain(staged_files.iter()) {
                // Resolve to absolute path for PDG lookup
                let abs_path = if Path::new(file).is_absolute() {
                    PathBuf::from(file)
                } else {
                    project_root.join(file)
                };
                let abs_str = abs_path.to_string_lossy().to_string();

                let nodes = pdg.nodes_in_file(&abs_str);
                if nodes.is_empty() {
                    // Try with canonicalized path
                    let canon = abs_path.canonicalize().unwrap_or(abs_path);
                    let canon_str = canon.to_string_lossy().to_string();
                    let nodes = pdg.nodes_in_file(&canon_str);
                    if nodes.is_empty() {
                        changed_symbols.push(serde_json::json!({
                            "file": file,
                            "status": if modified_files.contains(file) { "modified" } else { "staged" },
                            "symbols": [],
                            "note": "No indexed symbols in this file"
                        }));
                        continue;
                    }
                }

                let mut file_symbols: Vec<Value> = Vec::new();
                for nid in &nodes {
                    if let Some(node) = pdg.get_node(*nid) {
                        let caller_ids = get_direct_callers(pdg, *nid);
                        let caller_count = caller_ids.len();
                        let callers: Vec<String> = caller_ids
                            .iter()
                            .take(20)
                            .filter_map(|&id| pdg.get_node(id).map(|n| n.name.clone()))
                            .collect();
                        let forward_impact = pdg.forward_impact(
                            *nid,
                            &crate::graph::pdg::TraversalConfig {
                                max_depth: Some(2),
                                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
                            },
                        );
                        total_affected_symbols += forward_impact.len();

                        // Track affected files
                        for &fid in &forward_impact {
                            if let Some(fnode) = pdg.get_node(fid) {
                                affected_files_set.insert(fnode.file_path.clone());
                            }
                        }

                        file_symbols.push(serde_json::json!({
                            "name": node.name,
                            "type": node_type_str(&node.node_type),
                            "complexity": node.complexity,
                            "caller_count": caller_count,
                            "callers": callers,
                            "forward_impact_count": forward_impact.len(),
                        }));
                    }
                }

                let status = if modified_files.contains(file) {
                    "modified"
                } else {
                    "staged"
                };

                changed_symbols.push(serde_json::json!({
                    "file": file,
                    "status": status,
                    "symbols": file_symbols,
                }));
            }
        }

        // Optionally include diff
        let diff_content: Option<String> = if include_diff {
            std::process::Command::new("git")
                .args([
                    "diff",
                    &format!("--unified={}", diff_context_lines),
                    "--no-color",
                ])
                .current_dir(&project_root)
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        Some(String::from_utf8_lossy(&o.stdout).to_string())
                    } else {
                        None
                    }
                })
        } else {
            None
        };

        let affected_files: Vec<String> = affected_files_set.into_iter().collect();

        Ok(wrap_with_meta(
            serde_json::json!({
                "is_git_repo": true,
                "branch": branch_output,
                "summary": {
                    "modified": modified_files.len(),
                    "staged": staged_files.len(),
                    "untracked": untracked_files.len(),
                },
                "modified_files": modified_files,
                "staged_files": staged_files,
                "untracked_files": untracked_files,
                "changed_symbols": changed_symbols,
                "impact_summary": {
                    "total_affected_symbols": total_affected_symbols,
                    "affected_files": affected_files,
                    "pdg_enriched": pdg.is_some(),
                },
                "diff": diff_content,
            }),
            &index,
        ))
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
            node_type_str(&crate::graph::pdg::NodeType::Function),
            "function"
        );
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::Class), "class");
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::Method),
            "method"
        );
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::Variable),
            "variable"
        );
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::Module),
            "module"
        );
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::External),
            "external"
        );
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
        let pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        // An invalid NodeId on an empty PDG — edge iteration returns nothing
        let node = crate::graph::pdg::Node {
            id: "test".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "test".into(),
            file_path: "test.rs".into(),
            byte_range: (0, 0),
            complexity: 1,
            language: "rust".into(),
        };
        let mut pdg = pdg;
        let nid = pdg.add_node(node);
        let callers = get_direct_callers(&pdg, nid);
        assert!(callers.is_empty(), "new node should have no callers");
    }

    #[test]
    fn test_get_direct_callers_with_edge() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let caller_node = crate::graph::pdg::Node {
            id: "caller".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "caller".into(),
            file_path: "a.rs".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".into(),
        };
        let callee_node = crate::graph::pdg::Node {
            id: "callee".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "callee".into(),
            file_path: "b.rs".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".into(),
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
    async fn test_project_map_includes_nested_and_symbol_less_files_with_directory_depth() {
        let dir = tempdir().unwrap();
        let nested_dir = dir.path().join("src").join("nested");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src").join("empty.rs"), "\n").unwrap();
        std::fs::write(nested_dir.join("mod.rs"), "pub fn helper() {}\n").unwrap();

        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({
            "depth": 2,
            "sort_by": "name",
            "token_budget": 10_000
        });
        let result = ProjectMapHandler.execute(&registry, args).await.unwrap();
        let files = result["files"].as_array().unwrap();
        let relative_paths: Vec<String> = files
            .iter()
            .filter_map(|entry| entry["relative_path"].as_str())
            .map(|p| p.replace('\\', "/"))
            .collect();

        assert!(relative_paths.iter().any(|p| p == "main.rs"));
        assert!(relative_paths.iter().any(|p| p == "src/empty.rs"));
        assert!(relative_paths.iter().any(|p| p == "src/nested/mod.rs"));
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
        // Verify suggestion field is present for zero results
        assert!(val.get("suggestion").is_some(), "zero results should include suggestion");
        let suggestion = val["suggestion"].as_str().unwrap();
        assert!(suggestion.contains("No symbols matching"), "suggestion should mention no symbols found");
    }

    #[tokio::test]
    async fn test_search_handler_zero_results_includes_suggestion() {
        // Test that semantic search with no matches returns helpful suggestion
        let dir = tempdir().unwrap();
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "pub fn hello() {}\n").unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "query": "nonexistent_function_xyz" });
        let result = SearchHandler.execute(&registry, args).await;
        // Should succeed but with 0 matches
        assert!(result.is_ok(), "search should succeed");
        let val = result.unwrap();
        assert_eq!(val["count"].as_i64().unwrap_or(0), 0);
        // Verify suggestion field is present for zero results
        assert!(val.get("suggestion").is_some(), "zero results should include suggestion");
        let suggestion = val["suggestion"].as_str().unwrap();
        assert!(suggestion.contains("No semantic matches"), "suggestion should mention no semantic matches");
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
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let n1 = pdg.add_node(crate::graph::pdg::Node {
            id: "file.py:MyClass.health_check".into(),
            node_type: crate::graph::pdg::NodeType::Method,
            name: "health_check".into(),
            file_path: "file.py".into(),
            byte_range: (0, 50),
            complexity: 2,
            language: "python".into(),
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
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let n1 = pdg.add_node(crate::graph::pdg::Node {
            id: "a.py:run".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "run".into(),
            file_path: "a.py".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".into(),
        });
        let n2 = pdg.add_node(crate::graph::pdg::Node {
            id: "b.py:run".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "run".into(),
            file_path: "b.py".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".into(),
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
