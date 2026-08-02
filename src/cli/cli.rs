// CLI Interface
//
// This module provides the command-line interface for LeIndex.

use crate::cli::leindex::LeIndex;
use crate::cli::mcp::McpServer;
use crate::cli::mcp::output::render_tool_output;
#[cfg(test)]
use mcp_commands::find_tool_handler;
use mcp_commands::{
    cmd_mcp_socket_impl, cmd_mcp_stdio_impl, cmd_tools_impl, execute_tool_handler, merge_tool_args,
};

#[path = "mcp_commands.rs"]
mod mcp_commands;
use crate::phase::{DocsMode, FormatMode, PhaseOptions, PhaseSelection, run_phase_analysis};
use anyhow::Context;
use anyhow::Result as AnyhowResult;
use clap::{Parser, Subcommand, error::ErrorKind};
use serde_json::Value;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
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

    /// Write a lightweight memory summary to PATH on graceful shutdown.
    ///
    /// The report is a compact JSON file containing peak RSS and phase-level
    /// max/sample information. Also enabled by the `LEINDEX_MEMORY_REPORT`
    /// environment variable (the CLI flag takes precedence).
    #[arg(global = true, long = "memory-report", value_name = "PATH")]
    pub memory_report: Option<PathBuf>,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Available CLI commands
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Index a project for code search and analysis
    #[command(visible_alias = "leindex_index")]
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

        /// Self-imposed RSS limit in megabytes.
        ///
        /// When set, indexing monitors RSS and stops gracefully if the limit
        /// is approached (warning at 90%) or exceeded (error). On Linux a hard
        /// RLIMIT_AS ceiling is also set at 110% of the cap to prevent system
        /// OOM kills. No-op on non-Linux platforms if monitoring is unavailable.
        #[arg(long = "max-memory", value_name = "MB")]
        max_memory: Option<u64>,
    },

    /// Search indexed code
    #[command(visible_alias = "leindex_search")]
    Search {
        /// Search query
        #[arg(value_name = "QUERY")]
        query: String,

        /// Maximum number of results to return
        #[arg(long = "top-k", default_value = "10")]
        top_k: usize,
    },

    /// Perform deep analysis with context expansion
    #[command(visible_alias = "leindex_deep_analyze")]
    Analyze {
        /// Analysis query
        #[arg(value_name = "QUERY")]
        query: String,

        /// Maximum tokens for context expansion
        #[arg(long = "tokens", default_value = "2000")]
        token_budget: usize,
    },

    /// Expand context around a symbol or node
    #[command(visible_alias = "leindex_context")]
    Context {
        /// Symbol or node ID to expand
        #[arg(value_name = "NODE_ID")]
        node_id: String,

        /// Maximum tokens for context expansion
        #[arg(long = "tokens", default_value = "2000")]
        token_budget: usize,
    },

    /// Run additive 5-phase analysis workflow
    #[command(visible_aliases = ["leindex_phase_analysis", "phase_analysis"])]
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
    #[command(visible_alias = "leindex_diagnostics")]
    Diagnostics,

    /// List, inspect, or run the MCP tool surface directly from the CLI
    #[command(disable_help_subcommand = true)]
    Tools {
        /// Tool action to perform
        #[command(subcommand)]
        command: ToolCommands,
    },

    /// Start MCP server for AI assistant integration
    Serve {
        /// Host address to bind to
        #[arg(long = "host", default_value = "127.0.0.1")]
        host: String,

        /// Port to listen on (default: 47500, override with LEINDEX_PORT env var)
        #[arg(long = "port", default_value = "47500")]
        port: u16,
    },

    /// Run MCP server in stdio mode (for AI tool subprocess integration)
    Mcp {
        /// Compatibility flag for some AI tools
        #[arg(long = "stdio")]
        stdio: bool,

        /// Path to a Unix domain socket to listen on (instead of stdio).
        ///
        /// Each connection gets its own MCP session. The socket file is
        /// removed when the server shuts down. Only available on Unix.
        #[arg(long = "socket")]
        socket: Option<PathBuf>,

        /// Exit the MCP server after this many seconds with no requests.
        /// Overrides `[mcp] idle_timeout_secs` from leindex.toml. `0` disables
        /// idle exit (server lives until stdin EOF). MCP clients respawn the
        /// server on the next tool call. Memory-pressure remediation 1.11.0.
        #[arg(long = "mcp-idle-timeout-secs")]
        idle_timeout_secs: Option<u64>,
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

    /// Remove stale LeIndex temp artifacts
    Cleanup {
        /// Maximum age in days for artifacts to keep (default: 7)
        #[arg(long = "max-age-days", default_value = "7")]
        max_age_days: u64,

        /// Show what would be removed without actually removing
        #[arg(long = "dry-run")]
        dry_run: bool,
    },

    /// Configure neural search: install ORT, set up models, and write config
    ///
    /// Run `leindex setup` for an interactive wizard, or use flags for
    /// non-interactive mode. Neural embeddings provide semantic code search
    /// (finding symbols by meaning). TF-IDF search works without setup.
    Setup {
        /// Enable neural embeddings (non-interactive mode)
        #[arg(long = "neural")]
        neural: bool,

        /// Disable neural embeddings (TF-IDF only, non-interactive)
        #[arg(long = "no-neural", conflicts_with = "neural")]
        no_neural: bool,

        /// Use CPU execution provider (conflicts with --gpu)
        #[arg(long = "cpu", conflicts_with = "gpu")]
        cpu: bool,

        /// Use GPU execution provider: amd (MIGraphX/ROCm) or nvidia (CUDA)
        #[arg(long = "gpu", value_name = "amd|nvidia", conflicts_with = "cpu")]
        gpu: Option<String>,

        /// Check current setup status without modifying anything
        #[arg(long = "check")]
        check: bool,

        /// Pre-compile MIGraphX kernels by spawning the embed worker and
        /// sending a warmup inference request. This populates the MIGraphX
        /// cache so subsequent index runs skip compilation.
        /// Auto-runs on cold cache when neural is enabled with a GPU provider.
        #[arg(long = "warmup")]
        warmup: bool,
    },
}

