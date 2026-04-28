//! Edit command types and data definitions.
//!
//! Contains the core data structures for edit operations:
//! [`EditChange`], [`EditRequest`], [`EditResult`], [`EditPreview`],
//! [`ImpactAnalysis`], [`RiskLevel`], and [`EditCommand`].

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
