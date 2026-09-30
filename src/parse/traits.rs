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

/// Configures a parser and parses source with consistent errors.
pub fn parse_tree(
    parser: &mut tree_sitter::Parser,
    language: &tree_sitter::Language,
    source: &[u8],
    language_name: &str,
) -> Result<tree_sitter::Tree> {
    parser
        .set_language(language)
        .map_err(|error| Error::ParseFailed(error.to_string()))?;

    parser
        .parse(source, None)
        .ok_or_else(|| Error::ParseFailed(format!("Failed to parse {language_name} source")))
}

/// Finds a node by ID using breadth-first traversal.
pub fn find_node_by_id<'tree>(
    root: &tree_sitter::Node<'tree>,
    id: usize,
) -> Option<tree_sitter::Node<'tree>> {
    let mut queue = std::collections::VecDeque::from([*root]);

    while let Some(node) = queue.pop_front() {
        if node.id() == id {
            return Some(node);
        }

        let mut cursor = node.walk();
        queue.extend(node.children(&mut cursor));
    }

    None
}

thread_local! {
    static LITE_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// RAII guard that switches the current thread into signature-only extraction.
///
/// While alive, parser leaf helpers that compute data callers of
/// [`CodeIntelligence::get_signatures_lite`] never read (call lists, flow
/// facts, docstrings, imports, complexity) return empty values. The flag is
/// thread-local, so rayon closures must create their own guard.
pub struct LiteGuard(());

impl LiteGuard {
    /// Enter signature-only extraction on the current thread.
    pub fn enter() -> Self {
        LITE_DEPTH.with(|d| d.set(d.get() + 1));
        LiteGuard(())
    }
}

impl Drop for LiteGuard {
    fn drop(&mut self) {
        LITE_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// True when the current thread is inside a [`LiteGuard`] scope.
#[inline]
pub fn lite() -> bool {
    LITE_DEPTH.with(|d| d.get() > 0)
}

/// Compute cyclomatic-complexity metrics for a node and its descendants.
///
/// The skeleton (nesting depth, line floor, token/child count, recursion) is
/// universal; only the set of node kinds that count as decision points varies
/// per language, supplied via `decision_kinds`.
pub fn calculate_complexity(
    node: &tree_sitter::Node<'_>,
    metrics: &mut ComplexityMetrics,
    depth: usize,
    decision_kinds: &[&str],
) {
    metrics.nesting_depth = metrics.nesting_depth.max(depth);
    // line_count: the symbol's own line span (set once, on the top node).
    if depth == 0 {
        metrics.line_count = node.end_position().row - node.start_position().row + 1;
    }
    if decision_kinds.contains(&node.kind()) {
        metrics.cyclomatic += 1;
    }
    metrics.token_count += node.child_count();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calculate_complexity(&child, metrics, depth + 1, decision_kinds);
    }
}

/// Strip a call expression down to its callee name by truncating at the first
/// `(`. The default for most languages; parsers with extra call syntax (optional
/// chaining in JS/Python, turbofish in Rust) keep their own variant.
pub fn clean_call_text(raw: &str) -> String {
    raw.split('(').next().unwrap_or(raw).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{find_node_by_id, parse_tree};

    #[test]
    fn test_parse_tree_finds_descendant_and_returns_none_for_missing_id() {
        let source = b"fn outer() { let answer = 42; }";
        let language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();

        let tree = parse_tree(&mut parser, &language, source, "Rust").expect("Rust source parses");
        let root = tree.root_node();
        let function = root.named_child(0).expect("function descendant");

        assert_eq!(
            find_node_by_id(&root, function.id())
                .expect("finds descendant")
                .id(),
            function.id()
        );
        assert!(find_node_by_id(&root, usize::MAX).is_none());
    }
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

    /// Cyclomatic complexity extracted from AST
    #[serde(default)]
    pub cyclomatic_complexity: u32,

    /// Bounded, source-level value-flow facts extracted from the body.
    ///
    /// Older persisted signatures deserialize with an empty list. Facts are
    /// intentionally shallow: they describe explicit arguments, returns, and
    /// command channels without attempting alias analysis or macro expansion.
    #[serde(default)]
    pub flow_facts: Vec<FlowFact>,
}

/// Channel through which a value or side effect flows between symbols.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowChannel {
    /// Ordinary call argument flow, identified by ordinal position.
    Argument,
    /// A returned value or tail expression.
    ReturnValue,
    /// A state read (for example `verify` or `get`).
    StateRead,
    /// A state write (for example `insert`, `record`, or `save`).
    StateWrite,
    /// An argument passed to an external command builder.
    CommandArgument,
    /// An environment variable passed to an external command.
    Environment,
    /// Standard input passed to an external command.
    Stdin,
}

/// A bounded source-level value-flow fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlowFact {
    /// Channel carrying the value or side effect.
    pub channel: FlowChannel,
    /// Source label (argument name, receiver, or literal).
    pub source: String,
    /// Target label (callee parameter, command channel, or state method).
    pub target: String,
    /// Argument ordinal when the fact came from a call expression.
    pub position: Option<usize>,
    /// Byte range of the expression that produced the fact.
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

    /// Extract only the signature header fields (`name`, `qualified_name`,
    /// `parameters`, `return_type`, `visibility`, `is_async`, `is_method`,
    /// `byte_range`).
    ///
    /// `calls`, `flow_facts`, `docstring`, `imports` and `cyclomatic_complexity`
    /// are left empty. Header fields are identical to the full extraction, so
    /// correctness never depends on a parser opting in: the default enters a
    /// [`LiteGuard`] and delegates to [`Self::get_signatures_with_parser`].
    fn get_signatures_lite(
        &self,
        source: &[u8],
        parser: &mut tree_sitter::Parser,
    ) -> Result<Vec<SignatureInfo>> {
        let _guard = LiteGuard::enter();
        self.get_signatures_with_parser(source, parser)
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
    fn extract_complexity(&self, node: &tree_sitter::Node<'_>) -> ComplexityMetrics;
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
        crate::parse::grammar::LanguageId::from_extension(ext).map(|id| id.config())
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
    use crate::parse::traits::LanguageConfig;
    use tree_sitter::Language;

    /// Python language support.
    pub mod python {
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
        use once_cell::sync::Lazy;

        /// JavaScript language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "JavaScript".to_string(),
            extensions: vec![
                "js".to_string(),
                "jsx".to_string(),
                "mjs".to_string(),
                "cjs".to_string(),
            ],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for JavaScript.
        pub fn language() -> Language {
            tree_sitter_javascript::LANGUAGE.into()
        }
    }

    /// TypeScript language support.
    pub mod typescript {
        use super::{Language, LanguageConfig};
        use once_cell::sync::Lazy;

        /// TypeScript language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "TypeScript".to_string(),
            extensions: vec![
                "ts".to_string(),
                "tsx".to_string(),
                "mts".to_string(),
                "cts".to_string(),
            ],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for TypeScript.
        pub fn language() -> Language {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    /// Go language support.
    pub mod go {
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
        use once_cell::sync::Lazy;

        /// C++ language configuration.
        pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
            name: "C++".to_string(),
            extensions: vec![
                "cpp".to_string(),
                "cc".to_string(),
                "cxx".to_string(),
                "hpp".to_string(),
                "h".to_string(),
            ],
            queries: LanguageConfig::default_queries(),
        });

        /// Get the tree-sitter language for C++.
        pub fn language() -> Language {
            tree_sitter_cpp::LANGUAGE.into()
        }
    }

    /// C# language support.
    pub mod csharp {
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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

    /// Define a Tier-0 language module: CONFIG + tree-sitter loader.
    ///
    /// One line per language keeps the 100+ breadth goal maintainable; the
    /// bespoke parsers (kotlin/swift/dart) and the generic Tier-0 parser
    /// (`parse::generic`) consume these through `language_by_name`.
    macro_rules! tier0_language {
        ($modname:ident, $display:expr, $exts:expr, $loader:expr) => {
            /// Tier-0 language module (see the table in `parse::generic`).
            pub mod $modname {
                use super::{Language, LanguageConfig};
                use once_cell::sync::Lazy;

                /// Language configuration.
                pub static CONFIG: Lazy<LanguageConfig> = Lazy::new(|| LanguageConfig {
                    name: $display.to_string(),
                    extensions: $exts.iter().map(|e| e.to_string()).collect(),
                    queries: LanguageConfig::default_queries(),
                });

                /// Get the tree-sitter language.
                pub fn language() -> Language {
                    $loader.into()
                }
            }
        };
    }

    tier0_language!(swift, "Swift", ["swift"], tree_sitter_swift::LANGUAGE);
    tier0_language!(
        kotlin,
        "Kotlin",
        ["kt", "kts"],
        tree_sitter_kotlin_ng::LANGUAGE
    );
    tier0_language!(dart, "Dart", ["dart"], tree_sitter_dart::LANGUAGE);
    tier0_language!(html, "HTML", ["html", "htm"], tree_sitter_html::LANGUAGE);
    tier0_language!(css, "CSS", ["css"], tree_sitter_css::LANGUAGE);
    tier0_language!(scss, "SCSS", ["scss"], tree_sitter_scss::language());
    tier0_language!(yaml, "YAML", ["yaml", "yml"], tree_sitter_yaml::LANGUAGE);
    tier0_language!(cmake, "CMake", ["cmake"], tree_sitter_cmake::LANGUAGE);
    tier0_language!(
        elixir,
        "Elixir",
        ["ex", "exs"],
        tree_sitter_elixir::LANGUAGE
    );
    tier0_language!(
        erlang,
        "Erlang",
        ["erl", "hrl"],
        tree_sitter_erlang::LANGUAGE
    );
    tier0_language!(haskell, "Haskell", ["hs"], tree_sitter_haskell::LANGUAGE);
    tier0_language!(perl, "Perl", ["pl", "pm"], tree_sitter_perl::LANGUAGE);
    tier0_language!(r, "R", ["r", "R"], tree_sitter_r::LANGUAGE);
    tier0_language!(zig, "Zig", ["zig"], tree_sitter_zig::LANGUAGE);
    tier0_language!(
        graphql,
        "GraphQL",
        ["graphql", "gql"],
        tree_sitter_graphql::LANGUAGE
    );
    tier0_language!(
        hcl,
        "HCL",
        ["hcl", "tf", "tfvars"],
        tree_sitter_hcl::LANGUAGE
    );
    tier0_language!(
        make,
        "Make",
        ["makefile", "mak", "mk"],
        tree_sitter_make::LANGUAGE
    );
    tier0_language!(elisp, "Emacs Lisp", ["el"], tree_sitter_elisp::LANGUAGE);
    tier0_language!(julia, "Julia", ["jl"], tree_sitter_julia::LANGUAGE);
    tier0_language!(d, "D", ["d", "di"], tree_sitter_d::LANGUAGE);
    tier0_language!(
        glsl,
        "GLSL",
        ["glsl", "vert", "frag", "comp"],
        tree_sitter_glsl::LANGUAGE_GLSL
    );
    tier0_language!(
        embedded_template,
        "Embedded Template",
        ["ejs", "erb", "liquid"],
        tree_sitter_embedded_template::LANGUAGE
    );
    // Docs tier: pulldown-cmark/regex parsers — no tree-sitter grammar. The
    // language() stand-ins exist only for LanguageId completeness; the
    // pipeline dispatches by language NAME to DocParser, which ignores the
    // tree-sitter handle entirely.
    tier0_language!(
        markdown,
        "Markdown",
        ["md", "markdown"],
        tree_sitter_json::LANGUAGE
    );
    tier0_language!(rst, "reStructuredText", ["rst"], tree_sitter_json::LANGUAGE);
    tier0_language!(
        adoc,
        "AsciiDoc",
        ["adoc", "asciidoc"],
        tree_sitter_json::LANGUAGE
    );
    tier0_language!(plaintext, "Plain Text", ["txt"], tree_sitter_json::LANGUAGE);

    /// Resolve a tree-sitter language by registry name (used by the generic
    /// Tier-0 parser). Panics on unknown names — callers pass table-driven
    /// names that are compile-time verified by the registry test.
    pub fn language_by_name(name: &str) -> Language {
        match name {
            "swift" => swift::language(),
            "kotlin" => kotlin::language(),
            "dart" => dart::language(),
            "html" => html::language(),
            "css" => css::language(),
            "scss" => scss::language(),
            "yaml" => yaml::language(),
            "cmake" => cmake::language(),
            "elixir" => elixir::language(),
            "erlang" => erlang::language(),
            "haskell" => haskell::language(),
            "perl" => perl::language(),
            "r" => r::language(),
            "zig" => zig::language(),
            "graphql" => graphql::language(),
            "hcl" => hcl::language(),
            "make" => make::language(),
            "elisp" => elisp::language(),
            "julia" => julia::language(),
            "d" => d::language(),
            "glsl" => glsl::language(),
            "embedded_template" => embedded_template::language(),
            _ => panic!("language_by_name: unknown language {name}"),
        }
    }

    /// Whether `name` has a Tier-0 language module (registry test helper).
    pub fn language_by_name_is_registered(name: &str) -> bool {
        matches!(
            name,
            "swift"
                | "kotlin"
                | "dart"
                | "html"
                | "css"
                | "scss"
                | "yaml"
                | "cmake"
                | "elixir"
                | "erlang"
                | "haskell"
                | "perl"
                | "r"
                | "zig"
                | "graphql"
                | "hcl"
                | "make"
                | "elisp"
                | "julia"
                | "d"
                | "glsl"
                | "embedded_template"
        )
    }

    /// Lua language support.
    pub mod lua {
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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
        use super::{Language, LanguageConfig};
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
