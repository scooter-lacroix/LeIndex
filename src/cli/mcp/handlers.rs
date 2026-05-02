// MCP Tool Handlers
//
// This module implements the handlers for each MCP tool that the server exposes.
// Individual handler implementations live in their own `*_handler.rs` files.
// The `dispatch_handler!` macro generates the `ToolHandler` enum and all
// dispatch methods from a single compact table — adding a new tool is just
// one extra line.

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
pub use super::write_handler::WriteHandler;

// ── Tool surface definition ──────────────────────────────────────────────
//
// One line per tool.  The macro expands to:
//   • `enum ToolHandler { … }`          (with #[derive(Clone)])
//   • `fn all_tool_handlers() -> Vec`   (construction list)
//   • `impl ToolHandler { name, description, argument_schema, execute }`
//
// To add a new tool: import the handler struct above, then add one line here.
dispatch_handler! {
    /// Handler for project indexing
    Index               => IndexHandler,
    /// Handler for semantic search
    Search              => SearchHandler,
    /// Handler for deep code analysis
    DeepAnalyze         => DeepAnalyzeHandler,
    /// Handler for code context expansion
    Context             => ContextHandler,
    /// Handler for system diagnostics
    Diagnostics         => DiagnosticsHandler,
    /// Handler for multi-phase analysis
    PhaseAnalysis       => PhaseAnalysisHandler,
    /// Handler for multi-phase analysis (alias)
    PhaseAnalysisAlias  => PhaseAnalysisAliasHandler,
    /// Handler for file summary
    FileSummary         => FileSummaryHandler,
    /// Handler for symbol relationship lookup
    SymbolLookup        => SymbolLookupHandler,
    /// Handler for project map
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
    /// Handler for text search
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
