use super::{ToolCommands, get_project_path};
use crate::cli::leindex::LeIndex;
use crate::cli::mcp::handlers::{ToolHandler, all_tool_handlers};
use crate::cli::mcp::lock::{LockOutcome, McpProjectLock};
use crate::cli::mcp::protocol::{JsonRpcError, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse};
use crate::cli::mcp::server::{ProcessIdleClock, idle_exit_due};
use crate::cli::registry::{DEFAULT_MAX_PROJECTS, ProjectRegistry};
use anyhow::{Context, Result as AnyhowResult};
use serde_json::{Map, Value};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

pub(super) async fn cmd_tools_impl(
    command: ToolCommands,
    project: Option<PathBuf>,
) -> AnyhowResult<()> {
    match command {
        ToolCommands::List { verbose } => tools_list(verbose),
        ToolCommands::Inspect { name } => tools_inspect(&name),
        ToolCommands::Schema { name } => tools_schema(&name),
        ToolCommands::Run {
            name,
            args_json,
            set,
        } => tools_run(&name, &args_json, &set, project).await,
    }
}

/// `tools list` — the router table plus a pointer to inspect/run.
fn tools_list(verbose: bool) -> AnyhowResult<()> {
    print!("{}", crate::cli::mcp::grouped::cli_tools_table(verbose));
    println!(
        "\nRun `leindex tools inspect <tool>` for arguments, or `leindex tools run <tool> --set mode=<branch> ...`."
    );
    Ok(())
}

/// Resolve a branch handler by name, or fail with the standard not-found error.
fn resolve_tool(name: &str) -> AnyhowResult<ToolHandler> {
    find_tool_handler(name).ok_or_else(|| tool_not_found(name))
}

/// `tools inspect` — for a router: title, description and the oneOf schema of
/// its branches; for a branch: the handler's argument help.
fn tools_inspect(name: &str) -> AnyhowResult<()> {
    if let Some(group) = crate::cli::mcp::grouped::group_by_name(name) {
        println!(
            "{}\n{}\n",
            group.title,
            crate::cli::mcp::grouped::full_description(group)
        );
        println!("Schema:");
        return print_json_value(&crate::cli::mcp::grouped::group_schema_oneof(
            group,
            &all_tool_handlers(),
        ));
    }
    print_tool_help(&resolve_tool(name)?);
    Ok(())
}

/// `tools schema` — the raw JSON argument schema (routers print their oneOf form).
fn tools_schema(name: &str) -> AnyhowResult<()> {
    if let Some(group) = crate::cli::mcp::grouped::group_by_name(name) {
        return print_json_value(&crate::cli::mcp::grouped::group_schema_oneof(
            group,
            &all_tool_handlers(),
        ));
    }
    print_json_value(&resolve_tool(name)?.argument_schema())
}

/// Raise `max_latency_ms` to the one-shot default when the caller left it unset.
///
/// One-shot CLI mode: hydration happens inside this process (~1-2 s), which the
/// 250 ms resident-server default budget can never cover — every enrichment
/// silently downgraded to empty. No-op for non-object args.
fn apply_default_latency_budget(args: &mut Value) {
    let Some(object) = args.as_object_mut() else {
        return;
    };
    if object.contains_key("max_latency_ms") {
        return;
    }
    object.insert("max_latency_ms".to_string(), serde_json::json!(5000));
}

/// Print a tool result through the unified renderer — same path used by the MCP
/// transport so CLI and LLM-visible payloads stay in lock-step. The freshness
/// footer goes to stderr so stdout remains parseable JSON for tools that emit
/// raw JSON.
fn emit_rendered_tool_output(name: &str, value: &Value, parsed_args: &Value) -> AnyhowResult<()> {
    let (formatted, footer) =
        crate::cli::mcp::output::render_tool_output_split(name, value, parsed_args);
    println!("{}", formatted);
    if let Some(footer) = footer {
        eprintln!("{}", footer);
    }
    Ok(())
}

/// `tools run` — execute one tool and emit its rendered output.
async fn tools_run(
    name: &str,
    args_json: &str,
    set: &[String],
    project: Option<PathBuf>,
) -> AnyhowResult<()> {
    let parsed_args = parse_tool_args_json(args_json)?;
    let mut args = merge_tool_args(parsed_args.clone(), set, project.as_ref())?;
    apply_default_latency_budget(&mut args);
    // The four public tools pick their operation with `action`
    // (`--set action=text`); resolve to the underlying tool so the
    // CLI renders exactly what the MCP transport does.
    let (name, args) = crate::cli::mcp::grouped::resolve_call(name, args)
        .map_err(|error| anyhow::anyhow!("{}", error))?;
    let mut parsed_args = parsed_args;
    if let Some(object) = parsed_args.as_object_mut() {
        object.remove("action");
    }
    let value = execute_tool_handler(&name, args, project).await?;
    emit_rendered_tool_output(&name, &value, &parsed_args)
}

