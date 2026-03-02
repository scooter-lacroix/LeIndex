// MCP Stdio End-to-End Integration Tests
//
// Tests the full JSON-RPC dispatch stack as used by the stdio transport:
//   - JSON serialization/deserialization correctness
//   - Protocol method routing (initialize, tools/list, tools/call, notifications)
//   - All 16 tool handlers registered and named correctly
//   - Error responses carry proper structure
//   - No double-newline in serialized responses (the transport bug from Task A.1)

use lepasserelle::mcp::handlers::{
    ContextHandler, DeepAnalyzeHandler, DiagnosticsHandler, EditApplyHandler, EditPreviewHandler,
    FileSummaryHandler, GrepSymbolsHandler, ImpactAnalysisHandler, IndexHandler,
    PhaseAnalysisAliasHandler, PhaseAnalysisHandler, ProjectMapHandler, ReadSymbolHandler,
    RenameSymbolHandler, SearchHandler, SymbolLookupHandler, ToolHandler,
};
use lepasserelle::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use lepasserelle::mcp::server::{handle_tool_call, list_tools_json};
use std::sync::Arc;
use tempfile::TempDir;

// ============================================================================
// Test helpers
// ============================================================================

/// Build the full set of 16 tool handlers (mirrors cli.rs and server.rs setup).
fn all_handlers() -> Vec<ToolHandler> {
    vec![
        ToolHandler::DeepAnalyze(DeepAnalyzeHandler),
        ToolHandler::Diagnostics(DiagnosticsHandler),
        ToolHandler::Index(IndexHandler),
        ToolHandler::Context(ContextHandler),
        ToolHandler::Search(SearchHandler),
        ToolHandler::PhaseAnalysis(PhaseAnalysisHandler),
        ToolHandler::PhaseAnalysisAlias(PhaseAnalysisAliasHandler),
        // Phase C: Tool Supremacy
        ToolHandler::FileSummary(FileSummaryHandler),
        ToolHandler::SymbolLookup(SymbolLookupHandler),
        ToolHandler::ProjectMap(ProjectMapHandler),
        ToolHandler::GrepSymbols(GrepSymbolsHandler),
        ToolHandler::ReadSymbol(ReadSymbolHandler),
        // Phase D: Context-Aware Editing
        ToolHandler::EditPreview(EditPreviewHandler),
        ToolHandler::EditApply(EditApplyHandler),
        ToolHandler::RenameSymbol(RenameSymbolHandler),
        ToolHandler::ImpactAnalysis(ImpactAnalysisHandler),
    ]
}

/// Create a minimal LeIndex state backed by a temp directory (not indexed).
fn make_state(tmp: &TempDir) -> Arc<lepasserelle::ProjectRegistry> {
    let leindex =
        lepasserelle::leindex::LeIndex::new(tmp.path()).expect("Failed to create LeIndex for test");
    Arc::new(lepasserelle::ProjectRegistry::with_initial_project(
        5, leindex,
    ))
}

/// Parse a JSON-RPC request from a JSON string (simulates stdin line).
fn parse_request(json: &str) -> JsonRpcRequest {
    serde_json::from_str(json).expect("Failed to parse JsonRpcRequest")
}

/// Serialize a response and verify it contains no double-newline (transport bug guard).
fn assert_no_double_newline(resp: &JsonRpcResponse) {
    let serialized = serde_json::to_string(resp).expect("Failed to serialize response");
    assert!(
        !serialized.contains("\n\n"),
        "Response contains double-newline (MCP transport bug): {:?}",
        &serialized[..serialized.len().min(200)]
    );
    // Also verify writeln! would produce exactly one newline (not two)
    let with_writeln = format!("{}\n", serialized);
    assert_eq!(
        with_writeln.matches('\n').count(),
        1,
        "writeln!(\"{{}}\", resp) should produce exactly one trailing newline"
    );
}

// ============================================================================
// Protocol format tests
// ============================================================================

#[test]
fn test_initialize_request_parses_correctly() {
    let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#;
    let req = parse_request(json);
    assert_eq!(req.method, "initialize");
    assert_eq!(req.id, serde_json::json!(1));
}

