//! Edit command types and data definitions.
//!
//! Contains the core data structures for edit operations:
//! [`EditChange`], [`ResolvedEditChange`], [`EditRequest`], [`EditResult`],
//! [`EditPreview`], [`ImpactAnalysis`], [`RiskLevel`], and [`EditCommand`].

use std::collections::HashMap;
use std::path::PathBuf;

use crate::storage::UniqueProjectId;

/// Edit change operations
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EditChange {
    /// Replace text at a specific byte range
    ReplaceText {
        /// Start byte offset
        start: usize,
        /// End byte offset
        end: usize,
        /// New text to insert
        new_text: String,
    },

    /// Rename a symbol across all references
    RenameSymbol {
        /// Old symbol name
        old_name: String,
        /// New symbol name
        new_name: String,
    },

    /// Extract selected code into a new function
    ExtractFunction {
        /// Start of selection
        start: usize,
        /// End of selection
        end: usize,
        /// Name for the new function
        function_name: String,
    },

    /// Inline a variable at its usage sites
    InlineVariable {
        /// Variable name to inline
        variable_name: String,
    },
}

impl EditChange {
    /// Estimated byte size of this change for cache accounting.
    pub fn estimated_size(&self) -> usize {
        match self {
            Self::ReplaceText { new_text, .. } => new_text.len() + 32,
            Self::RenameSymbol { old_name, new_name } => old_name.len() + new_name.len() + 32,
            Self::ExtractFunction { function_name, .. } => function_name.len() + 32,
            Self::InlineVariable { variable_name } => variable_name.len() + 32,
        }
    }
}

/// Edit request
#[derive(Debug, Clone)]
pub struct EditRequest {
    /// Project identifier
    pub project_id: UniqueProjectId,

    /// File path to edit
    pub file_path: PathBuf,

    /// Changes to apply
    pub changes: Vec<EditChange>,

    /// Preview only (don't apply changes)
    pub preview_only: bool,
}

/// Preview of an edit operation
#[derive(Debug, Clone)]
pub struct EditPreview {
    /// Unified diff
    pub diff: String,

    /// Impact analysis
    pub impact: ImpactAnalysis,

    /// Files affected by this edit
    pub files_affected: Vec<PathBuf>,
}

/// Result of an edit operation
#[derive(Debug, Clone)]
pub struct EditResult {
    /// Success status
    pub success: bool,

    /// Number of changes applied
    pub changes_applied: usize,

    /// Files modified
    pub files_modified: Vec<PathBuf>,

    /// Error message if failed
    pub error: Option<String>,

    /// Map of file_path → original content before the operation.
    /// Used internally to record undo history. `None` for single-file edits
    /// (which capture original_content in EditCommand::Edit directly).
    pub original_contents: Option<HashMap<String, String>>,

    /// Map of file_path → content after the operation.
    /// Used internally to record redo history. `None` for single-file edits.
    /// Stores the exact post-rename content (result of `replace_near_definitions`)
    /// so redo can restore the precise state without re-running replacement.
    pub modified_contents: Option<HashMap<String, String>>,
}

/// Impact analysis for an edit
#[derive(Debug, Clone)]
pub struct ImpactAnalysis {
    /// Nodes directly affected
    pub affected_nodes: Vec<String>,

    /// Files that will be modified
    pub affected_files: Vec<PathBuf>,

    /// Potential breaking changes
    pub breaking_changes: Vec<String>,

    /// Estimated risk level
    pub risk_level: RiskLevel,
}

/// Risk level for an edit
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// Low risk - localized change
    Low,
    /// Medium risk - affects multiple files
    Medium,
    /// High risk - affects critical components
    High,
}

/// Edit command for history
#[derive(Debug, Clone)]
pub enum EditCommand {
    /// Standard edit operation
    Edit {
        /// Project identifier
        project_id: UniqueProjectId,

        /// File path to edit
        file_path: PathBuf,

        /// Changes to apply
        changes: Vec<EditChange>,

        /// Timestamp of the edit operation
        timestamp: chrono::DateTime<chrono::Utc>,

        /// Original file content before the edit (for undo).
        original_content: Option<String>,

        /// Modified content after the edit (for redo).
        /// Captured during apply_edit to ensure exact replay.
        modified_content: Option<String>,
    },

    /// Multi-file rename operation with full rollback support.
    /// Original and modified contents of all files are captured for precise
    /// undo (restore originals) and redo (restore modifieds).
    Rename {
        /// Project identifier
        project_id: UniqueProjectId,

        /// Old symbol name
        old_name: String,

        /// New symbol name
        new_name: String,

        /// Timestamp of the rename operation
        timestamp: chrono::DateTime<chrono::Utc>,

        /// Map of file path → original content before rename (for undo)
        original_contents: HashMap<String, String>,

        /// Map of file path → content after rename (for redo).
        /// Stored as the exact post-rename content so redo restores the precise
        /// result of `replace_near_definitions` rather than re-running
        /// `replace_whole_word` which could corrupt comments/strings.
        modified_contents: HashMap<String, String>,
    },

