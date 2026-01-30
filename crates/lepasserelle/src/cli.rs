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
#[command(subcommand_required = false)]
#[command(arg_required_else_help = false)]
pub struct Cli {
    /// Path to the project directory
    #[arg(global = true, long = "project", short = 'p')]
    pub project_path: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(global = true, long = "verbose", short = 'v')]
    pub verbose: bool,

    /// Compatibility flag for some AI tools (defaults to MCP stdio mode)
    #[arg(long = "stdio")]
    pub stdio: bool,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Option<Commands>,
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
    Mcp {
        /// Compatibility flag for some AI tools
        #[arg(long = "stdio")]
        stdio: bool,
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
        // Default to Mcp if no command is provided or if --stdio is set
        let command = if self.stdio {
            Commands::Mcp { stdio: true }
        } else {
            self.command.unwrap_or(Commands::Mcp { stdio: false })
        };

        match command {
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
            Commands::Mcp { .. } => {
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
async fn cmd_index_impl(path: PathBuf, force: bool, _progress: bool) -> AnyhowResult<()> {
    let canonical_path = path.canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Indexing project at: {}", canonical_path.display());

    // Check if already indexed (unless force)
    // TODO: Add force check logic

    // Create LeIndex and index the project
    let mut leindex = LeIndex::new(&canonical_path)
        .context("Failed to create LeIndex instance")?;

    let stats = tokio::task::spawn_blocking(move || {
        leindex.index_project(force)
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
        println!("{}. {} ({})", i + 1, result.symbol_name, result.file_path);
        println!("   ID: {}", result.node_id);
        println!("   Overall Score: {:.2}", result.score.overall);
        println!("   Explanation: [Semantic: {:.2}, Text: {:.2}, Structural: {:.2}]", 
                 result.score.semantic, result.score.text_match, result.score.structural);
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
    use std::io::{self, BufRead, Read, Write};

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
    let mut reader = io::BufReader::new(stdin.lock());
    let mut use_content_length = false;

    loop {
        let mut line = String::new();
        let bytes = match reader.read_line(&mut line) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[ERROR] Failed to read stdin: {}", e);
                continue;
            }
        };
        if bytes == 0 {
            break;
        }

        let line_trim = line.trim_end();
        if line_trim.is_empty() {
            continue;
        }

        let (json_payload, framed) = if line_trim.to_ascii_lowercase().starts_with("content-length:") {
            let len_str = line_trim.split(':').nth(1).unwrap_or("").trim();
            let length: usize = match len_str.parse() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[ERROR] Invalid Content-Length header: {}", e);
                    continue;
                }
            };

            // Consume remaining header lines until blank line
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 {
                    break;
                }
                if header.trim().is_empty() {
                    break;
                }
            }

            let mut buf = vec![0u8; length];
            if let Err(e) = reader.read_exact(&mut buf) {
                eprintln!("[ERROR] Failed to read JSON payload: {}", e);
                break;
            }

            (String::from_utf8_lossy(&buf).to_string(), true)
        } else {
            (line_trim.to_string(), false)
        };

        use_content_length = use_content_length || framed;

        // Parse JSON-RPC request
        let request: JsonRpcRequest = match serde_json::from_str(&json_payload) {
            Ok(r) => r,
            Err(e) => {
                let error_response = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    JsonRpcError::parse_error(e.to_string())
                );
                let response = serde_json::to_string(&error_response).unwrap_or_default();
                if use_content_length {
                    let _ = writeln!(stdout, "Content-Length: {}\r\n\r\n{}", response.len(), response);
                } else if writeln!(stdout, "{}\n", response).is_err() {
                    break;
                }
                let _ = stdout.flush();
                continue;
            }
        };

        // Extract request ID before moving request
        let request_id = request.id.clone();

        // Check if this is a notification (no response expected for notifications)
        let is_notification = request_id.is_null();

        // Handle the request
        let response = match handle_mcp_request(request, project_path.clone()).await {
            Ok(r) => r,
            Err(e) => {
                JsonRpcResponse::error(
                    request_id.clone(),
                    JsonRpcError::internal_error(e.to_string())
                )
            }
        };

        // Only send response if this is not a notification
        if !is_notification {
            // Write response to stdout
            let response_json = match serde_json::to_string(&response) {
                Ok(j) => j,
                Err(e) => {
                    // If we can't serialize, create a simple error response
                    format!("{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32700,\"message\":\"Failed to serialize response: {}\"}}}}", e)
                }
            };

            if use_content_length {
                if writeln!(stdout, "Content-Length: {}\r\n\r\n{}", response_json.len(), response_json).is_err() {
                    eprintln!("[ERROR] Failed to write to stdout");
                    break;
                }
            } else if writeln!(stdout, "{}\n", response_json).is_err() {
                eprintln!("[ERROR] Failed to write to stdout");
                break;
            }

            // Flush to ensure immediate delivery
            let _ = stdout.flush();
        }
    }

    Ok(())
}

/// Handle a single MCP request and return the response
async fn handle_mcp_request(request: JsonRpcRequest, _project_path: PathBuf) -> anyhow::Result<JsonRpcResponse> {
    use crate::mcp::server::{HANDLERS, SERVER_STATE, handle_tool_call, list_tools_json};

    let method_name = request.method.clone();
    let id = request.id.clone();

    // Get the global state and handlers
    let state = SERVER_STATE.get()
        .ok_or_else(|| anyhow::anyhow!("Server state not initialized"))?;

    let handlers = HANDLERS.get()
        .ok_or_else(|| anyhow::anyhow!("Handlers not initialized"))?;

    // Handle different methods
    match method_name.as_str() {
        "initialize" => {
            // MCP protocol initialization handshake
            // Return server capabilities
            Ok(JsonRpcResponse::success(id, serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "leindex",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })))
        }
        "notifications/initialized" => {
            // Client notification sent after successful initialization
            // No response needed for notifications
            Ok(JsonRpcResponse::success(id, serde_json::json!({})))
        }
        "tools/call" => {
            // Use the centralized tool call handler that formats for MCP
            let result = handle_tool_call(state, handlers, request).await;
            Ok(JsonRpcResponse::from_result(id, result))
        }
        "tools/list" => {
            // List all available tools using centralized formatter
            Ok(JsonRpcResponse::success(id, list_tools_json(handlers)))
        }
        _ => {
            Ok(JsonRpcResponse::error(id, crate::mcp::protocol::JsonRpcError::method_not_found(method_name)))
        }
    }
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
        assert!(matches!(cli.command, Some(Commands::Index { .. })));
    }

    #[test]
    fn test_mcp_command_parsing() {
        let cli = Cli::try_parse_from(["leindex", "mcp"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Mcp { .. })));
    }

    #[test]
    fn test_stdio_flag_parsing() {
        let cli = Cli::try_parse_from(["leindex", "--stdio"]).unwrap();
        assert!(cli.stdio);
    }

    #[test]
    fn test_search_command() {
        let cli = Cli::try_parse_from(["leindex", "search", "test query"]).unwrap();
        match cli.command {
            Some(Commands::Search { query, top_k, .. }) => {
                assert_eq!(query, "test query");
                assert_eq!(top_k, 10);
            }
            _ => panic!("Expected Search command"),
        }
    }
}
