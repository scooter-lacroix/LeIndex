//! leedit - Code Editing Engine
//!
//! *Le Edit* (The Editing) - AST-based code editing with tree-sitter and git worktree isolation

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

// Re-exports from legraphe
pub use legraphe::pdg::{ProgramDependenceGraph as PDG, Node, NodeType, Edge, EdgeType};

// Re-exports from lestockage
pub use lestockage::{Storage, StorageConfig, UniqueProjectId};

/// Error types for the edit engine.
#[derive(Error, Debug)]
pub enum EditError {
    /// Generic edit error
    #[error("Edit error: {0}")]
    Generic(String),

    /// File not found
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    /// Git operation failed
    #[error("Git operation failed: {0}")]
    GitError(String),

    /// Parse error
    #[error("Parse error: {0}")]
    ParseError(String),

    /// Invalid edit range
    #[error("Invalid edit range: {start}-{end} in file {file}")]
    InvalidRange {
        /// Start of range
        start: usize,
        /// End of range
        end: usize,
        /// File path
        file: PathBuf,
    },

    /// Worktree error
    #[error("Worktree error: {0}")]
    WorktreeError(String),

    /// History error
    #[error("History error: {0}")]
    HistoryError(String),

    /// Symbol not found
    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    /// Storage error
    #[error("Storage error: {0}")]
    StorageError(String),
}

/// Result type for edit operations
pub type Result<T> = std::result::Result<T, EditError>;

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

/// Edit engine
pub struct EditEngine {
    /// Program Dependence Graph for impact analysis
    pub pdg: Arc<PDG>,

    /// Worktree manager for isolated edits
    pub worktree_manager: Arc<WorktreeManager>,

    /// Edit history
    pub history: Arc<tokio::sync::Mutex<EditHistory>>,
}

impl EditEngine {
    /// Create a new edit engine
    pub fn new(pdg: Arc<PDG>, _storage: Arc<Storage>) -> Result<Self> {
        let worktree_manager = Arc::new(WorktreeManager::new());
        let history = Arc::new(tokio::sync::Mutex::new(EditHistory::new()));

        Ok(Self {
            pdg,
            worktree_manager,
            history,
        })
    }

    /// Preview an edit without applying it
    pub async fn preview_edit(&self, request: &EditRequest) -> Result<EditPreview> {
        // Read the original content
        let _content = self.read_file_content(&request.file_path).await?;

        // Generate diff for each change
        let mut diff_output = String::new();
        let affected_files = vec![request.file_path.clone()];

        for (idx, _change) in request.changes.iter().enumerate() {
            let change_diff = self.generate_diff(idx)?;
            diff_output.push_str(&change_diff);
            diff_output.push('\n');
        }

        // Analyze impact using PDG
        let impact = self.analyze_impact(request).await?;

        Ok(EditPreview {
            diff: diff_output,
            impact,
            files_affected: affected_files,
        })
    }

