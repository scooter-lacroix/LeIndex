// CLI Interface
//
// This module provides the command-line interface for LeIndex.

use anyhow::Context;
use crate::leindex::LeIndex;
use crate::mcp::McpServer;
use anyhow::Result as AnyhowResult;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::{info, warn};

/// LeIndex - Code Search and Analysis Engine
#[derive(Parser, Debug)]
#[command(name = "leindex")]
#[command(author = "LeIndex Contributors")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Index, search, and analyze codebases with semantic understanding", long_about = None)]
pub struct Cli {
    /// Path to the project directory
    #[arg(global = true, long = "project", short = 'p')]
    pub project_path: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(global = true, long = "verbose", short = 'v')]
    pub verbose: bool,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Commands,
}

/// Available CLI commands
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Index a project for code search and analysis
    Index {
        /// Path to the project directory
        #[arg(value_name = "PATH")]
        path: PathBuf,

        /// Force re-indexing even if already indexed
        #[arg(long = "force")]
        force: bool,

        /// Show detailed progress
        #[arg(long = "progress")]
        progress: bool,
    },

    /// Search indexed code
    Search {
        /// Search query
        #[arg(value_name = "QUERY")]
        query: String,

        /// Maximum number of results to return
        #[arg(long = "top-k", default_value = "10")]
        top_k: usize,
    },

    /// Perform deep analysis with context expansion
    Analyze {
        /// Analysis query
        #[arg(value_name = "QUERY")]
        query: String,

        /// Maximum tokens for context expansion
        #[arg(long = "tokens", default_value = "2000")]
        token_budget: usize,
    },

    /// Show system diagnostics
    Diagnostics,

    /// Start MCP server for AI assistant integration
    Serve {
        /// Host address to bind to
        #[arg(long = "host", default_value = "127.0.0.1")]
        host: String,

        /// Port to listen on
        #[arg(long = "port", default_value = "3000")]
        port: u16,
    },
}

impl Cli {
    /// Run the CLI
    pub async fn run(self) -> AnyhowResult<()> {
        // Initialize logging
        init_logging_impl(self.verbose);

        // Get global project path
        let global_project = self.project_path;

        // Execute the appropriate command
        match self.command {
            Commands::Index { path, force, progress } => {
                cmd_index_impl(path, force, progress).await
            }
            Commands::Search { query, top_k } => {
                cmd_search_impl(query, top_k, global_project).await
            }
            Commands::Analyze { query, token_budget } => {
                cmd_analyze_impl(query, token_budget, global_project).await
            }
            Commands::Diagnostics => {
                cmd_diagnostics_impl(global_project).await
            }
            Commands::Serve { host, port } => {
                cmd_serve_impl(host, port).await
            }
        }
    }
}

/// Initialize logging implementation
fn init_logging_impl(verbose: bool) {
    let level = if verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    let subscriber = tracing_subscriber::fmt()
        .with_max_level(level)
        .finish();

    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Get project path from explicit path or current directory
fn get_project_path(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| std::env::current_dir().unwrap())
}

/// Index command implementation
async fn cmd_index_impl(path: PathBuf, _force: bool, _progress: bool) -> AnyhowResult<()> {
    let canonical_path = path.canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Indexing project at: {}", canonical_path.display());

    // Check if already indexed (unless force)
    // TODO: Add force check logic

    // Create LeIndex and index the project
    let mut leindex = LeIndex::new(&canonical_path)
        .context("Failed to create LeIndex instance")?;

    let stats = tokio::task::spawn_blocking(move || {
        leindex.index_project()
    }).await
        .context("Indexing task failed")?
        .context("Indexing failed")?;

    // Print results
    println!("\n✓ Indexing complete!");
    println!("  Files parsed: {}", stats.files_parsed);
    println!("  Successful: {}", stats.successful_parses);
    println!("  Failed: {}", stats.failed_parses);
    println!("  Signatures: {}", stats.total_signatures);
    println!("  PDG nodes: {}", stats.pdg_nodes);
    println!("  PDG edges: {}", stats.pdg_edges);
    println!("  Indexed nodes: {}", stats.indexed_nodes);
    println!("  Time: {}ms", stats.indexing_time_ms);

    Ok(())
}

/// Search command implementation
async fn cmd_search_impl(query: String, top_k: usize, project: Option<PathBuf>) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path.canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Searching for: {}", query);

    // Create LeIndex and try to load from storage
    let mut leindex = LeIndex::new(&canonical_path)
        .context("Failed to create LeIndex instance")?;

    // Load from storage if available
    if let Err(e) = leindex.load_from_storage() {
        warn!("Failed to load from storage: {}", e);
        warn!("Project may not be indexed. Run 'leindex index' first.");
    }

    // Perform search
    let results = leindex.search(&query, top_k)
        .context("Search failed")?;

    if results.is_empty() {
        println!("No results found for: {}", query);
        return Ok(());
    }

    // Print results
    println!("\nFound {} result(s) for: '{}'\n", results.len(), query);
    for (i, result) in results.iter().enumerate() {
        println!("{}. {} ({}:{})", i + 1, result.symbol_name, result.file_path, result.node_id);
        println!("   Score: {:.2}", result.score.overall);
        if let Some(context) = &result.context {
            let context_preview = if context.len() > 100 {
                format!("{}...", &context[..100])
            } else {
                context.clone()
            };
            println!("   Context: {}", context_preview);
        }
        println!();
    }

    Ok(())
}

