use super::{ToolCommands, get_project_path};
use crate::cli::leindex::LeIndex;
use crate::cli::mcp::handlers::{ToolHandler, all_tool_handlers};
use crate::cli::mcp::protocol::{JsonRpcError, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse};
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
        ToolCommands::List => {
            // Display as `LeIndex [Tool Name]  description` so the user sees the
            // human-readable title first (what they'll write in prompts), with
            // the canonical dotted name available via `leindex tools help`.
            let mut handlers = all_tool_handlers();
            handlers.sort_by(|a, b| a.title().cmp(b.title()));
            for handler in handlers {
                println!("{}\t{}", handler.title(), handler.description());
            }
            Ok(())
        }
        ToolCommands::Help { name } => {
            let handler = find_tool_handler(&name)
                .ok_or_else(|| anyhow::anyhow!("Unknown tool '{}'", name))?;
            print_tool_help(&handler);
            Ok(())
        }
        ToolCommands::Schema { name } => {
            let handler = find_tool_handler(&name)
                .ok_or_else(|| anyhow::anyhow!("Unknown tool '{}'", name))?;
            print_json_value(&handler.argument_schema())?;
            Ok(())
        }
        ToolCommands::Run {
            name,
            args_json,
            set,
        } => {
            let parsed_args = parse_tool_args_json(&args_json)?;
            let args = merge_tool_args(parsed_args.clone(), &set, project.as_ref())?;
            let value = execute_tool_handler(&name, args, project).await?;

            // Use the unified renderer — same path used by the MCP transport
            // so CLI and LLM-visible payloads stay in lock-step.
            let formatted =
                crate::cli::mcp::output::render_tool_output(&name, &value, &parsed_args);

            println!("{}", formatted);
            Ok(())
        }
    }
}
/// MCP stdio command implementation - Run MCP server in stdio mode
/// This mode allows AI tools to start LeIndex as a subprocess for automatic integration
///
/// Initialization is deferred: the server enters the stdin read loop immediately
/// (no SQLite open, no PDG load, no TF-IDF rebuild, no file watcher at startup).
/// Projects are loaded lazily on first tool call via `ProjectRegistry::get_or_load()`.
pub(super) async fn cmd_mcp_stdio_impl(project: Option<PathBuf>) -> AnyhowResult<()> {
    info!("Starting LeIndex MCP stdio server (lazy project loading)");
    crate::cli::memory_report::observe_rss("mcp_stdio_startup");

    let server = crate::cli::mcp::server::McpServer::new(
        crate::cli::mcp::server::McpServerConfig::default(),
    )
    .context("Failed to create MCP server")?;
    spawn_stdio_cleanup(server.clone());
    set_default_project(project).await?;

    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    let mut framed_responses = false;

    loop {
        let input = match read_stdio_input(&mut reader) {
            Ok(StdioInput::Payload { json, framed }) => {
                framed_responses |= framed;
                json
            }
            Ok(StdioInput::Skip) => continue,
            Ok(StdioInput::End) => break,
            Err(error) => {
                tracing::debug!("MCP stdio: fatal read error, breaking loop: {}", error);
                break;
            }
        };
        let Some((response, parse_error)) = response_for_payload(&input).await else {
            continue;
        };
        if write_stdio_response(&mut stdout, &response, framed_responses).is_err() {
            if framed_responses && parse_error {
                continue;
            }
            tracing::debug!("MCP stdio: failed to write to stdout");
            break;
        }
    }
    Ok(())
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
) -> AnyhowResult<()> {
    let project_path = get_project_path(project);
    let canonical_path = project_path
        .canonicalize()
        .context("Failed to canonicalize project path")?;

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

    server.run_socket(socket_path).await
}

/// MCP Unix socket command implementation — stub for non-Unix platforms.
#[cfg(not(unix))]
pub(super) async fn cmd_mcp_socket_impl(
    _socket_path: &std::path::Path,
    _project: Option<PathBuf>,
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
    let handler =
        find_tool_handler(name).ok_or_else(|| anyhow::anyhow!("Unknown tool '{}'", name))?;
    let registry = build_tool_registry(project)?;
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

    let mut leindex =
        LeIndex::new(&project_root).context("Failed to create LeIndex instance for tool run")?;
    let _ = leindex.load_from_storage();

    Ok(Arc::new(ProjectRegistry::with_initial_project(
        DEFAULT_MAX_PROJECTS,
        leindex,
    )))
}
/// Handle a single MCP request and return the response.
#[allow(clippy::needless_return)]
async fn handle_mcp_request(
    request: JsonRpcRequest,
    _project_path: PathBuf,
) -> anyhow::Result<Option<JsonRpcResponse>> {
    use crate::cli::mcp::server::{
        HANDLERS, SERVER_INSTANCE, SERVER_STATE, handle_tool_call, list_tools_json,
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
    fn test_framed_response_mode_is_sticky() {
        let mut framed_responses = false;
        for framed_input in [false, true, false] {
            framed_responses |= framed_input;
        }
        let mut output = Vec::new();
        write_stdio_response(&mut output, "{}", framed_responses).unwrap();
        assert_eq!(output, b"Content-Length: 2\r\n\r\n{}");
    }
}
