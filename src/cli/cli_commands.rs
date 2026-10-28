//! Declarative clap command surface for the LeIndex CLI.
//!
//! Moved out of `cli.rs` by pure code motion (Large File Detection gate).
//! Nothing here executes: it is the argument grammar only, re-exported from
//! `cli` so `crate::cli::cli::Commands` and the nested action enums keep
//! resolving.

use clap::Subcommand;
use std::path::PathBuf;

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

        /// Rerank backend for this run (overrides [search] rerank_backend).
        #[arg(
            long = "rerank",
            value_name = "BACKEND",
            value_parser = ["onnx", "system-one", "off"]
        )]
        rerank: Option<String>,
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

        /// Sweep stale daemon sidecars in ~/.leindex/run/ (dead-pid or
        /// too-old worker/MCP lock, pid, sock, status, start files)
        #[arg(long = "stale-daemons")]
        stale_daemons: bool,

        /// Clean the project's `.leindex/` generation store (Task 7):
        /// removes orphaned CAS blobs (refcount 0), stale generations
        /// (not current/previous/leased), abandoned staging, and runs
        /// embed-cache compaction. NEVER touches leased/current/rollback
        /// generations (VAL-ROLLOUT-010).
        #[arg(long = "store")]
        store: bool,
    },

    /// Report generation-store retention state (WS4 Task 9)
    ///
    /// Prints the read-only retention report for the project's generation
    /// store: generation count, CAS bytes, job bytes, dedup ratio, and GC
    /// candidates. Nothing is deleted. Use `--project` (or run inside the
    /// project directory) to select the project.
    #[command(visible_alias = "leindex_retention")]
    Retention {
        /// Print the retention report (generation count, CAS bytes, job
        /// bytes, dedup ratio, GC candidates). Read-only; deletes nothing.
        #[arg(long = "report")]
        report: bool,

        /// Run the retention sweep: prune generations outside the retained
        /// window (current + its `--max-generations - 1` predecessors),
        /// GC orphaned CAS blobs (CAS stores), and byte-cap completed jobs.
        /// Safe for both CAS and legacy full-copy stores; the current
        /// generation is never removed.
        #[arg(long = "gc")]
        gc: bool,

        /// Number of generations to retain when running `--gc`
        /// (default 3: current + two rollback points).
        #[arg(long = "max-generations", default_value_t = 3)]
        max_generations: usize,

        /// With `--gc`: report what would be removed without deleting.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },

    /// One-time legacy → CAS generation-store migration (WS4 Task 10)
    ///
    /// Converts a legacy full-copy `.leindex/` store to the content-addressed
    /// generation layout: the current + previous generations become manifests
    /// referencing deduplicated CAS blobs, `CURRENT` is swapped atomically
    /// last, stale generations and completed jobs are removed, and the jobs
    /// directory is byte-bounded. Idempotent and crash-safe; a second run is a
    /// no-op. This is a destructive sweep — back up `.leindex/` first.
    #[command(visible_alias = "leindex_storage_migrate")]
    Storage {
        /// Run the migration now.
        #[arg(long = "migrate")]
        migrate: bool,

        /// Print the migration state (legacy layout detected? already
        /// migrated?) without changing anything.
        #[arg(long = "status")]
        status: bool,

        /// Job-byte cap for the sweep (default: 128 MiB).
        #[arg(long = "job-bytes-max", value_name = "BYTES")]
        job_bytes_max: Option<u64>,

        /// Total `.leindex/` footprint goal in MiB (default: 200).
        #[arg(long = "footprint-mib", value_name = "MIB")]
        footprint_mib: Option<u64>,
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

        /// Provision the embeddinggemma-2 remote-embeddings preset: download
        /// the pinned GGUF into the model dir and wire [services.gemma] +
        /// [indexing.embeddings] in leindex.toml. Implies the neural path
        /// (like --cpu/--gpu) — resolved in cmd_setup_impl, not clap
        /// `requires`, which would *demand* --neural instead.
        #[arg(long = "remote-gemma")]
        remote_gemma: bool,

        /// Use the alternate-screen fullscreen setup editor. Without this flag,
        /// setup renders inline and leaves the terminal scrollback intact.
        #[arg(long = "fs")]
        fs: bool,

        /// Model route, managed runtime, and external endpoint options.
        ///
        /// Boxed because this variant is by far the largest in the clap enum:
        /// `SetupOptions` is a flattened bag of every setup flag, so the variant
        /// crosses clippy's 200-byte `large_enum_variant` threshold and would
        /// make EVERY `Commands` value pay for the setup surface even when the
        /// user runs `leindex search`. `Box` keeps one indirection on the setup
        /// path only. Note this must be built as `Box::new(..)`/`Box::default()`
        /// at the call sites — `#[command(flatten)]` itself is unaffected.
        #[command(flatten)]
        options: Box<crate::cli::leindex::setup::SetupOptions>,
    },

    /// Manage the user-scoped leindexd daemon.
    ///
    /// Use `status` to inspect endpoint liveness, `start` to launch it,
    /// `stop` to request graceful shutdown, or `restart` to do both. The
    /// daemon stores its endpoint sidecar under `$LEINDEX_HOME/run`.
    #[cfg(unix)]
    Daemon {
        /// Lifecycle action (status when omitted): status, start, stop, restart
        #[command(subcommand)]
        action: Option<DaemonAction>,
    },

    /// Manage managed model-server services ([services.<name>] sections)
    ///
    /// List, inspect, start, stop and restart the llama-server-compatible
    /// processes declared in leindex.toml. Health probes the service's HTTP
    /// /health endpoint (503-aware); `status` shows pid (from our pidfile,
    /// verified), endpoint, model and auto-start policy.
    Services {
        /// Action to perform (list when omitted)
        #[command(subcommand)]
        action: Option<ServiceAction>,
    },

    /// Inspect and edit leindex.toml (show/get/set/unset/validate/path)
    Config {
        /// Config action to perform
        #[command(subcommand)]
        action: crate::cli::leindex::setup_config::ConfigAction,
    },

    /// Terminal UI: browse the project map, search, and index state
    ///
    /// Reads through the same `LeIndex` engine entry points as the CLI
    /// subcommands, so nothing here is a second source of truth. Requires the
    /// `tui` feature.
    #[cfg(feature = "tui")]
    Tui {
        /// Project directory to inspect (defaults to the current directory)
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
}

/// Actions for `leindex daemon`.
#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Show whether leindexd is running, its verified PID, and socket endpoint.
    /// Returns `stopped` when no endpoint sidecar exists and `stale` when the
    /// sidecar does not match a live daemon or the current protocol.
    Status,
    /// Start leindexd in the background using the user-scoped run directory.
    Start,
    /// Ask the running leindexd process to shut down, then remove its sidecars.
    Stop,
    /// Stop leindexd if needed and start a fresh background instance.
    Restart,
}