    /// Apply an edit
    pub async fn apply_edit(&self, request: &EditRequest) -> Result<EditResult> {
        // Create worktree session
        let mut session = self.worktree_manager.create_session(
            &request.project_id,
            &format!("edit-{}", chrono::Utc::now().timestamp()),
        ).await?;

        // Apply changes in worktree
        let mut changes_applied = 0;
        let mut files_modified = Vec::new();

        for change in &request.changes {
            match self.apply_change(&mut session, &request.file_path, change).await {
                Ok(modified) => {
                    if modified {
                        changes_applied += 1;
                        if !files_modified.contains(&request.file_path) {
                            files_modified.push(request.file_path.clone());
                        }
                    }
                }
                Err(e) => {
                    // Discard worktree on error
                    let _ = session.discard().await;
                    return Ok(EditResult {
                        success: false,
                        changes_applied: 0,
                        files_modified: vec![],
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        // Merge worktree back to main
        session.merge().await?;

        // Record in history
        let mut history = self.history.lock().await;
        history.record_command(EditCommand::Edit {
            project_id: request.project_id.clone(),
            file_path: request.file_path.clone(),
            changes: request.changes.clone(),
            timestamp: chrono::Utc::now(),
        });

        Ok(EditResult {
            success: true,
            changes_applied,
            files_modified,
            error: None,
        })
    }

    /// Generate diff for a change
    fn generate_diff(&self, idx: usize) -> Result<String> {
        // For now, return a simple placeholder
        // In a full implementation, this would use diffy or similar
        Ok(format!("--- Change {} ---\n", idx))
    }

    /// Analyze impact of an edit
    async fn analyze_impact(&self, request: &EditRequest) -> Result<ImpactAnalysis> {
        let mut affected_nodes = Vec::new();
        let affected_files = vec![request.file_path.clone()];
        let mut breaking_changes = Vec::new();

        // Check each change for impact
        for change in &request.changes {
            match change {
                EditChange::RenameSymbol { old_name, new_name: _ } => {
                    // Find all references in PDG
                    if let Some(_node) = self.pdg.find_by_symbol(old_name) {
                        affected_nodes.push(old_name.clone());
                    } else {
                        // Warn that symbol wasn't found
                        breaking_changes.push(format!("Symbol '{}' not found in PDG", old_name));
                    }
                }
                EditChange::ReplaceText { .. } => {
                    // Text replacement is low impact
                }
                EditChange::ExtractFunction { .. } => {
                    // Extracting a function affects the current scope
                }
                EditChange::InlineVariable { variable_name } => {
                    // Find variable references
                    if let Some(_node) = self.pdg.find_by_symbol(variable_name) {
                        affected_nodes.push(variable_name.clone());
                    }
                }
            }
        }

        // Calculate risk level
        let risk_level = if affected_nodes.len() > 5 || affected_files.len() > 2 {
            RiskLevel::High
        } else if affected_nodes.len() > 1 || affected_files.len() > 1 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        Ok(ImpactAnalysis {
            affected_nodes,
            affected_files,
            breaking_changes,
            risk_level,
        })
    }

    /// Apply a single change
    async fn apply_change(
        &self,
        _session: &mut WorktreeSession,
        _file_path: &Path,
        _change: &EditChange,
    ) -> Result<bool> {
        // Placeholder - in full implementation, this would:
        // 1. Read file content
        // 2. Parse with tree-sitter
        // 3. Apply the change
        // 4. Write back to worktree
        Ok(true)
    }

    /// Read file content from storage
    async fn read_file_content(&self, file_path: &Path) -> Result<String> {
        // For now, return a placeholder
        // In full implementation, this would read from storage
        Ok(format!("// Content of {:?}\n", file_path))
    }

    /// Undo last edit
    pub async fn undo(&self) -> Result<EditResult> {
        let mut history = self.history.lock().await;
        match history.undo() {
            Some(EditCommand::Edit { file_path, .. }) => {
                // In full implementation, we'd need to store the original content
                // to properly undo changes
                Ok(EditResult {
                    success: true,
                    changes_applied: 1,
                    files_modified: vec![file_path.clone()],
                    error: None,
                })
            }
            Some(_) | None => Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                error: Some("No edit to undo".to_string()),
            }),
        }
    }

    /// Redo last undone edit
    pub async fn redo(&self) -> Result<EditResult> {
        let mut history = self.history.lock().await;
        match history.redo() {
            Some(EditCommand::Edit { file_path, .. }) => {
                Ok(EditResult {
                    success: true,
                    changes_applied: 1,
                    files_modified: vec![file_path.clone()],
                    error: None,
                })
            }
            Some(_) | None => Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                error: Some("No edit to redo".to_string()),
            }),
        }
    }

    /// Create a rollback point
    pub async fn create_rollback_point(&self, name: String) -> Result<()> {
        let mut history = self.history.lock().await;
        history.create_rollback_point(name);
        Ok(())
    }

    /// Rollback to a named point
    pub async fn rollback(&self, name: &str) -> Result<EditResult> {
        let mut history = self.history.lock().await;
        match history.rollback(name) {
            Some(_) => Ok(EditResult {
                success: true,
                changes_applied: 1,
                files_modified: vec![],
                error: None,
            }),
            None => Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                error: Some(format!("Rollback point '{}' not found", name)),
            }),
        }
    }

    /// Get current history state
    pub async fn history_state(&self) -> (usize, usize) {
        let history = self.history.lock().await;
        (history.current_index(), history.len())
    }
}