/// Subcommands for inspecting and executing MCP tools from the CLI.
#[derive(Subcommand, Debug)]
pub enum ToolCommands {
    /// List every MCP/CLI tool name and description
    List,

    /// Show comprehensive help for a tool
    Help {
        /// Tool name (for example: leindex_project_map, project_map, or project-map)
        name: String,
    },

    /// Print the JSON argument schema for a tool
    Schema {
        /// Tool name (for example: leindex_project_map, project_map, or project-map)
        name: String,
    },

    /// Execute a tool by name with JSON arguments
    Run {
        /// Tool name (for example: leindex_project_map, project_map, or project-map)
        name: String,

        /// JSON object of tool arguments
        #[arg(long = "args", default_value = "{}")]
        args_json: String,

        /// Additional key=value arguments merged on top of --args
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
}

impl Cli {
    /// Run the CLI
    pub async fn run(self) -> AnyhowResult<()> {
        // Initialize logging
        init_logging_impl(self.verbose);

        // Set up optional memory report tracker.
        // The tracker observes RSS during execution and writes a compact JSON
        // summary on drop (graceful shutdown). Also enabled via
        // LEINDEX_MEMORY_REPORT env var; CLI flag takes precedence.
        if let Some(path) =
            crate::cli::memory_report::resolve_report_path(self.memory_report.as_deref())
        {
            crate::cli::memory_report::init_tracker(
                crate::cli::memory_report::MemoryReportTracker::new(path),
            );
        }

        // Get global project path
        let global_project = self.project_path;

        // Execute the appropriate command
        // Default to Mcp if no command is provided or if --stdio is set
        let command = if self.stdio {
            Commands::Mcp {
                stdio: true,
                socket: None,
                idle_timeout_secs: None,
            }
        } else {
            self.command.unwrap_or(Commands::Mcp {
                stdio: false,
                socket: None,
                idle_timeout_secs: None,
            })
        };

        maybe_complete_post_install_actions(&command);

        let result = match command {
            Commands::Index {
                path,
                force,
                progress,
                max_memory,
            } => cmd_index_impl(path, force, progress, max_memory).await,
            Commands::Search { query, top_k } => {
                cmd_search_impl(query, top_k, global_project).await
            }
            Commands::Analyze {
                query,
                token_budget,
            } => cmd_analyze_impl(query, token_budget, global_project).await,
            Commands::Context {
                node_id,
                token_budget,
            } => cmd_context_impl(node_id, token_budget, global_project).await,
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
            Commands::Tools { command } => cmd_tools_impl(command, global_project).await,
            Commands::Serve { host, port } => cmd_serve_impl(host, port).await,
            Commands::Mcp {
                socket,
                idle_timeout_secs: _idle_timeout_secs,
                ..
            } => {
                // T2 (memory-pressure remediation) wires `_idle_timeout_secs`
                // (CLI override of `[mcp] idle_timeout_secs`) into the stdio
                // and socket server loops; parsed here so the flag surface is
                // complete in T1.
                if let Some(ref socket_path) = socket {
                    cmd_mcp_socket_impl(socket_path, global_project).await
                } else {
                    cmd_mcp_stdio_impl(global_project).await
                }
            }
            Commands::Dashboard { port, prod } => cmd_dashboard_impl(port, prod).await,
            Commands::Cleanup {
                max_age_days,
                dry_run,
            } => cmd_cleanup_impl(max_age_days, dry_run).await,
            Commands::Setup {
                neural,
                no_neural,
                cpu,
                gpu,
                check,
                warmup,
            } => cmd_setup_impl(neural, no_neural, cpu, gpu, check, warmup).await,
        };

        // Write the memory report (if tracking was enabled) before returning.
        // Rust does not run Drop for statics, so this must be explicit.
        crate::cli::memory_report::shutdown();

        result
    }
}

/// Initialize logging implementation.
///
/// In non-verbose mode the default level is WARN so that routine `info!()`
/// chatter (storage paths, cache hits, PDG node counts, etc.) stays off the
/// terminal.  Only warnings and errors reach stderr unless `--verbose` is
/// passed, in which case DEBUG-level output is enabled.
///
/// This keeps CLI output clean and token-efficient for LLM consumers while
/// preserving full diagnostics behind the `--verbose` flag.
fn init_logging_impl(verbose: bool) {
    let level = if verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
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
    fs::create_dir_all(leindex_home).context("failed to create LEINDEX_HOME")?;
    cleanup_legacy_user_installations(leindex_home);

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
                .unwrap_or_else(|| current_exe
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf())
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
async fn cmd_index_impl(
    path: PathBuf,
    force: bool,
    _progress: bool,
    max_memory: Option<u64>,
) -> AnyhowResult<()> {
    let canonical_path = path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

    info!("Indexing project at: {}", canonical_path.display());

    // Apply hard RSS limit via rlimit if requested (Linux-only)
    if let Some(mb) = max_memory {
        crate::cli::memory_cap::apply_hard_limit(mb)?;
    }

    // Create a single LeIndex instance and reuse it for both the staleness
    // check and the indexing operation (VAL-QUALITY-015).
    let mut leindex = LeIndex::new(&canonical_path).context("Failed to create LeIndex instance")?;

    // Check if already indexed (unless force)
    if !force && leindex.is_indexed() && !leindex.is_stale_fast() {
        println!("Project already indexed and up-to-date. Use --force to re-index.");
        return Ok(());
    }
    // If indexed but stale, fall through to incremental reindex
    // (VAL-INDEX-005). If not indexed at all, fall through to full index.

    let max_memory_bytes = max_memory.map(|mb| mb * 1024 * 1024);
    let stats = tokio::task::spawn_blocking(move || {
        let result = leindex.index_project_with_memory_cap(force, max_memory_bytes);
        // Force-shutdown any persistent ONNX daemon spawned during indexing.
        // CLI commands are short-lived: the daemon has no reason to persist
        // beyond the process lifetime. Without this, the daemon survives as
        // an orphan, living until its idle timeout (up to 10 minutes).
        // This runs regardless of success or failure to ensure cleanup in
        // all exit paths.
        leindex.shutdown_daemon();
        result
    })
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

    // Force-shutdown any persistent ONNX daemon spawned during search.
    leindex.shutdown_daemon();

    if results.is_empty() {
        println!("No results found for: {}", query);
        return Ok(());
    }

    // Convert results to JSON value for formatter
    let results_json: Vec<Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "rank": r.rank,
                "symbol": r.symbol_name,
                "file_path": r.file_path,
                "node_id": r.node_id,
                "score": r.score.overall,
                "tfidf_score": r.score.tfidf,
                "neural_score": r.score.neural,
                "text_score": r.score.text_match,
                "structural_score": r.score.structural,
                "context": r.context,
                "language": r.language,
            })
        })
        .collect();

    // Use the unified CLI renderer so the search output matches the
    // shape of `leindex tools run leindex.search`.
    println!(
        "{}",
        render_tool_output(
            "leindex.search",
            &serde_json::json!(results_json),
            &serde_json::json!({ "query": &query }),
        )
    );

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

    // Force-shutdown any persistent ONNX daemon spawned during analysis.
    leindex.shutdown_daemon();

    // Print results with nice formatting
    let output = format_analysis_output(&query, &result);
    println!("{}", output);

    Ok(())
}

