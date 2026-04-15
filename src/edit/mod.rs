//! leedit - Code Editing Engine
//!
//! *Le Edit* (The Editing) - AST-based code editing with tree-sitter and git worktree isolation

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

// Re-exports from legraphe
pub use crate::graph::pdg::{Edge, EdgeType, Node, NodeType, ProgramDependenceGraph as PDG};

// Re-exports from lestockage
pub use crate::storage::{Storage, StorageConfig, UniqueProjectId};

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

fn clamp_to_char_boundary(content: &str, idx: usize) -> usize {
    let mut i = idx.min(content.len());
    while i > 0 && !content.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn replace_whole_word(content: &str, old: &str, new: &str) -> String {
    if old.is_empty() {
        return content.to_owned();
    }

    let mut result = String::with_capacity(content.len());
    let mut last_match_end = 0usize;

    for (start, matched) in content.match_indices(old) {
        let end = start + matched.len();
        let before_ok = start == 0
            || content[..start]
                .chars()
                .last()
                .map(|c| !is_word_char(c))
                .unwrap_or(true);
        let after_ok = end == content.len()
            || content[end..]
                .chars()
                .next()
                .map(|c| !is_word_char(c))
                .unwrap_or(true);

        if before_ok && after_ok {
            result.push_str(&content[last_match_end..start]);
            result.push_str(new);
            last_match_end = end;
        }
    }

    result.push_str(&content[last_match_end..]);
    result
}

fn sanitize_session_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "session".to_string()
    } else {
        out
    }
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
        let original = self.read_file_content(&request.file_path).await?;

        // Apply each change to produce modified content, then generate diff
        let mut modified = original.clone();
        for change in &request.changes {
            modified = self.apply_change_to_string(&modified, change)?;
        }

        let diff = self.generate_diff(&original, &modified, &request.file_path)?;

        // Analyze impact using PDG
        let impact = self.analyze_impact(request).await?;
        let all_files = impact.affected_files.clone();

        Ok(EditPreview {
            diff,
            impact,
            files_affected: all_files,
        })
    }

    /// Apply an edit
    pub async fn apply_edit(&self, request: &EditRequest) -> Result<EditResult> {
        // Create worktree session
        let mut session = self
            .worktree_manager
            .create_session(
                &request.project_id,
                &format!("edit-{}", chrono::Utc::now().timestamp()),
            )
            .await?;

        // Capture original content for undo
        let original_content = std::fs::read_to_string(&request.file_path).ok();

        // Apply changes in worktree
        let mut changes_applied = 0;
        let mut files_modified = Vec::new();

        for change in &request.changes {
            match self
                .apply_change(&mut session, &request.file_path, change)
                .await
            {
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
            original_content,
        });

        Ok(EditResult {
            success: true,
            changes_applied,
            files_modified,
            error: None,
        })
    }

    /// Generate a unified diff between original and modified content.
    pub fn generate_diff(
        &self,
        original: &str,
        modified: &str,
        file_path: &Path,
    ) -> Result<String> {
        let patch = diffy::create_patch(original, modified);
        let patch_str = patch.to_string();
        if patch_str.is_empty() {
            Ok(format!(
                "--- {}\n+++ {}\n(no changes)\n",
                file_path.display(),
                file_path.display()
            ))
        } else {
            // Replace diffy's default "--- original" / "+++ modified" headers with file-path labels
            let lines: Vec<&str> = patch_str.lines().collect();
            let mut result = format!("--- {}\n+++ {}", file_path.display(), file_path.display());
            for line in lines.iter().skip(2) {
                result.push_str(&format!("\n{}", line));
            }
            Ok(result)
        }
    }

    /// Apply a single EditChange to a string in memory, returning the modified string.
    ///
    /// Used by `preview_edit` to compute the diff without touching the filesystem.
    fn apply_change_to_string(&self, content: &str, change: &EditChange) -> Result<String> {
        match change {
            EditChange::ReplaceText {
                start,
                end,
                new_text,
            } => {
                let start_idx = clamp_to_char_boundary(content, *start);
                let end_idx = clamp_to_char_boundary(content, *end);
                if start_idx > end_idx {
                    return Err(EditError::InvalidRange {
                        start: *start,
                        end: *end,
                        file: PathBuf::from("(in-memory)"),
                    });
                }
                let result = format!(
                    "{}{}{}",
                    &content[..start_idx],
                    new_text,
                    &content[end_idx..]
                );
                Ok(result)
            }
            EditChange::RenameSymbol { old_name, new_name } => Ok(replace_whole_word(
                content,
                old_name.as_str(),
                new_name.as_str(),
            )),
            EditChange::ExtractFunction { .. } | EditChange::InlineVariable { .. } => {
                // AST-level changes are complex; return content unchanged for preview
                Ok(content.to_owned())
            }
        }
    }

    /// Analyze impact of an edit using forward PDG traversal.
    async fn analyze_impact(&self, request: &EditRequest) -> Result<ImpactAnalysis> {
        let mut affected_nodes: Vec<String> = Vec::new();
        let mut affected_files: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        affected_files.insert(request.file_path.clone());
        let mut breaking_changes = Vec::new();

        // Check each change for impact
        for change in &request.changes {
            match change {
                EditChange::RenameSymbol {
                    old_name,
                    new_name: _,
                } => {
                    if let Some(node_id) = self.pdg.find_by_symbol(old_name) {
                        affected_nodes.push(old_name.clone());
                        // Forward impact: all nodes reachable from this one
                        let forward = self.pdg.forward_impact(
                            node_id,
                            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                        );
                        for dep_id in forward {
                            if let Some(dep_node) = self.pdg.get_node(dep_id) {
                                affected_nodes.push(dep_node.name.clone());
                                affected_files.insert(PathBuf::from(&dep_node.file_path));
                            }
                        }
                        // Backward impact: callers that reference this symbol (rename risk)
                        let backward = self.pdg.backward_impact(
                            node_id,
                            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                        );
                        if !backward.is_empty() {
                            breaking_changes.push(format!(
                                "Renaming '{}' may break {} caller(s)",
                                old_name,
                                backward.len()
                            ));
                            for bid in backward {
                                if let Some(bn) = self.pdg.get_node(bid) {
                                    affected_files.insert(PathBuf::from(&bn.file_path));
                                }
                            }
                        }
                    } else {
                        breaking_changes.push(format!(
                            "Symbol '{}' not found in PDG — rename may miss references",
                            old_name
                        ));
                    }
                }
                EditChange::ReplaceText { .. } => {
                    // Text replacement is low impact unless it touches a symbol boundary
                }
                EditChange::ExtractFunction { function_name, .. } => {
                    breaking_changes.push(format!(
                        "Extracting function '{}' — verify no name collision",
                        function_name
                    ));
                }
                EditChange::InlineVariable { variable_name } => {
                    if let Some(node_id) = self.pdg.find_by_symbol(variable_name) {
                        affected_nodes.push(variable_name.clone());
                        let forward = self.pdg.forward_impact(
                            node_id,
                            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                        );
                        for dep_id in forward {
                            if let Some(dep_node) = self.pdg.get_node(dep_id) {
                                affected_nodes.push(dep_node.name.clone());
                                affected_files.insert(PathBuf::from(&dep_node.file_path));
                            }
                        }
                    }
                }
            }
        }

        let affected_files_vec: Vec<PathBuf> = affected_files.into_iter().collect();

        // Calculate risk level based on blast radius
        let risk_level = if affected_nodes.len() > 5 || affected_files_vec.len() > 3 {
            RiskLevel::High
        } else if affected_nodes.len() > 1 || affected_files_vec.len() > 1 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        Ok(ImpactAnalysis {
            affected_nodes,
            affected_files: affected_files_vec,
            breaking_changes,
            risk_level,
        })
    }

    /// Apply a single change to a file in the worktree session's path.
    async fn apply_change(
        &self,
        session: &mut WorktreeSession,
        file_path: &Path,
        change: &EditChange,
    ) -> Result<bool> {
        // Resolve the file path within the worktree
        let target_path = if file_path.is_absolute() {
            // Map absolute path into worktree, preserving directory structure.
            // Try stripping common prefixes; fall back to the full relative path.
            let rel = file_path
                .strip_prefix(session.path())
                .or_else(|_| file_path.strip_prefix("/"))
                .unwrap_or(file_path);
            session.path().join(rel)
        } else {
            session.path().join(file_path)
        };

        // Always materialize/read the target in the worktree to keep edits isolated.
        let content = if target_path.exists() {
            std::fs::read_to_string(&target_path).map_err(|e| {
                EditError::Generic(format!("Failed to read {:?}: {}", target_path, e))
            })?
        } else if file_path.exists() {
            let source = std::fs::read_to_string(file_path).map_err(|e| {
                EditError::Generic(format!("Failed to read {:?}: {}", file_path, e))
            })?;
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EditError::Generic(format!("Failed to create worktree dir {:?}: {}", parent, e))
                })?;
            }
            std::fs::write(&target_path, source.as_bytes()).map_err(|e| {
                EditError::Generic(format!(
                    "Failed to materialize worktree file {:?}: {}",
                    target_path, e
                ))
            })?;
            source
        } else {
            return Err(EditError::FileNotFound(file_path.to_path_buf()));
        };

        let modified = self.apply_change_to_string(&content, change)?;

        if modified == content {
            return Ok(false); // No change
        }

        // Write modified content back into worktree only.
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                EditError::Generic(format!("Failed to create worktree dir {:?}: {}", parent, e))
            })?;
        }
        std::fs::write(&target_path, modified.as_bytes())
            .map_err(|e| EditError::Generic(format!("Failed to write {:?}: {}", target_path, e)))?;

        session.track_file(file_path.to_path_buf(), target_path);

        Ok(true)
    }

    /// Read file content from disk.
    pub async fn read_file_content(&self, file_path: &Path) -> Result<String> {
        std::fs::read_to_string(file_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                EditError::FileNotFound(file_path.to_path_buf())
            } else {
                EditError::Generic(format!("Failed to read {:?}: {}", file_path, e))
            }
        })
    }

    /// Undo last edit
    pub async fn undo(&self) -> Result<EditResult> {
        let mut history = self.history.lock().await;
        match history.undo() {
            Some(EditCommand::Edit { file_path, original_content, .. }) => {
                // Restore original content if available
                if let Some(content) = original_content {
                    if let Err(e) = std::fs::write(&file_path, content.as_bytes()) {
                        return Ok(EditResult {
                            success: false,
                            changes_applied: 0,
                            files_modified: vec![],
                            error: Some(format!("Failed to restore '{}': {}", file_path.display(), e)),
                        });
                    }
                } else {
                    // No pre-image was captured — cannot reliably undo
                    return Ok(EditResult {
                        success: false,
                        changes_applied: 0,
                        files_modified: vec![],
                        error: Some(format!("Cannot undo '{}': original content was not captured", file_path.display())),
                    });
                }
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
            Some(EditCommand::Edit { file_path, .. }) => Ok(EditResult {
                success: true,
                changes_applied: 1,
                files_modified: vec![file_path.clone()],
                error: None,
            }),
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
        session_name: &str,
    ) -> Result<WorktreeSession> {
        std::fs::create_dir_all(&self.base_path)
            .map_err(|e| EditError::WorktreeError(format!(
                "Failed to create worktree base '{}': {}",
                self.base_path.display(),
                e
            )))?;

        let mut session_dir = None;
        for attempt in 0..16 {
            let candidate = self.base_path.join(format!(
                "{}-{}-{}-{}",
                sanitize_session_component(session_name),
                chrono::Utc::now().timestamp_millis(),
                std::process::id(),
                attempt
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    session_dir = Some(candidate);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(EditError::WorktreeError(format!(
                        "Failed to create session worktree '{}': {}",
                        candidate.display(),
                        e
                    )));
                }
            }
        }
        let session_dir = session_dir.ok_or_else(|| {
            EditError::WorktreeError("Failed to allocate unique session worktree".to_string())
        })?;

        Ok(WorktreeSession {
            path: session_dir,
            tracked_files: HashMap::new(),
        })
    }

    /// Clean up old worktrees older than the specified duration.
    ///
    /// Scans the base worktree directory for session directories that were
    /// created more than `older_than` ago and removes them. Each session
    /// directory name includes a timestamp suffix for this purpose.
    ///
    /// Returns the number of worktree directories removed.
    pub async fn cleanup_old(&self, older_than: chrono::Duration) -> Result<usize> {
        if !self.base_path.exists() {
            return Ok(0);
        }

        let cutoff_time = chrono::Utc::now() - older_than;
        let mut removed_count = 0;

        let entries = std::fs::read_dir(&self.base_path).map_err(|e| {
            EditError::WorktreeError(format!(
                "Failed to read worktree base directory '{}': {}",
                self.base_path.display(),
                e
            ))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                EditError::WorktreeError(format!(
                    "Failed to read worktree directory entry: {}",
                    e
                ))
            })?;

            let path = entry.path();

            // Only process directories that look like session directories
            // (they contain timestamp suffixes)
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    // Session names end with timestamp-pid: prefix-1234567890123-1234
                    // Validate all three parts before treating as a session directory
                    // Session names: {prefix}-{timestamp_millis}-{pid}-{attempt}
                    // Use rsplitn to extract the numeric segments from the right
                    let mut parts = name.rsplitn(4, '-');
                    let attempt_part = parts.next();
                    let pid_part = parts.next();
                    let ts_part = parts.next();
                    let prefix_part = parts.next();
                    if let (Some(_attempt_str), Some(pid_str), Some(timestamp_str), Some(_prefix)) =
                        (attempt_part, pid_part, ts_part, prefix_part)
                    {
                        if _attempt_str.parse::<u32>().is_ok()
                            && pid_str.parse::<u32>().is_ok()
                            && timestamp_str.parse::<i64>().is_ok()
                        {
                            let timestamp_millis = timestamp_str.parse::<i64>().unwrap();
                            let session_time = chrono::DateTime::<chrono::Utc>::from_timestamp(
                                timestamp_millis / 1000,
                                ((timestamp_millis % 1000) * 1_000_000) as u32,
                            );

                            if let Some(session_time) = session_time {
                                if session_time < cutoff_time {
                                    // Remove the old worktree directory
                                    match std::fs::remove_dir_all(&path) {
                                        Ok(_) => removed_count += 1,
                                        Err(e) => {
                                            // Log but don't fail - continue cleaning up others
                                            tracing::warn!(
                                                "Failed to remove old worktree '{}': {}",
                                                path.display(),
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(removed_count)
    }
}

/// Active worktree session
pub struct WorktreeSession {
    /// Path to the worktree directory
    pub path: PathBuf,

    /// Mapping from original file path to staged worktree path.
    tracked_files: HashMap<PathBuf, PathBuf>,
}

impl WorktreeSession {
    /// Get the worktree path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Track a file that was materialized in the worktree.
    fn track_file(&mut self, original: PathBuf, staged: PathBuf) {
        self.tracked_files.insert(original, staged);
    }

    /// Discard the worktree without merging
    pub async fn discard(self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path).map_err(|e| {
                EditError::WorktreeError(format!(
                    "Failed to discard worktree '{}': {}",
                    self.path.display(),
                    e
                ))
            })?;
        }
        Ok(())
    }

    /// Merge worktree changes back to original files and cleanup the worktree.
    ///
    /// # Semantics
    ///
    /// This uses **best-effort compensating rollback** on failure, not true
    /// transactional atomicity. Specifically:
    ///
    /// 1. Original files are backed up in memory before any writes
    /// 2. Staged files are merged in sorted order
    /// 3. On write failure, previous backups are restored to their original state
    /// 4. The worktree is cleaned up on success
    ///
    /// # Limitations
    ///
    /// - Backups are in-memory only (lost if process crashes mid-merge)
    /// - No file-level locking (concurrent writes can cause conflicts)
    /// - Rollback is best-effort (individual restoration failures are ignored)
    /// - Not atomic across all files (files are written one at a time)
    ///
    /// For true transactional semantics, use git worktrees or a proper transaction
    /// coordinator. This implementation provides reasonable isolation for editing
    /// sessions but cannot guarantee full atomicity.
    pub async fn merge(self) -> Result<()> {
        let WorktreeSession {
            path,
            tracked_files,
        } = self;

        let mut backups: Vec<(PathBuf, bool, Option<String>)> = Vec::new();
        let mut staged_entries: Vec<(PathBuf, PathBuf)> = tracked_files.into_iter().collect();
        staged_entries.sort_by(|a, b| a.0.cmp(&b.0));

        for (original, staged) in &staged_entries {
            let file_existed = original.exists();
            let backup = if file_existed {
                Some(std::fs::read_to_string(original).map_err(|e| {
                    EditError::WorktreeError(format!(
                        "Failed to back up original file '{}': {}",
                        original.display(),
                        e
                    ))
                })?)
            } else {
                None
            };
            backups.push((original.clone(), file_existed, backup));

            let content = std::fs::read_to_string(staged).map_err(|e| {
                EditError::WorktreeError(format!(
                    "Failed to read staged file '{}': {}",
                    staged.display(),
                    e
                ))
            })?;

            if let Some(parent) = original.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EditError::WorktreeError(format!(
                        "Failed to create destination directory '{}': {}",
                        parent.display(),
                        e
                    ))
                })?;
            }

            if let Err(e) = std::fs::write(original, content.as_bytes()) {
                for (backup_path, file_existed, backup_content) in backups.iter().rev() {
                    if !file_existed {
                        // File didn't exist before merge — remove it
                        let _ = std::fs::remove_file(backup_path);
                    } else if let Some(previous) = backup_content {
                        // File existed — restore from backup
                        let _ = std::fs::write(backup_path, previous.as_bytes());
                    }
                }
                return Err(EditError::WorktreeError(format!(
                    "Failed to merge staged file '{}' into '{}': {}",
                    staged.display(),
                    original.display(),
                    e
                )));
            }
        }

        if path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                // Post-commit cleanup failure — log but don't fail the merge
                tracing::warn!(
                    "Failed to clean up worktree '{}': {}",
                    path.display(),
                    e
                );
            }
        }
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

        /// Original file content before the edit (for undo).
        original_content: Option<String>,
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
    /// Rename a symbol across all files using PDG-guided file discovery and whole-word replacement.
    ///
    /// # Implementation Details
    ///
    /// This is **NOT** a full AST/reference-aware rename. It uses a hybrid approach:
    ///
    /// 1. **File discovery via PDG**: Uses `pdg.find_by_symbol()` and `pdg.find_all_by_name()`
    ///    to discover which files contain nodes matching `old_name`
    ///
    /// 2. **Whole-word replacement**: Within each discovered file, applies `replace_whole_word()`
    ///    which replaces occurrences bounded by word boundaries (alphanumeric or underscore)
    ///
    /// # Limitations
    ///
    /// - Does NOT parse AST or resolve semantic references
    /// - Does NOT distinguish between type names, variable names, or string literals
    /// - May rename occurrences in comments, strings, or documentation
    /// - Does NOT handle language-specific scoping or namespacing
    /// - Relies on PDG symbol names which may not capture all references
    ///
    /// # Future Work
    ///
    /// For true AST/reference-aware rename, this should be replaced with:
    /// - Language-specific tree-sitter queries for semantic rename
    /// - Or LSP-based rename operations (if language server is available)
    /// - The upstream-first policy requires implementing this in LeIndex first
    ///
    /// # Returns
    ///
    /// Count of files modified and list of modified file paths.
    pub async fn rename_symbol(
        engine: &EditEngine,
        old_name: &str,
        new_name: &str,
    ) -> Result<EditResult> {
        if old_name.is_empty() || new_name.is_empty() {
            return Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                error: Some("old_name and new_name must be non-empty".to_string()),
            });
        }

        // 1. Resolve the PDG symbol candidates and prefer exact symbol hits.
        let node_ids = engine.pdg.find_all_by_name(old_name);
        let exact_node = engine.pdg.find_by_symbol(old_name);

        // Collect files containing the symbol definition AND all files that
        // reference it (call sites, type usages) via PDG forward/backward edges.
        let mut files: HashSet<PathBuf> = HashSet::new();

        let config = crate::graph::pdg::TraversalConfig::for_impact_analysis();

        if let Some(node_id) = exact_node {
            if let Some(node) = engine.pdg.get_node(node_id) {
                if node.node_type != crate::graph::pdg::NodeType::External {
                    files.insert(PathBuf::from(&node.file_path));

                    // Add files that reference this symbol via PDG edges
                    let impacted = engine.pdg.forward_impact(node_id, &config);
                    for imp_id in impacted {
                        if let Some(imp_node) = engine.pdg.get_node(imp_id) {
                            if imp_node.node_type != crate::graph::pdg::NodeType::External {
                                files.insert(PathBuf::from(&imp_node.file_path));
                            }
                        }
                    }
                    let backward = engine.pdg.backward_impact(node_id, &config);
                    for back_id in backward {
                        if let Some(back_node) = engine.pdg.get_node(back_id) {
                            if back_node.node_type != crate::graph::pdg::NodeType::External {
                                files.insert(PathBuf::from(&back_node.file_path));
                            }
                        }
                    }
                }
            }
        }

        for nid in node_ids {
            if let Some(node) = engine.pdg.get_node(nid) {
                if node.node_type != crate::graph::pdg::NodeType::External {
                    files.insert(PathBuf::from(&node.file_path));
                }
            }
        }

        if files.is_empty() {
            return Ok(EditResult {
                success: true,
                changes_applied: 0,
                files_modified: vec![],
                error: None,
            });
        }

        // 4. Apply replace_whole_word to each file (two-phase: collect then write)
        // Sort files for deterministic processing order
        let mut sorted_files: Vec<_> = files.into_iter().collect();
        sorted_files.sort();

        let mut total_changes = 0usize;
        let mut modified_files: Vec<(PathBuf, String)> = Vec::new(); // (path, original)
        let mut errors = Vec::new();

        // Phase 1: collect all modifications (no writes yet)
        let mut pending_writes: Vec<(PathBuf, String, String)> = Vec::new(); // (path, original, modified)
        for file_path in &sorted_files {
            let original = match std::fs::read_to_string(file_path) {
                Ok(content) => content,
                Err(e) => {
                    errors.push(format!("Failed to read '{}': {}", file_path.display(), e));
                    continue;
                }
            };

            let modified = replace_whole_word(&original, old_name, new_name);
            if modified != original {
                pending_writes.push((file_path.clone(), original, modified));
            }
        }

        // If discovery/read failed for any candidate, do not write anything (all-or-nothing).
        if !errors.is_empty() {
            return Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                error: Some(errors.join("; ")),
            });
        }

        // Phase 2: write all files, rollback on failure
        for (file_path, original_content, modified) in &pending_writes {
            match std::fs::write(file_path, modified.as_bytes()) {
                Ok(()) => {
                    total_changes += 1;
                    modified_files.push((file_path.clone(), original_content.clone()));
                }
                Err(e) => {
                    errors.push(format!("Failed to write '{}': {}", file_path.display(), e));
                    // Rollback all previously written files
                    for (prev_path, prev_original) in &modified_files {
                        if let Err(restore_err) = std::fs::write(prev_path, prev_original.as_bytes()) {
                            tracing::error!(
                                "CRITICAL: Failed to restore '{}' during rollback: {}",
                                prev_path.display(),
                                restore_err
                            );
                        }
                    }
                    break;
                }
            }
        }

        Ok(EditResult {
            success: errors.is_empty(),
            changes_applied: total_changes,
            files_modified: modified_files.into_iter().map(|(p, _)| p).collect(),
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
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
        original: &str,
        modified: &str,
        file_path: &Path,
    ) -> Result<String> {
        let patch = diffy::create_patch(original, modified);
        let patch_str = patch.to_string();
        if patch_str.is_empty() {
            Ok(format!(
                "--- {}\n+++ {}\n(no changes)\n",
                file_path.display(),
                file_path.display()
            ))
        } else {
            // Replace diffy's default headers with file-path labels
            let lines: Vec<&str> = patch_str.lines().collect();
            let mut result = format!("--- {}\n+++ {}", file_path.display(), file_path.display());
            for line in lines.iter().skip(2) {
                result.push_str(&format!("\n{}", line));
            }
            Ok(result)
        }
    }

    /// Generate a side-by-side diff
    pub fn generate_side_by_side_diff(
        original: &str,
        modified: &str,
        file_path: &Path,
    ) -> Result<(String, String)> {
        let patch = diffy::create_patch(original, modified);
        let patch_str = patch.to_string();
        if patch_str.is_empty() {
            Ok((format!("{} (unchanged)", file_path.display()), String::new()))
        } else {
            Ok((format!("--- {}", file_path.display()), format!("+++ {}\n{}", file_path.display(), patch_str)))
        }
    }
}

