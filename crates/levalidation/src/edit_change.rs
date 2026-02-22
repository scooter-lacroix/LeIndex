//! Edit change representation for validation

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Represents a single edit change to be validated
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditChange {
    /// Path to the file being edited
    pub file_path: PathBuf,
    /// Original content before the edit
    pub original_content: String,
    /// New content after the edit
    pub new_content: String,
    /// Programming language (optional, inferred from extension if not provided)
    pub language: Option<String>,
    /// Edit type for additional context
    pub edit_type: EditType,
}

/// Type of edit being performed
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EditType {
    /// Insert new code
    Insert,
    /// Delete existing code
    Delete,
    /// Replace existing code
    Replace,
    /// Move code from one location to another
    Move,
    /// Rename operation
    Rename,
}

impl EditChange {
    /// Create a new edit change
    pub fn new(
        file_path: PathBuf,
        original_content: String,
        new_content: String,
    ) -> Self {
        let edit_type = if original_content.is_empty() {
            EditType::Insert
        } else if new_content.is_empty() {
            EditType::Delete
        } else {
            EditType::Replace
        };

        Self {
            file_path,
            original_content,
            new_content,
            language: None,
            edit_type,
        }
    }

    /// Create an insert edit
    pub fn insert(file_path: PathBuf, content: String) -> Self {
        Self {
            file_path,
            original_content: String::new(),
            new_content: content,
            language: None,
            edit_type: EditType::Insert,
        }
    }

    /// Create a delete edit
    pub fn delete(file_path: PathBuf, content: String) -> Self {
        Self {
            file_path,
            original_content: content,
            new_content: String::new(),
            language: None,
            edit_type: EditType::Delete,
        }
    }

    /// Create a replace edit
    pub fn replace(file_path: PathBuf, original: String, new: String) -> Self {
        Self {
            file_path,
            original_content: original,
            new_content: new,
            language: None,
            edit_type: EditType::Replace,
        }
    }

    /// Set the language explicitly
    pub fn with_language(mut self, language: String) -> Self {
        self.language = Some(language);
        self
    }

    /// Set the edit type explicitly
    pub fn with_edit_type(mut self, edit_type: EditType) -> Self {
        self.edit_type = edit_type;
        self
    }

    /// Get the file extension
    pub fn extension(&self) -> Option<&str> {
        self.file_path
            .extension()
            .and_then(|ext| ext.to_str())
    }

    /// Infer language from file extension
    pub fn infer_language(&self) -> &str {
        if let Some(ref lang) = self.language {
            return lang;
        }

        match self.extension() {
            Some("py") => "python",
            Some("js") => "javascript",
            Some("jsx") => "javascript",
            Some("ts") => "typescript",
            Some("tsx") => "typescript",
            Some("go") => "go",
            Some("rs") => "rust",
            Some("java") => "java",
            Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") | Some("h") => "cpp",
            Some("cs") => "csharp",
            Some("rb") => "ruby",
            Some("php") => "php",
            Some("lua") => "lua",
            Some("scala") | Some("sc") => "scala",
            Some("c") => "c",
            Some("sh") | Some("bash") => "bash",
            Some("json") => "json",
            _ => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_change_new_insert() {
        let change = EditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            "print('hello')".to_string(),
        );
        assert_eq!(change.edit_type, EditType::Insert);
        assert!(change.original_content.is_empty());
        assert_eq!(change.new_content, "print('hello')");
    }

    #[test]
    fn test_edit_change_new_delete() {
        let change = EditChange::new(
            PathBuf::from("test.py"),
            "print('hello')".to_string(),
            String::new(),
        );
        assert_eq!(change.edit_type, EditType::Delete);
        assert_eq!(change.original_content, "print('hello')");
        assert!(change.new_content.is_empty());
    }

    #[test]
    fn test_edit_change_new_replace() {
        let change = EditChange::new(
            PathBuf::from("test.py"),
            "print('hello')".to_string(),
            "print('world')".to_string(),
        );
        assert_eq!(change.edit_type, EditType::Replace);
    }

    #[test]
    fn test_edit_change_insert() {
        let change = EditChange::insert(PathBuf::from("test.py"), "x = 1".to_string());
        assert_eq!(change.edit_type, EditType::Insert);
        assert!(change.original_content.is_empty());
        assert_eq!(change.new_content, "x = 1");
    }

    #[test]
    fn test_edit_change_delete() {
        let change = EditChange::delete(PathBuf::from("test.py"), "x = 1".to_string());
        assert_eq!(change.edit_type, EditType::Delete);
        assert_eq!(change.original_content, "x = 1");
        assert!(change.new_content.is_empty());
    }

    #[test]
    fn test_edit_change_replace() {
        let change = EditChange::replace(
            PathBuf::from("test.py"),
            "x = 1".to_string(),
            "x = 2".to_string(),
        );
        assert_eq!(change.edit_type, EditType::Replace);
        assert_eq!(change.original_content, "x = 1");
        assert_eq!(change.new_content, "x = 2");
    }

    #[test]
    fn test_with_language() {
        let change = EditChange::insert(PathBuf::from("test.txt"), "content".to_string())
            .with_language("python".to_string());
        assert_eq!(change.language, Some("python".to_string()));
        assert_eq!(change.infer_language(), "python");
    }

    #[test]
    fn test_infer_language() {
        let cases = [
            ("test.py", "python"),
            ("test.js", "javascript"),
            ("test.ts", "typescript"),
            ("test.go", "go"),
            ("test.rs", "rust"),
            ("test.java", "java"),
            ("test.cpp", "cpp"),
            ("test.rb", "ruby"),
            ("test.php", "php"),
            ("test.lua", "lua"),
            ("test.scala", "scala"),
            ("test.c", "c"),
            ("test.sh", "bash"),
            ("test.json", "json"),
        ];

        for (file, expected_lang) in cases {
            let change = EditChange::insert(PathBuf::from(file), "content".to_string());
            assert_eq!(change.infer_language(), expected_lang, "Failed for {}", file);
        }
    }

    #[test]
    fn test_extension() {
        let change = EditChange::insert(PathBuf::from("test.py"), "content".to_string());
        assert_eq!(change.extension(), Some("py"));
    }

    #[test]
    fn test_edit_type_equality() {
        assert_eq!(EditType::Insert, EditType::Insert);
        assert_ne!(EditType::Insert, EditType::Delete);
    }
}
