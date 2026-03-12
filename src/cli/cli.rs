// CLI Interface
//
// This module provides the command-line interface for LeIndex.

use crate::cli::leindex::LeIndex;
use crate::cli::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::cli::mcp::McpServer;
use crate::cli::registry::{ProjectRegistry, DEFAULT_MAX_PROJECTS};
use crate::phase::{run_phase_analysis, DocsMode, FormatMode, PhaseOptions, PhaseSelection};
use anyhow::Context;
use anyhow::Result as AnyhowResult;
use clap::{error::ErrorKind, Parser, Subcommand};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tracing::{info, warn};

const POST_INSTALL_SKIP_ENV: &str = "LEINDEX_SKIP_POST_INSTALL_HOOK";
const POST_INSTALL_STAR_MARKER: &str = ".github-starred";
const POST_INSTALL_VERSION_MARKER: &str = ".post-install-version";
const REPO_STAR_ENDPOINT: &str = "user/starred/scooter-lacroix/LeIndex";

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

    /// Run additive 5-phase analysis workflow
    Phase {
        /// Specific phase to run (1..5)
        #[arg(long = "phase")]
        phase: Option<u8>,

        /// Run all phases (1..5)
        #[arg(long = "all", default_value_t = false)]
        all: bool,

        /// Formatting mode: ultra|balanced|verbose
        #[arg(long = "mode", default_value = "balanced")]
        mode: String,

        /// Path to analyze (defaults to current/global project)
        #[arg(long = "path")]
        path: Option<PathBuf>,

        /// Maximum files to consider
        #[arg(long = "max-files", default_value = "2000")]
        max_files: usize,

        /// Maximum focus files in phase 3
        #[arg(long = "max-focus-files", default_value = "20")]
        max_focus_files: usize,

        /// Top-N entries for ranking phases
        #[arg(long = "top-n", default_value = "10")]
        top_n: usize,

        /// Maximum output characters
        #[arg(long = "max-chars", default_value = "12000")]
        max_output_chars: usize,

        /// Explicitly opt in to Markdown/Text analysis
        #[arg(long = "include-docs", default_value_t = false)]
        include_docs: bool,

        /// Docs mode: off|markdown|text|all
        #[arg(long = "docs-mode", default_value = "off")]
        docs_mode: String,

        /// Disable incremental freshness checks (forces full refresh)
        #[arg(long = "no-incremental-refresh", default_value_t = false)]
        no_incremental_refresh: bool,
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

    /// Start the frontend dashboard
    Dashboard {
        /// Port to run the dashboard on (default: 5173)
        #[arg(long = "port", default_value = "5173")]
        port: u16,

        /// Build for production instead of dev server
        #[arg(long = "prod")]
        prod: bool,
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

        maybe_complete_post_install_actions(&command);

        match command {
            Commands::Index {
                path,
                force,
                progress,
            } => cmd_index_impl(path, force, progress).await,
            Commands::Search { query, top_k } => {
                cmd_search_impl(query, top_k, global_project).await
            }
            Commands::Analyze {
                query,
                token_budget,
            } => cmd_analyze_impl(query, token_budget, global_project).await,
            Commands::Phase {
                phase,
                all,
                mode,
                path,
                max_files,
                max_focus_files,
                top_n,
                max_output_chars,
                include_docs,
                docs_mode,
                no_incremental_refresh,
            } => {
                cmd_phase_impl(
                    phase,
                    all,
                    mode,
                    path,
                    global_project,
                    max_files,
                    max_focus_files,
                    top_n,
                    max_output_chars,
                    include_docs,
                    docs_mode,
                    no_incremental_refresh,
                )
                .await
            }
            Commands::Diagnostics => cmd_diagnostics_impl(global_project).await,
            Commands::Serve { host, port } => cmd_serve_impl(host, port).await,
            Commands::Mcp { .. } => cmd_mcp_stdio_impl(global_project).await,
            Commands::Dashboard { port, prod } => cmd_dashboard_impl(port, prod).await,
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

fn maybe_complete_post_install_actions(command: &Commands) {
    if std::env::var_os(POST_INSTALL_SKIP_ENV).is_some()
        || matches!(command, Commands::Mcp { .. })
        || !running_from_cargo_bin()
    {
        return;
    }

    let leindex_home = match resolve_leindex_home() {
        Ok(path) => path,
        Err(error) => {
            warn!("Post-install actions skipped: {}", error);
            return;
        }
    };

    if post_install_is_current(&leindex_home) {
        return;
    }

    if let Err(error) = complete_post_install_actions(command, &leindex_home) {
        warn!("Post-install actions skipped: {}", error);
    }
}

fn complete_post_install_actions(
    command: &Commands,
    leindex_home: &std::path::Path,
) -> AnyhowResult<()> {
    fs::create_dir_all(&leindex_home).context("failed to create LEINDEX_HOME")?;
    cleanup_legacy_user_installations(&leindex_home);

    let marker_path = leindex_home.join(POST_INSTALL_STAR_MARKER);
    if !marker_path.exists() {
        emit_post_install_message(command, "Thank you for installing LeIndex.");

        if try_star_repo() {
            emit_post_install_message(command, "Starred scooter-lacroix/LeIndex on GitHub.");
            fs::write(&marker_path, b"starred\n").context("failed to persist star marker")?;
        } else {
            emit_post_install_message(
                command,
                "Could not star the GitHub repo automatically. If GitHub CLI is signed in, run: gh api -X PUT user/starred/scooter-lacroix/LeIndex",
            );
            fs::write(&marker_path, b"prompted\n").context("failed to persist star marker")?;
        }
    }

    warn_if_path_is_shadowed(command);
    write_post_install_version_marker(leindex_home)?;

    Ok(())
}

fn resolve_leindex_home() -> AnyhowResult<PathBuf> {
    if let Ok(path) = std::env::var("LEINDEX_HOME") {
        return Ok(PathBuf::from(path));
    }

    let home = dirs::home_dir().context("HOME is not available")?;
    Ok(home.join(".leindex"))
}

fn post_install_is_current(leindex_home: &std::path::Path) -> bool {
    let marker_path = leindex_home.join(POST_INSTALL_VERSION_MARKER);
    match fs::read_to_string(marker_path) {
        Ok(version) => version.trim() == env!("CARGO_PKG_VERSION"),
        Err(_) => false,
    }
}

fn write_post_install_version_marker(leindex_home: &std::path::Path) -> AnyhowResult<()> {
    let marker_path = leindex_home.join(POST_INSTALL_VERSION_MARKER);
    fs::write(marker_path, format!("{}\n", env!("CARGO_PKG_VERSION")))
        .context("failed to persist post-install marker")
}

fn cleanup_legacy_user_installations(leindex_home: &std::path::Path) {
    let Some(home) = dirs::home_dir() else {
        return;
    };

    let binary_name = platform_binary_name("leindex");
    let legacy_local_bin = home.join(".local").join("bin").join(&binary_name);
    if legacy_local_bin.exists() {
        match fs::remove_file(&legacy_local_bin) {
            Ok(_) => info!("Removed legacy install at {}", legacy_local_bin.display()),
            Err(error) => warn!(
                "Failed to remove legacy install at {}: {}",
                legacy_local_bin.display(),
                error
            ),
        }
    }

    let legacy_home_bin = leindex_home.join("bin").join(binary_name);
    if legacy_home_bin.exists() {
        match fs::remove_file(&legacy_home_bin) {
            Ok(_) => info!("Removed legacy install at {}", legacy_home_bin.display()),
            Err(error) => warn!(
                "Failed to remove legacy install at {}: {}",
                legacy_home_bin.display(),
                error
            ),
        }
    }
}

fn running_from_cargo_bin() -> bool {
    let Ok(current_exe) = std::env::current_exe() else {
        return false;
    };

    let cargo_home = cargo_home_dir();

    let Some(cargo_home) = cargo_home else {
        return false;
    };

    current_exe == cargo_home.join("bin").join(platform_binary_name("leindex"))
}

fn resolve_path_binary(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }

        if cfg!(windows) {
            let exe_candidate = entry.join(platform_binary_name(binary_name));
            if exe_candidate.is_file() {
                return Some(exe_candidate);
            }
        }
    }
    None
}

fn warn_if_path_is_shadowed(command: &Commands) {
    let Ok(current_exe) = std::env::current_exe() else {
        return;
    };

    let Some(resolved) = resolve_path_binary("leindex") else {
        return;
    };

    if resolved == current_exe {
        return;
    }

    emit_post_install_message(
        command,
        &format!(
            "`leindex` currently resolves to {} instead of {}. Remove the older binary or move {} earlier in PATH.",
            resolved.display(),
            current_exe.display(),
            cargo_bin_dir()
                .unwrap_or_else(|| current_exe.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf())
                .display()
        ),
    );
}

fn cargo_home_dir() -> Option<PathBuf> {
    std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cargo")))
}

fn cargo_bin_dir() -> Option<PathBuf> {
    cargo_home_dir().map(|cargo_home| cargo_home.join("bin"))
}

fn platform_binary_name(binary_name: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", binary_name)
    } else {
        binary_name.to_string()
    }
}

