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
/// - id: Optional request identifier (absent for notifications)
/// - params: Optional parameters (can be omitted if not needed)
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC version string, must be "2.0"
    pub jsonrpc: String,
    /// Unique identifier for the request (None for notifications)
    #[serde(default)]
    pub id: Option<Value>,
    /// Method name to be invoked
    pub method: String,
    /// Parameters for the method call
    #[serde(default)]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Validate the request conforms to JSON-RPC 2.0
    pub fn validate(&self) -> Result<(), JsonRpcError> {
        if self.jsonrpc != JSONRPC_VERSION {
            return Err(JsonRpcError::invalid_request(format!(
                "Unsupported JSON-RPC version: {}",
                self.jsonrpc
            )));
        }
        Ok(())
    }

    /// Check if this is a notification (no id field)
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Extract tool call parameters from the request
    ///
    /// Expects params to contain: {name: string, arguments: object}
    pub fn extract_tool_call(&self) -> Result<ToolCallParams, JsonRpcError> {
        let params = self
            .params
            .as_ref()
            .ok_or_else(|| JsonRpcError::invalid_params("Missing params"))?;

        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| JsonRpcError::invalid_params("Missing or invalid 'name' field"))?;

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        Ok(ToolCallParams {
            name: name.to_string(),
            arguments,
        })
    }
}

/// JSON-RPC 2.0 Notification
///
/// A notification is a Request object without an "id" member.
/// The Server MUST NOT reply to a notification, including errors.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcNotification {
    /// JSON-RPC version string, must be "2.0"
    pub jsonrpc: String,
    /// Method name for the notification
    pub method: String,
    /// Parameters for the notification
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    /// Create a new notification
    pub fn new(method: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params: None,
        }
    }

    /// Create a notification with parameters
    pub fn with_params(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params: Some(params),
        }
    }

    /// Check if this is a known MCP notification type
    pub fn notification_type(&self) -> NotificationType {
        match self.method.as_str() {
            "notifications/initialized" => NotificationType::Initialized,
            "notifications/cancelled" => NotificationType::Cancelled,
            "notifications/progress" => NotificationType::Progress,
            "notifications/message" => NotificationType::Message,
            "notifications/resources/updated" => NotificationType::ResourcesUpdated,
            "notifications/resources/list_changed" => NotificationType::ResourcesListChanged,
            "notifications/tools/list_changed" => NotificationType::ToolsListChanged,
            "notifications/prompts/list_changed" => NotificationType::PromptsListChanged,
            "notifications/roots/list_changed" => NotificationType::RootsListChanged,
            _ => NotificationType::Unknown(self.method.clone()),
        }
    }
}

/// Known MCP notification types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationType {
    /// Sent by the client after initialization is complete
    Initialized,
    /// Sent to cancel a request
    Cancelled,
    /// Progress notification for long-running operations
    Progress,
    /// Logging message notification
    Message,
    /// Resource content updated notification
    ResourcesUpdated,
    /// Resource list changed notification
    ResourcesListChanged,
    /// Tool list changed notification
    ToolsListChanged,
    /// Prompt list changed notification
    PromptsListChanged,
    /// Roots list changed notification
    RootsListChanged,
    /// Unknown notification type
    Unknown(String),
}

impl std::fmt::Display for NotificationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotificationType::Initialized => write!(f, "notifications/initialized"),
            NotificationType::Cancelled => write!(f, "notifications/cancelled"),
            NotificationType::Progress => write!(f, "notifications/progress"),
            NotificationType::Message => write!(f, "notifications/message"),
            NotificationType::ResourcesUpdated => write!(f, "notifications/resources/updated"),
            NotificationType::ResourcesListChanged => {
                write!(f, "notifications/resources/list_changed")
            }
            NotificationType::ToolsListChanged => write!(f, "notifications/tools/list_changed"),
            NotificationType::PromptsListChanged => write!(f, "notifications/prompts/list_changed"),
            NotificationType::RootsListChanged => write!(f, "notifications/roots/list_changed"),
            NotificationType::Unknown(method) => write!(f, "{}", method),
        }
    }
}

/// JSON-RPC 2.0 Message (Request or Notification)
///
/// This enum allows parsing either a request (with id) or notification (without id)
/// from incoming JSON data.
#[derive(Debug, Clone)]
pub enum JsonRpcMessage {
    /// A request with an id (expects response)
    Request(JsonRpcRequest),
    /// A notification without id (no response expected)
    Notification(JsonRpcNotification),
}

impl JsonRpcMessage {
    /// Parse a JSON string into a JsonRpcMessage
    pub fn from_json(json: &str) -> Result<Self, JsonRpcError> {
        let raw: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| JsonRpcError::parse_error(format!("Invalid JSON: {}", e)))?;

        if !raw.is_object() {
            return Err(JsonRpcError::invalid_request(
                "Message must be a JSON object",
            ));
        }

        let obj = raw.as_object().unwrap();