    /// Rollback point
    RollbackPoint {
        /// Name of the rollback point
        name: String,

        /// Timestamp of the rollback operation
        timestamp: chrono::DateTime<chrono::Utc>,
    },
}

/// Type of edit being performed (for validation context)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
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

/// A resolved edit change with full content context, used by the validation pipeline.
///
/// Unlike [`EditChange`] which describes the edit *operation* (byte ranges, symbol names),
/// `ResolvedEditChange` carries the resolved *content* — the file path, original content,
/// and new content — needed by syntax validators, reference checkers, and drift analyzers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ResolvedEditChange {
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

impl ResolvedEditChange {
    /// Create a new resolved edit change
    pub fn new(file_path: PathBuf, original_content: String, new_content: String) -> Self {
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
        self.file_path.extension().and_then(|ext| ext.to_str())
    }

    /// Infer language from file extension
    pub fn infer_language(&self) -> &str {
        if let Some(ref lang) = self.language {
            return lang;
        }

        let ext = self.extension().map(|e| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("py") => "python",
            Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => "javascript",
            Some("ts") | Some("tsx") | Some("mts") | Some("cts") => "typescript",
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
mod tests_resolved {
    use super::*;

    #[test]
    fn test_resolved_edit_change_new_insert() {
        let change = ResolvedEditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            "print('hello')".to_string(),
        );
        assert_eq!(change.edit_type, EditType::Insert);
        assert!(change.original_content.is_empty());
        assert_eq!(change.new_content, "print('hello')");
    }

    #[test]
    fn test_resolved_edit_change_new_delete() {
        let change = ResolvedEditChange::new(
            PathBuf::from("test.py"),
            "print('hello')".to_string(),
            String::new(),
        );
        assert_eq!(change.edit_type, EditType::Delete);
        assert_eq!(change.original_content, "print('hello')");
        assert!(change.new_content.is_empty());
    }

    #[test]
    fn test_resolved_edit_change_new_replace() {
        let change = ResolvedEditChange::new(
            PathBuf::from("test.py"),
            "print('hello')".to_string(),
            "print('world')".to_string(),
        );
        assert_eq!(change.edit_type, EditType::Replace);
    }

    #[test]
    fn test_resolved_edit_change_insert() {
        let change = ResolvedEditChange::insert(PathBuf::from("test.py"), "x = 1".to_string());
        assert_eq!(change.edit_type, EditType::Insert);
        assert!(change.original_content.is_empty());
        assert_eq!(change.new_content, "x = 1");
    }

    #[test]
    fn test_resolved_edit_change_delete() {
        let change = ResolvedEditChange::delete(PathBuf::from("test.py"), "x = 1".to_string());
        assert_eq!(change.edit_type, EditType::Delete);
        assert_eq!(change.original_content, "x = 1");
        assert!(change.new_content.is_empty());
    }

    #[test]
    fn test_resolved_edit_change_replace() {
        let change = ResolvedEditChange::replace(
            PathBuf::from("test.py"),
            "x = 1".to_string(),
            "x = 2".to_string(),
        );
        assert_eq!(change.edit_type, EditType::Replace);
        assert_eq!(change.original_content, "x = 1");
        assert_eq!(change.new_content, "x = 2");
    }

    #[test]
    fn test_resolved_with_language() {
        let change = ResolvedEditChange::insert(PathBuf::from("test.txt"), "content".to_string())
            .with_language("python".to_string());
        assert_eq!(change.language, Some("python".to_string()));
        assert_eq!(change.infer_language(), "python");
    }

    #[test]
    fn test_resolved_infer_language() {
        let cases = [
            ("test.py", "python"),
            ("test.js", "javascript"),
            ("test.mjs", "javascript"),
            ("test.MJS", "javascript"),
            ("test.cjs", "javascript"),
            ("test.ts", "typescript"),
            ("test.mts", "typescript"),
            ("test.MTS", "typescript"),
            ("test.cts", "typescript"),
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
            let change = ResolvedEditChange::insert(PathBuf::from(file), "content".to_string());
            assert_eq!(
                change.infer_language(),
                expected_lang,
                "Failed for {}",
                file
            );
        }
    }

    #[test]
    fn test_resolved_extension() {
        let change = ResolvedEditChange::insert(PathBuf::from("test.py"), "content".to_string());
        assert_eq!(change.extension(), Some("py"));
    }

    #[test]
    fn test_edit_type_equality() {
        assert_eq!(EditType::Insert, EditType::Insert);
        assert_ne!(EditType::Insert, EditType::Delete);
    }
}
