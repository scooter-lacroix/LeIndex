// Core traits for code intelligence extraction

use serde::{Deserialize, Serialize};
use once_cell::sync::Lazy;

/// Result type for parsing operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during parsing
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to parse source: {0}")]
    ParseFailed(String),

    #[error("Invalid syntax at position {position}: {message}")]
    SyntaxError { position: usize, message: String },

    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Function signature information
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignatureInfo {
    /// Function/method name
    pub name: String,

    /// Fully qualified name (including module/class)
    pub qualified_name: String,

    /// Parameters
    pub parameters: Vec<Parameter>,

    /// Return type
    pub return_type: Option<String>,

    /// Visibility (public, private, etc.)
    pub visibility: Visibility,

    /// Whether this is async
    pub is_async: bool,

    /// Whether this is a method (vs function)
    pub is_method: bool,

    /// Docstring if present
    pub docstring: Option<String>,
}

/// Function parameter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub type_annotation: Option<String>,
    pub default_value: Option<String>,
}

/// Visibility modifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
    Package,
}

/// Complexity metrics for a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityMetrics {
    /// Cyclomatic complexity
    pub cyclomatic: usize,

    /// Nesting depth
    pub nesting_depth: usize,

    /// Number of lines
    pub line_count: usize,

    /// Number of tokens (approximate)
    pub token_count: usize,
}

/// Control flow graph edge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub edge_type: EdgeType,
}

/// Control flow edge type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EdgeType {
    Unconditional,
    TrueBranch,
    FalseBranch,
    Loop,
    Exception,
}

/// Basic block in CFG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: usize,
    pub statements: Vec<String>,
}

/// Control flow graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph<Block, Edge> {
    pub blocks: Vec<Block>,
    pub edges: Vec<Edge>,
    pub entry_block: usize,
    pub exit_blocks: Vec<usize>,
}

/// Core trait for code intelligence extraction
///
/// This trait defines the interface for extracting structured information
/// from source code in various languages.
pub trait CodeIntelligence {
    /// Extract function/class signatures from source code
    ///
    /// # Arguments
    /// * `source` - Source code as bytes (for zero-copy parsing)
    ///
    /// # Returns
    /// Vector of signature information
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>>;

    /// Compute control flow graph for a node
    ///
    /// # Arguments
    /// * `source` - Source code as bytes
    /// * `node_id` - ID of the node to analyze
    ///
    /// # Returns
    /// Control flow graph structure
    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>>;

    /// Extract complexity metrics for a node
    ///
    /// # Arguments
    /// * `node` - AST node to analyze
    ///
    /// # Returns
    /// Complexity metrics
    fn extract_complexity(&self, node: &tree_sitter::Node) -> ComplexityMetrics;
}

/// Language configuration for parsing
#[derive(Debug, Clone)]
pub struct LanguageConfig {
    /// Language name
    pub name: String,

    /// File extensions for this language
    pub extensions: Vec<String>,

    /// Query patterns for common constructs
    pub queries: QueryPatterns,
}

/// Query patterns for extracting common constructs
#[derive(Debug, Clone)]
pub struct QueryPatterns {
    /// Pattern for matching function definitions
    pub function_definition: String,

    /// Pattern for matching class definitions
    pub class_definition: String,

    /// Pattern for matching method definitions
    pub method_definition: String,

    /// Pattern for matching imports
    pub import_statement: String,
}

impl LanguageConfig {
    /// Get language by file extension
    pub fn from_extension(ext: &str) -> Option<&'static Lazy<LanguageConfig>> {
        match ext.to_lowercase().as_str() {
            "py" => Some(&languages::python::CONFIG),
            "js" | "jsx" => Some(&languages::javascript::CONFIG),
            "ts" | "tsx" => Some(&languages::typescript::CONFIG),
            "go" => Some(&languages::go::CONFIG),
            "rs" => Some(&languages::rust::CONFIG),
            _ => None,
        }
    }

    const fn default_queries() -> QueryPatterns {
        QueryPatterns {
            function_definition: String::new(),
            class_definition: String::new(),
            method_definition: String::new(),
            import_statement: String::new(),
        }
    }
}

// Language-specific modules
pub mod languages {
    use once_cell::sync::Lazy;
    use tree_sitter::Language;
    use crate::traits::LanguageConfig;

    pub mod python {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Python".to_string(),
            extensions: vec!["py".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        pub fn language() -> Language {
            tree_sitter_python::LANGUAGE.into()
        }
    }

    pub mod javascript {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "JavaScript".to_string(),
            extensions: vec!["js".to_string(), "jsx".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        pub fn language() -> Language {
            tree_sitter_javascript::LANGUAGE.into()
        }
    }

    pub mod typescript {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "TypeScript".to_string(),
            extensions: vec!["ts".to_string(), "tsx".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        pub fn language() -> Language {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    pub mod go {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Go".to_string(),
            extensions: vec!["go".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        pub fn language() -> Language {
            tree_sitter_go::LANGUAGE.into()
        }
    }

    pub mod rust {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Rust".to_string(),
            extensions: vec!["rs".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        pub fn language() -> Language {
            tree_sitter_rust::LANGUAGE.into()
        }
    }
}
