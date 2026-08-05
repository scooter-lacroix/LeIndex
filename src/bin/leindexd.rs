//! `leindexd` — user-scoped LeIndex daemon binary (spec §4.2).
//!
//! Wraps the existing [`McpServer::run_socket`] accept loop. The daemon is a
//! thin entry point: it parses `--socket` and `--idle-timeout-secs`, acquires
//! the hard single-winner startup lock (refusing to run if a live daemon
//! already exists), builds an [`McpServer`], and calls `run_socket`.
//!
//! **No eager project or model load** (spec §4.2): the daemon does not open
//! any project database or load any model at startup. Project engines are
//! loaded lazily on the first tool call that references them. RSS stays flat
//! from socket bind until the first tool call.
//!
//! **Idle exit** (spec §4.2): after `--idle-timeout-secs` seconds with no
//! client connections or active tool calls, the daemon exits cleanly (code 0)
//! via the existing [`ProcessIdleClock`] mechanism.
//!
//! # Usage
//!
//! ```text
//! leindexd --socket /path/to/d.sock --idle-timeout-secs 300
//! ```
//!
//! The daemon writes a `daemon.endpoint` sidecar to the run directory after
//! binding the socket so clients discover it only once the socket is live.

#[cfg(unix)]
mod imp {
    use std::path::PathBuf;

    use leindex::cli::daemon::endpoint::{
        DaemonEndpoint, ENDPOINT_SIDECAR, StartupOutcome, resolve_endpoint, write_endpoint_sidecar,
    };
    use leindex::cli::daemon::handshake::DAEMON_PROTOCOL_VERSION;
    use leindex::cli::mcp::server::{McpServer, McpServerConfig, ProcessIdleClock};
    use tracing::{error, info};

    /// Environment variable for overriding the Tokio worker thread count.
    ///
    /// Spec §8.1: "Daemon Tokio workers: start at 2; benchmark 2-4."
    /// Mirrors the same constant in `src/bin/leindex.rs` so the daemon
    /// inherits the same containment default.
    const TOKIO_WORKERS_ENV: &str = "LEINDEX_TOKIO_WORKERS";

    /// Default Tokio worker thread count (spec §8.1 containment).
    const DEFAULT_TOKIO_WORKERS: usize = 2;

    /// Default idle timeout in seconds (5 minutes). Overridable via
    /// `--idle-timeout-secs`.
    const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;

    fn configured_worker_count() -> usize {
        std::env::var(TOKIO_WORKERS_ENV)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_TOKIO_WORKERS)
    }

    fn build_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(configured_worker_count())
            .enable_all()
            .build()
            .expect("failed to build Tokio runtime")
    }

    /// Parse a `--key value` pair from the argument list. Returns the value
    /// and the number of args consumed (1 if `--key=value`, 2 if `--key value`).
    fn parse_arg_value(args: &[String], prefix: &str) -> Option<(String, usize)> {
        for (i, arg) in args.iter().enumerate() {
            // --key=value
            if let Some(val) = arg.strip_prefix(&format!("{prefix}=")) {
                return Some((val.to_string(), 1));
            }
            // --key value
            if arg == prefix {
                if let Some(val) = args.get(i + 1) {
                    return Some((val.clone(), 2));
                }
            }
        }
        None
    }

    fn init_logging() {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(std::io::stderr)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    /// Entry point for the leindexd daemon binary.
    pub fn main() -> anyhow::Result<()> {
        init_logging();

        let args: Vec<String> = std::env::args().skip(1).collect();

        // Parse --socket <path> (required)
        let socket_path: PathBuf = parse_arg_value(&args, "--socket")
            .map(|(v, _)| PathBuf::from(v))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing required --socket <path> argument\n\
                 Usage: leindexd --socket <path> [--idle-timeout-secs <n>]"
                )
            })?;

        // Parse --idle-timeout-secs <n> (optional, default 300 = 5 min)
        let idle_timeout_secs: u64 = parse_arg_value(&args, "--idle-timeout-secs")
            .map(|(v, _)| v.parse::<u64>().unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS))
            .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);

        // Resolve the run directory: ~/.leindex/run/ (or $LEINDEX_HOME/run/).
        let run_dir = leindex::config::resolve_leindex_home()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve LeIndex home directory"))?
            .join("run");

        // Ensure the run directory exists so the sidecar and socket live there.
        std::fs::create_dir_all(&run_dir)?;

        // Hard startup lock: refuse to run if a live daemon already exists.
        let outcome = resolve_endpoint(&run_dir, DAEMON_PROTOCOL_VERSION)?;
        match outcome {
            StartupOutcome::Connect(ep) => {
                anyhow::bail!(
                    "daemon already live at {} (pid {}); refusing second instance",
                    ep.socket_path.display(),
                    ep.pid
                );
            }
            StartupOutcome::Won(_) => { /* proceed — this process won */ }
        }

        info!(
            "leindexd starting; socket={}, idle_timeout_secs={}",
            socket_path.display(),
            idle_timeout_secs
        );

        let rt = build_runtime();
        rt.block_on(async {
            // Build the MCP server (no eager project/model load — spec §4.2).
            let server = McpServer::new(McpServerConfig::default())?;

            let idle_timeout = if idle_timeout_secs == 0 {
                None // 0 = disabled
            } else {
                Some(std::time::Duration::from_secs(idle_timeout_secs))
            };

            // Post-bind callback: write the endpoint sidecar AFTER the socket
            // is bound so clients discover the daemon only once it is actually
            // listening (spec §4.2 post-bind callback pattern).
            let run_dir_clone = run_dir.clone();
            let post_bind = move |bound_socket: &std::path::Path| {
                let endpoint = DaemonEndpoint::current_process(
                    bound_socket.to_path_buf(),
                    DAEMON_PROTOCOL_VERSION,
                );
                match write_endpoint_sidecar(&run_dir_clone, &endpoint) {
                    Ok(()) => {
                        info!(
                            "wrote {} (socket={}, pid={})",
                            ENDPOINT_SIDECAR,
                            bound_socket.display(),
                            endpoint.pid
                        );
                    }
                    Err(e) => {
                        error!("failed to write endpoint sidecar: {e}");
                    }
                }
            };

            server
                .run_socket(
                    &socket_path,
                    ProcessIdleClock::new(),
                    idle_timeout,
                    Some(&post_bind),
                )
                .await
        })
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn main() -> anyhow::Result<()> {
        anyhow::bail!("leindexd is only supported on Unix platforms")
    }
}

fn main() -> anyhow::Result<()> {
    imp::main()
}