        if let Some(jsonrpc) = obj.get("jsonrpc") {
            if jsonrpc.as_str() != Some(JSONRPC_VERSION) {
                return Err(JsonRpcError::invalid_request(format!(
                    "Unsupported JSON-RPC version: {:?}",
                    jsonrpc
                )));
            }
        } else {
            return Err(JsonRpcError::invalid_request("Missing jsonrpc field"));
        }

        if !obj.contains_key("method") {
            return Err(JsonRpcError::invalid_request("Missing method field"));
        }

        if obj.contains_key("id") {
            let request: JsonRpcRequest = serde_json::from_value(raw)
                .map_err(|e| JsonRpcError::invalid_request(format!("Invalid request: {}", e)))?;
            Ok(JsonRpcMessage::Request(request))
        } else {
            let notification: JsonRpcNotification = serde_json::from_value(raw).map_err(|e| {
                JsonRpcError::invalid_request(format!("Invalid notification: {}", e))
            })?;
            Ok(JsonRpcMessage::Notification(notification))
        }
    }

    /// Check if this message is a notification
    pub fn is_notification(&self) -> bool {
        matches!(self, JsonRpcMessage::Notification(_))
    }

    /// Check if this message is a request
    pub fn is_request(&self) -> bool {
        matches!(self, JsonRpcMessage::Request(_))
    }

    /// Get the method name from the message
    pub fn method(&self) -> &str {
        match self {
            JsonRpcMessage::Request(req) => &req.method,
            JsonRpcMessage::Notification(notif) => &notif.method,
        }
    }

    /// Get the id if this is a request
    pub fn id(&self) -> Option<&Value> {
        match self {
            JsonRpcMessage::Request(req) => req.id.as_ref(),
            JsonRpcMessage::Notification(_) => None,
        }
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

    /// Create a successful response with optional id
    pub fn success_opt(id: Option<Value>, result: Value) -> Self {
        Self::success(id.unwrap_or(Value::Null), result)
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

    /// Create an error response with optional id
    pub fn error_opt(id: Option<Value>, error: JsonRpcError) -> Self {
        Self::error(id.unwrap_or(Value::Null), error)
    }

    /// Create a response from a Result
    pub fn from_result(id: Value, result: Result<Value, JsonRpcError>) -> Self {
        match result {
            Ok(value) => Self::success(id, value),
            Err(err) => Self::error(id, err),
        }
    }

    /// Create a response from a Result with optional id
    pub fn from_result_opt(id: Option<Value>, result: Result<Value, JsonRpcError>) -> Self {
        Self::from_result(id.unwrap_or(Value::Null), result)
    }
}

/// JSON-RPC 2.0 Error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Error code indicating the type of failure
    pub code: i32,
    /// Human-readable description of the error
    pub message: String,
    /// Optional additional error details
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    /// Create a new JSON-RPC error with the given code and message
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Create a new JSON-RPC error with additional data
    pub fn with_data(code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    /// Create a parse error (code -32700)
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self::new(error_codes::PARSE_ERROR, msg)
    }

    /// Create an invalid request error (code -32600)
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_REQUEST, msg)
    }

    /// Create a method not found error (code -32601)
    pub fn method_not_found(method: String) -> Self {
        Self::with_data(
            error_codes::METHOD_NOT_FOUND,
            "Method not found",
            serde_json::json!({ "method": method }),
        )
    }

    /// Create an invalid params error (code -32602)
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_PARAMS, msg)
    }

    /// Create an invalid params error with a suggestion
    pub fn invalid_params_with_suggestion(
        msg: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self::with_data(
            error_codes::INVALID_PARAMS,
            msg,
            serde_json::json!({ "suggestion": suggestion.into() }),
        )
    }

    /// Create an internal error (code -32603)
    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INTERNAL_ERROR, msg)
    }

    /// Create a project not found error
    pub fn project_not_found(project: String) -> Self {
        Self::with_data(
            error_codes::PROJECT_NOT_FOUND,
            "Project not found",
            serde_json::json!({ "project": project }),
        )
    }

    /// Create a project not indexed error
    pub fn project_not_indexed(project: String) -> Self {
        Self::with_data(
            error_codes::PROJECT_NOT_INDEXED,
            "Project not indexed",
            serde_json::json!({ "project": project, "suggestion": "Run leindex_index first" }),
        )
    }

    /// Create an indexing failed error
    pub fn indexing_failed(msg: impl Into<String>) -> Self {
        Self::new(error_codes::INDEXING_FAILED, msg)
    }

    /// Create a search failed error
    pub fn search_failed(msg: impl Into<String>) -> Self {
        Self::new(error_codes::SEARCH_FAILED, msg)
    }

    /// Create a context expansion failed error
    pub fn context_expansion_failed(msg: impl Into<String>) -> Self {
        Self::new(error_codes::CONTEXT_EXPANSION_FAILED, msg)
    }

    /// Create a memory limit exceeded error
    pub fn memory_limit_exceeded() -> Self {
        Self::with_data(
            error_codes::MEMORY_LIMIT_EXCEEDED,
            "Memory limit exceeded",
            serde_json::json!({ "suggestion": "Try a smaller operation or increase memory budget" }),
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
    #[serde(default)]
    pub arguments: Value,
}

/// Progress event for long-running operations
///
/// These events are sent via SSE during indexing to provide
/// real-time feedback without arbitrary timeouts.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    /// Event type: "progress" or "complete"
    #[serde(rename = "type")]
    pub event_type: String,

    /// Current stage of indexing
    pub stage: String,

    /// Current item count
    pub current: usize,

    /// Total items (0 if unknown)
    pub total: usize,

    /// Optional message with additional details

    #[serde(default, skip_serializing_if = "Option::is_none")]


    pub message: Option<String>,

    /// Timestamp in milliseconds
    pub timestamp_ms: u64,
}