fn try_star_repo() -> bool {
    let auth_ok = Command::new("gh")
        .args(["auth", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    if !auth_ok {
        return false;
    }

    Command::new("gh")
        .args([
            "api",
            "-X",
            "PUT",
            "-H",
            "Accept: application/vnd.github+json",
            REPO_STAR_ENDPOINT,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn emit_post_install_message(command: &Commands, message: &str) {
    if matches!(command, Commands::Serve { .. } | Commands::Dashboard { .. }) {
        info!("{}", message);
    } else {
        eprintln!("{}", message);
    }
}

/// Get project path from explicit path or current directory
fn get_project_path(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| std::env::current_dir().unwrap())
}

/// Index command implementation
async fn cmd_index_impl(path: PathBuf, force: bool, _progress: bool) -> AnyhowResult<()> {
    let canonical_path = path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Indexing project at: {}", canonical_path.display());

    // Check if already indexed (unless force)
    // TODO: Add force check logic

    // Create LeIndex and index the project
    let mut leindex = LeIndex::new(&canonical_path).context("Failed to create LeIndex instance")?;

    let stats = tokio::task::spawn_blocking(move || leindex.index_project(force))
        .await
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
async fn cmd_search_impl(
    query: String,
    top_k: usize,
    project: Option<PathBuf>,
) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Searching for: {}", query);

    // Create LeIndex and try to load from storage
    let mut leindex = LeIndex::new(&canonical_path).context("Failed to create LeIndex instance")?;

    // Load from storage if available
    if let Err(e) = leindex.load_from_storage() {
        warn!("Failed to load from storage: {}", e);
        warn!("Project may not be indexed. Run 'leindex index' first.");
    }

    // Perform search
    let results = leindex
        .search(&query, top_k, None)
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
        println!(
            "   Explanation: [Semantic: {:.2}, Text: {:.2}, Structural: {:.2}]",
            result.score.semantic, result.score.text_match, result.score.structural
        );
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
async fn cmd_analyze_impl(
    query: String,
    token_budget: usize,
    project: Option<PathBuf>,
) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Analyzing: {}", query);

    // Create LeIndex and try to load from storage
    let mut leindex = LeIndex::new(&canonical_path).context("Failed to create LeIndex instance")?;

    // Load from storage if available
    if let Err(e) = leindex.load_from_storage() {
        warn!("Failed to load from storage: {}", e);
        warn!("Project may not be indexed. Run 'leindex index' first.");
    }

    // Perform analysis
    let result = leindex
        .analyze(&query, token_budget)
        .context("Analysis failed")?;

    // Print results
    println!("\nAnalysis Results for: '{}'", query);
    println!("Found {} entry point(s)", result.results.len());
    println!("Tokens used: {}", result.tokens_used);
    println!("Processing time: {}ms\n", result.processing_time_ms);

    if let Some(context) = &result.context {
        println!("Context:");
        println!("{}", context);
    }

    Ok(())
}

/// Phase command implementation
#[allow(clippy::too_many_arguments)]
async fn cmd_phase_impl(
    phase: Option<u8>,
    all: bool,
    mode: String,
    path: Option<PathBuf>,
    project: Option<PathBuf>,
    max_files: usize,
    max_focus_files: usize,
    top_n: usize,
    max_output_chars: usize,
    include_docs: bool,
    docs_mode: String,
    no_incremental_refresh: bool,
) -> AnyhowResult<()> {
    if !all && phase.is_none() {
        anyhow::bail!("Specify either --phase <1..5> or --all");
    }

    if all && phase.is_some() {
        anyhow::bail!("Use either --phase or --all, not both");
    }

    let target_path = path
        .or(project)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let canonical_path = target_path
        .canonicalize()
        .context("Failed to canonicalize phase analysis path")?;

    let (root, focus_files) = if canonical_path.is_file() {
        let parent = canonical_path
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("phase analysis file path has no parent directory"))?;
        (parent, vec![canonical_path.clone()])
    } else {
        (canonical_path, Vec::new())
    };

    let parsed_mode = FormatMode::parse(&mode)
        .ok_or_else(|| anyhow::anyhow!("Invalid mode '{}'. Use ultra|balanced|verbose", mode))?;

    let parsed_docs_mode = DocsMode::parse(&docs_mode).ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid docs mode '{}'. Use off|markdown|text|all",
            docs_mode
        )
    })?;

    let selection = if all {
        PhaseSelection::All
    } else {
        let p = phase.unwrap();
        PhaseSelection::from_number(p)
            .ok_or_else(|| anyhow::anyhow!("Invalid phase '{}'. Use 1..5", p))?
    };

    let options = PhaseOptions {
        root,
        focus_files,
        mode: parsed_mode,
        max_files,
        max_focus_files,
        top_n,
        max_output_chars,
        use_incremental_refresh: !no_incremental_refresh,
        include_docs,
        docs_mode: parsed_docs_mode,
        hotspot_keywords: PhaseOptions::default().hotspot_keywords,
    };

    let report = tokio::task::spawn_blocking(move || run_phase_analysis(options, selection))
        .await
        .context("Phase task failed")??;

    println!("{}", report.formatted_output);
    Ok(())
}

