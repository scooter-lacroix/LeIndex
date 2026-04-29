//! Edit engine, worktree management, diff generation, and impact analysis.
//!
//! Contains the core [`EditEngine`] struct, [`WorktreeManager`],
//! [`WorktreeSession`], [`Diff`], and [`Impact`] utilities.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::command::{
    EditChange, EditCommand, EditPreview, EditRequest, EditResult, ImpactAnalysis, RiskLevel,
};
use super::history::EditHistory;
use crate::graph::pdg::ProgramDependenceGraph as PDG;
use crate::storage::{Storage, UniqueProjectId};

/// Error types for the edit engine.
#[derive(thiserror::Error, Debug)]
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

// ---- Helper functions ----

pub(super) fn clamp_to_char_boundary(content: &str, idx: usize) -> usize {
    let mut i = idx.min(content.len());
    while i > 0 && !content.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(super) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Replace `old` with `new` only within windows around known definition byte ranges.
/// Each window extends from the definition start minus a context buffer to the end
/// plus a buffer, covering nearby references that the PDG traversal identified.
/// Falls back gracefully for files without usable byte ranges.
pub fn replace_near_definitions(
    content: &str,
    old: &str,
    new: &str,
    def_ranges: &[(usize, usize)],
) -> String {
    if old.is_empty() || def_ranges.is_empty() {
        return content.to_owned();
    }

    // Context buffer (bytes) around each definition for targeted replacement.
    // Covers typical function bodies plus surrounding references.
    const REPLACE_CONTEXT_BYTES: usize = 2048;

    // Build sorted, non-overlapping windows around each definition
    let ctx = REPLACE_CONTEXT_BYTES;
    let mut windows: Vec<(usize, usize)> = def_ranges
        .iter()
        .map(|&(s, e)| {
            let start = clamp_to_char_boundary(content, s.saturating_sub(ctx));
            let end = clamp_to_char_boundary(content, (e + ctx).min(content.len()));
            (start, end)
        })
        .collect();
    windows.sort_by_key(|w| w.0);

    // Merge overlapping windows
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for w in windows {
        if let Some(last) = merged.last_mut() {
            if w.0 <= last.1 {
                last.1 = last.1.max(w.1);
                continue;
            }
        }
        merged.push(w);
    }

    // Find all whole-word match positions on the FULL content first,
    // then filter to matches within the merged windows.
    // This avoids false word boundaries at slice edges.
    let mut matches_in_windows: Vec<(usize, usize)> = Vec::new(); // (start, end)
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
            // Check if this match falls within any merged window
            for &(win_start, win_end) in &merged {
                if start >= win_start && end <= win_end {
                    matches_in_windows.push((start, end));
                    break;
                }
            }
        }
    }

    // Build result: replace matched positions, copy everything else verbatim
    let mut result = String::with_capacity(content.len());
    let mut pos = 0usize;
    for (start, end) in &matches_in_windows {
        if *start > pos {
            result.push_str(&content[pos..*start]);
        }
        result.push_str(new);
        pos = *end;
    }
    if pos < content.len() {
        result.push_str(&content[pos..]);
    }
    result
}