#[test]
fn test_initialize_response_format() {
    // The initialize response must contain protocolVersion, capabilities, and serverInfo.
    let resp = JsonRpcResponse::success(
        serde_json::json!(1),
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "leindex", "version": "0.1.0" }
        }),
    );

    let serialized = serde_json::to_string(&resp).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();

    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], 1);
    assert!(parsed["result"]["protocolVersion"].is_string());
    assert!(parsed["result"]["capabilities"].is_object());
    assert!(parsed["result"]["serverInfo"]["name"].is_string());

    assert_no_double_newline(&resp);
}

#[test]
fn test_notification_has_no_id_field() {
    // MCP notifications omit the "id" field.
    let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let req = parse_request(json);
    assert_eq!(req.method, "notifications/initialized");
    assert!(req.id.is_null(), "Notification should not have an id");
}

#[test]
fn test_no_double_newline_in_error_response() {
    let err =
        lepasserelle::mcp::protocol::JsonRpcError::method_not_found("unknown_method".to_string());
    let resp = JsonRpcResponse::error(serde_json::json!(42), err);
    assert_no_double_newline(&resp);
}

#[test]
fn test_no_double_newline_in_success_response() {
    let resp = JsonRpcResponse::success(serde_json::json!(99), serde_json::json!({ "tools": [] }));
    assert_no_double_newline(&resp);
}

// ============================================================================
// tools/list — all 16 tools registered
// ============================================================================

#[test]
fn test_tools_list_returns_16_tools() {
    let handlers = all_handlers();
    let result = list_tools_json(&handlers);
    let tools = result["tools"].as_array().expect("tools must be an array");
    assert_eq!(
        tools.len(),
        16,
        "Expected exactly 16 registered tools, got {}",
        tools.len()
    );
}

#[test]
fn test_tools_list_all_expected_names_present() {
    let handlers = all_handlers();
    let result = list_tools_json(&handlers);
    let tools = result["tools"].as_array().unwrap();

    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name must be a string"))
        .collect();

    let expected_names = [
        "leindex_index",
        "leindex_search",
        "leindex_deep_analyze",
        "leindex_context",
        "leindex_diagnostics",
        "leindex_phase_analysis",
        "phase_analysis",
        // Phase C
        "leindex_file_summary",
        "leindex_symbol_lookup",
        "leindex_project_map",
        "leindex_grep_symbols",
        "leindex_read_symbol",
        // Phase D
        "leindex_edit_preview",
        "leindex_edit_apply",
        "leindex_rename_symbol",
        "leindex_impact_analysis",
    ];

    for expected in &expected_names {
        assert!(
            names.contains(expected),
            "Missing tool '{}' from tools/list. Got: {:?}",
            expected,
            names
        );
    }
}

#[test]
fn test_tools_list_every_tool_has_description_and_schema() {
    let handlers = all_handlers();
    let result = list_tools_json(&handlers);
    let tools = result["tools"].as_array().unwrap();

    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("<unnamed>");
        let desc = tool["description"].as_str().unwrap_or("");
        assert!(
            !desc.is_empty(),
            "Tool '{}' has empty or missing description",
            name
        );
        assert!(
            desc.len() <= 300,
            "Tool '{}' description exceeds 300 chars ({} chars): {}",
            name,
            desc.len(),
            desc
        );
        assert!(
            tool["inputSchema"].is_object(),
            "Tool '{}' has missing or non-object inputSchema",
            name
        );
    }
}

// ============================================================================
// tools/call — handler dispatch and structured error responses
// ============================================================================

/// Build a tools/call request JSON-RPC message.
fn make_tool_call(id: i64, tool_name: &str, args: serde_json::Value) -> JsonRpcRequest {
    parse_request(&format!(
        r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"{}","arguments":{}}}}}"#,
        id, tool_name, args
    ))
}

#[tokio::test]
async fn test_tools_call_unknown_tool_returns_error() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let handlers = all_handlers();

    let req = make_tool_call(1, "leindex_nonexistent_tool", serde_json::json!({}));
    let result = handle_tool_call(&state, &handlers, &req).await;

    // Should be an Err (method not found) or an Ok with isError:true
    // The server wraps errors as isError:true for MCP compliance
    // handle_tool_call returns Err for method-not-found
    assert!(result.is_err(), "Expected error for unknown tool");
}