/// Worktree manager for isolated edit sessions
pub struct WorktreeManager {
    /// Base path for worktree directories
    pub base_path: PathBuf,
}

impl WorktreeManager {
    /// Create a new worktree manager
    pub fn new() -> Self {
        Self {
            base_path: PathBuf::from("/tmp/leedit-worktrees"),
        }
    }

    /// Create a new worktree session
    pub async fn create_session(
        &self,
        _project_id: &UniqueProjectId,
        _session_name: &str,
    ) -> Result<WorktreeSession> {
        // In full implementation, this would:
        // 1. Get project path from storage
        // 2. Create git worktree using git2
        // 3. Return session handle

        Ok(WorktreeSession {
            path: self.base_path.join("session"),
        })
    }

    /// Clean up old worktrees
    pub async fn cleanup_old(&self, _older_than: chrono::Duration) -> Result<usize> {
        // Cleanup implementation
        Ok(0)
    }
}

/// Active worktree session
pub struct WorktreeSession {
    /// Path to the worktree directory
    pub path: PathBuf,
}

impl WorktreeSession {
    /// Get the worktree path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Discard the worktree without merging
    pub async fn discard(self) -> Result<()> {
        // Remove worktree directory
        Ok(())
    }

    /// Merge worktree back to main and cleanup
    pub async fn merge(self) -> Result<()> {
        // Merge changes and cleanup
        Ok(())
    }
}

/// Edit history with command pattern
#[derive(Debug)]
pub struct EditHistory {
    /// List of recorded edit commands
    pub commands: Vec<EditCommand>,
    
    /// Current position in the command history
    pub current_index: usize,
    
    /// Named rollback points mapping to command indices
    pub rollback_points: HashMap<String, usize>,
}

impl EditHistory {
    /// Create a new empty history
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            current_index: 0,
            rollback_points: HashMap::new(),
        }
    }

    /// Record a command
    pub fn record_command(&mut self, command: EditCommand) {
        // Remove any commands after current index (redo stack)
        self.commands.truncate(self.current_index);
        self.commands.push(command);
        self.current_index += 1;
    }

    /// Undo last command
    pub fn undo(&mut self) -> Option<&EditCommand> {
        if self.current_index == 0 {
            return None;
        }
        self.current_index -= 1;
        self.commands.get(self.current_index)
    }

    /// Redo last undone command
    pub fn redo(&mut self) -> Option<&EditCommand> {
        if self.current_index >= self.commands.len() {
            return None;
        }
        let command = self.commands.get(self.current_index)?;
        self.current_index += 1;
        Some(command)
    }

    /// Create a rollback point
    pub fn create_rollback_point(&mut self, name: String) {
        self.rollback_points.insert(name, self.current_index);
    }

    /// Rollback to a named point
    pub fn rollback(&mut self, name: &str) -> Option<&EditCommand> {
        let index = self.rollback_points.get(name)?;
        self.current_index = *index;
        self.commands.get(self.current_index)
    }

    /// Get current index
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    /// Get history length
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Check if history is empty
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Get all commands
    pub fn commands(&self) -> &[EditCommand] {
        &self.commands
    }
}

impl Default for EditHistory {
    fn default() -> Self {
        Self::new()
    }
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
    },

    /// Rollback point
    RollbackPoint {
        /// Name of the rollback point
        name: String,
        
        /// Timestamp of the rollback operation
        timestamp: chrono::DateTime<chrono::Utc>,
    },
}