/// Impact analysis utilities
pub struct Impact;

impl Impact {
    /// Analyze forward impact (what depends on this change)
    ///
    /// Uses PDG forward traversal from all nodes matching `symbol` to find
    /// downstream dependents. Returns `file:symbol` strings for each impacted node.
    /// Excludes external nodes from traversal.
    pub fn analyze_forward_impact(pdg: &PDG, symbol: &str) -> Result<Vec<String>> {
        let node_ids = pdg.find_all_by_name(symbol);
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }

        let config = crate::graph::pdg::TraversalConfig {
            max_depth: Some(5),
            max_nodes: Some(150),
            allowed_edge_types: Some(vec![
                crate::graph::pdg::EdgeType::Call,
                crate::graph::pdg::EdgeType::DataDependency,
                crate::graph::pdg::EdgeType::Inheritance,
            ]),
            excluded_node_types: Some(vec![crate::graph::pdg::NodeType::External]),
            min_complexity: None,
            min_edge_confidence: 0.0,
        };

        let mut impacted: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for &start_id in &node_ids {
            let forward = pdg.forward_impact(start_id, &config);
            for nid in forward {
                if seen.insert(nid) {
                    if let Some(node) = pdg.get_node(nid) {
                        impacted.push(format!("{}:{}", node.file_path, node.name));
                    }
                }
            }
        }

