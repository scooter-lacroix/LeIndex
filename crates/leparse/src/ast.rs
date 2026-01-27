// AST node types for zero-copy parsing

use serde::{Deserialize, Serialize};
use crate::traits::{Visibility, Parameter};

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
    /// Symbol name if applicable (stored as byte range for zero-copy)
    pub name_range: Option<std::ops::Range<usize>>,

    /// Fully qualified name (computed on demand, not stored)
    pub qualified_name: Option<String>,

    /// Visibility
    pub visibility: Option<Visibility>,

    /// Docstring range (zero-copy reference into source)
    pub docstring_range: Option<std::ops::Range<usize>>,

    /// Whether node is exported/public
    pub is_exported: bool,
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
                name_range: None,
                qualified_name: None,
                visibility: None,
                docstring_range: None,
                is_exported: false,
            },
        }
    }

    /// Add a child node
    pub fn add_child(&mut self, child: AstNode) {
        self.children.push(child);
    }

    /// Get the source text for this node (with bounds checking)
    ///
    /// Returns an error if the byte range is out of bounds
    pub fn text<'source>(&self, source: &'source [u8]) -> Result<&'source str, crate::traits::Error> {
        if self.byte_range.end > source.len() {
            return Err(crate::traits::Error::ParseFailed(format!(
                "Byte range {}..{} out of bounds for source of length {}",
                self.byte_range.start,
                self.byte_range.end,
                source.len()
            )));
        }
        std::str::from_utf8(&source[self.byte_range.clone()])
            .map_err(crate::traits::Error::Utf8)
    }

    /// Get the name text for this node (if available)
    pub fn name<'source>(&self, source: &'source [u8]) -> Result<&'source str, crate::traits::Error> {
        if let Some(ref range) = self.metadata.name_range {
            if range.end > source.len() {
                return Err(crate::traits::Error::ParseFailed(format!(
                    "Name range {}..{} out of bounds for source of length {}",
                    range.start,
                    range.end,
                    source.len()
                )));
            }
            return std::str::from_utf8(&source[range.clone()])
                .map_err(crate::traits::Error::Utf8);
        }
        Err(crate::traits::Error::ParseFailed("No name range set".to_string()))
    }

    /// Get the docstring text for this node (if available)
    pub fn docstring<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        if let Some(ref range) = self.metadata.docstring_range {
            if range.end > source.len() {
                return Err(crate::traits::Error::ParseFailed(format!(
                    "Docstring range {}..{} out of bounds for source of length {}",
                    range.start,
                    range.end,
                    source.len()
                )));
            }
            let text = std::str::from_utf8(&source[range.clone()])
                .map_err(crate::traits::Error::Utf8)?;
            Ok(Some(text))
        } else {
            Ok(None)
        }
    }
}

/// Function element extracted from AST
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionElement {
    /// Function name range (zero-copy)
    pub name_range: std::ops::Range<usize>,

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

    /// Docstring range
    pub docstring_range: Option<std::ops::Range<usize>>,
}

/// Class element extracted from AST
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassElement {
    /// Class name range (zero-copy)
    pub name_range: std::ops::Range<usize>,

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

    /// Docstring range
    pub docstring_range: Option<std::ops::Range<usize>>,
}

/// Module element
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleElement {
    /// Module name range (zero-copy)
    pub name_range: std::ops::Range<usize>,

    /// Qualified name (computed from file path, not source)
    pub qualified_name: String,

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

    /// Byte range in source (for zero-copy access)
    pub byte_range: std::ops::Range<usize>,

    /// Line number
    pub line_number: usize,
}

/// Zero-copy text extraction trait
///
/// This trait provides methods to extract text from byte ranges
/// without allocating new strings.
pub trait ZeroCopyText {
    /// Get text from byte range
    fn get_text<'source>(&self, source: &'source [u8]) -> Result<&'source str, crate::traits::Error>;

    /// Get name text from byte range
    fn get_name<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error>;

    /// Get docstring text from byte range
    fn get_docstring<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error>;
}

