//! leedit - Code Editing Engine
//!
//! *Le Edit* (The Editing) - AST-based code editing with tree-sitter and git worktree isolation

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

// Submodules
mod command;
mod engine;
mod history;
mod refactor;

// Re-exports from legraphe
pub use crate::graph::pdg::{Edge, EdgeType, Node, NodeType, ProgramDependenceGraph as PDG};

// Re-exports from lestockage
pub use crate::storage::{Storage, StorageConfig, UniqueProjectId};

// Public API re-exports from command module
pub use command::{
    EditChange, EditCommand, EditPreview, EditRequest, EditResult, EditType, ImpactAnalysis,
    ResolvedEditChange, RiskLevel,
};

// Public API re-exports from engine module
pub use engine::{
    replace_near_definitions, replace_whole_word, Diff, EditEngine, EditError, Impact, Result,
    WorktreeManager, WorktreeSession,
};

// Public API re-exports from history module
pub use history::EditHistory;

// Public API re-exports from refactor module
pub use refactor::Refactor;

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
        let file_path = std::path::PathBuf::from("test.py");
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
            file_path: std::path::PathBuf::from("test.py"),
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
            file_path: std::path::PathBuf::from("test.py"),
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
            file_path: std::path::PathBuf::from("test.py"),
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
                file_path: std::path::PathBuf::from(format!("test{}.py", i)),
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
            file_path: std::path::PathBuf::from("test3.py"),
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
                file_path: std::path::PathBuf::from(format!("test{}.py", i)),
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
            file_path: std::path::PathBuf::from("new.py"),
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
        assert_eq!(
            manager.base_path,
            std::path::PathBuf::from("/tmp/leedit-worktrees")
        );
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
            modified_contents: None,
            original_contents: None,
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
            files_modified: vec![std::path::PathBuf::from("test.py")],
            modified_contents: None,
            original_contents: None,
            error: None,
        };

        assert!(result.success);
        assert_eq!(result.changes_applied, 5);
        assert_eq!(result.files_modified.len(), 1);
    }

    #[tokio::test]
    async fn test_edit_engine_creation() {
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());

        let engine = EditEngine::new(pdg, storage);
        assert!(engine.is_ok());
    }

    #[tokio::test]
    async fn test_edit_engine_preview_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("test.py");
        std::fs::write(&file_path, b"hello world").expect("write test file");

        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
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
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let (index, len) = engine.history_state().await;
        assert_eq!(index, 0);
        assert_eq!(len, 0);
    }

    #[tokio::test]
    async fn test_edit_engine_rollback_point() {
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let result = engine.create_rollback_point("test_point".to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_edit_engine_undo_no_history() {
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let result = engine.undo().await;
        assert!(result.is_ok());
        let edit_result = result.unwrap();
        assert!(!edit_result.success);
        assert!(edit_result.error.is_some());
    }

    #[tokio::test]
    async fn test_edit_engine_redo_no_history() {
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        let result = engine.redo().await;
        assert!(result.is_ok());
        let edit_result = result.unwrap();
        assert!(!edit_result.success);
        assert!(edit_result.error.is_some());
    }

    #[tokio::test]
    async fn test_edit_engine_rollback_nonexistent() {
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
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
            file_path: std::path::PathBuf::from("test.py"),
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
        let error = EditError::FileNotFound(std::path::PathBuf::from("missing.py"));
        let msg = format!("{}", error);
        assert!(msg.contains("missing.py"));
    }

    #[test]
    fn test_edit_error_invalid_range() {
        let error = EditError::InvalidRange {
            start: 10,
            end: 5,
            file: std::path::PathBuf::from("test.py"),
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
            files_modified: vec![std::path::PathBuf::from("test.py")],
            modified_contents: None,
            original_contents: None,
            error: None,
        };
        let cloned = result.clone();
        assert_eq!(result.success, cloned.success);
        assert_eq!(result.changes_applied, cloned.changes_applied);
    }

    #[test]
    fn test_refactor_rename_symbol() {
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
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
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        // Test that extract compiles
        let _result = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(Refactor::extract_function(
                &engine,
                std::path::Path::new("test.py"),
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
        let pdg = std::sync::Arc::new(create_test_pdg());
        let storage = std::sync::Arc::new(make_test_storage());
        let engine = EditEngine::new(pdg, storage).unwrap();

        // Test that inline compiles
        let _result = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(Refactor::inline_variable(
                &engine,
                std::path::Path::new("test.py"),
                "myVar",
            ))
        })
        .join()
        .unwrap();

        // In test environment, just verify no panic
    }
}