/// Diagnostics command implementation
async fn cmd_diagnostics_impl(project: Option<PathBuf>) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Fetching diagnostics");

    // Create LeIndex and try to load from storage
    let mut leindex = LeIndex::new(&canonical_path).context("Failed to create LeIndex instance")?;

    // Load from storage if available
    if let Err(e) = leindex.load_from_storage() {
        warn!("Failed to load from storage: {}", e);
    }

    // Get diagnostics
    let diag = leindex
        .get_diagnostics()
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
    println!(
        "  Current: {:.2} MB",
        diag.memory_usage_bytes as f64 / 1024.0 / 1024.0
    );
    println!(
        "  Total: {:.2} MB",
        diag.total_memory_bytes as f64 / 1024.0 / 1024.0
    );
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
        env_port.parse::<u16>().unwrap_or(port).min(65535)
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
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;

    let leindex = LeIndex::new(&current_dir).context("Failed to create LeIndex instance")?;

    // Create and run the MCP server
    let server = McpServer::with_address(addr, leindex).context("Failed to create MCP server")?;

    println!("\nLeIndex MCP Server\n");
    println!("Server starting on http://{}\n", addr);
    println!("Available endpoints:");
    println!("  POST /mcp           - JSON-RPC 2.0 endpoint");
    println!("  GET  /mcp/tools/list - List available tools");
    println!("  GET  /health         - Health check");
    println!("\nConfiguration:");
    println!("  Port: {} (override with LEINDEX_PORT env var)", port);
    println!("\nPress Ctrl+C to stop the server\n");

    server.run().await.context("Server error")?;

    Ok(())
}

