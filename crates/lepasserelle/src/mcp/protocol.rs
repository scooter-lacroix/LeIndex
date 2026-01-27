// MCP JSON-RPC Protocol Types
//
// This module defines the JSON-RPC 2.0 protocol types used by the MCP server.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 specification version
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC 2.0 Request
///
/// Per the JSON-RPC 2.0 spec, a request must have:
/// - jsonrpc: "2.0"
/// - method: A string containing the method name to invoke
/// - id: Request identifier (can be null for notifications)
/// - params: Optional parameters (can be omitted if not needed)
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC version string, must be "2.0"
    pub jsonrpc: String,
    /// Unique identifier for the request
    pub id: Value,
    /// Method name to be invoked
    pub method: String,
    /// Parameters for the method call
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Validate the request conforms to JSON-RPC 2.0
    pub fn validate(&self) -> Result<(), JsonRpcError> {
        if self.jsonrpc != JSONRPC_VERSION {
            return Err(JsonRpcError::invalid_request(
                format!("Unsupported JSON-RPC version: {}", self.jsonrpc)
            ));
        }
        Ok(())
    }

    /// Extract tool call parameters from the request
    ///
    /// Expects params to contain: {name: string, arguments: object}
    pub fn extract_tool_call(&self) -> Result<ToolCallParams, JsonRpcError> {
        let params = self.params.as_ref()
            .ok_or_else(|| JsonRpcError::invalid_params("Missing params"))?;

        let name = params.get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| JsonRpcError::invalid_params("Missing or invalid 'name' field"))?;

        let arguments = params.get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        Ok(ToolCallParams {
            name: name.to_string(),
            arguments,
        })
    }
}

/// JSON-RPC 2.0 Response
///
/// A response can contain either a result or an error, but not both.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcResponse {
    /// JSON-RPC version string, must be "2.0"
    pub jsonrpc: String,
    /// Identifier matching the original request
    pub id: Value,
    /// Result of the method call, if successful
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error information, if the method call failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Create a successful response
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response
    pub fn error(id: Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// Create a response from a Result
    pub fn from_result(id: Value, result: Result<Value, JsonRpcError>) -> Self {
        match result {
            Ok(value) => Self::success(id, value),
            Err(err) => Self::error(id, err),
        }
    }
}

/// JSON-RPC 2.0 Error
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcError {
    /// Error code indicating the type of failure
    pub code: i32,
    /// Human-readable description of the error
    pub message: String,
    /// Optional additional error details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self::new(error_codes::PARSE_ERROR, msg)
    }

    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_REQUEST, msg)
    }

    pub fn method_not_found(method: String) -> Self {
        Self::with_data(
            error_codes::METHOD_NOT_FOUND,
            "Method not found",
            serde_json::json!({ "method": method })
        )
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_PARAMS, msg)
    }

    pub fn invalid_params_with_suggestion(msg: impl Into<String>, suggestion: impl Into<String>) -> Self {
        Self::with_data(
            error_codes::INVALID_PARAMS,
            msg,
            serde_json::json!({ "suggestion": suggestion.into() })
        )
    }

    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INTERNAL_ERROR, msg)
    }

    pub fn project_not_found(project: String) -> Self {
        Self::with_data(
            error_codes::PROJECT_NOT_FOUND,
            "Project not found",
            serde_json::json!({ "project": project })
        )
    }

    pub fn project_not_indexed(project: String) -> Self {
        Self::with_data(
            error_codes::PROJECT_NOT_INDEXED,
            "Project not indexed",
            serde_json::json!({ "project": project, "suggestion": "Run leindex_index first" })
        )
    }

    pub fn indexing_failed(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INDEXING_FAILED, msg)
    }

    pub fn search_failed(msg: impl Into<String>) -> Self {
        Self::new(error_codes::SEARCH_FAILED, msg)
    }

    pub fn context_expansion_failed(msg: impl Into<String>) -> Self {
        Self::new(error_codes::CONTEXT_EXPANSION_FAILED, msg)
    }

    pub fn memory_limit_exceeded() -> Self {
        Self::with_data(
            error_codes::MEMORY_LIMIT_EXCEEDED,
            "Memory limit exceeded",
            serde_json::json!({ "suggestion": "Try a smaller operation or increase memory budget" })
        )
    }
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for JsonRpcError {}

/// Tool call parameters extracted from JSON-RPC request
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallParams {
    /// Name of the tool to be called
    pub name: String,
    /// Arguments for the tool call
    pub arguments: Value,
}

/// JSON-RPC Error Codes
///
/// Standard JSON-RPC 2.0 error codes are in the range -32700 to -32603.
/// Server-defined errors should be in the range -32099 to -32000.
pub mod error_codes {
    /// Invalid JSON was received by the server
    pub const PARSE_ERROR: i32 = -32700;

    /// The JSON sent is not a valid Request object
    pub const INVALID_REQUEST: i32 = -32600;

    /// The method does not exist / is not available
    pub const METHOD_NOT_FOUND: i32 = -32601;

    /// Invalid method parameter(s)
    pub const INVALID_PARAMS: i32 = -32602;

    /// Internal JSON-RPC error
    pub const INTERNAL_ERROR: i32 = -32603;

    // MCP-specific error codes (-32000 to -32099)

    /// Project directory not found
    pub const PROJECT_NOT_FOUND: i32 = -32001;

    /// Project exists but has not been indexed
    pub const PROJECT_NOT_INDEXED: i32 = -32002;

    /// Project indexing failed
    pub const INDEXING_FAILED: i32 = -32003;

    /// Search operation failed
    pub const SEARCH_FAILED: i32 = -32004;

    /// Context expansion failed
    pub const CONTEXT_EXPANSION_FAILED: i32 = -32005;

    /// Memory limit exceeded during operation
    pub const MEMORY_LIMIT_EXCEEDED: i32 = -32006;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jsonrpc_request_valid() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "test", "arguments": {}}
        }"#;

        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_ok());
        assert_eq!(req.method, "tools/call");
    }

    #[test]
    fn test_jsonrpc_request_invalid_version() {
        let json = r#"{
            "jsonrpc": "1.0",
            "id": 1,
            "method": "tools/call"
        }"#;

        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_jsonrpc_response_success() {
        let response = JsonRpcResponse::success(
            serde_json::json!(1),
            serde_json::json!({"result": "ok"})
        );

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_jsonrpc_response_error() {
        let error = JsonRpcError::invalid_params("Missing required field");
        let response = JsonRpcResponse::error(serde_json::json!(1), error);

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"error\""));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn test_extract_tool_call() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "leindex_search",
                "arguments": {"query": "test"}
            }
        }"#;

        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        let tool_call = req.extract_tool_call().unwrap();

        assert_eq!(tool_call.name, "leindex_search");
        assert_eq!(tool_call.arguments["query"], "test");
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
        assert_eq!(error_codes::PROJECT_NOT_FOUND, -32001);
        assert_eq!(error_codes::PROJECT_NOT_INDEXED, -32002);
    }
}