/// Actions for `leindex services`.
#[derive(Subcommand, Debug)]
pub enum ServiceAction {
    /// Show every configured service and its state (default)
    List,
    /// Show one service's state in detail
    Status {
        /// Service name ([services.<name>] key)
        name: String,
    },
    /// Start a service (waits for the port to answer)
    Start {
        /// Service name ([services.<name>] key)
        name: String,
    },
    /// Stop a service (SIGTERM its process group; refuses foreign pids)
    Stop {
        /// Service name ([services.<name>] key)
        name: String,
    },
    /// Stop then start a service
    Restart {
        /// Service name ([services.<name>] key)
        name: String,
    },
}

/// Subcommands for inspecting and executing MCP tools from the CLI.
#[derive(Subcommand, Debug)]
pub enum ToolCommands {
    /// List the MCP tools (the five routers and their branches)
    List {
        /// Include descriptions and a one-line summary per branch
        #[arg(long)]
        verbose: bool,
    },

    /// Show comprehensive help for a tool
    #[command(alias = "help")]
    Inspect {
        /// Tool name (for example: leindex_explore, explore, or leindex-explore)
        name: String,
    },

    /// Print the JSON argument schema for a tool (routers print their oneOf form)
    Schema {
        /// Tool name (for example: leindex_explore, explore, or leindex-explore)
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