/// AST refactoring operations
pub struct Refactor;

impl Refactor {
    /// Rename a symbol across all files
    pub async fn rename_symbol(
        _engine: &EditEngine,
        _old_name: &str,
        _new_name: &str,
    ) -> Result<EditResult> {
        // In full implementation:
        // 1. Find symbol in PDG
        // 2. Find all references
        // 3. Update each reference using tree-sitter
        // 4. Return result

        Ok(EditResult {
            success: true,
            changes_applied: 1,
            files_modified: vec![],
            error: None,
        })
    }

    /// Extract a function from selected code
    pub async fn extract_function(
        _engine: &EditEngine,
        _file_path: &Path,
        _selection: (usize, usize),
        _function_name: &str,
    ) -> Result<EditResult> {
        // In full implementation:
        // 1. Parse file with tree-sitter
        // 2. Extract selected nodes
        // 3. Create function definition
        // 4. Replace selection with call
        // 5. Update PDG

        Ok(EditResult {
            success: true,
            changes_applied: 1,
            files_modified: vec![],
            error: None,
        })
    }

    /// Inline a variable
    pub async fn inline_variable(
        _engine: &EditEngine,
        _file_path: &Path,
        _variable_name: &str,
    ) -> Result<EditResult> {
        // In full implementation:
        // 1. Find variable definition
        // 2. Find all usages
        // 3. Replace usages with value
        // 4. Remove definition
        // 5. Update PDG

        Ok(EditResult {
            success: true,
            changes_applied: 1,
            files_modified: vec![],
            error: None,
        })
    }
}

/// Diff generation utilities
pub struct Diff;

impl Diff {
    /// Generate a unified diff
    pub fn generate_unified_diff(
        _original: &str,
        _modified: &str,
        _file_path: &Path,
    ) -> Result<String> {
        // In full implementation, this would use diffy
        Ok(String::new())
    }

    /// Generate a side-by-side diff
    pub fn generate_side_by_side_diff(
        _original: &str,
        _modified: &str,
        _file_path: &Path,
    ) -> Result<(String, String)> {
        // In full implementation, this would use diffy
        Ok((String::new(), String::new()))
    }
}

/// Impact analysis utilities
pub struct Impact;

