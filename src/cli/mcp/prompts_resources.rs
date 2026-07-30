use super::{JsonRpcError, JsonRpcRequest};
use serde::Serialize;
use serde_json::Value;

/// List prompts as JSON
pub fn list_prompts_json() -> Value {
    let prompts = get_prompts();
    serde_json::json!({ "prompts": prompts })
}

/// Handle prompts/get request
pub fn handle_prompt_get(req: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
    let params = req
        .params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("Missing params for prompts/get"))?;

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("Missing or invalid 'name' field"))?;

    let arguments = params.get("arguments").cloned();
    let messages = get_prompt(name, arguments)?;

    Ok(serde_json::json!({
        "description": format!("Prompt: {}", name),
        "messages": messages
    }))
}

/// List resources as JSON
pub fn list_resources_json() -> Value {
    let resources = get_resources();
    serde_json::json!({ "resources": resources })
}

/// Handle resources/read request
pub fn handle_resource_read(req: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
    let params = req
        .params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("Missing params for resources/read"))?;

    let uri = params
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("Missing or invalid 'uri' field"))?;

    let content = get_resource(uri)?;
    Ok(serde_json::json!({ "contents": [content] }))
}

// ============================================================================
// MCP Prompts Implementation
// ============================================================================

/// A prompt definition for the MCP prompts capability
#[derive(Debug, Clone, Serialize)]
pub struct Prompt {
    /// Unique identifier for the prompt
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Optional arguments the prompt accepts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

/// A prompt argument definition
#[derive(Debug, Clone, Serialize)]
pub struct PromptArgument {
    /// Argument name
    pub name: String,
    /// Argument description
    pub description: String,
    /// Whether the argument is required
    pub required: bool,
}

/// A prompt message (content)
#[derive(Debug, Clone, Serialize)]
pub struct PromptMessage {
    /// Role of the message sender
    pub role: String,
    /// Content of the message
    pub content: PromptContent,
}

/// Content of a prompt message
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum PromptContent {
    /// Text content
    #[serde(rename = "text")]
    Text {
        /// The text content of the message
        text: String,
    },
}

/// Get the list of available prompts
pub fn get_prompts() -> Vec<Prompt> {
    vec![
        Prompt {
            name: "quickstart".to_string(),
            description: "Quick introduction to using LeIndex effectively".to_string(),
            arguments: None,
        },
        Prompt {
            name: "investigation_workflow".to_string(),
            description: "Step-by-step guide for investigating code with LeIndex".to_string(),
            arguments: Some(vec![PromptArgument {
                name: "query".to_string(),
                description: "What you're trying to find or understand".to_string(),
                required: true,
            }]),
        },
    ]
}

/// Get a specific prompt by name
pub fn get_prompt(
    name: &str,
    arguments: Option<Value>,
) -> Result<Vec<PromptMessage>, JsonRpcError> {
    match name {
        "quickstart" => Ok(vec![
            PromptMessage {
                role: "user".to_string(),
                content: PromptContent::Text {
                    text: "Welcome to LeIndex! Here's how to get started:\n\n1. **Indexing**: First, index your project with `leindex.index`\n2. **Searching**: Use `leindex.search` for semantic code search\n3. **Analysis**: Use `leindex.deep-analyze` for comprehensive code analysis\n4. **Context**: Use `leindex.context` to expand around specific symbols\n\nPro tip: LeIndex auto-indexes on first use, so you can start searching immediately!".to_string(),
                },
            },
        ]),
        "investigation_workflow" => {
            let query = arguments
                .as_ref()
                .and_then(|a| a.get("query"))
                .and_then(|q| q.as_str())
                .unwrap_or("your code investigation");

            Ok(vec![
                PromptMessage {
                    role: "user".to_string(),
                    content: PromptContent::Text {
                        text: format!(
                            "Let me help you investigate: {}\n\nHere's the recommended workflow:\n\n1. **Start broad**: Use `leindex.search` with a natural language query like '{}'\n2. **Find entry points**: Look for the most relevant symbols in the results\n3. **Deep dive**: Use `leindex.deep-analyze` on the most relevant symbol\n4. **Expand context**: Use `leindex.context` to see how the symbol is used\n5. **Navigate**: Follow symbol references with `leindex.read-symbol`\n\nWould you like me to help you with any specific step?",
                            query, query
                        ),
                    },
                },
            ])
        }
        _ => Err(JsonRpcError::method_not_found(format!("Prompt '{}' not found", name))),
    }
}

// ============================================================================
// MCP Resources Implementation
// ============================================================================

/// A resource definition for the MCP resources capability
#[derive(Debug, Clone, Serialize)]
pub struct Resource {
    /// Unique URI for the resource
    pub uri: String,
    /// Human-readable name
    pub name: String,
    /// MIME type of the resource
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Resource description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Resource content
#[derive(Debug, Clone, Serialize)]
pub struct ResourceContent {
    /// Resource URI
    pub uri: String,
    /// MIME type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Text content (if text resource)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Binary content (if binary resource)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// Get the list of available resources
pub fn get_resources() -> Vec<Resource> {
    vec![
        Resource {
            uri: "leindex://docs/quickstart".to_string(),
            name: "LeIndex Quickstart Guide".to_string(),
            mime_type: Some("text/markdown".to_string()),
            description: Some("Quick start guide for using LeIndex".to_string()),
        },
        Resource {
            uri: "leindex://docs/server-config".to_string(),
            name: "Server Configuration".to_string(),
            mime_type: Some("text/markdown".to_string()),
            description: Some("Configuration options for LeIndex server".to_string()),
        },
    ]
}

/// Get a specific resource by URI
pub fn get_resource(uri: &str) -> Result<ResourceContent, JsonRpcError> {
    match uri {
        "leindex://docs/quickstart" => Ok(ResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: Some(QUICKSTART_GUIDE.to_string()),
            blob: None,
        }),
        "leindex://docs/server-config" => Ok(ResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: Some(SERVER_CONFIG_GUIDE.to_string()),
            blob: None,
        }),
        _ => Err(JsonRpcError::method_not_found(format!(
            "Resource '{}' not found",
            uri
        ))),
    }
}

/// Quickstart guide content
const QUICKSTART_GUIDE: &str = r#"# LeIndex Quickstart Guide

## Installation

```bash
cargo install leindex
```

## Basic Usage

### 1. Index a Project

```bash
leindex index /path/to/project
```

Or use the MCP tool:
```json
{
  "name": "leindex.index",
  "arguments": {
    "project_path": "/path/to/project"
  }
}
```

### 2. Search Code

```bash
leindex search "how is authentication handled"
```

Or use the MCP tool:
```json
{
  "name": "leindex.search",
  "arguments": {
    "query": "how is authentication handled",
    "limit": 10
  }
}
```

### 3. Deep Analysis

```bash
leindex analyze --symbol "User::authenticate"
```

Or use the MCP tool:
```json
{
  "name": "leindex.deep-analyze",
  "arguments": {
    "query": "User::authenticate"
  }
}
```

## Available Tools

- `leindex.search` - Semantic code search
- `leindex.deep-analyze` - Comprehensive code analysis
- `leindex.context` - Expand symbol context
- `leindex.grep-symbols` - Search symbols by name
- `leindex.read-file` - Read file with PDG annotations
- `leindex.file-summary` - Get file structural summary

## Environment Variables

- `LEINDEX_HOME` - Storage directory (default: ~/.leindex)
- `LEINDEX_PORT` - Server port (default: 47268)
"#;

/// Server configuration guide content
const SERVER_CONFIG_GUIDE: &str = r#"# LeIndex Server Configuration

## Configuration Options

The LeIndex server can be configured via:

1. Command-line arguments
2. Environment variables
3. Configuration file (config.yaml)

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `LEINDEX_HOME` | Storage/index directory | `~/.leindex` |
| `LEINDEX_PORT` | HTTP server port | `47268` |
| `LEINDEX_HOST` | HTTP server host | `127.0.0.1` |

## MCP Server Mode

Start the MCP server:

```bash
leindex mcp --stdio
```

For HTTP transport:

```bash
leindex serve
```

## Feature Flags

When building from source:

- `full` - All features (default)
- `minimal` - Parse and search only
- `cli` - CLI + MCP server
- `server` - HTTP server only

## Multi-Project Support

The server supports multiple concurrent projects:

```bash
leindex serve --max-projects 10
```

Default maximum: 5 projects.
"#;