/// MCP stdio command implementation - Run MCP server in stdio mode
/// This mode allows AI tools to start LeIndex as a subprocess for automatic integration
async fn cmd_mcp_stdio_impl(project: Option<PathBuf>) -> AnyhowResult<()> {
    use crate::cli::mcp::handlers::{
        ContextHandler, DeepAnalyzeHandler, DiagnosticsHandler, EditApplyHandler,
        EditPreviewHandler, FileSummaryHandler, GitStatusHandler, GrepSymbolsHandler,
        ImpactAnalysisHandler, IndexHandler, PhaseAnalysisAliasHandler, PhaseAnalysisHandler,
        ProjectMapHandler, ReadFileHandler, ReadSymbolHandler, RenameSymbolHandler, SearchHandler,
        SymbolLookupHandler, TextSearchHandler,
    };
    use crate::cli::mcp::protocol::{JsonRpcError, JsonRpcMessage, JsonRpcResponse};
    use std::io::{self, BufRead, Read, Write};

    let project_path = get_project_path(project);
    let canonical_path = project_path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

    info!(
        "Starting LeIndex MCP stdio server for project: {}",
        canonical_path.display()
    );

    // Create LeIndex instance
    let mut leindex = LeIndex::new(&canonical_path).context("Failed to create LeIndex instance")?;

    // Try to load from storage, but don't fail if not indexed yet
    let _ = leindex.load_from_storage();

    // Initialize global state for handlers
    let registry = Arc::new(ProjectRegistry::with_initial_project(
        DEFAULT_MAX_PROJECTS,
        leindex,
    ));
    let _ = crate::cli::mcp::server::SERVER_STATE.set(registry.clone());

    // Initialize handlers
    let _ = crate::cli::mcp::server::HANDLERS.set(vec![
        crate::cli::mcp::handlers::ToolHandler::DeepAnalyze(DeepAnalyzeHandler),
        crate::cli::mcp::handlers::ToolHandler::Diagnostics(DiagnosticsHandler),
        crate::cli::mcp::handlers::ToolHandler::Index(IndexHandler),
        crate::cli::mcp::handlers::ToolHandler::Context(ContextHandler),
        crate::cli::mcp::handlers::ToolHandler::Search(SearchHandler),
        crate::cli::mcp::handlers::ToolHandler::PhaseAnalysis(PhaseAnalysisHandler),
        crate::cli::mcp::handlers::ToolHandler::PhaseAnalysisAlias(PhaseAnalysisAliasHandler),
        // Phase C: Tool Supremacy
        crate::cli::mcp::handlers::ToolHandler::FileSummary(FileSummaryHandler),
        crate::cli::mcp::handlers::ToolHandler::SymbolLookup(SymbolLookupHandler),
        crate::cli::mcp::handlers::ToolHandler::ProjectMap(ProjectMapHandler),
        crate::cli::mcp::handlers::ToolHandler::GrepSymbols(GrepSymbolsHandler),
        crate::cli::mcp::handlers::ToolHandler::ReadSymbol(ReadSymbolHandler),
        // Phase D: Context-Aware Editing
        crate::cli::mcp::handlers::ToolHandler::EditPreview(EditPreviewHandler),
        crate::cli::mcp::handlers::ToolHandler::EditApply(EditApplyHandler),
        crate::cli::mcp::handlers::ToolHandler::RenameSymbol(RenameSymbolHandler),
        crate::cli::mcp::handlers::ToolHandler::ImpactAnalysis(ImpactAnalysisHandler),
        // Phase E: Precision Tooling
        crate::cli::mcp::handlers::ToolHandler::TextSearch(TextSearchHandler),
        crate::cli::mcp::handlers::ToolHandler::ReadFile(ReadFileHandler),
        crate::cli::mcp::handlers::ToolHandler::GitStatus(GitStatusHandler),
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

        let (json_payload, framed) = if line_trim
            .to_ascii_lowercase()
            .starts_with("content-length:")
        {
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

        // Parse JSON-RPC message (request or notification)
        let message = match JsonRpcMessage::from_json(&json_payload) {
            Ok(m) => m,
            Err(e) => {
                let error_response = JsonRpcResponse::error(serde_json::Value::Null, e);
                let response = serde_json::to_string(&error_response).unwrap_or_default();
                if use_content_length {
                    let _ = writeln!(
                        stdout,
                        "Content-Length: {}\r\n\r\n{}",
                        response.len(),
                        response
                    );
                } else if writeln!(stdout, "{}", response).is_err() {
                    break;
                }
                let _ = stdout.flush();
                continue;
            }
        };

        // Handle based on message type
        match message {
            JsonRpcMessage::Notification(notification) => {
                eprintln!(
                    "[INFO] Received notification: {} (type: {})",
                    notification.method,
                    notification.notification_type()
                );
                continue;
            }
            JsonRpcMessage::Request(request) => {
                let request_id = request.id.clone().unwrap_or(serde_json::Value::Null);

                let response = match handle_mcp_request(request, project_path.clone()).await {
                    Ok(r) => r,
                    Err(e) => JsonRpcResponse::error(
                        request_id,
                        JsonRpcError::internal_error(e.to_string()),
                    ),
                };

                let response_json = match serde_json::to_string(&response) {
                    Ok(j) => j,
                    Err(e) => {
                        format!("{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32700,\"message\":\"Failed to serialize response: {}\"}}}}", e)
                    }
                };

                if use_content_length {
                    if writeln!(
                        stdout,
                        "Content-Length: {}\r\n\r\n{}",
                        response_json.len(),
                        response_json
                    )
                    .is_err()
                    {
                        eprintln!("[ERROR] Failed to write to stdout");
                        break;
                    }
                } else if writeln!(stdout, "{}", response_json).is_err() {
                    eprintln!("[ERROR] Failed to write to stdout");
                    break;
                }
                let _ = stdout.flush();
            }
        }
    }

    Ok(())
}

/// Dashboard command implementation - Start the frontend dashboard
async fn cmd_dashboard_impl(port: u16, prod: bool) -> AnyhowResult<()> {
    use std::process::Command;

    // Find the dashboard directory.
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;
    let dashboard_path = {
        let mut candidates = Vec::new();

        // 1) Current directory.
        candidates.push(current_dir.join("dashboard"));

        // 2) Parent traversal for source checkouts.
        let mut parent = current_dir.as_path();
        for _ in 0..5 {
            if let Some(next) = parent.parent() {
                candidates.push(next.join("dashboard"));
                parent = next;
            } else {
                break;
            }
        }

        // 3) Explicit override for packaged installs.
        if let Ok(explicit) = std::env::var("LEINDEX_DASHBOARD_DIR") {
            candidates.push(PathBuf::from(explicit));
        }

        // 4) Installer default location.
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(PathBuf::from(home).join(".leindex").join("dashboard"));
        }

        candidates
            .into_iter()
            .find(|path| path.exists() && path.is_dir())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Dashboard directory not found. Checked current repo paths, LEINDEX_DASHBOARD_DIR, and ~/.leindex/dashboard."
                )
            })?
    };

    // Check if bun is installed
    let bun_exists = Command::new("bun")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !bun_exists {
        anyhow::bail!(
            "Bun is required to run the dashboard. Please install it first:\n  curl -fsSL https://bun.sh/install | bash"
        );
    }

    println!("\nLeIndex Dashboard\n");
    println!("Starting dashboard server...\n");

    if prod {
        // Build for production
        println!("Building dashboard for production...");
        let build_status = Command::new("bun")
            .current_dir(&dashboard_path)
            .arg("run")
            .arg("build")
            .status()
            .context("Failed to build dashboard")?;

        if !build_status.success() {
            anyhow::bail!("Dashboard build failed");
        }

        println!("\nDashboard built successfully!");
        println!("Built files: {}/dist", dashboard_path.display());
        println!("\nTo serve the production build, use:");
        println!("  cd {} && bun run start", dashboard_path.display());
    } else {
        // Start dev server
        println!("Dashboard will be available at: http://localhost:{}", port);
        println!("Press Ctrl+C to stop the server\n");

        let status = Command::new("bun")
            .current_dir(&dashboard_path)
            .arg("run")
            .arg("dev")
            .status()
            .context("Failed to start dashboard")?;

        if !status.success() {
            anyhow::bail!("Dashboard server exited with error");
        }
    }

    Ok(())
}

