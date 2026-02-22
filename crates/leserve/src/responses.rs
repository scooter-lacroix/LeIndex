//! API response types matching frontend contract

use serde::{Deserialize, Serialize};

/// Generic API response wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    /// Response data
    pub data: T,

    /// Success flag
    pub success: bool,

    /// Optional error message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Default> ApiResponse<T> {
    /// Create a success response
    pub fn success(data: T) -> Self {
        Self {
            data,
            success: true,
            error: None,
        }
    }

    /// Create an error response
    pub fn error(message: String) -> Self {
        Self {
            data: T::default(),
            success: false,
            error: Some(message),
        }
    }
}

/// Generic empty response for POST/DELETE endpoints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyResponse {}

/// Codebase information matching frontend Codebase interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseResponse {
    /// Unique project ID (e.g., "leindex_a3f7d9e2_0")
    pub id: String,

    /// Unique project identifier
    pub unique_project_id: String,

    /// Base name of project
    pub base_name: String,

    /// BLAKE3 path hash
    pub path_hash: String,

    /// Instance number
    pub instance: u32,

    /// Project path
    pub project_path: String,

    /// Display name
    pub display_name: String,

    /// Project type/language
    pub project_type: String,

    /// Last indexed timestamp
    pub last_indexed: String,

    /// File count
    pub file_count: i64,

    /// Node count (symbols)
    pub node_count: i64,

    /// Edge count (dependencies)
    pub edge_count: i64,

    /// Validity flag
    pub is_valid: bool,

    /// Clone flag
    pub is_clone: bool,

    /// Original project ID if clone
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
}

/// Response for codebase list endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseListResponse {
    /// List of all codebases
    pub codebases: Vec<CodebaseResponse>,

    /// Total count
    pub total: usize,
}

impl CodebaseListResponse {
    /// Create empty response
    pub fn empty() -> Self {
        Self {
            codebases: Vec::new(),
            total: 0,
        }
    }
}

/// Response for single codebase endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseDetailResponse {
    /// Codebase data
    pub codebase: CodebaseResponse,
}

/// Sync report matching frontend SyncReport interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReportResponse {
    /// Newly discovered files
    pub newly_discovered: usize,

    /// Updated files
    pub updated: usize,

    /// Invalidated files
    pub invalidated: usize,

    /// Missing files
    pub missing: usize,

    /// Unchanged files
    pub unchanged: usize,

    /// Errors encountered
    pub errors: usize,
}

impl Default for SyncReportResponse {
    fn default() -> Self {
        Self {
            newly_discovered: 0,
            updated: 0,
            invalidated: 0,
            missing: 0,
            unchanged: 0,
            errors: 0,
        }
    }
}

/// File tree node for file listing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    /// File/directory name
    pub name: String,

    /// Full path
    pub path: String,

    /// Type: file or directory
    #[serde(rename = "type")]
    pub node_type: String,

    /// File size in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,

    /// Last modified timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,

    /// Child nodes (if directory)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FileNode>,
}

impl FileNode {
    /// Create a file node
    pub fn file(name: String, path: String, size: u64) -> Self {
        Self {
            name,
            path,
            node_type: "file".to_string(),
            size: Some(size),
            last_modified: None,
            children: Vec::new(),
        }
    }

    /// Create a directory node
    pub fn directory(name: String, path: String) -> Self {
        Self {
            name,
            path,
            node_type: "directory".to_string(),
            size: None,
            last_modified: None,
            children: Vec::new(),
        }
    }

    /// Add a child node
    pub fn add_child(&mut self, child: FileNode) {
        self.children.push(child);
    }
}

/// File tree response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeResponse {
    /// Root file nodes
    pub tree: Vec<FileNode>,
}

/// File content response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContentResponse {
    /// File path
    pub path: String,

    /// File content
    pub content: String,

    /// File encoding
    pub encoding: String,

    /// Line count
    pub line_count: usize,

    /// Byte size
    pub size: usize,
}

/// Graph node matching frontend GraphNode interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeResponse {
    /// Unique node ID
    pub id: String,

    /// Display name
    pub name: String,

    /// Node type (function, class, method, variable, module)
    #[serde(rename = "type")]
    pub node_type: String,

    /// Size/value for visualization
    pub val: u32,

    /// Node color (computed from type)
    pub color: String,

    /// Programming language
    pub language: String,

    /// Complexity score
    pub complexity: u32,

    /// File path
    pub file_path: String,

    /// Byte range in source
    pub byte_range: [usize; 2],

    /// Optional X position
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,

    /// Optional Y position
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
}