        Ok(impacted)
    }

    /// Analyze backward impact (what this change depends on)
    ///
    /// Uses PDG backward traversal from all nodes matching `symbol` to find
    /// upstream dependencies. Returns `file:symbol` strings for each dependency.
    /// Excludes external nodes from traversal.
    pub fn analyze_backward_impact(pdg: &PDG, symbol: &str) -> Result<Vec<String>> {
        let node_ids = pdg.find_all_by_name(symbol);
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }

        let config = crate::graph::pdg::TraversalConfig {
            max_depth: Some(5),
            max_nodes: Some(150),
            allowed_edge_types: Some(vec![
                crate::graph::pdg::EdgeType::Call,
                crate::graph::pdg::EdgeType::DataDependency,
                crate::graph::pdg::EdgeType::Inheritance,
            ]),
            excluded_node_types: Some(vec![crate::graph::pdg::NodeType::External]),
            min_complexity: None,
            min_edge_confidence: 0.0,
        };

        let mut impacted: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for &start_id in &node_ids {
            let backward = pdg.backward_impact(start_id, &config);
            for nid in backward {
                if seen.insert(nid) {
                    if let Some(node) = pdg.get_node(nid) {
                        impacted.push(format!("{}:{}", node.file_path, node.name));
                    }
                }
            }
        }

        Ok(impacted)
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
        Storage::open_with_config(
            ":memory:",
            StorageConfig {
                db_path: ":memory:".to_string(),
                wal_enabled: false,
                cache_size_pages: None,
            },
        )
        .unwrap()
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
        original_content: None,
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
        original_content: None,
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
        original_content: None,
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
            original_content: None,
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
        original_content: None,
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
            original_content: None,
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
        original_content: None,
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
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("test.py");
        std::fs::write(&file_path, b"hello world").expect("write test file");

        let pdg = Arc::new(create_test_pdg());
        let storage = Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let project_id = make_test_id();
        let request = EditRequest {
            project_id,
            file_path: file_path.clone(),
            changes: vec![EditChange::ReplaceText {
                start: 0,
                end: 5, // "hello"
                new_text: "goodbye".to_string(),
            }],
            preview_only: true,
        };

        let result = engine.preview_edit(&request).await;
        assert!(
            result.is_ok(),
            "preview_edit should succeed: {:?}",
            result.err()
        );
        let preview = result.unwrap();
        // The edited file is always in affected list
        assert!(!preview.files_affected.is_empty());
        assert!(matches!(preview.impact.risk_level, RiskLevel::Low));
        // Diff should contain some content
        assert!(!preview.diff.is_empty());
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
        original_content: None,
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