/// MCP stdio command implementation - Run MCP server in stdio mode
/// This mode allows AI tools to start LeIndex as a subprocess for automatic integration
///
/// Initialization is deferred: the server enters the stdin read loop immediately
/// (no SQLite open, no PDG load, no TF-IDF rebuild, no file watcher at startup).
/// Projects are loaded lazily on first tool call via `ProjectRegistry::get_or_load()`.
pub(super) async fn cmd_mcp_stdio_impl(
    project: Option<PathBuf>,
    idle_timeout_secs: Option<u64>,
) -> AnyhowResult<()> {
    info!("Starting LeIndex MCP stdio server (lazy project loading)");
    crate::cli::memory_report::observe_rss("mcp_stdio_startup");

    // Log feature-flag state at startup (§12.3: flag state visible at start).
    crate::feature_flags::log_flag_state();

    // D-3 advisory single-instance lock: warn when a live sibling already
    // serves the same canonical project, but NEVER hard-exit — a stdio server
    // is 1:1 with its agent's pipe, and exiting would break that agent's
    // client (GrayHill design-flaw resolution, msg 74).
    let lock_target = project
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .and_then(|path| path.canonicalize().ok());
    if let Some(canonical) = lock_target {
        let (outcome, guard) = McpProjectLock::try_acquire(&canonical);
        if let LockOutcome::AlreadyOwned { pid } = outcome {
            tracing::warn!(
                "Another leindex mcp already serves this project (pid {pid}); \
                 this instance continues in advisory mode (D-3)"
            );
        }
        // Held for the process lifetime; Drop releases the sidecars on exit.
        let _lock_guard = guard;
    }

    let server = crate::cli::mcp::server::McpServer::new(
        crate::cli::mcp::server::McpServerConfig::default(),
    )
    .context("Failed to create MCP server")?;
    spawn_stdio_cleanup(server.clone());
    set_default_project(project).await?;

    let idle_timeout = effective_mcp_idle_timeout(idle_timeout_secs);
    if let Some(timeout) = idle_timeout {
        info!(
            "MCP stdio idle self-exit enabled: exiting after {}s with no requests (D-1)",
            timeout.as_secs()
        );
    }
    let idle_clock = ProcessIdleClock::new();

    // D-1 idle exit: the blocking stdin read cannot be interrupted by tokio,
    // so a dedicated reader thread feeds the async loop over a channel. The
    // thread exits naturally with the process.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<io::Result<StdioInput>>(8);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = io::BufReader::new(stdin.lock());
        loop {
            let input = read_stdio_input(&mut reader);
            let terminal = matches!(input, Ok(StdioInput::End)) | input.is_err();
            if tx.blocking_send(input).is_err() {
                break;
            }
            if terminal {
                break;
            }
        }
    });

    let writer = StdioWriter::spawn();
    let dispatcher = StdioDispatcher::new(writer.sender());
    let mut framed_responses = false;
    let mut idle_ticker = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {
            maybe_input = rx.recv() => {
                let input = match maybe_input {
                    Some(Ok(input)) => input,
                    Some(Err(error)) => {
                        tracing::debug!("MCP stdio: fatal read error, breaking loop: {}", error);
                        break;
                    }
                    None => break,
                };
                // Any payload (including ping/notifications) resets the clock.
                idle_clock.touch();
                if !dispatcher.dispatch(input, &mut framed_responses).await {
                    break;
                }
            }
            _ = idle_ticker.tick() => {
                if writer.failed() {
                    tracing::debug!("MCP stdio: stdout closed, shutting down");
                    break;
                }
                // A call that outlives the idle window is not "idle".
                if dispatcher.in_flight() == 0
                    && idle_exit_due(idle_clock.idle_duration(), idle_timeout)
                {
                    info!(
                        "MCP stdio server idle for {:?}; exiting (D-1 memory-pressure idle exit)",
                        idle_timeout
                    );
                    break;
                }
            }
        }
    }
    // Answer everything already accepted before the process exits (a piped
    // `printf ... | leindex mcp` closes stdin right after the last request).
    dispatcher.drain().await;
    drop(dispatcher);
    writer.finish();
    Ok(())
}

/// Upper bound on tool calls executing at once on one stdio connection.
const MAX_CONCURRENT_STDIO_CALLS: usize = 64;

/// Upper bound on tool calls WAITING for an execution permit. Admission is
/// bounded in both dimensions: a client that keeps sending while never
/// reading stdout gets an explicit JSON-RPC busy error past this point
/// instead of silently accumulating spawned tasks (each holding its request
/// payload) until the process exhausts memory.
const MAX_PENDING_STDIO_CALLS: usize = 256;

/// Upper bound on COMPLETED responses queued behind the stdout writer. A
/// bounded channel turns a client that stops reading stdout into pipe
/// backpressure (senders park, stdin reads stall) instead of an unbounded
/// response queue.
const STDIO_OUTBOUND_CAPACITY: usize = 256;

/// One response queued for the stdout writer.
struct StdioOutbound {
    response: String,
    framed: bool,
    /// A framed parse-error reply may fail to write without ending the session.
    recoverable: bool,
}

/// Owns stdout on a dedicated thread so response writes (a pipe that a slow
/// client drains lazily) can never park a tokio worker.
struct StdioWriter {
    tx: tokio::sync::mpsc::Sender<StdioOutbound>,
    thread: std::thread::JoinHandle<()>,
    failed: Arc<std::sync::atomic::AtomicBool>,
}