impl ProgressEvent {
    /// Create a new progress event
    pub fn progress(
        stage: impl Into<String>,
        current: usize,
        total: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            event_type: "progress".to_string(),
            stage: stage.into(),
            current,
            total,
            message: Some(message.into()),
            timestamp_ms: {
                let duration = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap();
                duration.as_millis() as u64
            },
        }
    }

    /// Create a completion event

    pub fn complete(stage: impl Into<String>, message: impl Into<String>) -> Self {

    pub fn complete(
        stage: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {

        Self {
            event_type: "complete".to_string(),
            stage: stage.into(),
            current: 0,
            total: 0,
            message: Some(message.into()),
            timestamp_ms: {
                let duration = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap();
                duration.as_millis() as u64
            },
        }
    }

    /// Create an error event
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            event_type: "error".to_string(),
            stage: "error".into(),
            current: 0,
            total: 0,
            message: Some(message.into()),
            timestamp_ms: {
                let duration = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap();
                duration.as_millis() as u64
            },
        }
    }
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
        assert!(!req.is_notification());
        assert_eq!(req.id, Some(serde_json::json!(1)));
    }

    #[test]
    fn test_jsonrpc_request_notification() {
        let json = r#"{
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }"#;

        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert!(req.validate().is_ok());
        assert!(req.is_notification());
        assert!(req.id.is_none());
    }

    #[test]
    fn test_jsonrpc_notification_parsing() {
        let json = r#"{
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }"#;

        let notif: JsonRpcNotification = serde_json::from_str(json).unwrap();
        assert_eq!(notif.method, "notifications/initialized");
        assert_eq!(notif.notification_type(), NotificationType::Initialized);
    }

    #[test]
    fn test_jsonrpc_message_request() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {"name": "test"}
        }"#;

        let msg = JsonRpcMessage::from_json(json).unwrap();
        assert!(msg.is_request());
        assert!(!msg.is_notification());
        assert_eq!(msg.method(), "tools/call");
        assert_eq!(msg.id(), Some(&serde_json::json!(42)));
    }

    #[test]
    fn test_jsonrpc_message_notification() {
        let json = r#"{
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }"#;

        let msg = JsonRpcMessage::from_json(json).unwrap();
        assert!(msg.is_notification());
        assert!(!msg.is_request());
        assert_eq!(msg.method(), "notifications/initialized");
        assert!(msg.id().is_none());
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
    fn test_jsonrpc_message_invalid_version() {
        let json = r#"{
            "jsonrpc": "1.0",
            "id": 1,
            "method": "tools/call"
        }"#;

        let result = JsonRpcMessage::from_json(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_jsonrpc_message_missing_method() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1
        }"#;

        let result = JsonRpcMessage::from_json(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_jsonrpc_response_success() {
        let response =
            JsonRpcResponse::success(serde_json::json!(1), serde_json::json!({"result": "ok"}));

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
    fn test_jsonrpc_response_with_null_id() {
        let response =
            JsonRpcResponse::success(serde_json::Value::Null, serde_json::json!({"result": "ok"}));

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"id\":null"));
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

    #[test]
    fn test_notification_types() {
        assert_eq!(
            JsonRpcNotification::new("notifications/initialized").notification_type(),
            NotificationType::Initialized
        );
        assert_eq!(
            JsonRpcNotification::new("notifications/cancelled").notification_type(),
            NotificationType::Cancelled
        );
        assert_eq!(
            JsonRpcNotification::new("notifications/progress").notification_type(),
            NotificationType::Progress
        );
        assert_eq!(
            JsonRpcNotification::new("notifications/message").notification_type(),
            NotificationType::Message
        );
        assert_eq!(
            JsonRpcNotification::new("notifications/resources/updated").notification_type(),
            NotificationType::ResourcesUpdated
        );
        assert_eq!(
            JsonRpcNotification::new("unknown/notification").notification_type(),
            NotificationType::Unknown("unknown/notification".to_string())
        );
    }

    #[test]
    fn test_backwards_compatibility() {
        // Ensure requests with id still work as before
        let json = r#"{
            "jsonrpc": "2.0",
            "id": "test-id-123",
            "method": "initialize",
            "params": {}
        }"#;

        let msg = JsonRpcMessage::from_json(json).unwrap();
        assert!(msg.is_request());

        if let JsonRpcMessage::Request(req) = msg {
            assert_eq!(req.id, Some(serde_json::json!("test-id-123")));
            assert!(!req.is_notification());
        } else {
            panic!("Expected request");
        }
    }
}