/// Analyze command implementation
async fn cmd_analyze_impl(query: String, token_budget: usize, project: Option<PathBuf>) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path.canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Analyzing: {}", query);

    // Create LeIndex and try to load from storage
    let mut leindex = LeIndex::new(&canonical_path)
        .context("Failed to create LeIndex instance")?;

    // Load from storage if available
    if let Err(e) = leindex.load_from_storage() {
        warn!("Failed to load from storage: {}", e);
        warn!("Project may not be indexed. Run 'leindex index' first.");
    }

    // Perform analysis
    let result = leindex.analyze(&query, token_budget)
        .context("Analysis failed")?;

    // Print results
    println!("\nAnalysis Results for: '{}'\n", query);
    println!("Found {} entry point(s)", result.results.len());
    println!("Tokens used: {}", result.tokens_used);
    println!("Processing time: {}ms\n", result.processing_time_ms);

    if let Some(context) = &result.context {
        println!("Context:");
        println!("{}", context);
    }

    Ok(())
}

/// Diagnostics command implementation
async fn cmd_diagnostics_impl(project: Option<PathBuf>) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path.canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Fetching diagnostics");

    // Create LeIndex and try to load from storage
    let mut leindex = LeIndex::new(&canonical_path)
        .context("Failed to create LeIndex instance")?;

    // Load from storage if available
    if let Err(e) = leindex.load_from_storage() {
        warn!("Failed to load from storage: {}", e);
    }

    // Get diagnostics
    let diag = leindex.get_diagnostics()
        .context("Failed to get diagnostics")?;

    // Print diagnostics
    println!("\nLeIndex Diagnostics\n");
    println!("Project: {}", diag.project_id);
    println!("Path: {}", diag.project_path);
    println!("\nIndex Statistics:");
    println!("  Files parsed: {}", diag.stats.files_parsed);
    println!("  Successful: {}", diag.stats.successful_parses);
    println!("  Failed: {}", diag.stats.failed_parses);
    println!("  Total signatures: {}", diag.stats.total_signatures);
    println!("  PDG nodes: {}", diag.stats.pdg_nodes);
    println!("  PDG edges: {}", diag.stats.pdg_edges);
    println!("  Indexed nodes: {}", diag.stats.indexed_nodes);
    println!("\nMemory Usage:");
    println!("  Current: {:.2} MB", diag.memory_usage_bytes as f64 / 1024.0 / 1024.0);
    println!("  Total: {:.2} MB", diag.total_memory_bytes as f64 / 1024.0 / 1024.0);
    println!("  Usage: {:.1}%", diag.memory_usage_percent);
    if diag.memory_threshold_exceeded {
        println!("  ⚠️  Memory threshold exceeded!");
    }

    Ok(())
}

/// Serve command implementation - Start MCP server
async fn cmd_serve_impl(host: String, port: u16) -> AnyhowResult<()> {
    // Parse the address
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .context("Invalid address or port")?;

    info!("Starting MCP server on {}", addr);

    // Create a default LeIndex instance for the server
    // The server will use the current directory as the project path
    let current_dir = std::env::current_dir()
        .context("Failed to get current directory")?;

    let leindex = LeIndex::new(&current_dir)
        .context("Failed to create LeIndex instance")?;

    // Create and run the MCP server
    let server = McpServer::with_address(addr, leindex)
        .context("Failed to create MCP server")?;

    println!("\nMaestro-LeIndex MCP Server\n");
    println!("Server starting on http://{}\n", addr);
    println!("Available endpoints:");
    println!("  POST /mcp           - JSON-RPC 2.0 endpoint");
    println!("  GET  /mcp/tools/list - List available tools");
    println!("  GET  /health         - Health check");
    println!("\nMCP Configuration for AI Assistants:");
    println!("  {{");
    println!("    \"mcpServers\": {{");
    println!("      \"maestro-leindex\": {{");
    println!("        \"command\": \"leindex\",");
    println!("        \"args\": [\"serve\", \"--host\", \"127.0.0.1\", \"--port\", \"{}\"]", port);
    println!("      }}");
    println!("    }}");
    println!("  }}");
    println!("\nPress Ctrl+C to stop the server\n");

    server.run().await
        .context("Server error")?;

    Ok(())
}

/// Main entry point for the CLI
pub async fn main() -> AnyhowResult<()> {
    let cli = Cli::parse();
    cli.run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parsing() {
        let cli = Cli::try_parse_from(["leindex", "index", "/path/to/project"]).unwrap();
        assert!(matches!(cli.command, Commands::Index { .. }));
    }

    #[test]
    fn test_search_command() {
        let cli = Cli::try_parse_from(["leindex", "search", "test query"]).unwrap();
        match cli.command {
            Commands::Search { query, top_k, .. } => {
                assert_eq!(query, "test query");
                assert_eq!(top_k, 10);
            }
            _ => panic!("Expected Search command"),
        }
    }
}
