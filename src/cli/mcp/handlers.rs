// MCP Tool Handlers
//
// This module implements the handlers for each MCP tool that the server exposes.
// Individual handler implementations live in their own `*_handler.rs` files.
// The `dispatch_handler!` macro generates the tool dispatch table.

// Import all handler implementations
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
pub use super::write_handler::WriteHandler;

// ── Tool surface definition ──────────────────────────────────────────────
//
// One line per tool.  The macro expands to:
//   • `enum ToolHandler { … }`          (with #[derive(Clone)])
//   • `pub fn all_tool_handlers() -> Vec<ToolHandler> { … }`
//   • Standard method delegations: `name()`, `description()`, `argument_schema()`, `execute()`
//
dispatch_handler! {
    /// Handler for indexing a project
    Index               => IndexHandler,
    /// Handler for semantic code search
    Search              => SearchHandler,
    /// Handler for deep PDG analysis
    DeepAnalyze         => DeepAnalyzeHandler,
    /// Handler for expanding PDG context
    Context             => ContextHandler,
    /// Handler for project diagnostics
    Diagnostics         => DiagnosticsHandler,
    /// Handler for multi-phase analysis
    PhaseAnalysis       => PhaseAnalysisHandler,
    /// Alias for phase analysis (compatibility)
    PhaseAnalysisAlias  => PhaseAnalysisAliasHandler,
    /// Handler for structured file overview
    FileSummary         => FileSummaryHandler,
    /// Handler for call-graph lookups
    SymbolLookup        => SymbolLookupHandler,
    /// Handler for project structure mapping
    ProjectMap          => ProjectMapHandler,
    /// Handler for symbol grep
    GrepSymbols         => GrepSymbolsHandler,
    /// Handler for reading symbol source
    ReadSymbol          => ReadSymbolHandler,
    /// Handler for atomic file write
    Write               => WriteHandler,
    /// Handler for edit preview
    EditPreview         => EditPreviewHandler,
    /// Handler for edit apply
    EditApply           => EditApplyHandler,
    /// Handler for symbol rename
    RenameSymbol        => RenameSymbolHandler,
    /// Handler for impact analysis
    ImpactAnalysis      => ImpactAnalysisHandler,
    /// Handler for PDG-aware text search
    TextSearch           => TextSearchHandler,
    /// Handler for file reading
    ReadFile            => ReadFileHandler,
    /// Handler for git status
    GitStatus           => GitStatusHandler,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handler_names() {
        assert_eq!(IndexHandler.name(), "LeIndex [Index]");
        assert_eq!(SearchHandler.name(), "LeIndex [Search]");
        assert_eq!(DeepAnalyzeHandler.name(), "LeIndex [Deep Analyze]");
        assert_eq!(ContextHandler.name(), "LeIndex [Context]");
        assert_eq!(DiagnosticsHandler.name(), "LeIndex [Diagnostics]");
        assert_eq!(PhaseAnalysisHandler.name(), "LeIndex [Phase Analysis]");
        assert_eq!(PhaseAnalysisAliasHandler.name(), "phase_analysis");
        // Phase C handlers
        assert_eq!(FileSummaryHandler.name(), "LeIndex [File Summary]");
        assert_eq!(SymbolLookupHandler.name(), "LeIndex [Symbol Lookup]");
        assert_eq!(ProjectMapHandler.name(), "LeIndex [Project Map]");
        assert_eq!(GrepSymbolsHandler.name(), "LeIndex [Grep Symbols]");
        assert_eq!(ReadSymbolHandler.name(), "LeIndex [Read Symbol]");
        assert_eq!(WriteHandler.name(), "LeIndex [Write]");
        // Phase D handlers
        assert_eq!(EditPreviewHandler.name(), "LeIndex [Edit Preview]");
        assert_eq!(EditApplyHandler.name(), "LeIndex [Edit Apply]");
        assert_eq!(RenameSymbolHandler.name(), "LeIndex [Rename Symbol]");
        assert_eq!(ImpactAnalysisHandler.name(), "LeIndex [Impact Analysis]");
    }

    #[test]
    fn test_argument_schemas() {
        let schemas = vec![
            IndexHandler.argument_schema(),
            SearchHandler.argument_schema(),
            DeepAnalyzeHandler.argument_schema(),
            WriteHandler.argument_schema(),
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
            WriteHandler.argument_schema(),
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
