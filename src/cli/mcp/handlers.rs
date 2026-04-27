// MCP Tool Handlers
//
// This module implements the handlers for each MCP tool that the server exposes.
// Individual handler implementations live in their own `*_handler.rs` files.
// This file contains the ToolHandler enum and dispatch methods.

use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

// Import and re-export handler structs from submodules
pub use super::context_handler::ContextHandler;
pub use super::deep_analyze_handler::DeepAnalyzeHandler;
pub use super::diagnostics_handler::DiagnosticsHandler;
pub use super::edit_apply_handler::EditApplyHandler;
pub use super::edit_preview_handler::EditPreviewHandler;
pub use super::file_summary_handler::FileSummaryHandler;
pub use super::git_status_handler::GitStatusHandler;
pub use super::grep_symbols_handler::GrepSymbolsHandler;
pub use super::impact_analysis_handler::ImpactAnalysisHandler;
pub use super::index_handler::IndexHandler;
pub use super::phase_handler::{PhaseAnalysisAliasHandler, PhaseAnalysisHandler};
pub use super::project_map_handler::ProjectMapHandler;
pub use super::read_file_handler::ReadFileHandler;
pub use super::read_symbol_handler::ReadSymbolHandler;
pub use super::rename_symbol_handler::RenameSymbolHandler;
pub use super::search_handler::SearchHandler;
pub use super::symbol_lookup_handler::SymbolLookupHandler;
pub use super::text_search_handler::TextSearchHandler;

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
    /// Handler for multi-phase analysis
    PhaseAnalysis(PhaseAnalysisHandler),
    /// Handler for multi-phase analysis (alias)
    PhaseAnalysisAlias(PhaseAnalysisAliasHandler),
    /// Handler for file summary
    FileSummary(FileSummaryHandler),
    /// Handler for symbol relationship lookup
    SymbolLookup(SymbolLookupHandler),
    /// Handler for project map
    ProjectMap(ProjectMapHandler),
    /// Handler for symbol grep
    GrepSymbols(GrepSymbolsHandler),
    /// Handler for reading symbol source
    ReadSymbol(ReadSymbolHandler),
    /// Handler for edit preview
    EditPreview(EditPreviewHandler),
    /// Handler for edit apply
    EditApply(EditApplyHandler),
    /// Handler for symbol rename
    RenameSymbol(RenameSymbolHandler),
    /// Handler for impact analysis
    ImpactAnalysis(ImpactAnalysisHandler),
    /// Handler for text search
    TextSearch(TextSearchHandler),
    /// Handler for file reading
    ReadFile(ReadFileHandler),
    /// Handler for git status
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

#[cfg(test)]
mod tests {
    use super::*;

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
