//! Syntax validation using tree-sitter

use crate::edit_change::EditChange;
use crate::ValidationError;
use leparse::grammar::LanguageId;
use std::path::PathBuf;

/// Severity of a syntax error
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    /// Error that prevents parsing
    Error,
    /// Warning that doesn't prevent parsing
    Warning,
}

/// A syntax error found during validation
#[derive(Debug, Clone)]
pub struct SyntaxError {
    /// File path where the error occurred
    pub file_path: PathBuf,
    /// Line number (1-indexed)
    pub line: usize,
    /// Column number (1-indexed, in bytes)
    pub column: usize,
    /// Error message
    pub message: String,
    /// Severity level
    pub severity: ErrorSeverity,
}

impl SyntaxError {
    /// Create a new syntax error
    pub fn new(
        file_path: PathBuf,
        line: usize,
        column: usize,
        message: String,
        severity: ErrorSeverity,
    ) -> Self {
        Self {
            file_path,
            line,
            column,
            message,
            severity,
        }
    }

    /// Create an error from a tree-sitter node
    pub fn from_tree_sitter_node(
        file_path: PathBuf,
        node: &tree_sitter::Node,
        message: impl Into<String>,
        source: &[u8],
    ) -> Self {
        let message = message.into();
        let (line, column) = Self::line_column_from_node(node, source);
        Self {
            file_path,
            line,
            column,
            message,
            severity: ErrorSeverity::Error,
        }
    }

    /// Convert byte offset to line and column
    fn line_column_from_node(node: &tree_sitter::Node, source: &[u8]) -> (usize, usize) {
        let start = node.start_byte();
        let mut line = 1;
        let mut column = 1;

        for (i, &byte) in source.iter().enumerate() {
            if i == start {
                break;
            }
            if byte == b'\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }

        (line, column)
    }
}

/// Syntax validator using tree-sitter
#[derive(Debug, Clone)]
pub struct SyntaxValidator {
    /// Whether to enable strict mode
    strict_mode: bool,
}

impl SyntaxValidator {
    /// Create a new syntax validator
    pub fn new() -> Self {
        Self {
            strict_mode: false,
        }
    }

    /// Create a strict syntax validator
    pub fn strict() -> Self {
        Self {
            strict_mode: true,
        }
    }

    /// Get LanguageId from language name string
    fn language_from_name(name: &str) -> Option<LanguageId> {
        match name {
            "python" => Some(LanguageId::Python),
            "javascript" => Some(LanguageId::JavaScript),
            "typescript" => Some(LanguageId::TypeScript),
            "go" => Some(LanguageId::Go),
            "rust" => Some(LanguageId::Rust),
            "java" => Some(LanguageId::Java),
            "cpp" | "c++" => Some(LanguageId::Cpp),
            "csharp" | "c#" => Some(LanguageId::CSharp),
            "ruby" => Some(LanguageId::Ruby),
            "php" => Some(LanguageId::Php),
            "lua" => Some(LanguageId::Lua),
            "scala" => Some(LanguageId::Scala),
            "c" => Some(LanguageId::C),
            "bash" => Some(LanguageId::Bash),
            "json" => Some(LanguageId::Json),
            _ => None,
        }
    }

    /// Validate syntax for multiple edit changes
    ///
    /// # Arguments
    /// * `changes` - Edit changes to validate
    ///
    /// # Returns
    /// Vector of syntax errors found
    pub fn validate_syntax(&self, changes: &[EditChange]) -> Result<Vec<SyntaxError>, ValidationError> {
        let mut errors = Vec::new();

        for change in changes {
            let lang = change.infer_language();
            let language_id = SyntaxValidator::language_from_name(lang)
                .ok_or_else(|| ValidationError::Parse(format!("Unsupported language: {}", lang)))?;

            // Parse the new content to check for syntax errors
            match self.parse_content(&change.new_content, language_id) {
                Ok(_) => {
                    // Check for common issues even if parsing succeeded
                    if self.strict_mode {
                        if let Some(warning) = self.check_style_issues(change, language_id) {
                            errors.push(warning);
                        }
                    }
                }
                Err(parse_error) => {
                    errors.push(parse_error);
                }
            }
        }

        Ok(errors)
    }

    /// Parse content and detect syntax errors
    fn parse_content(
        &self,
        content: &str,
        language_id: LanguageId,
    ) -> Result<(), SyntaxError> {
        let mut parser = tree_sitter::Parser::new();
        let language = language_id.from_cache()
            .map_err(|_| SyntaxError::new(
                PathBuf::from("<unknown>"),
                0,
                0,
                format!("Failed to load language for {:?}", language_id),
                ErrorSeverity::Error,
            ))?;
        parser.set_language(&language)
            .map_err(|_| SyntaxError::new(
                PathBuf::from("<unknown>"),
                0,
                0,
                format!("Failed to set language for {:?}", language_id),
                ErrorSeverity::Error,
            ))?;

        let source = content.as_bytes();
        let tree = parser.parse(source, None)
            .ok_or_else(|| SyntaxError::new(
                PathBuf::from("<unknown>"),
                0,
                0,
                "Failed to parse source".to_string(),
                ErrorSeverity::Error,
            ))?;

        // Check for error nodes in the tree
        let root = tree.root_node();
        if root.has_error() {
            self.find_error_nodes(&root, source)?;
        }

        Ok(())
    }

