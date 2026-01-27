// CLI Interface
//
// This module provides the command-line interface for LeIndex.

use anyhow::Context;
use crate::leindex::LeIndex;
use crate::mcp::McpServer;
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use anyhow::Result as AnyhowResult;
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
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

        /// Port to listen on (default: 47268, override with LEINDEX_PORT env var)
        #[arg(long = "port", default_value = "47268")]
        port: u16,
    },

    /// Run MCP server in stdio mode (for AI tool subprocess integration)
    Mcp,
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
            Commands::Mcp => {
                cmd_mcp_stdio_impl(global_project).await
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
        .with_writer(std::io::stderr)
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
    // Check for environment variable override (for customization via LEINDEX_PORT)
    let port = if let Ok(env_port) = std::env::var("LEINDEX_PORT") {
        env_port.parse::<u16>()
            .unwrap_or(port)
            .min(65535)
    } else {
        port
    };

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

    println!("\nLeIndex MCP Server\n");
    println!("Server starting on http://{}\n", addr);
    println!("Available endpoints:");
    println!("  POST /mcp           - JSON-RPC 2.0 endpoint");
    println!("  GET  /mcp/tools/list - List available tools");
    println!("  GET  /health         - Health check");
    println!("\nConfiguration:");
    println!("  Port: {} (override with LEINDEX_PORT env var)", port);
    println!("\nPress Ctrl+C to stop the server\n");

    server.run().await
        .context("Server error")?;

    Ok(())
}

/// MCP stdio command implementation - Run MCP server in stdio mode
/// This mode allows AI tools to start LeIndex as a subprocess for automatic integration
async fn cmd_mcp_stdio_impl(project: Option<PathBuf>) -> AnyhowResult<()> {
    use crate::mcp::handlers::{IndexHandler, SearchHandler, DiagnosticsHandler, DeepAnalyzeHandler, ContextHandler};
    use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, JsonRpcError};
    use std::io::{self, BufRead, Write};

    let project_path = get_project_path(project);
    let canonical_path = project_path.canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Starting LeIndex MCP stdio server for project: {}", canonical_path.display());

    // Create LeIndex instance
    let mut leindex = LeIndex::new(&canonical_path)
        .context("Failed to create LeIndex instance")?;

    // Try to load from storage, but don't fail if not indexed yet
    let _ = leindex.load_from_storage();

    // Initialize global state for handlers
    let state = Arc::new(Mutex::new(leindex));
    let _ = crate::mcp::server::SERVER_STATE.set(state.clone());

    // Initialize handlers
    let _ = crate::mcp::server::HANDLERS.set(vec![
        crate::mcp::handlers::ToolHandler::DeepAnalyze(DeepAnalyzeHandler),
        crate::mcp::handlers::ToolHandler::Diagnostics(DiagnosticsHandler),
        crate::mcp::handlers::ToolHandler::Index(IndexHandler),
        crate::mcp::handlers::ToolHandler::Context(ContextHandler),
        crate::mcp::handlers::ToolHandler::Search(SearchHandler),
    ]);

    eprintln!("[INFO] LeIndex MCP stdio server starting");
    eprintln!("[INFO] Project: {}", canonical_path.display());
    eprintln!("[INFO] Reading JSON-RPC from stdin, writing to stdout");
    eprintln!("[INFO] Press Ctrl+C to stop\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    // Read stdin line by line for JSON-RPC requests
    let reader = stdin.lock();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[ERROR] Failed to read stdin: {}", e);
                continue;
            }
        };

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse JSON-RPC request
        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let error_response = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    JsonRpcError::parse_error(e.to_string())
                );
                let response = serde_json::to_string(&error_response).unwrap_or_default();
                if writeln!(stdout, "{}\n", response).is_err() {
                    break;
                }
                continue;
            }
        };

        // Extract request ID before moving request
        let request_id = request.id.clone();

        // Handle the request
        let response = match handle_mcp_request(request, project_path.clone()).await {
            Ok(r) => r,
            Err(e) => {
                JsonRpcResponse::error(
                    request_id,
                    JsonRpcError::internal_error(e.to_string())
                )
            }
        };

        // Write response to stdout
        let response_json = match serde_json::to_string(&response) {
            Ok(j) => j,
            Err(e) => {
                // If we can't serialize, create a simple error response
                format!("{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32700,\"message\":\"Failed to serialize response: {}\"}}}}", e)
            }
        };

        if writeln!(stdout, "{}\n", response_json).is_err() {
            eprintln!("[ERROR] Failed to write to stdout");
            break;
        }

        // Flush to ensure immediate delivery
        let _ = stdout.flush();
    }

    Ok(())
}

/// Handle a single MCP request and return the response
async fn handle_mcp_request(request: JsonRpcRequest, _project_path: PathBuf) -> anyhow::Result<JsonRpcResponse> {
    use crate::mcp::server::{HANDLERS, SERVER_STATE};

    let method_name = request.method.clone();
    let id = request.id.clone();

    // Get the global state and handlers
    let state = SERVER_STATE.get()
        .ok_or_else(|| anyhow::anyhow!("Server state not initialized"))?;

    let handlers = HANDLERS.get()
        .ok_or_else(|| anyhow::anyhow!("Handlers not initialized"))?;

    // Handle different methods
    let result = match method_name.as_str() {
        "tools/call" => {
            // Extract tool call from request
            let tool_call = request.extract_tool_call()
                .map_err(|e| anyhow::anyhow!("Failed to extract tool call: {}", e))?;

            // Find the handler for this tool
            let handler = handlers.iter()
                .find(|h| h.name() == tool_call.name)
                .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", tool_call.name))?;

            // Execute the handler
            handler.execute(state, tool_call.arguments).await
                .map_err(|e| anyhow::anyhow!("Handler execution failed: {:?}", e))?
        }
        "tools/list" => {
            // List all available tools
            let tools: Vec<_> = handlers.iter()
                .map(|handler| {
                    serde_json::json!({
                        "name": handler.name(),
                        "description": handler.description(),
                        "inputSchema": handler.argument_schema()
                    })
                })
                .collect();
            serde_json::json!({ "tools": tools })
        }
        _ => {
            return Err(anyhow::anyhow!("Unknown method: {}", method_name));
        }
    };

    Ok(JsonRpcResponse::success(id, result))
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