impl ZeroCopyText for AstNode {
    fn get_text<'source>(&self, source: &'source [u8]) -> Result<&'source str, crate::traits::Error> {
        self.text(source)
    }

    fn get_name<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        if let Some(ref range) = self.metadata.name_range {
            if range.end > source.len() {
                return Err(crate::traits::Error::ParseFailed(
                    "Name range out of bounds".to_string()
                ));
            }
            Ok(Some(std::str::from_utf8(&source[range.clone()])
                .map_err(crate::traits::Error::Utf8)?))
        } else {
            Ok(None)
        }
    }

    fn get_docstring<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        self.docstring(source)
    }
}

impl ZeroCopyText for FunctionElement {
    fn get_text<'source>(&self, source: &'source [u8]) -> Result<&'source str, crate::traits::Error> {
        if self.byte_range.end > source.len() {
            return Err(crate::traits::Error::ParseFailed(
                "Function byte range out of bounds".to_string()
            ));
        }
        std::str::from_utf8(&source[self.byte_range.clone()])
            .map_err(crate::traits::Error::Utf8)
    }

    fn get_name<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        if self.name_range.end > source.len() {
            return Err(crate::traits::Error::ParseFailed(
                "Function name range out of bounds".to_string()
            ));
        }
        Ok(Some(std::str::from_utf8(&source[self.name_range.clone()])
            .map_err(crate::traits::Error::Utf8)?))
    }

    fn get_docstring<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        if let Some(ref range) = self.docstring_range {
            if range.end > source.len() {
                return Err(crate::traits::Error::ParseFailed(
                    "Docstring range out of bounds".to_string()
                ));
            }
            Ok(Some(std::str::from_utf8(&source[range.clone()])
                .map_err(crate::traits::Error::Utf8)?))
        } else {
            Ok(None)
        }
    }
}

impl ZeroCopyText for ClassElement {
    fn get_text<'source>(&self, source: &'source [u8]) -> Result<&'source str, crate::traits::Error> {
        if self.byte_range.end > source.len() {
            return Err(crate::traits::Error::ParseFailed(
                "Class byte range out of bounds".to_string()
            ));
        }
        std::str::from_utf8(&source[self.byte_range.clone()])
            .map_err(crate::traits::Error::Utf8)
    }

    fn get_name<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        if self.name_range.end > source.len() {
            return Err(crate::traits::Error::ParseFailed(
                "Class name range out of bounds".to_string()
            ));
        }
        Ok(Some(std::str::from_utf8(&source[self.name_range.clone()])
            .map_err(crate::traits::Error::Utf8)?))
    }

    fn get_docstring<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        if let Some(ref range) = self.docstring_range {
            if range.end > source.len() {
                return Err(crate::traits::Error::ParseFailed(
                    "Docstring range out of bounds".to_string()
                ));
            }
            Ok(Some(std::str::from_utf8(&source[range.clone()])
                .map_err(crate::traits::Error::Utf8)?))
        } else {
            Ok(None)
        }
    }
}

impl ZeroCopyText for ModuleElement {
    fn get_text<'source>(&self, source: &'source [u8]) -> Result<&'source str, crate::traits::Error> {
        if self.byte_range.end > source.len() {
            return Err(crate::traits::Error::ParseFailed(
                "Module byte range out of bounds".to_string()
            ));
        }
        std::str::from_utf8(&source[self.byte_range.clone()])
            .map_err(crate::traits::Error::Utf8)
    }

    fn get_name<'source>(&self, source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        if self.name_range.end > source.len() {
            return Err(crate::traits::Error::ParseFailed(
                "Module name range out of bounds".to_string()
            ));
        }
        Ok(Some(std::str::from_utf8(&source[self.name_range.clone()])
            .map_err(crate::traits::Error::Utf8)?))
    }

    fn get_docstring<'source>(&self, _source: &'source [u8]) -> Result<Option<&'source str>, crate::traits::Error> {
        // Module elements don't have docstrings in the same way
        Ok(None)
    }
}