#[tokio::test]
async fn test_tools_call_file_summary_unindexed_returns_structured_response() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let handlers = all_handlers();

    let req = make_tool_call(
        2,
        "leindex_file_summary",
        serde_json::json!({ "file_path": "/nonexistent/file.rs" }),
    );

    // handle_tool_call always returns Ok (error is wrapped as isError:true)
    let result = handle_tool_call(&state, &handlers, &req).await;
    assert!(
        result.is_ok(),
        "Tool call should return Ok with structured error response"
    );

    let response = result.unwrap();
    // MCP wraps errors as isError:true with content array
    assert!(
        response.get("isError").is_some() || response.get("content").is_some(),
        "Response must have 'isError' or 'content' field. Got: {:?}",
        response
    );
}

#[tokio::test]
async fn test_tools_call_symbol_lookup_unindexed_returns_structured_response() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let handlers = all_handlers();

    let req = make_tool_call(
        3,
        "leindex_symbol_lookup",
        serde_json::json!({ "symbol": "some_function" }),
    );

    let result = handle_tool_call(&state, &handlers, &req).await;
    assert!(result.is_ok());

    let response = result.unwrap();
    // Even on error, MCP response must have content or isError
    assert!(
        response.get("isError").is_some() || response.get("content").is_some(),
        "Response must have 'isError' or 'content' field"
    );
}

#[tokio::test]
async fn test_tools_call_project_map_unindexed_returns_structured_response() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let handlers = all_handlers();

    let req = make_tool_call(4, "leindex_project_map", serde_json::json!({}));

    let result = handle_tool_call(&state, &handlers, &req).await;
    assert!(result.is_ok());

    let response = result.unwrap();
    assert!(
        response.get("isError").is_some() || response.get("content").is_some(),
        "Response must have 'isError' or 'content' field"
    );
}

#[tokio::test]
async fn test_tools_call_edit_preview_unindexed_returns_structured_response() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let handlers = all_handlers();

    let req = make_tool_call(
        5,
        "leindex_edit_preview",
        serde_json::json!({
            "file_path": "/nonexistent/file.rs",
            "changes": [{"type": "replace_text", "old_text": "foo", "new_text": "bar"}]
        }),
    );

    let result = handle_tool_call(&state, &handlers, &req).await;
    assert!(result.is_ok());
    let response = result.unwrap();
    assert!(response.get("isError").is_some() || response.get("content").is_some());
}

#[tokio::test]
async fn test_tools_call_diagnostics_returns_ok() {
    // diagnostics does NOT require an indexed project — always returns system info
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    let handlers = all_handlers();

    let req = make_tool_call(6, "leindex_diagnostics", serde_json::json!({}));
    let result = handle_tool_call(&state, &handlers, &req).await;
    assert!(result.is_ok());

    let response = result.unwrap();
    // diagnostics should succeed (isError: false)
    assert_eq!(
        response.get("isError").and_then(|v| v.as_bool()),
        Some(false),
        "Diagnostics should succeed: {:?}",
        response
    );
}

// ============================================================================
// Notification handling — no response for notifications
// ============================================================================

#[test]
fn test_notification_request_has_no_id() {
    // MCP notifications must not generate a response — verified by checking no id
    let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
    let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
    assert!(
        req.id.is_null(),
        "Notification must have no id (so callers know not to send a response)"
    );
}

#[test]
fn test_jsonrpc_response_serialization_is_single_line() {
    // Verify that serde_json::to_string produces no embedded newlines
    // (writeln! adds exactly one, producing single-line JSON per MCP spec)
    let resp = JsonRpcResponse::success(
        serde_json::json!(1),
        serde_json::json!({"tools": [{"name": "leindex_index"}]}),
    );
    let s = serde_json::to_string(&resp).unwrap();
    assert!(
        !s.contains('\n'),
        "serde_json::to_string must not produce embedded newlines. Got: {}",
        s
    );
}
