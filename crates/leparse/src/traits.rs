// Core traits for code intelligence extraction

use serde::{Deserialize, Serialize};

/// Result type for parsing operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during parsing
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Failed to parse the source code
    #[error("Failed to parse source: {0}")]
    ParseFailed(String),

    /// Syntax error at a specific position
    #[error("Invalid syntax at position {position}: {message}")]
    SyntaxError {
        /// Position of the error
        position: usize,
        /// Error message
        message: String,
    },

    /// The language is not supported
    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),

    /// Input/Output error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// UTF-8 decoding error
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Import information extracted from a file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportInfo {
    /// Imported path (module, namespace, or file)
    pub path: String,

    /// Optional alias for the import
    pub alias: Option<String>,
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

    /// List of called functions/methods
    pub calls: Vec<String>,

    /// Imports in the current file
    pub imports: Vec<ImportInfo>,

    /// Byte range in source code
    pub byte_range: (usize, usize),
}

/// Function parameter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Parameter {
    /// Parameter name
    pub name: String,
    /// Type annotation if present
    pub type_annotation: Option<String>,
    /// Default value if present
    pub default_value: Option<String>,
}

/// Visibility modifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Visibility {
    /// Publicly accessible
    Public,
    /// Private to the class/module
    Private,
    /// Protected (accessible to subclasses)
    Protected,
    /// Internal to the crate/package
    Internal,
    /// Package-private
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
    /// Source block ID
    pub from: usize,
    /// Destination block ID
    pub to: usize,
    /// Type of edge
    pub edge_type: EdgeType,
}

/// Control flow edge type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EdgeType {
    /// Unconditional jump
    Unconditional,
    /// True branch of a conditional
    TrueBranch,
    /// False branch of a conditional
    FalseBranch,
    /// Loop back edge
    Loop,
    /// Exception handling edge
    Exception,
}

/// Basic block in CFG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// Unique block ID
    pub id: usize,
    /// Statements within the block
    pub statements: Vec<String>,
}

/// Control flow graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph<Block, Edge> {
    /// Blocks in the graph
    pub blocks: Vec<Block>,
    /// Edges in the graph
    pub edges: Vec<Edge>,
    /// ID of the entry block
    pub entry_block: usize,
    /// IDs of the exit blocks
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

    /// Extract signatures using a provided parser instance (for pooling)
    ///
    /// # Arguments
    /// * `source` - Source code as bytes
    /// * `parser` - Tree-sitter parser instance to reuse
    fn get_signatures_with_parser(
        &self,
        source: &[u8],
        _parser: &mut tree_sitter::Parser,
    ) -> Result<Vec<SignatureInfo>> {
        // Default implementation delegates to get_signatures
        // Implementations should override this to provide pooling benefits
        self.get_signatures(source)
    }

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
    ///
    /// This method delegates to `LanguageId::from_extension` to eliminate
    /// duplicate extension mapping logic and maintain a single source of truth.
    pub fn from_extension(ext: &str) -> Option<&'static LanguageConfig> {
        crate::grammar::LanguageId::from_extension(ext)
            .map(|id| id.config())
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
/// Language-specific configurations and grammar loaders.
pub mod languages {
    use tree_sitter::Language;
    use crate::traits::LanguageConfig;

    /// Python language support.
    pub mod python {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Python language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Python".to_string(),
            extensions: vec!["py".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Python.
        pub fn language() -> Language {
            tree_sitter_python::LANGUAGE.into()
        }
    }

    /// JavaScript language support.
    pub mod javascript {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// JavaScript language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "JavaScript".to_string(),
            extensions: vec!["js".to_string(), "jsx".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for JavaScript.
        pub fn language() -> Language {
            tree_sitter_javascript::LANGUAGE.into()
        }
    }