/// Replace all whole-word occurrences of `old` with `new` in `content`.
pub fn replace_whole_word(content: &str, old: &str, new: &str) -> String {
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

// ---- EditEngine ----

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

        // Capture original content for undo — fail fast if pre-image unreadable
        let original_content = Some(std::fs::read_to_string(&request.file_path).map_err(|e| {
            EditError::Generic(format!(
                "Failed to capture original content for '{}': {}",
                request.file_path.display(),
                e
            ))
        })?);

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
                        modified_contents: None,
                        original_contents: None,
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        // Merge worktree back to main
        session.merge().await?;

        // Capture modified content for redo
        let modified_content = std::fs::read_to_string(&request.file_path).ok();

        // Record in history
        let mut history = self.history.lock().await;
        history.record_command(EditCommand::Edit {
            project_id: request.project_id.clone(),
            file_path: request.file_path.clone(),
            changes: request.changes.clone(),
            timestamp: chrono::Utc::now(),
            original_content,
            modified_content,
        });

        Ok(EditResult {
            success: true,
            changes_applied,
            files_modified,
            modified_contents: None,
            original_contents: None,
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
        Self::format_unified_diff(original, modified, file_path)
    }

    /// Generate a unified diff (public alias used by other call sites).
    pub fn generate_unified_diff(
        &self,
        original: &str,
        modified: &str,
        file_path: &Path,
    ) -> Result<String> {
        Self::format_unified_diff(original, modified, file_path)
    }

    /// Shared implementation: create a diffy patch and replace its default
    /// "--- original" / "+++ modified" headers with the actual file path.
    pub(crate) fn format_unified_diff(
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
                result.push('\n');
                result.push_str(line);
            }
            result.push('\n');
            Ok(result)
        }
    }

    /// Apply a single EditChange to a string in memory, returning the modified string.
    ///
    /// Used by `preview_edit` to compute the diff without touching the filesystem.
    pub(crate) fn apply_change_to_string(
        &self,
        content: &str,
        change: &EditChange,
    ) -> Result<String> {
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
    pub(crate) async fn analyze_impact(&self, request: &EditRequest) -> Result<ImpactAnalysis> {
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
                                affected_files.insert(PathBuf::from(&*dep_node.file_path));
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
                                    affected_files.insert(PathBuf::from(&*bn.file_path));
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
                                affected_files.insert(PathBuf::from(&*dep_node.file_path));
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
        // Note: single sync fs::write — acceptable overhead for a single file restore.
        // If this evolves to multi-file undo, wrap in spawn_blocking.
        let mut history = self.history.lock().await;
        // Extract the command data before potentially modifying history again
        let cmd = history.undo().cloned();
        match cmd {
            Some(EditCommand::Edit {
                file_path,
                original_content,
                ..
            }) => {
                // Restore original content if available
                if let Some(content) = original_content {
                    if let Err(e) = std::fs::write(&file_path, content.as_bytes()) {
                        // Restore failed — revert the history cursor so undo/redo stay consistent
                        history.redo();
                        return Ok(EditResult {
                            success: false,
                            changes_applied: 0,
                            files_modified: vec![],
                            modified_contents: None,
                            original_contents: None,
                            error: Some(format!(
                                "Failed to restore '{}': {}",
                                file_path.display(),
                                e
                            )),
                        });
                    }
                } else {
                    // No pre-image was captured — revert cursor, cannot reliably undo
                    history.redo();
                    return Ok(EditResult {
                        success: false,
                        changes_applied: 0,
                        files_modified: vec![],
                        modified_contents: None,
                        original_contents: None,
                        error: Some(format!(
                            "Cannot undo '{}': original content was not captured",
                            file_path.display()
                        )),
                    });
                }
                Ok(EditResult {
                    success: true,
                    changes_applied: 1,
                    files_modified: vec![file_path.clone()],
                    modified_contents: None,
                    original_contents: None,
                    error: None,
                })
            }
            Some(EditCommand::Rename {
                original_contents, ..
            }) => {
                // Restore all files to their pre-rename state (all-or-nothing).
                // On partial failure, leave cursor in the undone state (don't revert
                // to "renamed") so the user can retry or manually fix.
                let mut restored = Vec::new();
                let mut errors = Vec::new();
                for (file_path, content) in original_contents {
                    match std::fs::write(&file_path, content.as_bytes()) {
                        Ok(()) => restored.push(PathBuf::from(file_path)),
                        Err(e) => errors.push(format!("Failed to restore '{}': {}", file_path, e)),
                    }
                }
                if !errors.is_empty() {
                    // Leave cursor in undone state — don't call history.redo().
                    // The files are partially restored; the user needs to resolve manually.
                    return Ok(EditResult {
                        success: false,
                        changes_applied: restored.len(),
                        files_modified: restored,
                        modified_contents: None,
                        original_contents: None,
                        error: Some(format!(
                            "Partial undo (cursor left in undone state): {}",
                            errors.join("; ")
                        )),
                    });
                }
                Ok(EditResult {
                    success: true,
                    changes_applied: restored.len(),
                    files_modified: restored,
                    modified_contents: None,
                    original_contents: None,
                    error: None,
                })
            }
            Some(EditCommand::RollbackPoint { .. }) | None => Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                modified_contents: None,
                original_contents: None,
                error: Some("No edit to undo".to_string()),
            }),
        }
    }

    /// Redo last undone edit
    pub async fn redo(&self) -> Result<EditResult> {
        let mut history = self.history.lock().await;
        match history.redo() {
            Some(EditCommand::Edit {
                file_path,
                modified_content,
                ..
            }) => {
                // Write the modified content back to the file
                if let Some(content) = modified_content {
                    std::fs::write(file_path, content.as_bytes()).map_err(|e| {
                        EditError::Generic(format!(
                            "Failed to write '{}': {}",
                            file_path.display(),
                            e
                        ))
                    })?;
                }
                Ok(EditResult {
                    success: true,
                    changes_applied: 1,
                    files_modified: vec![file_path.clone()],
                    modified_contents: None,
                    original_contents: None,
                    error: None,
                })
            }
            Some(EditCommand::Rename {
                modified_contents,
                original_contents,
                ..
            }) => {
                // Re-apply the exact post-rename content for each file.
                // Uses modified_contents (the precise result of replace_near_definitions)
                // rather than re-running replace_whole_word which could corrupt
                // comments, strings, or unrelated same-name tokens.
                // All-or-nothing semantics: on any I/O failure, rollback to originals.
                let mut re_applied = Vec::new();
                let mut failed = None;
                for (file_path, post_rename_content) in modified_contents {
                    match std::fs::write(file_path, post_rename_content.as_bytes()) {
                        Ok(()) => re_applied.push(PathBuf::from(file_path)),
                        Err(e) => {
                            failed = Some(e);
                            break;
                        }
                    }
                }

                if failed.is_some() {
                    // Rollback: restore all successfully written files to their pre-redo state
                    // (original_contents holds the pre-rename content, which is the undone state)
                    for (fp, content) in original_contents {
                        let _ = std::fs::write(fp, content.as_bytes());
                    }
                    return Ok(EditResult {
                        success: false,
                        changes_applied: 0,
                        files_modified: vec![],
                        modified_contents: None,
                        original_contents: None,
                        error: Some(
                            "Redo failed: I/O error during rename re-application, rolled back"
                                .to_string(),
                        ),
                    });
                }

                Ok(EditResult {
                    success: true,
                    changes_applied: re_applied.len(),
                    files_modified: re_applied,
                    modified_contents: None,
                    original_contents: None,
                    error: None,
                })
            }
            Some(_) | None => Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                modified_contents: None,
                original_contents: None,
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
                modified_contents: None,
                original_contents: None,
                error: None,
            }),
            None => Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                modified_contents: None,
                original_contents: None,
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