/// Handle a single MCP request and return the response
async fn handle_mcp_request(
    request: JsonRpcRequest,
    _project_path: PathBuf,
) -> anyhow::Result<JsonRpcResponse> {
    use crate::cli::mcp::server::{handle_tool_call, list_tools_json, HANDLERS, SERVER_STATE};

    let method_name = request.method.clone();
    let id = request.id.clone().unwrap_or(serde_json::Value::Null);

    // Get the global state and handlers
    let state = SERVER_STATE
        .get()
        .ok_or_else(|| anyhow::anyhow!("Server state not initialized"))?;

    let handlers = HANDLERS
        .get()
        .ok_or_else(|| anyhow::anyhow!("Handlers not initialized"))?;

    // Handle different methods
    match method_name.as_str() {
        "initialize" => {
            // MCP protocol initialization handshake
            // Return server capabilities with comprehensive description
            Ok(JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {
                            "listChanged": true
                        },
                        "prompts": {
                            "listChanged": true
                        },
                        "resources": {
                            "listChanged": true,
                            "subscribe": false
                        },
                        "logging": {},
                        "progress": true
                    },
                    "serverInfo": {
                        "name": "leindex",
                        "version": env!("CARGO_PKG_VERSION"),
                        "description": "LeIndex MCP Server - Semantic code indexing and analysis with PDG-based tools. Provides 18+ specialized tools for code comprehension: semantic search, symbol lookup, impact analysis, structural code queries, and intelligent editing. Uses Program Dependence Graphs for superior code understanding compared to traditional text-based tools."
                    }
                }),
            ))
        }

        "notifications/initialized" => {
            // Client notification sent after successful initialization
            // No response needed for notifications
            Ok(JsonRpcResponse::success(id, serde_json::json!({})))
        }
        "ping" => {
            // Simple health check
            Ok(JsonRpcResponse::success(id, serde_json::json!({})))
        }
        "tools/call" => {
            // Use the centralized tool call handler that formats for MCP
            let result = handle_tool_call(state, handlers, &request).await;
            Ok(JsonRpcResponse::from_result(id, result))
        }
        "tools/list" => {
            // List all available tools using centralized formatter
            Ok(JsonRpcResponse::success(id, list_tools_json(handlers)))
        }
        _ => Ok(JsonRpcResponse::error(
            id,
            crate::cli::mcp::protocol::JsonRpcError::method_not_found(method_name),
        )),
    }
}