impl Impact {
    /// Analyze forward impact (what depends on this change)
    pub fn analyze_forward_impact(
        _pdg: &PDG,
        _symbol: &str,
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Analyze backward impact (what this change depends on)
    pub fn analyze_backward_impact(
        _pdg: &PDG,
        _symbol: &str,
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_pdg() -> PDG {
        PDG::new()
    }

    /// Helper to create a test UniqueProjectId
    fn make_test_id() -> UniqueProjectId {
        UniqueProjectId::new("test_project".to_string(), "abcd1234".to_string(), 0)
    }

    /// Helper to create test storage
    fn make_test_storage() -> Storage {
        Storage::open_with_config(":memory:", StorageConfig {
            db_path: ":memory:".to_string(),
            wal_enabled: false,
            cache_size_pages: None,
        }).unwrap()
    }

    #[test]
    fn test_edit_request_creation() {
        let project_id = make_test_id();
        let file_path = PathBuf::from("test.py");
        let changes = vec![EditChange::ReplaceText {
            start: 0,
            end: 10,
            new_text: "new content".to_string(),
        }];

        let request = EditRequest {
            project_id,
            file_path,
            changes,
            preview_only: true,
        };

        assert!(request.preview_only);
        assert_eq!(request.changes.len(), 1);
    }

    #[test]
    fn test_edit_change_replace_text() {
        let change = EditChange::ReplaceText {
            start: 0,
            end: 10,
            new_text: "replacement".to_string(),
        };

        assert!(matches!(change, EditChange::ReplaceText { .. }));
    }

    #[test]
    fn test_edit_change_rename_symbol() {
        let change = EditChange::RenameSymbol {
            old_name: "oldFunc".to_string(),
            new_name: "newFunc".to_string(),
        };

        if let EditChange::RenameSymbol { old_name, new_name } = change {
            assert_eq!(old_name, "oldFunc");
            assert_eq!(new_name, "newFunc");
        } else {
            panic!("Expected RenameSymbol");
        }
    }

    #[test]
    fn test_edit_change_extract_function() {
        let change = EditChange::ExtractFunction {
            start: 10,
            end: 50,
            function_name: "extractedFunc".to_string(),
        };

        assert!(matches!(change, EditChange::ExtractFunction { .. }));
    }

    #[test]
    fn test_edit_change_inline_variable() {
        let change = EditChange::InlineVariable {
            variable_name: "myVar".to_string(),
        };

        assert!(matches!(change, EditChange::InlineVariable { .. }));
    }

    #[test]
    fn test_risk_level_comparison() {
        assert_eq!(RiskLevel::Low, RiskLevel::Low);
        assert_ne!(RiskLevel::Low, RiskLevel::High);
    }

    #[test]
    fn test_edit_history_new() {
        let history = EditHistory::new();
        assert!(history.is_empty());
        assert_eq!(history.current_index(), 0);
    }

    #[test]
    fn test_edit_history_record_command() {
        let mut history = EditHistory::new();
        let project_id = make_test_id();
        let command = EditCommand::Edit {
            project_id,
            file_path: PathBuf::from("test.py"),
            changes: vec![],
            timestamp: chrono::Utc::now(),
        };

        history.record_command(command);
        assert_eq!(history.len(), 1);
        assert_eq!(history.current_index(), 1);
        assert!(!history.is_empty());
    }

    #[test]
    fn test_edit_history_undo() {
        let mut history = EditHistory::new();
        let project_id = make_test_id();
        let command = EditCommand::Edit {
            project_id,
            file_path: PathBuf::from("test.py"),
            changes: vec![],
            timestamp: chrono::Utc::now(),
        };

        history.record_command(command.clone());
        let undone = history.undo();

        assert!(undone.is_some());
        assert_eq!(history.current_index(), 0);
    }

    #[test]
    fn test_edit_history_undo_empty() {
        let mut history = EditHistory::new();
        let result = history.undo();
        assert!(result.is_none());
    }

    #[test]
    fn test_edit_history_redo() {
        let mut history = EditHistory::new();
        let project_id = make_test_id();
        let command = EditCommand::Edit {
            project_id,
            file_path: PathBuf::from("test.py"),
            changes: vec![],
            timestamp: chrono::Utc::now(),
        };

        history.record_command(command.clone());
        history.undo();
        let redone = history.redo();

        assert!(redone.is_some());
        assert_eq!(history.current_index(), 1);
    }

    #[test]
    fn test_edit_history_redo_empty() {
        let mut history = EditHistory::new();
        let result = history.redo();
        assert!(result.is_none());
    }

    #[test]
    fn test_edit_history_rollback_point() {
        let mut history = EditHistory::new();
        let project_id = make_test_id();

        // Add some commands
        for i in 0..3 {
            let command = EditCommand::Edit {
                project_id: project_id.clone(),
                file_path: PathBuf::from(format!("test{}.py", i)),
                changes: vec![],
                timestamp: chrono::Utc::now(),
            };
            history.record_command(command);
        }

        // Create rollback point
        history.create_rollback_point("before_change_3".to_string());
        assert_eq!(history.current_index(), 3);

        // Add more commands
        let command = EditCommand::Edit {
            project_id: project_id.clone(),
            file_path: PathBuf::from("test3.py"),
            changes: vec![],
            timestamp: chrono::Utc::now(),
        };
        history.record_command(command);
        assert_eq!(history.current_index(), 4);

        // Rollback
        let _ = history.rollback("before_change_3");
        assert_eq!(history.current_index(), 3);
    }

    #[test]
    fn test_edit_history_rollback_nonexistent() {
        let mut history = EditHistory::new();
        let result = history.rollback("nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_edit_history_undo_clears_redo_stack() {
        let mut history = EditHistory::new();
        let project_id = make_test_id();

        // Add 3 commands
        for i in 0..3 {
            let command = EditCommand::Edit {
                project_id: project_id.clone(),
                file_path: PathBuf::from(format!("test{}.py", i)),
                changes: vec![],
                timestamp: chrono::Utc::now(),
            };
            history.record_command(command);
        }

        // Undo twice
        history.undo();
        history.undo();
        assert_eq!(history.current_index(), 1);

        // Add a new command - should clear redo stack
        let command = EditCommand::Edit {
            project_id: project_id.clone(),
            file_path: PathBuf::from("new.py"),
            changes: vec![],
            timestamp: chrono::Utc::now(),
        };
        history.record_command(command);

        assert_eq!(history.len(), 2); // Only 2 commands now
        assert_eq!(history.current_index(), 2);
    }

    #[test]
    fn test_worktree_manager_new() {
        let manager = WorktreeManager::new();
        assert_eq!(manager.base_path, PathBuf::from("/tmp/leedit-worktrees"));
    }

    #[test]
    fn test_impact_analysis_default() {
        let analysis = ImpactAnalysis {
            affected_nodes: vec![],
            affected_files: vec![],
            breaking_changes: vec![],
            risk_level: RiskLevel::Low,
        };

        assert_eq!(analysis.risk_level, RiskLevel::Low);
        assert!(analysis.affected_nodes.is_empty());
    }

    #[test]
    fn test_edit_preview_default() {
        let preview = EditPreview {
            diff: String::new(),
            impact: ImpactAnalysis {
                affected_nodes: vec![],
                affected_files: vec![],
                breaking_changes: vec![],
                risk_level: RiskLevel::Low,
            },
            files_affected: vec![],
        };

        assert!(preview.diff.is_empty());
        assert_eq!(preview.impact.risk_level, RiskLevel::Low);
    }

    #[test]
    fn test_edit_result_default() {
        let result = EditResult {
            success: false,
            changes_applied: 0,
            files_modified: vec![],
            error: None,
        };

        assert!(!result.success);
        assert_eq!(result.changes_applied, 0);
        assert!(result.files_modified.is_empty());
        assert!(result.error.is_none());
    }

    #[test]
    fn test_edit_result_success() {
        let result = EditResult {
            success: true,
            changes_applied: 5,
            files_modified: vec![PathBuf::from("test.py")],
            error: None,
        };

        assert!(result.success);
        assert_eq!(result.changes_applied, 5);
        assert_eq!(result.files_modified.len(), 1);
    }

    #[tokio::test]
    async fn test_edit_engine_creation() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());

        let engine = EditEngine::new(pdg, storage);
        assert!(engine.is_ok());
    }

    #[tokio::test]
    async fn test_edit_engine_preview_edit() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let project_id = make_test_id();
        let request = EditRequest {
            project_id,
            file_path: PathBuf::from("test.py"),
            changes: vec![EditChange::ReplaceText {
                start: 0,
                end: 10,
                new_text: "new".to_string(),
            }],
            preview_only: true,
        };

        let result = engine.preview_edit(&request).await;
        assert!(result.is_ok());
        let preview = result.unwrap();
        assert_eq!(preview.files_affected.len(), 1);
        assert!(matches!(preview.impact.risk_level, RiskLevel::Low));
    }