/// Graph link matching frontend GraphLink interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphLinkResponse {
    /// Source node ID
    pub source: String,

    /// Target node ID
    pub target: String,

    /// Link type (call, data_dependency, inheritance, import)
    #[serde(rename = "type")]
    pub link_type: String,

    /// Link value/thickness
    pub value: u32,
}

/// Graph data response matching frontend GraphData interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphDataResponse {
    /// All nodes in graph
    pub nodes: Vec<GraphNodeResponse>,

    /// All edges in graph
    pub links: Vec<GraphLinkResponse>,
}

/// Score breakdown matching frontend Score interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResponse {
    /// Semantic similarity score (0.0-1.0)
    pub semantic: f32,

    /// Text match score (0.0-1.0)
    pub text_match: f32,

    /// Structural relevance score (0.0-1.0)
    pub structural: f32,

    /// Overall combined score (0.0-1.0)
    pub overall: f32,
}

/// Search result matching frontend SearchResult interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultResponse {
    /// Result rank (1-based)
    pub rank: usize,

    /// Node ID
    pub node_id: String,

    /// File path
    pub file_path: String,

    /// Symbol name
    pub symbol_name: String,

    /// Programming language
    pub language: String,

    /// Relevance scores
    pub score: ScoreResponse,

    /// Optional context snippet
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    /// Byte range in source
    pub byte_range: [usize; 2],
}

/// Search results response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultsResponse {
    /// Search results
    pub results: Vec<SearchResultResponse>,
}

impl SearchResultsResponse {
    /// Create empty response
    pub fn empty() -> Self {
        Self {
            results: Vec::new(),
        }
    }
}

/// Graph node detail response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeDetailResponse {
    /// Node details
    pub node: GraphNodeResponse,

    /// Neighboring nodes
    pub neighbors: Vec<GraphNodeResponse>,
}

/// Phantom data marker for empty responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhantomData {}

impl Default for PhantomData {
    fn default() -> Self {
        Self {}
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_response_success() {
        let response = ApiResponse::<String>::success("test data".to_string());
        assert!(response.success);
        assert_eq!(response.data, "test data");
        assert!(response.error.is_none());
    }

    #[test]
    fn test_api_response_error() {
        let response = ApiResponse::<String>::error("error message".to_string());
        assert!(!response.success);
        assert_eq!(response.error, Some("error message".to_string()));
    }

    #[test]
    fn test_codebase_list_response_empty() {
        let response = CodebaseListResponse::empty();
        assert_eq!(response.codebases.len(), 0);
        assert_eq!(response.total, 0);
    }

    #[test]
    fn test_file_node_file() {
        let node = FileNode::file("test.rs".to_string(), "/path/to/test.rs".to_string(), 1024);
        assert_eq!(node.name, "test.rs");
        assert_eq!(node.node_type, "file");
        assert_eq!(node.size, Some(1024));
        assert!(node.children.is_empty());
    }

    #[test]
    fn test_file_node_directory() {
        let node = FileNode::directory("src".to_string(), "/path/to/src".to_string());
        assert_eq!(node.name, "src");
        assert_eq!(node.node_type, "directory");
        assert!(node.size.is_none());
    }

    #[test]
    fn test_file_node_add_child() {
        let mut parent = FileNode::directory("src".to_string(), "/src".to_string());
        let child = FileNode::file("lib.rs".to_string(), "/src/lib.rs".to_string(), 100);
        parent.add_child(child);
        assert_eq!(parent.children.len(), 1);
        assert_eq!(parent.children[0].name, "lib.rs");
    }

    #[test]
    fn test_sync_report_default() {
        let report = SyncReportResponse::default();
        assert_eq!(report.newly_discovered, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(report.invalidated, 0);
        assert_eq!(report.missing, 0);
        assert_eq!(report.unchanged, 0);
        assert_eq!(report.errors, 0);
    }

    #[test]
    fn test_graph_data_empty() {
        let response = GraphDataResponse {
            nodes: vec![],
            links: vec![],
        };
        assert!(response.nodes.is_empty());
        assert!(response.links.is_empty());
    }

    #[test]
    fn test_score_response() {
        let score = ScoreResponse {
            semantic: 0.95,
            text_match: 0.8,
            structural: 0.7,
            overall: 0.85,
        };
        assert_eq!(score.semantic, 0.95);
        assert_eq!(score.text_match, 0.8);
        assert_eq!(score.structural, 0.7);
        assert_eq!(score.overall, 0.85);
    }


    #[test]
    fn test_search_results_empty() {
        let response = SearchResultsResponse::empty();
        assert!(response.results.is_empty());
    }
}
