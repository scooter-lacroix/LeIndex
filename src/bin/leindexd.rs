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

    use leindex::cli::daemon::client::{LOCK_NAME, SOCKET_NAME};
    use leindex::cli::daemon::endpoint::{
        DaemonEndpoint, ENDPOINT_SIDECAR, read_sidecar, write_endpoint_sidecar,
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

    /// Default idle timeout in seconds (15 minutes with no client attached). Overridable via
    /// `--idle-timeout-secs`.
    const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 900;

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

    /// Take the daemon's exclusive lock. The returned file must stay alive for
    /// the daemon's lifetime.
    fn acquire_daemon_lock(path: &std::path::Path) -> std::io::Result<std::fs::File> {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: `flock` on a descriptor we own; no memory is touched.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked == 0 {
            Ok(file)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn init_logging() {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    /// Entry point for the leindexd daemon binary.
    pub fn main() -> anyhow::Result<()> {
        init_logging();

        let args: Vec<String> = std::env::args().skip(1).collect();

        // Parse --idle-timeout-secs <n> (optional, default 300 = 5 min)
        let idle_timeout_secs: u64 = parse_arg_value(&args, "--idle-timeout-secs")
            .map(|(v, _)| v.parse::<u64>().unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS))
            .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);

        // Resolve the run directory: ~/.leindex/run/ (or $LEINDEX_HOME/run/).
        let run_dir = leindex::config::resolve_leindex_home()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve LeIndex home directory"))?
            .join("run");
        std::fs::create_dir_all(&run_dir)?;
        // The socket is a capability: only this user may connect or even
        // reach the lock. `create_dir_all` applies the umask (typically
        // leaving 0755), and a daemon started directly (documented usage)
        // never went through the shim's permission hardening, so restrict
        // here too.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        // The socket defaults to the well-known path the shim connects to.
        let socket_path: PathBuf = parse_arg_value(&args, "--socket")
            .map(|(v, _)| PathBuf::from(v))
            .unwrap_or_else(|| run_dir.join(SOCKET_NAME));

        // Single winner: an exclusive advisory lock held for the daemon's whole
        // life. The kernel drops it when the process dies, however it dies, so
        // there is no stale-lock heuristic (pid reuse, start times) to get
        // wrong, and any number of shims may race to start a daemon.
        let _lock = match acquire_daemon_lock(&run_dir.join(LOCK_NAME)) {
            Ok(lock) => lock,
            Err(_) => {
                let holder = read_sidecar(&run_dir.join(ENDPOINT_SIDECAR)).ok().flatten();
                match holder {
                    Some(ep) => anyhow::bail!(
                        "daemon already live at {} (pid {}); refusing second instance",
                        ep.socket_path.display(),
                        ep.pid
                    ),
                    None => anyhow::bail!(
                        "daemon already live (lock {} is held); refusing second instance",
                        run_dir.join(LOCK_NAME).display()
                    ),
                }
            }
        };

        info!(
            "leindexd starting; socket={}, idle_timeout_secs={}",
            socket_path.display(),
            idle_timeout_secs
        );

        // Log feature-flag state at startup (§12.3: flag state visible at
        // daemon start).
        leindex::feature_flags::log_flag_state();

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
                // Defense in depth on top of the 0700 run directory: the
                // bind leaves the socket at `0777 & !umask` (typically
                // 0755); restrict it to this user.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Err(e) = std::fs::set_permissions(
                        bound_socket,
                        std::fs::Permissions::from_mode(0o600),
                    ) {
                        error!("failed to restrict socket {}: {e}", bound_socket.display());
                    }
                }
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

            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                served = server.run_socket(
                    &socket_path,
                    ProcessIdleClock::new(),
                    idle_timeout,
                    Some(&post_bind),
                ) => served,
                _ = terminate.recv() => {
                    // A replacement daemon or the user asked us to stop.
                    info!("leindexd received SIGTERM; exiting");
                    let _ = std::fs::remove_file(&socket_path);
                    Ok(())
                }
            }
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