impl StdioWriter {
    fn spawn() -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StdioOutbound>(STDIO_OUTBOUND_CAPACITY);
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failed_flag = Arc::clone(&failed);
        let thread = std::thread::spawn(move || {
            let mut stdout = io::stdout().lock();
            while let Some(out) = rx.blocking_recv() {
                if write_stdio_response(&mut stdout, &out.response, out.framed).is_err() {
                    if out.framed && out.recoverable {
                        continue;
                    }
                    tracing::debug!("MCP stdio: failed to write to stdout");
                    failed_flag.store(true, std::sync::atomic::Ordering::Release);
                    break;
                }
            }
        });
        Self { tx, thread, failed }
    }

    fn sender(&self) -> tokio::sync::mpsc::Sender<StdioOutbound> {
        self.tx.clone()
    }

    fn failed(&self) -> bool {
        self.failed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Flush what is queued and join the writer thread.
    fn finish(self) {
        drop(self.tx);
        let _ = self.thread.join();
    }
}

/// Routes stdio payloads. Cheap protocol methods (`initialize`, `ping`,
/// `tools/list`, ...) are answered inline so ordering guarantees hold — the
/// handshake completes before anything after it runs. `tools/call` is
/// spawned: a slow or blocked tool must not queue every other request
/// (including `ping`) behind it, which is how a single cold index used to
/// make the whole MCP server look hung while the one-shot CLI was fine.
struct StdioDispatcher {
    out: tokio::sync::mpsc::Sender<StdioOutbound>,
    calls: std::sync::Mutex<tokio::task::JoinSet<()>>,
    limiter: Arc<tokio::sync::Semaphore>,
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    pending: Arc<std::sync::atomic::AtomicUsize>,
}

impl StdioDispatcher {
    fn new(out: tokio::sync::mpsc::Sender<StdioOutbound>) -> Self {
        Self {
            out,
            calls: std::sync::Mutex::new(tokio::task::JoinSet::new()),
            limiter: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_STDIO_CALLS)),
            in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn in_flight(&self) -> usize {
        self.in_flight.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Returns `false` when the server loop should exit (stdin `End`).
    async fn dispatch(&self, input: StdioInput, framed_responses: &mut bool) -> bool {
        let json = match input {
            StdioInput::Payload { json, framed } => {
                *framed_responses |= framed;
                json
            }
            StdioInput::Skip => return true,
            StdioInput::End => return false,
        };
        let framed = *framed_responses;
        match tool_call_id(&json) {
            Some(id) => self.admit_tool_call(json, id, framed).await,
            None => {
                if let Some((response, recoverable)) = response_for_payload(&json).await {
                    // Bounded channel: a client that stopped reading stdout
                    // parks this send, which stalls stdin reads — pipe
                    // backpressure instead of an unbounded response queue.
                    let _ = self
                        .out
                        .send(StdioOutbound {
                            response,
                            framed,
                            recoverable,
                        })
                        .await;
                }
            }
        }
        true
    }

    /// Admit one tool call under BOTH bounds: at most
    /// `MAX_CONCURRENT_STDIO_CALLS` execute, and at most
    /// `MAX_PENDING_STDIO_CALLS` wait for a permit. Past the pending bound
    /// the request is refused with a JSON-RPC busy error — visible
    /// backpressure — instead of accumulating unbounded spawned tasks.
    async fn admit_tool_call(&self, json: String, id: Value, framed: bool) {
        use std::sync::atomic::Ordering;
        // A free permit means the task executes without queueing.
        let permit = self.limiter.clone().try_acquire_owned().ok();
        if permit.is_none() {
            let waiting = self.pending.fetch_add(1, Ordering::AcqRel) + 1;
            if waiting > MAX_PENDING_STDIO_CALLS {
                self.pending.fetch_sub(1, Ordering::AcqRel);
                let failure = JsonRpcResponse::error(
                    id,
                    JsonRpcError::server_busy(format!(
                        "too many queued tool calls (limit {MAX_PENDING_STDIO_CALLS}); \
                         wait for earlier calls to finish"
                    )),
                );
                if let Ok(response) = serde_json::to_string(&failure) {
                    let _ = self
                        .out
                        .send(StdioOutbound {
                            response,
                            framed,
                            recoverable: false,
                        })
                        .await;
                }
                return;
            }
        }
        let out = self.out.clone();
        let limiter = Arc::clone(&self.limiter);
        let in_flight = Arc::clone(&self.in_flight);
        let pending = Arc::clone(&self.pending);
        // Whether THIS call is consuming a pending slot: only queued calls
        // incremented the counter above, so only they may decrement it (a
        // decrement on the immediate-permit path would wrap 0 -> usize::MAX
        // and admit a later burst past the bound).
        let was_queued = permit.is_none();
        in_flight.fetch_add(1, Ordering::AcqRel);
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        // Reap finished tasks so the set does not grow for the whole session.
        while calls.try_join_next().is_some() {}
        calls.spawn(async move {
            let _permit = match permit {
                Some(permit) => Some(permit),
                None => limiter.acquire_owned().await.ok(),
            };
            if was_queued {
                pending.fetch_sub(1, Ordering::AcqRel);
            }
            // A panicking handler must still answer: a request that never
            // gets a response is indistinguishable from a hang to the client.
            let response =
                match tokio::spawn(async move { response_for_payload(&json).await }).await {
                    Ok(response) => response.map(|(body, _)| body),
                    Err(error) => {
                        tracing::error!("MCP tool call task failed: {error}");
                        let failure = JsonRpcResponse::error(
                            id,
                            JsonRpcError::internal_error(format!("Tool call aborted: {error}")),
                        );
                        serde_json::to_string(&failure).ok()
                    }
                };
            if let Some(response) = response {
                let _ = out
                    .send(StdioOutbound {
                        response,
                        framed,
                        recoverable: false,
                    })
                    .await;
            }
            in_flight.fetch_sub(1, Ordering::AcqRel);
        });
    }

    /// Wait for every accepted tool call to publish its response.
    async fn drain(&self) {
        let mut pending = {
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *calls)
        };
        while pending.join_next().await.is_some() {}
    }
}