fn format_analysis_output(query: &str, result: &crate::cli::leindex::AnalysisResult) -> String {
    use crate::cli::mcp::output::{BOLD, DIM, LIGHT_CYAN, RESET};

    let mut out = String::new();
    out.push_str(&format!(
        "{}┌─ Analysis: {} ─┐{}\n",
        LIGHT_CYAN, query, RESET
    ));
    out.push_str(&format!(
        "  {}Found:{} {} entry point(s)\n",
        BOLD,
        RESET,
        result.results.len()
    ));
    out.push_str(&format!(
        "  {}Tokens:{} {}\n",
        BOLD, RESET, result.tokens_used
    ));
    out.push_str(&format!(
        "  {}Time:{} {}ms\n",
        BOLD, RESET, result.processing_time_ms
    ));

    if let Some(context) = &result.context {
        out.push('\n');
        out.push_str(&format!("  {}{}{}\n", BOLD, "Context:", RESET));
        let context_str: &str = context.as_str();
        let truncated = crate::cli::mcp::output::truncate_chars(context_str, 300);
        out.push_str(&format!("  {}{}{}", DIM, truncated, RESET));
    }

    out
}

/// Context command implementation
async fn cmd_context_impl(
    node_id: String,
    token_budget: usize,
    project: Option<PathBuf>,
) -> AnyhowResult<()> {
    let args = merge_tool_args(
        serde_json::json!({
            "node_id": node_id,
            "token_budget": token_budget
        }),
        &[],
        project.as_ref(),
    )?;

    let value = execute_tool_handler("leindex_context", args, project).await?;

    println!(
        "{}",
        render_tool_output(
            "leindex.context",
            &value,
            &serde_json::json!({ "node_id": &node_id })
        )
    );

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

    // Diagnostics must remain a snapshot operation. Do not hydrate the PDG or
    // rebuild search state here; persisted stats/health plus one live Git
    // status provide the useful facts without the multi-gigabyte resident load
    // that previously made `leindex diagnostics` take tens of seconds.
    if let Err(e) = leindex.load_stats_from_storage() {
        warn!("Failed to load persisted stats: {}", e);
    }

    // Get diagnostics
    let diag = leindex
        .get_diagnostics()
        .context("Failed to get diagnostics")?;

    let health = crate::cli::index_freshness::load_health(leindex.storage_path());
    let indexed_ct = health
        .as_ref()
        .map(|snapshot| snapshot.indexed_file_count)
        .unwrap_or(diag.stats.files_parsed);
    let (changed, deleted) = crate::cli::git::status(leindex.project_path())
        .ok()
        .map(|status| {
            let changed = status
                .modified
                .into_iter()
                .chain(status.staged)
                .chain(status.untracked)
                .map(|path| leindex.project_path().join(path))
                .collect::<Vec<_>>();
            (changed, status.deleted)
        })
        .unwrap_or_else(|| (Vec::new(), Vec::new()));
    let health_stale = health.as_ref().is_some_and(|snapshot| {
        matches!(
            snapshot.status,
            crate::cli::leindex::ComponentStatus::Stale
                | crate::cli::leindex::ComponentStatus::Partial
                | crate::cli::leindex::ComponentStatus::Failed
        )
    });
    // Tree-OID drift: a clean worktree is NOT proof of freshness — after
    // `git checkout`/`pull` of a different revision, `git status` reports no
    // modified/deleted paths while the persisted index was built from the
    // previous tree. Compare the indexed tree OID against the current one. If
    // we have a saved tree OID but git itself fails (lock contention, corrupt
    // repo, permissions), treat it as drift (conservative — report stale
    // rather than silently miss it, matching is_stale_fast). Only Ok(None)
    // (non-git) and a missing saved OID fall back to the dirtiness check.
    let tree_drift = match health.as_ref().and_then(|h| h.tree_oid.as_deref()) {
        Some(indexed) => match crate::cli::git::tree_oid(leindex.project_path()) {
            Ok(Some(current)) => indexed != current,
            Ok(None) => false,
            Err(_) => true,
        },
        None => false,
    };
    let stale = health_stale || !changed.is_empty() || !deleted.is_empty() || tree_drift;

    // Estimate last_indexed_secs_ago from storage_path mtime
    let storage_path = leindex.storage_path();
    let last_indexed_secs_ago = std::fs::metadata(storage_path.join("leindex.db"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs());

    // Populate issues matching DiagnosticsHandler::execute shape
    let mut issues: Vec<serde_json::Value> = Vec::new();
    if diag.stats.failed_parses > 0 {
        issues.push(serde_json::json!({
            "severity": "warning",
            "message": format!("{} files failed to parse", diag.stats.failed_parses),
        }));
    }
    if stale {
        issues.push(serde_json::json!({
            "severity": "warning",
            "message": "Index may be stale. Call LeIndex [Index] with force_reindex=true for fresh results.",
        }));
    }

    // Determine embedding model from diagnostics or compile-time features.
    // VAL-ONNX-006: Diagnostics must report ONNX/hybrid embedding status.
    let embedding_model = if diag.embedding_model != "unknown" {
        diag.embedding_model.clone()
    } else {
        // Lightweight path: embedder not loaded, determine from compile-time features
        #[cfg(feature = "onnx")]
        {
            "onnx_hybrid".to_string()
        }
        #[cfg(all(not(feature = "onnx"), not(feature = "remote-embeddings")))]
        {
            "tfidf_only".to_string()
        }
        #[cfg(all(not(feature = "onnx"), feature = "remote-embeddings"))]
        {
            "remote_hybrid".to_string()
        }
    };

    // VAL-CROSS-015 / VAL-ORT-022: Report the resolved ORT dylib path, the
    // detected ORT version, and the configured execution provider so support
    // engineers can debug any install surface identically. The diagnostic
    // command must NOT load ORT itself (the leindex-embed worker does), so we
    // walk the chain via `discover_path_only()` and fall back to the config
    // file when no candidate exists on disk.
    let (ort_path, ort_version, execution_provider) = collect_ort_diagnostics();

    // Convert to JSON for formatter
    let diag_json = serde_json::json!({
        "project_path": diag.project_path,
        "indexed_files": indexed_ct,
        "index_size_mb": diag.memory_usage_bytes as f64 / 1024.0 / 1024.0,
        "symbol_count": diag.stats.indexed_nodes,
        "stale": stale,
        "freshness": health,
        "last_indexed_secs_ago": last_indexed_secs_ago,
        "embedding_model": embedding_model,
        "ort_path": ort_path,
        "ort_version": ort_version,
        "execution_provider": execution_provider,
        "issues": issues,
    });

    // Use the unified CLI renderer so `leindex diagnostics` and
    // `leindex tools run leindex.diagnostics` produce identical output.
    println!(
        "{}",
        render_tool_output("leindex.diagnostics", &diag_json, &serde_json::json!({}))
    );

    Ok(())
}

/// Collect ORT-related diagnostics for the `leindex diagnostics` command.
///
/// VAL-CROSS-015 / VAL-ORT-022: surfaces the same ORT info shape on every
/// install surface (cargo, npm, PyPI, GitHub Release bundle) so support
/// engineers can debug identically. Returns a `(ort_path, ort_version,
/// execution_provider)` triple where:
///
///   * `ort_path` is the resolved ORT dylib path the discovery chain would
///     load right now, falling back to the path recorded in the user's config
///     if no candidate exists on disk. `None` when neither resolves.
///   * `ort_version` is the live-detected onnxruntime version (queried via
///     pip/python), falling back to the version recorded during setup.
///     `None` when neither is available.
///   * `execution_provider` is the configured provider string ("cpu",
///     "cuda", "migraphx", or "auto"). Defaults to "auto" when no config
///     exists, matching the setup command's default.
///
/// This function does NOT call `ort::init_from()` and therefore does NOT
/// load ORT into the main daemon process. That keeps the diagnostics command
/// cheap and side-effect-free; the leindex-embed worker performs its own
/// discovery at spawn time.
pub(crate) fn collect_ort_diagnostics() -> (Option<String>, Option<String>, String) {
    use crate::cli::leindex::setup;

    // ort_path: prefer the live discovery chain, fall back to configured path.
    #[cfg(feature = "onnx")]
    let live_path = crate::embed::ort_discovery::discover_path_only()
        .map(|outcome| outcome.path.display().to_string());
    #[cfg(not(feature = "onnx"))]
    let live_path: Option<String> = None;

    let config_path = crate::config::LeIndexConfig::load()
        .ok()
        .and_then(|c| c.neural.ort_dylib_path);

    let ort_path = live_path.or(config_path);

    // ort_version: prefer the live-detected version, fall back to the recorded one.
    let live_version = setup::get_ort_version();
    let recorded_version = crate::config::LeIndexConfig::load()
        .ok()
        .and_then(|c| c.neural.ort_version);
    let ort_version = live_version.or(recorded_version);

    // execution_provider: from config, default to "auto" when unset.
    let execution_provider = crate::config::LeIndexConfig::load()
        .ok()
        .map(|c| c.neural.execution_provider)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".to_string());

    (ort_path, ort_version, execution_provider)
}

/// Serve command implementation - Start MCP server
async fn cmd_serve_impl(host: String, port: u16) -> AnyhowResult<()> {
    // Check for environment variable override (for customization via LEINDEX_PORT).
    // `LEINDEX_PORT` always wins — useful for power users and CI overrides.
    let port = if let Ok(env_port) = std::env::var("LEINDEX_PORT") {
        env_port.parse::<u16>().unwrap_or(port)
    } else {
        port
    };

    // Parse the address
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .context("Invalid address or port")?;

    info!("Starting MCP server on {}", addr);

    // Create the MCP server and bind the listener BEFORE printing
    // the startup banner. The previous flow printed
    // `Server starting on http://{addr}` and then bound inside
    // `McpServer::run`; if the preferred port was occupied the
    // bind fell back to a different port, the process kept
    // running, but the advertised URL still pointed at the
    // occupied port. External clients / service managers that
    // parsed the printed URL would then connect to the wrong
    // process (or to a process that no longer owns the port).
    // Doing the bind here lets us print the actual bound address.
    let server = McpServer::with_address(addr).context("Failed to create MCP server")?;
    let listener = crate::cli::mcp::server::bind_with_fallback(addr)
        .await
        .context("Bind failed")?;
    let bound_addr = listener
        .local_addr()
        .context("Failed to read bound address")?;
    if bound_addr.port() != addr.port() {
        eprintln!(
            "\nWARNING: preferred port {} was unavailable; bound to fallback {}\n",
            addr.port(),
            bound_addr.port()
        );
    }

    println!("\nLeIndex MCP Server\n");
    println!("Server starting on http://{}\n", bound_addr);
    println!("Available endpoints:");
    println!("  POST /mcp             - JSON-RPC 2.0 endpoint");
    println!("  GET  /mcp/tools/list  - List available tools");
    println!("  GET  /health          - Health check");
    println!("\nConfiguration:");
    println!(
        "  Port: {} (override with LEINDEX_PORT env var; auto-falls back to next consecutive ports if taken)",
        bound_addr.port()
    );
    println!("\nPress Ctrl+C to stop the server\n");

    server.serve(listener).await.context("Server error")?;

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

/// Setup command implementation — interactive and non-interactive setup wizard.
///
/// VAL-SETUP-001: `leindex setup` is listed in --help
/// VAL-SETUP-009-015: Non-interactive flag handling and conflict detection
/// VAL-SETUP-014: --check mode reads config and reports status
async fn cmd_setup_impl(
    neural: bool,
    no_neural: bool,
    cpu: bool,
    gpu: Option<String>,
    check: bool,
    warmup: bool,
) -> AnyhowResult<()> {
    use crate::cli::leindex::setup;

    // VAL-SETUP-014: --check mode is read-only
    if check {
        check_neutral_conflicts(neural, no_neural, cpu, gpu.as_deref())?;
        let result = setup::run_check().map_err(|e| anyhow::anyhow!("{}", e))?;
        // Exit code reflects completeness
        if !result.fully_configured {
            anyhow::bail!("setup check incomplete");
        }
        return Ok(());
    }

    // Parse GPU vendor if provided
    let gpu_vendor = if let Some(gpu_str) = &gpu {
        Some(setup::parse_gpu_vendor(gpu_str).map_err(|e| anyhow::anyhow!("{}", e))?)
    } else {
        None
    };

    // Determine the mode: interactive or non-interactive.
    // VAL-SETUP-015: conflicts are validated by clap (conflicts_with) and also
    // redundantly checked here for the --neural + --no-neural case.
    let has_flags = neural || no_neural || cpu || gpu.is_some();
    if has_flags {
        check_neutral_conflicts(neural, no_neural, cpu, gpu.as_deref())?;
    }

    // Resolve choices from flags, interactive prompts, or emit guidance.
    let choices = resolve_setup_choices(neural, no_neural, cpu, gpu_vendor, has_flags)?;

    // Execute the setup with the resolved choices
    let result = setup::execute_setup(&choices).map_err(|e| anyhow::anyhow!("{}", e))?;

    // Print the final summary
    // VAL-SETUP-034: surfaces neural on/off, provider, ORT, model, config
    setup::print_summary(&result);

    // Smoke test (fatal on real failure) + MIGraphX warmup (non-fatal).
    #[cfg(feature = "onnx")]
    let do_warmup = warmup || should_auto_warmup(&choices, &result);
    #[cfg(not(feature = "onnx"))]
    let do_warmup = warmup;
    handle_smoke_and_warmup(&result, do_warmup)
}

/// Resolve setup choices: from explicit flags, interactive prompts, or by
/// printing guidance and bailing when neither applies.
fn resolve_setup_choices(
    neural: bool,
    no_neural: bool,
    cpu: bool,
    gpu_vendor: Option<crate::cli::leindex::setup::GpuVendor>,
    has_flags: bool,
) -> AnyhowResult<crate::cli::leindex::setup::SetupChoices> {
    use crate::cli::leindex::setup;
    if has_flags {
        // Non-interactive mode: resolve from flags
        setup::resolve_from_flags(neural, no_neural, cpu, gpu_vendor)
            .map_err(|e| anyhow::anyhow!("{}", e))
    } else if setup::is_interactive() {
        // Interactive mode: show prompts
        // VAL-SETUP-002: prompts neural? -> CPU/GPU -> AMD/NVIDIA
        setup::run_interactive_flow().map_err(|e| anyhow::anyhow!("{}", e))
    } else {
        // No flags and not interactive: show guidance and exit with error
        eprintln!("No setup options specified and stdin is not interactive (not a TTY).");
        eprintln!("For non-interactive setup, use flags:");
        eprintln!("  leindex setup --neural --cpu       # CPU neural search");
        eprintln!("  leindex setup --neural --gpu amd   # AMD GPU (MIGraphX)");
        eprintln!("  leindex setup --neural --gpu nvidia # NVIDIA GPU (CUDA)");
        eprintln!("  leindex setup --no-neural          # TF-IDF only");
        eprintln!("  leindex setup --check              # Show current status");
        anyhow::bail!("No setup options specified in non-interactive mode")
    }
}

/// Run the post-setup smoke test (fatal on a real failure) and the MIGraphX
/// warmup pre-compilation (non-fatal). A *skipped* smoke test (compiled without
/// `onnx`) does not count as failure; the binary remains usable for TF-IDF.
fn handle_smoke_and_warmup(
    result: &crate::cli::leindex::setup::SetupResult,
    do_warmup: bool,
) -> AnyhowResult<()> {
    // VAL-SETUP-026: a failed smoke test means the install produced no working
    // neural configuration — exit non-zero so CI/scripts detect the failure.
    if let Some(smoke) = &result.smoke_test {
        if !smoke.passed && !smoke.skipped {
            anyhow::bail!("setup smoke test failed");
        }
    }

    // VAL-DAEMON-007: MIGraphX warmup pre-compilation.
    //
    // When --warmup is explicitly requested (or auto-triggered for a cold
    // MIGraphX cache), spawn the embed worker, send a dummy embed request to
    // trigger MIGraphX compilation, and shut down gracefully. This populates
    // the cache so subsequent index runs skip the multi-minute compile step.
    #[cfg(feature = "onnx")]
    if do_warmup {
        if let Err(e) = crate::cli::leindex::setup::run_warmup() {
            eprintln!("  -> MIGraphX warmup warning: {}", e);
            // Warmup failure is non-fatal; indexing will still work.
        }
    }
    #[cfg(not(feature = "onnx"))]
    if do_warmup {
        eprintln!("  -> MIGraphX warmup skipped: ONNX feature not enabled");
    }

    Ok(())
}

/// Determine whether auto-warmup should run on a cold MIGraphX cache.
///
/// Auto-warmup triggers when the smoke test reported MIGraphX as the **active**
/// provider (the actual runtime selection, not the requested enum — Auto may
/// resolve to MIGraphX on an AMD host) and the MIGraphX cache is cold.
#[cfg(feature = "onnx")]
fn should_auto_warmup(
    _choices: &crate::cli::leindex::setup::SetupChoices,
    result: &crate::cli::leindex::setup::SetupResult,
) -> bool {
    // Warm only active MIGraphX: the smoke test is the authority on which EP
    // the worker actually used. An explicit `--gpu amd` that fell back to CPU
    // must not trigger warmup; an Auto selection that resolved to MIGraphX
    // must trigger it.
    let active_is_migraphx = result
        .smoke_test
        .as_ref()
        .and_then(|s| s.execution_provider.as_deref())
        == Some("migraphx");
    if !active_is_migraphx {
        return false;
    }

    // Only auto-warm if setup succeeded (ORT + model present).
    if !result.ort_installed || !result.model_present {
        return false;
    }

    // Auto-warm only when the MIGraphX cache is cold (does not exist yet).
    let cache_path = crate::search::onnx::client::migraphx_cache_path("qwen3-embed-0.6b-dynamic");
    !cache_path.exists()
}

/// Validate flag conflicts that clap cannot always catch via `conflicts_with`.
///
/// clap's `conflicts_with` on Option/String fields works well, but boolean flag
/// pairs like --neural + --no-neural need an explicit check because clap only
/// errors if both are explicitly set (which is the desired behavior). This is a
/// redundancy safety net.
fn check_neutral_conflicts(
    neural: bool,
    no_neural: bool,
    cpu: bool,
    gpu: Option<&str>,
) -> AnyhowResult<()> {
    if neural && no_neural {
        anyhow::bail!("Cannot use --neural and --no-neural together. Choose one.");
    }
    if cpu && gpu.is_some() {
        anyhow::bail!("Cannot use --cpu and --gpu together. Choose one execution provider.");
    }
    Ok(())
}

/// Cleanup command implementation — remove stale LeIndex temp artifacts.
async fn cmd_cleanup_impl(max_age_days: u64, dry_run: bool) -> AnyhowResult<()> {
    use crate::cli::cleanup::run_gc;
    use std::time::Duration;

    let max_age = Duration::from_secs(max_age_days * 24 * 3600);

    if dry_run {
        // In dry-run mode we scan but do not remove
        println!("LeIndex Cleanup (dry run)\n");
        println!(
            "Scanning for artifacts older than {} day(s)...\n",
            max_age_days
        );

        let report = run_gc_dry_run(max_age);
        println!("{}", report);
    } else {
        println!("LeIndex Cleanup\n");
        println!("Removing artifacts older than {} day(s)...\n", max_age_days);

        let report = run_gc(max_age);
        println!("{}", report);
    }

    Ok(())
}

/// Dry-run GC: scan and report without removing anything.
fn run_gc_dry_run(max_age: std::time::Duration) -> crate::cli::cleanup::GcReport {
    use crate::cli::cleanup::artifact_scan_roots;
    use std::time::SystemTime;
    use tracing::debug;

    let mut report = crate::cli::cleanup::GcReport::default();
    let cutoff = SystemTime::now() - max_age;

    for root in artifact_scan_roots() {
        if !root.exists() {
            continue;
        }

        if root
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("lephase-"))
            .unwrap_or(false)
        {
            count_artifact(&root, &cutoff, &mut report);
            continue;
        }

        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(err) => {
                debug!("Cannot read {}: {}", root.display(), err);
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.file_name().map(|n| n == ".leindex").unwrap_or(false) {
                continue;
            }
            count_artifact(&path, &cutoff, &mut report);
        }
    }

    report
}

fn count_artifact(
    dir: &std::path::Path,
    cutoff: &std::time::SystemTime,
    report: &mut crate::cli::cleanup::GcReport,
) {
    use crate::cli::cleanup::{
        artifact_age, dir_size, is_leindex_artifact, is_leindex_artifact_by_pattern,
    };
    use tracing::debug;

    if !is_leindex_artifact(dir) && !is_leindex_artifact_by_pattern(dir) {
        return;
    }

    report.scanned += 1;

    let age = artifact_age(dir);
    if age >= *cutoff {
        debug!("Artifact {} is not stale yet", dir.display());
        return;
    }

    let size = dir_size(dir);
    debug!(
        "Would remove stale artifact: {} ({:.2} MB)",
        dir.display(),
        size as f64 / 1024.0 / 1024.0
    );
    report.removed += 1;
    report.bytes_freed += size;
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

    #[test]
    fn test_tools_help_command_parsing() {
        let cli = Cli::try_parse_from(["leindex", "tools", "help", "project_map"]).unwrap();
        match cli.command {
            Some(Commands::Tools {
                command: ToolCommands::Help { name },
            }) => assert_eq!(name, "project_map"),
            _ => panic!("Expected tools help command"),
        }
    }

    #[test]
    fn test_tools_run_command_parsing() {
        let cli = Cli::try_parse_from([
            "leindex",
            "tools",
            "run",
            "project_map",
            "--args",
            "{\"depth\":1}",
            "--set",
            "include_symbols=true",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Tools {
                command:
                    ToolCommands::Run {
                        name,
                        args_json,
                        set,
                    },
            }) => {
                assert_eq!(name, "project_map");
                assert_eq!(args_json, "{\"depth\":1}");
                assert_eq!(set, vec!["include_symbols=true"]);
            }
            _ => panic!("Expected tools run command"),
        }
    }

    #[test]
    fn test_find_tool_handler_accepts_short_and_full_names() {
        assert!(find_tool_handler("LeIndex [Project Map]").is_some());
        assert!(find_tool_handler("project_map").is_some());
        assert!(find_tool_handler("project-map").is_some());
    }

    #[test]
    fn test_cleanup_command_parsing() {
        let cli = Cli::try_parse_from(["leindex", "cleanup"]).unwrap();
        match cli.command {
            Some(Commands::Cleanup {
                max_age_days,
                dry_run,
            }) => {
                assert_eq!(max_age_days, 7);
                assert!(!dry_run);
            }
            _ => panic!("Expected Cleanup command"),
        }
    }

    #[test]
    fn test_cleanup_command_with_flags() {
        let cli = Cli::try_parse_from(["leindex", "cleanup", "--max-age-days", "14", "--dry-run"])
            .unwrap();
        match cli.command {
            Some(Commands::Cleanup {
                max_age_days,
                dry_run,
            }) => {
                assert_eq!(max_age_days, 14);
                assert!(dry_run);
            }
            _ => panic!("Expected Cleanup command"),
        }
    }

    #[test]
    fn test_memory_report_flag_parsing() {
        // VAL-MEASURE-020: --memory-report is an opt-in CLI surface
        let cli = Cli::try_parse_from([
            "leindex",
            "--memory-report",
            "/tmp/mem-report.json",
            "index",
            "/tmp/project",
        ])
        .unwrap();
        assert_eq!(
            cli.memory_report,
            Some(PathBuf::from("/tmp/mem-report.json"))
        );
        assert!(matches!(cli.command, Some(Commands::Index { .. })));
    }

    #[test]
    fn test_memory_report_flag_absent_by_default() {
        let cli = Cli::try_parse_from(["leindex", "index", "/tmp/project"]).unwrap();
        assert!(cli.memory_report.is_none());
    }

    #[test]
    fn test_setup_command_parsing() {
        // VAL-SETUP-001: setup command is registered
        let cli = Cli::try_parse_from(["leindex", "setup", "--neural", "--cpu"]).unwrap();
        match cli.command {
            Some(Commands::Setup {
                neural,
                no_neural,
                cpu,
                gpu,
                check,
                warmup: _,
            }) => {
                assert!(neural);
                assert!(!no_neural);
                assert!(cpu);
                assert!(gpu.is_none());
                assert!(!check);
            }
            _ => panic!("Expected Setup command"),
        }
    }

    #[test]
    fn test_setup_command_gpu_amd() {
        let cli = Cli::try_parse_from(["leindex", "setup", "--neural", "--gpu", "amd"]).unwrap();
        match cli.command {
            Some(Commands::Setup {
                neural, cpu, gpu, ..
            }) => {
                assert!(neural);
                assert!(!cpu);
                assert_eq!(gpu.as_deref(), Some("amd"));
            }
            _ => panic!("Expected Setup command"),
        }
    }

    #[test]
    fn test_setup_command_gpu_nvidia() {
        let cli = Cli::try_parse_from(["leindex", "setup", "--neural", "--gpu", "nvidia"]).unwrap();
        match cli.command {
            Some(Commands::Setup {
                neural, cpu, gpu, ..
            }) => {
                assert!(neural);
                assert!(!cpu);
                assert_eq!(gpu.as_deref(), Some("nvidia"));
            }
            _ => panic!("Expected Setup command"),
        }
    }

    #[test]
    fn test_setup_command_no_neural() {
        // VAL-SETUP-013: --no-neural flag
        let cli = Cli::try_parse_from(["leindex", "setup", "--no-neural"]).unwrap();
        match cli.command {
            Some(Commands::Setup {
                neural,
                no_neural,
                cpu,
                gpu,
                check,
                warmup: _,
            }) => {
                assert!(!neural);
                assert!(no_neural);
                assert!(!cpu);
                assert!(gpu.is_none());
                assert!(!check);
            }
            _ => panic!("Expected Setup command"),
        }
    }

    #[test]
    fn test_setup_command_check() {
        // VAL-SETUP-014: --check flag
        let cli = Cli::try_parse_from(["leindex", "setup", "--check"]).unwrap();
        match cli.command {
            Some(Commands::Setup { check, .. }) => assert!(check),
            _ => panic!("Expected Setup command"),
        }
    }

    #[test]
    fn test_setup_command_neural_gpu_conflict_rejected() {
        // VAL-SETUP-015: conflicting flags produce error
        let result = Cli::try_parse_from(["leindex", "setup", "--neural", "--cpu", "--gpu", "amd"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_setup_command_neural_no_neural_conflict_rejected() {
        // VAL-SETUP-015: --neural + --no-neural is a conflict
        let result = Cli::try_parse_from(["leindex", "setup", "--neural", "--no-neural"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_setup_help_is_valid() {
        // VAL-SETUP-001: leindex setup --help exits 0
        let result = Cli::try_parse_from(["leindex", "setup", "--help"]);
        assert!(result.is_err()); // clap exits with error for --help
        let err = result.unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::DisplayHelp));
    }
}