    /// Recursively find error nodes in the tree
    fn find_error_nodes(
        &self,
        node: &tree_sitter::Node,
        source: &[u8],
    ) -> Result<(), SyntaxError> {
        if node.is_error() || node.is_missing() {
            return Err(SyntaxError::from_tree_sitter_node(
                PathBuf::from("<unknown>"),
                node,
                if node.is_error() {
                    "Syntax error"
                } else {
                    "Missing syntax element"
                },
                source,
            ));
        }

        for child in node.children(&mut node.walk()) {
            self.find_error_nodes(&child, source)?;
        }

        Ok(())
    }

    /// Check for style issues (warnings, not errors)
    fn check_style_issues(
        &self,
        change: &EditChange,
        _language_id: LanguageId,
    ) -> Option<SyntaxError> {
        // Check for empty edits (inserting only whitespace)
        if change.edit_type == crate::edit_change::EditType::Insert
            && change.new_content.trim().is_empty()
        {
            return Some(SyntaxError::new(
                change.file_path.clone(),
                1,
                1,
                "Inserting only whitespace".to_string(),
                ErrorSeverity::Warning,
            ));
        }

        // Check for very long lines
        for (line_num, line) in change.new_content.lines().enumerate() {
            if line.len() > 200 {
                return Some(SyntaxError::new(
                    change.file_path.clone(),
                    line_num + 1,
                    200,
                    format!("Line exceeds 200 characters (length: {})", line.len()),
                    ErrorSeverity::Warning,
                ));
            }
        }

        None
    }
}

impl Default for SyntaxValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit_change::EditType;

    #[test]
    fn test_syntax_validator_new() {
        let validator = SyntaxValidator::new();
        assert!(!validator.strict_mode);
    }

    #[test]
    fn test_syntax_validator_strict() {
        let validator = SyntaxValidator::strict();
        assert!(validator.strict_mode);
    }

    #[test]
    fn test_validate_syntax_valid_python() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            "def hello():\n    print('world')".to_string(),
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_invalid_python() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            "def hello(\n    print('world')".to_string(), // Missing closing paren
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(!errors.is_empty());
        assert_eq!(errors[0].severity, ErrorSeverity::Error);
    }

    #[test]
    fn test_validate_syntax_valid_javascript() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.js"),
            String::new(),
            "function hello() {\n  console.log('world');\n}".to_string(),
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_invalid_javascript() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.js"),
            String::new(),
            "function hello( {\n  console.log('world');\n}".to_string(), // Syntax error
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_valid_rust() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.rs"),
            String::new(),
            "fn hello() {\n    println!(\"world\");\n}".to_string(),
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_invalid_rust() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.rs"),
            String::new(),
            "fn hello( {\n    println!(\"world\");\n}".to_string(), // Syntax error
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_unsupported_language() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.xyz"),
            String::new(),
            "some content".to_string(),
        );
        let result = validator.validate_syntax(&[change]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::Parse(_)));
    }

    #[test]
    fn test_syntax_error_new() {
        let error = SyntaxError::new(
            PathBuf::from("test.py"),
            10,
            5,
            "Test error".to_string(),
            ErrorSeverity::Error,
        );
        assert_eq!(error.file_path, PathBuf::from("test.py"));
        assert_eq!(error.line, 10);
        assert_eq!(error.column, 5);
        assert_eq!(error.message, "Test error");
        assert_eq!(error.severity, ErrorSeverity::Error);
    }

    #[test]
    fn test_error_severity_equality() {
        assert_eq!(ErrorSeverity::Error, ErrorSeverity::Error);
        assert_ne!(ErrorSeverity::Error, ErrorSeverity::Warning);
    }

    #[test]
    fn test_validate_syntax_long_line_warning() {
        let validator = SyntaxValidator::strict();
        let long_line = "x".repeat(250);
        let change = EditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            long_line,
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(!errors.is_empty());
        assert_eq!(errors[0].severity, ErrorSeverity::Warning);
        assert!(errors[0].message.contains("exceeds 200 characters"));
    }

    #[test]
    fn test_validate_syntax_whitespace_only_warning() {
        let validator = SyntaxValidator::strict();
        let change = EditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            "   \n   \n".to_string(),
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(!errors.is_empty());
        assert_eq!(errors[0].severity, ErrorSeverity::Warning);
        assert!(errors[0].message.contains("whitespace"));
    }

    #[test]
    fn test_syntax_validator_default() {
        let validator = SyntaxValidator::default();
        assert!(!validator.strict_mode);
    }

    #[test]
    fn test_validate_syntax_valid_go() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.go"),
            String::new(),
            "package main\n\nfunc main() {\n\tprintln(\"hello\")\n}".to_string(),
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_valid_json() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.json"),
            String::new(),
            "{\"key\": \"value\"}".to_string(),
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_invalid_json() {
        let validator = SyntaxValidator::new();
        let change = EditChange::new(
            PathBuf::from("test.json"),
            String::new(),
            "{\"key\": }".to_string(), // Invalid JSON
        );
        let errors = validator.validate_syntax(&[change]).unwrap();
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_validate_syntax_multiple_changes() {
        let validator = SyntaxValidator::new();
        let change1 = EditChange::new(
            PathBuf::from("test1.py"),
            String::new(),
            "def foo(): pass".to_string(),
        );
        let change2 = EditChange::new(
            PathBuf::from("test2.py"),
            String::new(),
            "def bar(:\n    pass".to_string(), // Syntax error
        );
        let errors = validator.validate_syntax(&[change1, change2]).unwrap();
        assert_eq!(errors.len(), 1);
    }
}