    #[tokio::test]
    async fn test_edit_engine_history_state() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let (index, len) = engine.history_state().await;
        assert_eq!(index, 0);
        assert_eq!(len, 0);
    }

    #[tokio::test]
    async fn test_edit_engine_rollback_point() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let result = engine.create_rollback_point("test_point".to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_edit_engine_undo_no_history() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let result = engine.undo().await;
        assert!(result.is_ok());
        let edit_result = result.unwrap();
        assert!(!edit_result.success);
        assert!(edit_result.error.is_some());
    }

    #[tokio::test]
    async fn test_edit_engine_redo_no_history() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let result = engine.redo().await;
        assert!(result.is_ok());
        let edit_result = result.unwrap();
        assert!(!edit_result.success);
        assert!(edit_result.error.is_some());
    }

    #[tokio::test]
    async fn test_edit_engine_rollback_nonexistent() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let result = engine.rollback("nonexistent").await;
        assert!(result.is_ok());
        let edit_result = result.unwrap();
        assert!(!edit_result.success);
        assert!(edit_result.error.is_some());
    }

    #[test]
    fn test_edit_command_edit_variant() {
        let project_id = make_test_id();
        let command = EditCommand::Edit {
            project_id,
            file_path: PathBuf::from("test.py"),
            changes: vec![],
            timestamp: chrono::Utc::now(),
        };

        assert!(matches!(command, EditCommand::Edit { .. }));
    }

    #[test]
    fn test_edit_command_rollback_point_variant() {
        let command = EditCommand::RollbackPoint {
            name: "test_point".to_string(),
            timestamp: chrono::Utc::now(),
        };

        assert!(matches!(command, EditCommand::RollbackPoint { .. }));
    }

    #[test]
    fn test_edit_error_display() {
        let error = EditError::FileNotFound(PathBuf::from("missing.py"));
        let msg = format!("{}", error);
        assert!(msg.contains("missing.py"));
    }

    #[test]
    fn test_edit_error_invalid_range() {
        let error = EditError::InvalidRange {
            start: 10,
            end: 5,
            file: PathBuf::from("test.py"),
        };
        let msg = format!("{}", error);
        assert!(msg.contains("10-5"));
        assert!(msg.contains("test.py"));
    }

    #[test]
    fn test_edit_error_symbol_not_found() {
        let error = EditError::SymbolNotFound("mySymbol".to_string());
        let msg = format!("{}", error);
        assert!(msg.contains("mySymbol"));
    }

    #[test]
    fn test_edit_change_clone() {
        let change = EditChange::ReplaceText {
            start: 0,
            end: 10,
            new_text: "test".to_string(),
        };
        let cloned = change.clone();
        assert_eq!(change, cloned);
    }

    #[test]
    fn test_edit_change_equality() {
        let change1 = EditChange::ReplaceText {
            start: 0,
            end: 10,
            new_text: "test".to_string(),
        };
        let change2 = EditChange::ReplaceText {
            start: 0,
            end: 10,
            new_text: "test".to_string(),
        };
        assert_eq!(change1, change2);
    }

    #[test]
    fn test_edit_history_default() {
        let history = EditHistory::default();
        assert!(history.is_empty());
    }

    #[test]
    fn test_edit_result_clone() {
        let result = EditResult {
            success: true,
            changes_applied: 1,
            files_modified: vec![PathBuf::from("test.py")],
            error: None,
        };
        let cloned = result.clone();
        assert_eq!(result.success, cloned.success);
        assert_eq!(result.changes_applied, cloned.changes_applied);
    }

    #[test]
    fn test_refactor_rename_symbol() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        // Test that rename compiles
        let _result = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(Refactor::rename_symbol(&engine, "old", "new"))
        })
        .join()
        .unwrap();

        // In test environment, just verify no panic
    }

    #[test]
    fn test_refactor_extract_function() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        // Test that extract compiles
        let _result = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(Refactor::extract_function(
                &engine,
                Path::new("test.py"),
                (0, 10),
                "newFunc",
            ))
        })
        .join()
        .unwrap();

        // In test environment, just verify no panic
    }

    #[test]
    fn test_refactor_inline_variable() {
        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        // Test that inline compiles
        let _result = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(Refactor::inline_variable(
                &engine,
                Path::new("test.py"),
                "myVar",
            ))
        })
        .join()
        .unwrap();

        // In test environment, just verify no panic
    }
}