// ---- WorktreeManager ----

/// Worktree manager for isolated edit sessions
pub struct WorktreeManager {
    /// Base path for worktree directories
    pub base_path: PathBuf,
}

impl Default for WorktreeManager {
    fn default() -> Self {
        Self::new()
    }
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
        std::fs::create_dir_all(&self.base_path).map_err(|e| {
            EditError::WorktreeError(format!(
                "Failed to create worktree base '{}': {}",
                self.base_path.display(),
                e
            ))
        })?;

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
                EditError::WorktreeError(format!("Failed to read worktree directory entry: {}", e))
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

// ---- WorktreeSession ----

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
    pub(crate) fn track_file(&mut self, original: PathBuf, staged: PathBuf) {
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
        // Spawn blocking since merge performs synchronous file I/O
        tokio::task::spawn_blocking(move || Self::merge_blocking(self))
            .await
            .map_err(|e| EditError::WorktreeError(format!("Merge task panicked: {}", e)))?
    }

    /// Synchronous merge implementation — performs file I/O.
    /// Separated from the async wrapper to avoid blocking the executor.
    fn merge_blocking(session: WorktreeSession) -> Result<()> {
        let WorktreeSession {
            path,
            tracked_files,
        } = session;

        let mut staged_entries: Vec<(PathBuf, PathBuf)> = tracked_files.into_iter().collect();
        staged_entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Phase 1 (prepare): read all staged files, back up originals, create dirs.
        // If any prepare step fails, nothing has been written yet — just return the error.
        let mut backups: Vec<(PathBuf, bool, Option<String>)> = Vec::new();
        // TODO: The two-phase approach reads all staged content into memory for
        // rollback safety. For projects with very large files, consider a streaming
        // approach with temp-file renames for atomicity without full buffering.
        let mut prepared: Vec<(PathBuf, String)> = Vec::new(); // (original, staged_content)

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

            prepared.push((original.clone(), content));
        }