/// `Some(id)` when `payload` is a `tools/call` request (has an id).
fn tool_call_id(payload: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(payload).ok()?;
    if value.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    value.get("id").cloned()
}

/// Resolve the effective MCP idle self-exit window (D-1): the CLI flag wins
/// over `[mcp] idle_timeout_secs`; `0` disables the feature (`None`).
fn effective_mcp_idle_timeout(cli_flag: Option<u64>) -> Option<std::time::Duration> {
    let secs = cli_flag.unwrap_or_else(|| {
        crate::config::LeIndexConfig::load_cached()
            .mcp
            .idle_timeout_secs
    });
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

fn spawn_stdio_cleanup(server: crate::cli::mcp::server::McpServer) {
    let cleanup_handle = tokio::spawn(async move {
        const CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
        const SESSION_MAX_IDLE: std::time::Duration = std::time::Duration::from_secs(300);
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        loop {
            interval.tick().await;
            let removed = server.cleanup_stale_sessions(SESSION_MAX_IDLE);
            if removed > 0 {
                tracing::debug!("Cleaned up {} stale session(s)", removed);
            }
            // D-2 (memory-pressure): evict loaded project engines idle past
            // `[mcp] engine_max_idle_secs` so a long-lived MCP process does
            // not retain every project it ever touched (mmaps + heap freed).
            if let Some(registry) = crate::cli::mcp::server::SERVER_STATE.get() {
                let engine_max_idle = std::time::Duration::from_secs(
                    crate::config::LeIndexConfig::load_cached()
                        .mcp
                        .engine_max_idle_secs,
                );
                let evicted = registry.evict_idle_engines(engine_max_idle).await;
                if evicted > 0 {
                    tracing::info!("Evicted {evicted} idle project engine(s) (D-2)");
                }
            }
        }
    });
    tokio::spawn(async move {
        match cleanup_handle.await {
            Ok(_) => {}
            Err(error) => tracing::error!("MCP stdio cleanup task died: {error}"),
        }
    });
}

async fn set_default_project(project: Option<PathBuf>) -> AnyhowResult<()> {
    let registry = crate::cli::mcp::server::SERVER_STATE
        .get()
        .context("Server state not initialized")?;
    let resolved_path = match project {
        Some(path) => path
            .canonicalize()
            .with_context(|| format!("Cannot resolve project path '{}'", path.display()))?,
        None => std::env::current_dir().context("Cannot determine current working directory")?,
    };
    registry.set_default_path(resolved_path.clone()).await;
    info!("Default project path set to: {}", resolved_path.display());
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum StdioInput {
    Payload { json: String, framed: bool },
    Skip,
    End,
}

fn read_stdio_input(reader: &mut impl BufRead) -> io::Result<StdioInput> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(StdioInput::End);
    }
    let line = line.trim_end();
    if line.is_empty() {
        return Ok(StdioInput::Skip);
    }
    if !line.to_ascii_lowercase().starts_with("content-length:") {
        return Ok(StdioInput::Payload {
            json: line.to_string(),
            framed: false,
        });
    }

    let length = match line.split(':').nth(1).unwrap_or("").trim().parse::<usize>() {
        Ok(length) => length,
        Err(error) => {
            tracing::debug!("MCP stdio: invalid Content-Length header: {}", error);
            return Ok(StdioInput::Skip);
        }
    };
    const MAX_STDIN_PAYLOAD: usize = 10 * 1024 * 1024;
    let oversized = length > MAX_STDIN_PAYLOAD;
    if oversized {
        eprintln!(
            "[ERROR] Payload too large: {} bytes (max: {} bytes)",
            length, MAX_STDIN_PAYLOAD
        );
    }
    consume_stdio_headers(reader);
    if oversized {
        io::copy(&mut reader.take(length as u64), &mut io::sink())?;
        return Ok(StdioInput::Skip);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(StdioInput::Payload {
        json: String::from_utf8_lossy(&body).into_owned(),
        framed: true,
    })
}

fn consume_stdio_headers(reader: &mut impl BufRead) {
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            return;
        }
    }
}

async fn response_for_payload(payload: &str) -> Option<(String, bool)> {
    let message = match JsonRpcMessage::from_json(payload) {
        Ok(message) => message,
        Err(error) => {
            let response = JsonRpcResponse::error(Value::Null, error);
            return Some((serde_json::to_string(&response).unwrap_or_default(), true));
        }
    };
    let JsonRpcMessage::Request(request) = message else {
        return None;
    };
    let request_id = request.id.clone().unwrap_or(Value::Null);
    let response = match handle_mcp_request(request, PathBuf::new()).await {
        Ok(response) => response?,
        Err(error) => {
            JsonRpcResponse::error(request_id, JsonRpcError::internal_error(error.to_string()))
        }
    };
    Some((
        serde_json::to_string(&response).unwrap_or_else(|error| {
            format!("{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32700,\"message\":\"Failed to serialize response: {}\"}}}}", error)
        }),
        false,
    ))
}

