// AST node types for zero-copy parsing

use serde::{Deserialize, Serialize};

/// AST node with byte range reference
///
/// This struct holds byte ranges for zero-copy access where possible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstNode {
    /// Node type (function, class, module, etc.)
    pub node_type: NodeType,

    /// Byte range into source (zero-copy reference)
    pub byte_range: std::ops::Range<usize>,

    /// Line number
    pub line_number: usize,

    /// Column number
    pub column_number: usize,

    /// Child nodes
    pub children: Vec<AstNode>,

    /// Node metadata
    pub metadata: NodeMetadata,
}

/// Type of AST node
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NodeType {
    /// Module/file level
    Module,

    /// Function definition
    Function,

    /// Class definition
    Class,

    /// Method definition
    Method,

    /// Variable declaration
    Variable,

    /// Import statement
    Import,

    /// Expression
    Expression,

    /// Statement
    Statement,

    /// Unknown/other
    Unknown,
}

/// Metadata associated with an AST node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetadata {
    /// Symbol name if applicable
    pub name: Option<String>,

    /// Fully qualified name
    pub qualified_name: Option<String>,

    /// Visibility
    pub visibility: Option<Visibility>,

    /// Docstring (extracted and owned)
    pub docstring: Option<String>,

    /// Whether node is exported/public
    pub is_exported: bool,
}

/// Visibility modifier (duplicate from traits for AST use)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
    Package,
}

impl AstNode {
    /// Create a new AST node
    pub fn new(
        node_type: NodeType,
        byte_range: std::ops::Range<usize>,
        line_number: usize,
        column_number: usize,
    ) -> Self {
        Self {
            node_type,
            byte_range,
            line_number,
            column_number,
            children: Vec::new(),
            metadata: NodeMetadata {
                name: None,
                qualified_name: None,
                visibility: None,
                docstring: None,
                is_exported: false,
            },
        }
    }

    /// Add a child node
    pub fn add_child(&mut self, child: AstNode) {
        self.children.push(child);
    }

    /// Get the source text for this node (requires source reference)
    pub fn text<'source>(&self, source: &'source [u8]) -> Result<&'source str, std::str::Utf8Error> {
        std::str::from_utf8(&source[self.byte_range.clone()])
    }
}

/// Function element extracted from AST
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionElement {
    /// Function name
    pub name: String,

    /// Qualified name
    pub qualified_name: String,

    /// Parameters
    pub parameters: Vec<Parameter>,

    /// Return type
    pub return_type: Option<String>,

    /// Byte range in source
    pub byte_range: std::ops::Range<usize>,

    /// Line number
    pub line_number: usize,

    /// Whether async
    pub is_async: bool,

    /// Docstring
    pub docstring: Option<String>,
}

/// Parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub type_annotation: Option<String>,
    pub default_value: Option<String>,
}

/// Class element extracted from AST
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassElement {
    /// Class name
    pub name: String,

    /// Qualified name
    pub qualified_name: String,

    /// Base classes
    pub base_classes: Vec<String>,

    /// Methods
    pub methods: Vec<FunctionElement>,

    /// Byte range in source
    pub byte_range: std::ops::Range<usize>,

    /// Line number
    pub line_number: usize,

    /// Docstring
    pub docstring: Option<String>,
}

/// Module element
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleElement {
    /// Module name
    pub name: String,

    /// Functions defined in module
    pub functions: Vec<FunctionElement>,

    /// Classes defined in module
    pub classes: Vec<ClassElement>,

    /// Imports
    pub imports: Vec<Import>,

    /// Byte range in source
    pub byte_range: std::ops::Range<usize>,
}

/// Import statement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    /// Module path being imported
    pub module_path: String,

    /// Specific items imported (if any)
    pub items: Vec<String>,

    /// Alias for import
    pub alias: Option<String>,

    /// Line number
    pub line_number: usize,
}