/// Main entry point for the CLI
pub async fn main() -> AnyhowResult<()> {
    match Cli::try_parse() {
        Ok(cli) => cli.run().await,
        Err(err) => {
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                maybe_complete_post_install_actions(&Commands::Diagnostics);
            }
            err.exit()
        }
    }
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

    #[test]
    fn test_phase_command_parsing() {
        let cli =
            Cli::try_parse_from(["leindex", "phase", "--phase", "2", "--mode", "ultra"]).unwrap();
        match cli.command {
            Some(Commands::Phase {
                phase, all, mode, ..
            }) => {
                assert_eq!(phase, Some(2));
                assert!(!all);
                assert_eq!(mode, "ultra");
            }
            _ => panic!("Expected Phase command"),
        }
    }

    #[test]
    fn test_dashboard_command_parsing() {
        let cli = Cli::try_parse_from(["leindex", "dashboard"]).unwrap();
        match cli.command {
            Some(Commands::Dashboard { port, prod }) => {
                assert_eq!(port, 5173);
                assert!(!prod);
            }
            _ => panic!("Expected Dashboard command"),
        }
    }

    #[test]
    fn test_dashboard_command_with_port() {
        let cli = Cli::try_parse_from(["leindex", "dashboard", "--port", "3000"]).unwrap();
        match cli.command {
            Some(Commands::Dashboard { port, prod }) => {
                assert_eq!(port, 3000);
                assert!(!prod);
            }
            _ => panic!("Expected Dashboard command"),
        }
    }

    #[test]
    fn test_dashboard_command_prod() {
        let cli = Cli::try_parse_from(["leindex", "dashboard", "--prod"]).unwrap();
        match cli.command {
            Some(Commands::Dashboard { port, prod }) => {
                assert_eq!(port, 5173);
                assert!(prod);
            }
            _ => panic!("Expected Dashboard command"),
        }
    }
}