fn write_stdio_response(writer: &mut impl Write, response: &str, framed: bool) -> io::Result<()> {
    if framed {
        write!(
            writer,
            "Content-Length: {}\r\n\r\n{}",
            response.len(),
            response
        )?;
    } else {
        writeln!(writer, "{}", response)?;
    }
    writer.flush()
}

/// MCP Unix socket command implementation — run MCP server on a Unix domain socket.
#[cfg(unix)]
pub(super) async fn cmd_mcp_socket_impl(
    socket_path: &std::path::Path,
    project: Option<PathBuf>,
    idle_timeout_secs: Option<u64>,
) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

    // D-3 advisory single-instance lock (warn + continue, never hard-exit).
    let (lock_outcome, guard) = McpProjectLock::try_acquire(&canonical_path);
    if let LockOutcome::AlreadyOwned { pid } = lock_outcome {
        tracing::warn!(
            "Another leindex mcp already serves this project (pid {pid}); \
             this instance continues in advisory mode (D-3)"
        );
    }
    let _lock_guard = guard;

    info!(
        "Starting LeIndex MCP Unix socket server at {} for project: {}",
        socket_path.display(),
        canonical_path.display()
    );

    // Create LeIndex instance
    let mut leindex = LeIndex::new(&canonical_path).context("Failed to create LeIndex instance")?;
    let _ = leindex.load_from_storage();

    // Initialize global state for handlers
    let registry = Arc::new(ProjectRegistry::with_initial_project(
        DEFAULT_MAX_PROJECTS,
        leindex,
    ));
    let _ = crate::cli::mcp::server::SERVER_STATE.set(registry.clone());

    // Initialize handlers
    let _ = crate::cli::mcp::server::HANDLERS.set(all_tool_handlers());

    // Create MCP server instance
    let server = crate::cli::mcp::server::McpServer::new(
        crate::cli::mcp::server::McpServerConfig::default(),
    )
    .context("Failed to create MCP server")?;

    println!("\nLeIndex MCP Unix Socket Server\n");
    println!("Socket: {}", socket_path.display());
    println!("Project: {}", canonical_path.display());
    println!("\nPress Ctrl+C to stop the server\n");

    let idle_timeout = effective_mcp_idle_timeout(idle_timeout_secs);
    server
        .run_socket(socket_path, ProcessIdleClock::new(), idle_timeout, None)
        .await
}

/// MCP Unix socket command implementation — stub for non-Unix platforms.
#[cfg(not(unix))]
pub(super) async fn cmd_mcp_socket_impl(
    _socket_path: &std::path::Path,
    _project: Option<PathBuf>,
    _idle_timeout_secs: Option<u64>,
) -> AnyhowResult<()> {
    anyhow::bail!("Unix sockets are not supported on this platform");
}

fn parse_tool_args_json(args_json: &str) -> AnyhowResult<Value> {
    let value: Value =
        serde_json::from_str(args_json).context("Tool arguments must be valid JSON")?;
    if !value.is_object() {
        anyhow::bail!("Tool arguments must be a JSON object");
    }
    Ok(value)
}

pub(super) fn merge_tool_args(
    args: Value,
    set_args: &[String],
    project: Option<&PathBuf>,
) -> AnyhowResult<Value> {
    let mut object = match args {
        Value::Object(map) => map,
        _ => Map::new(),
    };

    for entry in set_args {
        let (key, raw_value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid --set '{}'. Use KEY=VALUE", entry))?;
        let value = serde_json::from_str(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_string()));
        object.insert(key.to_string(), value);
    }

    if let Some(project) = project {
        if !object.contains_key("project_path") {
            let canonical = project.canonicalize().unwrap_or_else(|_| project.clone());
            object.insert(
                "project_path".to_string(),
                Value::String(canonical.display().to_string()),
            );
        }
    }

    Ok(Value::Object(object))
}

fn print_json_value(value: &Value) -> AnyhowResult<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("Failed to format JSON output")?
    );
    Ok(())
}