    /// TypeScript language support.
    pub mod typescript {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// TypeScript language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "TypeScript".to_string(),
            extensions: vec!["ts".to_string(), "tsx".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for TypeScript.
        pub fn language() -> Language {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    /// Go language support.
    pub mod go {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Go language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Go".to_string(),
            extensions: vec!["go".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Go.
        pub fn language() -> Language {
            tree_sitter_go::LANGUAGE.into()
        }
    }

    /// Rust language support.
    pub mod rust {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Rust language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Rust".to_string(),
            extensions: vec!["rs".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Rust.
        pub fn language() -> Language {
            tree_sitter_rust::LANGUAGE.into()
        }
    }

    /// Java language support.
    pub mod java {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Java language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Java".to_string(),
            extensions: vec!["java".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Java.
        pub fn language() -> Language {
            tree_sitter_java::LANGUAGE.into()
        }
    }

    /// C++ language support.
    pub mod cpp {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// C++ language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "C++".to_string(),
            extensions: vec!["cpp".to_string(), "cc".to_string(), "cxx".to_string(), "hpp".to_string(), "h".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for C++.
        pub fn language() -> Language {
            tree_sitter_cpp::LANGUAGE.into()
        }
    }

    /// C# language support.
    pub mod csharp {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// C# language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "C#".to_string(),
            extensions: vec!["cs".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for C#.
        pub fn language() -> Language {
            tree_sitter_c_sharp::LANGUAGE.into()
        }
    }

    /// Ruby language support.
    pub mod ruby {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Ruby language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Ruby".to_string(),
            extensions: vec!["rb".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Ruby.
        pub fn language() -> Language {
            tree_sitter_ruby::LANGUAGE.into()
        }
    }

    /// PHP language support.
    pub mod php {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// PHP language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "PHP".to_string(),
            extensions: vec!["php".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for PHP.
        pub fn language() -> Language {
            // tree_sitter_php provides LANGUAGE_PHP constant (LanguageFn type)
            tree_sitter_php::LANGUAGE_PHP.into()
        }
    }

    // Swift language implementation - disabled due to tree-sitter version incompatibility (grammar v15 vs library v13-14)
    // pub mod swift {
    //     use super::{LanguageConfig, Language};
    //     use once_cell::sync::Lazy;
    //
    //     pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
    //         name: "Swift".to_string(),
    //         extensions: vec!["swift".to_string()],
    //         queries: LanguageConfig::default_queries(),
    //     });
    //
    //     pub fn language() -> Language {
    //         tree_sitter_swift::LANGUAGE.into()
    //     }
    // }

    // TODO: Kotlin support disabled due to tree-sitter version incompatibility (0.20.10 vs 0.24.7)
    // The kotlin crate depends on an older version of tree-sitter, causing duplicate symbol errors
    // pub mod kotlin {
    //     use super::{LanguageConfig, Language};
    //     use once_cell::sync::Lazy;
    //
    //     pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
    //         name: "Kotlin".to_string(),
    //         extensions: vec!["kt".to_string(), "kts".to_string()],
    //         queries: LanguageConfig::default_queries(),
    //     });
    //
    //     pub fn language() -> Language {
    //         tree_sitter_kotlin::language()
    //     }
    // }

    // Dart language implementation - disabled due to parsing issues
    // pub mod dart {
    //     use super::{LanguageConfig, Language};
    //     use once_cell::sync::Lazy;
    //
    //     pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
    //         name: "Dart".to_string(),
    //         extensions: vec!["dart".to_string()],
    //         queries: LanguageConfig::default_queries(),
    //     });
    //
    //     pub fn language() -> Language {
    //         tree_sitter_dart::language()
    //     }
    // }

    /// Lua language support.
    pub mod lua {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Lua language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Lua".to_string(),
            extensions: vec!["lua".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Lua.
        pub fn language() -> Language {
            tree_sitter_lua::LANGUAGE.into()
        }
    }

    /// Scala language support.
    pub mod scala {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Scala language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Scala".to_string(),
            extensions: vec!["scala".to_string(), "sc".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Scala.
        pub fn language() -> Language {
            tree_sitter_scala::LANGUAGE.into()
        }
    }

    /// C language support.
    pub mod c {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// C language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "C".to_string(),
            extensions: vec!["c".to_string(), "h".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for C.
        pub fn language() -> Language {
            tree_sitter_c::LANGUAGE.into()
        }
    }

    /// Bash language support.
    pub mod bash {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// Bash language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "Bash".to_string(),
            extensions: vec!["sh".to_string(), "bash".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for Bash.
        pub fn language() -> Language {
            tree_sitter_bash::LANGUAGE.into()
        }
    }

    /// JSON language support.
    pub mod json {
        use super::{LanguageConfig, Language};
        use once_cell::sync::Lazy;

        /// JSON language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "JSON".to_string(),
            extensions: vec!["json".to_string()],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for JSON.
        pub fn language() -> Language {
            tree_sitter_json::LANGUAGE.into()
        }
    }
}
