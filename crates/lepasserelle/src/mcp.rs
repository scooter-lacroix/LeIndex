// Unified MCP tool implementation

use serde::{Deserialize, Serialize};

/// LeIndex deep analyze MCP tool
///
/// Provides unified interface for semantic search and graph expansion.
#[derive(Debug, Clone)]
pub struct LeIndexDeepAnalyze {
    /// Project path
    project_path: String,

    /// Token budget for context expansion
    token_budget: usize,

    /// Whether to use semantic search
    use_semantic: bool,
}

impl LeIndexDeepAnalyze {
    /// Create a new MCP tool instance
    pub fn new(project_path: String) -> Self {
        Self {
            project_path,
            token_budget: 2000,
            use_semantic: true,
        }
    }

    /// Set token budget
    pub fn with_token_budget(mut self, budget: usize) -> Self {
        self.token_budget = budget;
        self
    }

    /// Enable/disable semantic search
    pub fn with_semantic(mut self, use_semantic: bool) -> Self {
        self.use_semantic = use_semantic;
        self
    }

    /// Execute deep analysis query
    pub async fn analyze(&self, query: &str) -> Result<AnalysisResult, Error> {
        // Step 1: Semantic search for entry point
        let entry_points = if self.use_semantic {
            self.semantic_search(query, 5).await?
        } else {
            Vec::new()
        };

        // Step 2: Trigger Rust analyzer for graph expansion
        let context = self.expand_context(&entry_points).await?;
        let tokens_used = context.len() / 4; // Rough estimate

        // Step 3: Return LLM-ready summary
        Ok(AnalysisResult {
            query: query.to_string(),
            entry_points,
            context,
            tokens_used,
        })
    }

    /// Semantic search for entry points
    async fn semantic_search(&self, query: &str, top_k: usize) -> Result<Vec<EntryPoint>, Error> {
        // Placeholder - will use lerecherche during sub-track
        Ok(vec![EntryPoint {
            node_id: "placeholder".to_string(),
            symbol_name: "placeholder_function".to_string(),
            file_path: "placeholder.py".to_string(),
            relevance: 0.9,
        }])
    }

    /// Expand context using graph traversal
    async fn expand_context(&self, entry_points: &[EntryPoint]) -> Result<String, Error> {
        // Placeholder - will use legraphe during sub-track
        Ok(format!(
            "// Context expansion for {} entry points\n// Token budget: {}\n// Placeholder implementation",
            entry_points.len(),
            self.token_budget
        ))
    }
}

/// Analysis result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// Original query
    pub query: String,

    /// Entry points found
    pub entry_points: Vec<EntryPoint>,

    /// Expanded context (LLM-ready)
    pub context: String,

    /// Tokens used
    pub tokens_used: usize,
}

/// Entry point for graph expansion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryPoint {
    /// Node ID
    pub node_id: String,

    /// Symbol name
    pub symbol_name: String,

    /// File path
    pub file_path: String,

    /// Relevance score
    pub relevance: f32,
}

/// MCP tool errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Search failed: {0}")]
    SearchFailed(String),

    #[error("Context expansion failed: {0}")]
    ContextExpansionFailed(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),

    #[error("Invalid query: {0}")]
    InvalidQuery(String),
}

/// MCP tool request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRequest {
    /// Query text
    pub query: String,

    /// Optional project path
    pub project_path: Option<String>,

    /// Optional token budget
    pub token_budget: Option<usize>,

    /// Whether to use semantic search
    pub semantic: Option<bool>,
}

/// MCP tool response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResponse {
    /// Analysis result
    pub result: AnalysisResult,

    /// Whether result was truncated due to budget
    pub truncated: bool,

    /// Processing time in milliseconds
    pub processing_time_ms: u64,
}

impl McpResponse {
    /// Convert to LLM-friendly format
    pub fn to_llm_string(&self) -> String {
        format!(
            "# Analysis Results for: {}\n\n{}\n\n---\nEntry Points: {}\nTokens Used: {}\n{}",
            self.result.query,
            self.result.context,
            self.result.entry_points.len(),
            self.result.tokens_used,
            if self.truncated { "[Result truncated due to token budget]" } else { "" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_tool_creation() {
        let tool = LeIndexDeepAnalyze::new("/test/path".to_string());
        let result = tool.analyze("test query").await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_builder_pattern() {
        let tool = LeIndexDeepAnalyze::new("/test/path".to_string())
            .with_token_budget(5000)
            .with_semantic(false);

        assert_eq!(tool.token_budget, 5000);
        assert!(!tool.use_semantic);
    }

    #[test]
    fn test_mcp_response() {
        let result = AnalysisResult {
            query: "test".to_string(),
            entry_points: vec![],
            context: "// test context".to_string(),
            tokens_used: 100,
        };

        let response = McpResponse {
            result,
            truncated: false,
            processing_time_ms: 50,
        };

        let llm_string = response.to_llm_string();
        assert!(llm_string.contains("test"));
        assert!(llm_string.contains("test context"));
    }
}