fn print_tool_help(handler: &ToolHandler) {
    let schema = handler.argument_schema();
    let normalized = normalize_tool_name(handler.name());
    let short_name = normalized
        .strip_prefix("leindex_")
        .unwrap_or(normalized.as_str())
        .to_string();
    let kebab_short = short_name.replace('_', "-");
    let kebab_full = normalized.replace('_', "-");

    println!("{}", format_tool_title(handler.title()));
    println!("{}", handler.description());
    println!();
    println!("Aliases:");
    println!("  {}", handler.name());
    if short_name != handler.name() {
        println!("  {}", short_name);
    }
    if kebab_short != short_name {
        println!("  {}", kebab_short);
    }
    if kebab_full != normalized && kebab_full != kebab_short {
        println!("  {}", kebab_full);
    }
    println!();
    println!("Usage:");
    println!("  leindex tools help {}", handler.name());
    println!("  leindex tools schema {}", handler.name());
    println!(
        "  leindex tools run {} --args '<json-object>'",
        handler.name()
    );
    println!(
        "  leindex tools run {} --set key=value --set other=true",
        handler.name()
    );

    if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
        println!();
        println!("Arguments:");

        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();

        for (name, property) in properties {
            let required_marker = if required.contains(name.as_str()) {
                "required"
            } else {
                "optional"
            };
            let property_type = property
                .get("type")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    property
                        .get("oneOf")
                        .and_then(|v| v.as_array())
                        .map(|_| "multiple")
                })
                .unwrap_or("value");
            let description = property
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let default = property.get("default");

            println!("  {} ({}, {})", name, property_type, required_marker);
            if !description.is_empty() {
                println!("    {}", description);
            }
            if let Some(default) = default {
                println!("    default: {}", default);
            }
        }
    }

    println!();
    println!("Schema:");
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).unwrap_or_else(|_| "{}".to_string())
    );
}

fn normalize_tool_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

fn format_tool_title(title: &str) -> String {
    if let Some(rest) = title
        .strip_prefix("LeIndex [")
        .and_then(|s| s.strip_suffix(']'))
    {
        format!("LEINDEX [{}]", rest)
    } else {
        title.to_string()
    }
}

fn tool_not_found(name: &str) -> anyhow::Error {
    anyhow::anyhow!("{}", crate::cli::mcp::grouped::suggest_unknown_tool(name))
}

pub(super) fn find_tool_handler(name: &str) -> Option<ToolHandler> {
    let normalized = normalize_tool_name(name);

    all_tool_handlers().into_iter().find(|handler| {
        let handler_name = normalize_tool_name(handler.name());
        let title = handler.title();
        let short_name = extract_short_name(&handler_name);

        // Check all possible formats:
        // 1. handler.name() - e.g., "leindex.context"
        // 2. short name from handler - e.g., "context"
        // 3. title - e.g., "leindex_context" (normalized)
        // 4. legacy format - e.g., "leindex_context" in input matches "context" from handler
        // 5. MCP-compliant format - e.g., "leindex.index" in input matches "leindex.index" from handler
        // 6. Direct legacy format - e.g., "leindex_search" matches handler name "leindex.search" after normalization

        handler_name == normalized
            || short_name == normalized
            || normalize_tool_name(title) == normalized
            // Legacy leindex_* format - check if input has leindex_ prefix matching short name
            || (normalized.starts_with("leindex_") && short_name == normalized.strip_prefix("leindex_").unwrap_or(""))
    })
}

/// Extract short name from a tool name.
/// For "leindex_foo" returns "foo", for "leindex [foo bar]" returns "foo_bar", for "leindex.foo-bar" returns "foo_bar".
fn extract_short_name(name: &str) -> String {
    // Handle "leindex [foo bar]" format (normalized to "leindex [foo bar]")
    if let Some(inside) = name.strip_prefix("leindex [") {
        if let Some(inside) = inside.strip_suffix(']') {
            let with_underscores = inside.replace(' ', "_");
            return normalize_tool_name(&with_underscores);
        }
    }
    // Handle "leindex.foo-bar" format (MCP-compliant: leindex.search, leindex.project-map)
    if let Some(inside) = name.strip_prefix("leindex.") {
        return normalize_tool_name(inside);
    }
    // Handle old "leindex_foo" format
    name.strip_prefix("leindex_")
        .map(normalize_tool_name)
        .unwrap_or_else(|| normalize_tool_name(name))
}

pub(super) async fn execute_tool_handler(
    name: &str,
    args: Value,
    project: Option<PathBuf>,
) -> AnyhowResult<Value> {
    let handler = find_tool_handler(name).ok_or_else(|| tool_not_found(name))?;
    let registry = build_tool_registry(project)?;
    let level = crate::cli::mcp::server::hydration_for_tool(&normalize_tool_name(name));
    registry
        .ensure_hydrated(args.get("project_path").and_then(Value::as_str), level)
        .await;
    handler
        .execute(&registry, args)
        .await
        .map_err(|error| anyhow::anyhow!("{}", error))
}