        // Phase 2 (write): apply all prepared writes. On failure, roll back everything.
        for (written, (original, content)) in prepared.iter().enumerate() {
            if let Err(e) = std::fs::write(original, content.as_bytes()) {
                // Roll back all previously written files AND the failed file
                for (backup_path, file_existed, backup_content) in
                    backups.iter().take(written + 1).rev()
                {
                    if !file_existed {
                        let _ = std::fs::remove_file(backup_path);
                    } else if let Some(previous) = backup_content {
                        if let Err(restore_err) = std::fs::write(backup_path, previous.as_bytes()) {
                            tracing::error!(
                                "CRITICAL: Failed to restore '{}' during rollback: {}",
                                backup_path.display(),
                                restore_err
                            );
                        }
                    }
                }
                return Err(EditError::WorktreeError(format!(
                    "Failed to merge staged file into '{}': {}",
                    original.display(),
                    e
                )));
            }
        }

        // Cleanup: remove the worktree directory
        if path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                // Post-commit cleanup failure — log but don't fail the merge
                tracing::warn!("Failed to clean up worktree '{}': {}", path.display(), e);
            }
        }
        Ok(())
    }
}

// ---- Diff generation utilities ----

/// Diff generation utilities
pub struct Diff;

impl Diff {
    /// Generate a unified diff
    pub fn generate_unified_diff(
        original: &str,
        modified: &str,
        file_path: &Path,
    ) -> Result<String> {
        EditEngine::format_unified_diff(original, modified, file_path)
    }

    /// Generate a split unified diff tuple (left header, right header + body).
    ///
    /// Despite the name, this returns a pair of strings representing the `---` and
    /// `+++` sides of a unified diff (not a true side-by-side layout). The first
    /// element is the `--- {file_path}` header; the second is `+++ {file_path}`
    /// followed by the diff body lines.
    pub fn generate_side_by_side_diff(
        original: &str,
        modified: &str,
        file_path: &Path,
    ) -> Result<(String, String)> {
        let patch = diffy::create_patch(original, modified);
        let patch_str = patch.to_string();
        if patch_str.is_empty() {
            Ok((
                format!("{} (unchanged)", file_path.display()),
                String::new(),
            ))
        } else {
            // diffy's output already contains ---/+++ headers with generic labels;
            // replace them with the actual file path
            let lines: Vec<&str> = patch_str.lines().collect();
            let body = lines.iter().skip(2).cloned().collect::<Vec<_>>().join("\n");

            Ok((
                format!("--- {}", file_path.display()),
                format!("+++ {}\n{}", file_path.display(), body),
            ))
        }
    }
}

// ---- Impact analysis utilities ----

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
            allowed_edge_types: Some(&[
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
            allowed_edge_types: Some(&[
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