fn build_tool_registry(project: Option<PathBuf>) -> AnyhowResult<Arc<ProjectRegistry>> {
    let initial = get_project_path(project);
    let canonical = initial.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize project path '{}'",
            initial.display()
        )
    })?;
    let project_root = if canonical.is_file() {
        canonical
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("File path '{}' has no parent", canonical.display()))?
    } else {
        canonical
    };

    let leindex =
        LeIndex::new(&project_root).context("Failed to create LeIndex instance for tool run")?;
    // Nothing is loaded up front: `execute_tool_handler` loads exactly what the
    // requested tool needs (see `hydration_for_tool`), so `find` and reads pay
    // nothing and graph tools skip the search engine.
    let registry = Arc::new(ProjectRegistry::with_initial_project(
        DEFAULT_MAX_PROJECTS,
        leindex,
    ));
    registry.mark_one_shot();
    Ok(registry)
}
/// Handle a single MCP request and return the response.
#[allow(clippy::needless_return)]
async fn handle_mcp_request(
    request: JsonRpcRequest,
    _project_path: PathBuf,
) -> anyhow::Result<Option<JsonRpcResponse>> {
    use crate::cli::mcp::server::{
        HANDLERS, SERVER_INSTANCE, SERVER_STATE, handle_prompt_get, handle_resource_read,
        handle_tool_call, list_prompts_json, list_resources_json, list_tools_json,
    };

    let method_name = request.method.clone();
    let id = request.id.clone().unwrap_or(serde_json::Value::Null);

    // Notifications (id is null) must not receive a response per JSON-RPC 2.0 spec
    if request.id.is_none() {
        tracing::debug!("Ignoring notification: {}", method_name);
        return Ok(None);
    }

    // Get server instance to check handshake status
    let server_instance = match SERVER_INSTANCE.get() {
        Some(s) => s,
        None => {
            return Ok(Some(JsonRpcResponse::error(
                id,
                crate::cli::mcp::protocol::JsonRpcError::new(
                    -32603,
                    "Server instance not initialized",
                ),
            )));
        }
    };

    // Check handshake completion for non-initialize requests
    if !server_instance
        .handshake_complete
        .load(std::sync::atomic::Ordering::SeqCst)
        && method_name != "initialize"
        && method_name != "ping"
    {
        return Ok(Some(JsonRpcResponse::error(
            id,
            crate::cli::mcp::protocol::JsonRpcError::new(
                -32000,
                "Server not initialized. Call 'initialize' first.",
            ),
        )));
    }

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
            // Mark handshake as complete
            server_instance
                .handshake_complete
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Hide the cold project load behind the model's think-time.
            state.spawn_prewarm();

            // Return server capabilities with comprehensive description
            return Ok(Some(JsonRpcResponse::success(
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
            )));
        }

        "ping" => {
            // Simple health check
            Ok(Some(JsonRpcResponse::success(id, serde_json::json!({}))))
        }
        "tools/call" => {
            // Correctness-critical work is owned by the registry/job layer;
            // awaiting it here never drops a spawned blocking build halfway
            // through persistence or publication.
            let result = handle_tool_call(state, handlers, &request).await;
            Ok(Some(JsonRpcResponse::from_result(id, result)))
        }
        "tools/list" => {
            // List all available tools using centralized formatter
            Ok(Some(JsonRpcResponse::success(
                id,
                list_tools_json(handlers),
            )))
        }
        // Prompts/resources are served on the HTTP and socket transports;
        // stdio answered them with method-not-found, which clients that probe
        // them at startup (before any tool call) treat as a broken server.
        "prompts/list" => Ok(Some(JsonRpcResponse::success(id, list_prompts_json()))),
        "prompts/get" => Ok(Some(JsonRpcResponse::from_result(
            id,
            handle_prompt_get(&request),
        ))),
        "resources/list" => Ok(Some(JsonRpcResponse::success(id, list_resources_json()))),
        "resources/read" => Ok(Some(JsonRpcResponse::from_result(
            id,
            handle_resource_read(&request),
        ))),
        _ => Ok(Some(JsonRpcResponse::error(
            id,
            crate::cli::mcp::protocol::JsonRpcError::method_not_found(method_name),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_read_stdio_input_skips_blank_line() {
        let mut input = Cursor::new(b"\n".as_slice());
        assert_eq!(read_stdio_input(&mut input).unwrap(), StdioInput::Skip);
    }

    #[test]
    fn test_read_stdio_input_reads_newline_json() {
        let mut input = Cursor::new(b"{\"jsonrpc\":\"2.0\"}\n".as_slice());
        assert_eq!(
            read_stdio_input(&mut input).unwrap(),
            StdioInput::Payload {
                json: "{\"jsonrpc\":\"2.0\"}".to_string(),
                framed: false,
            }
        );
    }

    #[test]
    fn test_read_stdio_input_reads_content_length_body() {
        let mut input =
            Cursor::new(b"Content-Length: 7\r\nX-Test: yes\r\n\r\n{\"a\":1}".as_slice());
        assert_eq!(
            read_stdio_input(&mut input).unwrap(),
            StdioInput::Payload {
                json: "{\"a\":1}".to_string(),
                framed: true,
            }
        );
    }

    #[test]
    fn test_read_stdio_input_returns_end_at_eof() {
        let mut input = Cursor::new(Vec::<u8>::new());
        assert_eq!(read_stdio_input(&mut input).unwrap(), StdioInput::End);
    }

    #[test]
    fn test_read_stdio_input_skips_invalid_content_length() {
        let mut input = Cursor::new(b"Content-Length: nope\r\n\r\n".as_slice());
        assert_eq!(read_stdio_input(&mut input).unwrap(), StdioInput::Skip);
    }

    #[test]
    fn test_read_stdio_input_drains_oversized_frame_and_preserves_alignment() {
        const OVERSIZED: usize = 10 * 1024 * 1024 + 1;
        let next = b"{\"next\":true}\n";
        let mut bytes = format!("Content-Length: {OVERSIZED}\r\nX-Test: yes\r\n\r\n").into_bytes();
        bytes.resize(bytes.len() + OVERSIZED, b'x');
        bytes.extend_from_slice(next);
        let mut input = Cursor::new(bytes);

        assert_eq!(read_stdio_input(&mut input).unwrap(), StdioInput::Skip);
        assert_eq!(
            read_stdio_input(&mut input).unwrap(),
            StdioInput::Payload {
                json: "{\"next\":true}".to_string(),
                framed: false,
            }
        );
    }

    #[test]
    fn test_read_stdio_input_rejects_truncated_frame() {
        let mut input = Cursor::new(b"Content-Length: 5\r\n\r\n{}".as_slice());
        assert_eq!(
            read_stdio_input(&mut input).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn test_read_stdio_input_rejects_malformed_header_termination() {
        let mut input = Cursor::new(b"Content-Length: 2\r\n{}".as_slice());
        assert_eq!(
            read_stdio_input(&mut input).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn test_response_for_payload_omits_notification_response() {
        let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(response_for_payload(notification).await.is_none());
    }

    #[tokio::test]
    async fn test_framed_parse_error_is_marked_for_write_failure_recovery() {
        let (_, parse_error) = response_for_payload("{").await.unwrap();
        let (_, normal_response) =
            response_for_payload(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
                .await
                .unwrap();
        assert!(parse_error);
        assert!(!normal_response);
    }

    #[test]
    fn test_write_stdio_response_preserves_framed_wire_format() {
        let mut output = Vec::new();
        write_stdio_response(&mut output, "{}", true).unwrap();
        assert_eq!(output, b"Content-Length: 2\r\n\r\n{}");
    }

    #[test]
    fn test_write_stdio_response_preserves_newline_wire_format() {
        let mut output = Vec::new();
        write_stdio_response(&mut output, "{}", false).unwrap();
        assert_eq!(output, b"{}\n");
    }

    #[test]
    fn test_effective_mcp_idle_timeout_priority_and_zero() {
        // CLI flag wins over `[mcp] idle_timeout_secs`.
        assert_eq!(
            effective_mcp_idle_timeout(Some(5)),
            Some(std::time::Duration::from_secs(5))
        );
        // `0` disables the feature (None).
        assert_eq!(effective_mcp_idle_timeout(Some(0)), None);
        // No flag: falls back to the config default (1800).
        assert_eq!(
            effective_mcp_idle_timeout(None),
            Some(std::time::Duration::from_secs(1800))
        );
    }

    #[test]
    fn test_framed_response_mode_is_sticky() {
        let mut framed_responses = false;
        for framed_input in [false, true, false] {
            framed_responses |= framed_input;
        }
        let mut output = Vec::new();
        write_stdio_response(&mut output, "{}", framed_responses).unwrap();
        assert_eq!(output, b"Content-Length: 2\r\n\r\n{}");
    }

    /// Admission is bounded in BOTH dimensions (round-11 Codex P2): past
    /// `MAX_PENDING_STDIO_CALLS` waiting calls, the request is refused with
    /// a visible JSON-RPC busy error instead of accumulating unbounded
    /// spawned tasks. The response channel is bounded for the same reason —
    /// a client that stops reading stdout gets pipe backpressure, not an
    /// unbounded response queue.
    #[tokio::test]
    async fn test_stdio_admission_refuses_past_the_pending_bound() {
        use std::sync::atomic::Ordering;
        let (tx, mut rx) = tokio::sync::mpsc::channel(STDIO_OUTBOUND_CAPACITY);
        let dispatcher = StdioDispatcher::new(tx);

        let request = |id: u64| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": "leindex_search", "arguments": {} }
            })
            .to_string()
        };

        // One call takes an IMMEDIATE permit (permits are still free): the
        // pending counter must stay at zero — an unconditional decrement on
        // this path wrapped 0 -> usize::MAX and permanently disabled the
        // bound (round-12 kilo P2). This call runs BEFORE the permits are
        // drained, or it would queue like the rest.
        dispatcher
            .admit_tool_call(request(0), serde_json::json!(0), false)
            .await;
        assert_eq!(
            dispatcher.pending.load(Ordering::Acquire),
            0,
            "an immediate-permit call must not touch the pending counter"
        );

        // Drain the remaining execution permits (the immediate call above
        // holds one) so each subsequent call has to queue.
        let _held: Vec<_> = (0..MAX_CONCURRENT_STDIO_CALLS - 1)
            .map(|_| {
                dispatcher
                    .limiter
                    .clone()
                    .try_acquire_owned()
                    .expect("permit available")
            })
            .collect();

        // Fill the pending queue exactly to the bound: every call is
        // accepted (spawned, waiting for a permit), none is answered.
        for id in 1..=MAX_PENDING_STDIO_CALLS as u64 {
            dispatcher
                .admit_tool_call(request(id), serde_json::json!(id), false)
                .await;
        }
        assert_eq!(
            dispatcher.pending.load(Ordering::Acquire),
            MAX_PENDING_STDIO_CALLS,
            "accepted-but-queued calls are counted against the pending bound"
        );
        assert!(rx.try_recv().is_err(), "no queued call produced a response");

        // One past the bound: refused with a busy error, and the pending
        // count returns to the bound.
        dispatcher
            .admit_tool_call(request(99_999), serde_json::json!(99_999i64), false)
            .await;
        let busy = rx.try_recv().expect("the refused call gets a busy error");
        assert!(
            busy.response.contains("too many queued tool calls"),
            "response: {}",
            busy.response
        );
        assert_eq!(
            dispatcher.pending.load(Ordering::Acquire),
            MAX_PENDING_STDIO_CALLS,
            "the refusal does not consume a pending slot"
        );
    }
}
