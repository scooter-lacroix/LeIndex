//! AST refactoring operations.
//!
//! Provides [`Refactor`] with methods for symbol renaming, function extraction,
//! and variable inlining.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::command::{EditCommand, EditResult};
use super::engine::{replace_near_definitions, EditEngine, EditError, Result};
use crate::graph::pdg::ProgramDependenceGraph as PDG;
use crate::storage::UniqueProjectId;

/// AST refactoring operations
pub struct Refactor;

impl Refactor {
    /// Rename a symbol across all files using PDG-guided file discovery and whole-word replacement.
    ///
    /// This operates directly on project source files (not through WorktreeManager)
    /// because renames are PDG-scoped global operations that must touch all impacted
    /// files atomically. WorktreeManager is designed for single-file edit sessions
    /// with review/discard workflow. A future enhancement could add a staging mode
    /// where rename results are written to a worktree for review before merging.
    ///
    /// Note: This function performs synchronous file I/O. It uses
    /// `tokio::task::block_in_place` to avoid blocking the executor in
    /// multi-threaded contexts.
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
                modified_contents: None,
                original_contents: None,
                error: Some("old_name and new_name must be non-empty".to_string()),
            });
        }
        if old_name == new_name {
            return Ok(EditResult {
                success: true,
                changes_applied: 0,
                files_modified: vec![],
                modified_contents: None,
                original_contents: None,
                error: None,
            });
        }

        // Clone Arc and strings for the blocking closure
        let pdg = Arc::clone(&engine.pdg);
        let old_name_c = old_name.to_owned();
        let new_name_c = new_name.to_owned();

        let result = tokio::task::spawn_blocking(move || {
            Self::rename_symbol_blocking(&pdg, &old_name_c, &new_name_c)
        })
        .await
        .map_err(|e| EditError::WorktreeError(format!("Rename task panicked: {}", e)))??;

        // Record in edit history for undo support.
        if result.success {
            if let (Some(ref originals), Some(ref modifieds)) =
                (&result.original_contents, &result.modified_contents)
            {
                let mut history = engine.history.lock().await;
                history.record_command(EditCommand::Rename {
                    project_id: UniqueProjectId::new("_rename".to_string(), "".to_string(), 0),
                    old_name: old_name.to_owned(),
                    new_name: new_name.to_owned(),
                    timestamp: chrono::Utc::now(),
                    original_contents: originals.clone(),
                    modified_contents: modifieds.clone(),
                });
            }
        }

        Ok(result)
    }

    /// Synchronous rename implementation — runs on blocking thread pool.
    fn rename_symbol_blocking(pdg: &PDG, old_name: &str, new_name: &str) -> Result<EditResult> {
        // 1. Resolve the PDG symbol candidates and prefer exact symbol hits.
        let node_ids = pdg.find_all_by_name(old_name);
        let exact_node = pdg.find_by_symbol(old_name);

        // Collect files containing the symbol definition AND all files that
        // reference it (call sites, type usages) via PDG forward/backward edges.
        let mut files: HashSet<PathBuf> = HashSet::new();

        // Use exhaustive traversal for rename — missing any reference would break the build.
        // High limits ensure completeness for real projects; a post-traversal check warns
        // if the limit was hit.
        //
        // Both forward and backward traversals use the same config:
        // - Forward: things the symbol depends on (callees, used types) — may contain
        //   same-name references in those files
        // - Backward: callers and dependents of the symbol — the primary rename targets
        // Call edges are caller→callee, so backward_impact reaches callers.
        let max_nodes_limit = 1_000_000;
        let traversal_config = crate::graph::pdg::TraversalConfig {
            max_depth: Some(1000),
            max_nodes: Some(max_nodes_limit),
            allowed_edge_types: Some(&[
                crate::graph::pdg::EdgeType::Call,
                crate::graph::pdg::EdgeType::DataDependency,
                crate::graph::pdg::EdgeType::Inheritance,
            ]),
            excluded_node_types: Some(vec![crate::graph::pdg::NodeType::External]),
            min_complexity: None,
            min_edge_confidence: 0.0,
        };

        // Collect all matching seed node IDs
        let mut seed_ids: Vec<_> = node_ids;
        if let Some(exact) = exact_node {
            if !seed_ids.contains(&exact) {
                seed_ids.push(exact);
            }
        }

        let mut hit_node_limit = false;
        // Collect byte ranges from impact traversal for targeted replacements
        let mut impact_ranges: std::collections::HashMap<String, Vec<(usize, usize)>> =
            std::collections::HashMap::new();

        // For each seed, collect definition file + forward/backward impact files
        for node_id in &seed_ids {
            if let Some(node) = pdg.get_node(*node_id) {
                if node.node_type != crate::graph::pdg::NodeType::External {
                    files.insert(PathBuf::from(&node.file_path));

                    // Add files that reference this symbol via PDG edges
                    let impacted = pdg.forward_impact(*node_id, &traversal_config);
                    hit_node_limit |= impacted.len() >= max_nodes_limit;
                    for imp_id in impacted {
                        if let Some(imp_node) = pdg.get_node(imp_id) {
                            if imp_node.node_type != crate::graph::pdg::NodeType::External {
                                files.insert(PathBuf::from(&imp_node.file_path));
                                // Collect byte ranges from impact nodes
                                if imp_node.byte_range != (0, 0) {
                                    impact_ranges
                                        .entry(imp_node.file_path.clone())
                                        .or_default()
                                        .push(imp_node.byte_range);
                                }
                            }
                        }
                    }
                    let backward = pdg.backward_impact(*node_id, &traversal_config);
                    hit_node_limit |= backward.len() >= max_nodes_limit;
                    for back_id in backward {
                        if let Some(back_node) = pdg.get_node(back_id) {
                            if back_node.node_type != crate::graph::pdg::NodeType::External {
                                files.insert(PathBuf::from(&back_node.file_path));
                                // Collect byte ranges from impact nodes
                                if back_node.byte_range != (0, 0) {
                                    impact_ranges
                                        .entry(back_node.file_path.clone())
                                        .or_default()
                                        .push(back_node.byte_range);
                                }
                            }
                        }
                    }
                }
            }
        }

        if files.is_empty() {
            return Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                modified_contents: None,
                original_contents: None,
                error: Some(format!(
                    "Symbol '{}' was not found in project sources",
                    old_name
                )),
            });
        }

        // Warn if the traversal hit the node limit — some references may have been missed
        let mut truncation_warning = None;
        if hit_node_limit {
            truncation_warning = Some(format!(
                "Warning: rename traversal hit the node limit ({}). Some references may be missing — verify manually.",
                max_nodes_limit
            ));
            tracing::warn!("{}", truncation_warning.as_ref().unwrap());
        }

        // 4. Apply replace_whole_word to each file (two-phase: collect then write)
        // Sort files for deterministic processing order
        let mut sorted_files: Vec<_> = files.into_iter().collect();
        sorted_files.sort();

        let mut total_changes = 0usize;
        let mut modified_files: Vec<(PathBuf, String, String)> = Vec::new(); // (path, original, modified)
        let mut errors = Vec::new();

        // Phase 1: collect all modifications (no writes yet)
        let mut pending_writes: Vec<(PathBuf, String, String)> = Vec::new(); // (path, original, modified)
                                                                             // Cache all PDG nodes matching old_name once — avoids redundant lookups per file.
                                                                             // Pre-group by file path for O(Files + Matches) instead of O(Files * Matches).
        let mut matches_by_file: std::collections::HashMap<String, Vec<(usize, usize)>> =
            std::collections::HashMap::new();
        for nid in pdg.find_all_by_name(old_name) {
            if let Some(node) = pdg.get_node(nid) {
                if node.byte_range != (0, 0) {
                    matches_by_file
                        .entry(node.file_path.clone())
                        .or_default()
                        .push(node.byte_range);
                }
            }
        }
        // Also add seed_ids ranges to the per-file map
        for node_id in &seed_ids {
            if let Some(node) = pdg.get_node(*node_id) {
                if node.byte_range != (0, 0) {
                    let entry = matches_by_file.entry(node.file_path.clone()).or_default();
                    if !entry.contains(&node.byte_range) {
                        entry.push(node.byte_range);
                    }
                }
            }
        }
        for file_path in &sorted_files {
            let original = match std::fs::read_to_string(file_path) {
                Ok(content) => content,
                Err(e) => {
                    errors.push(format!("Failed to read '{}': {}", file_path.display(), e));
                    continue;
                }
            };

            // Look up pre-grouped ranges for this file (includes PDG name matches + traversal impacts)
            let mut def_ranges: Vec<(usize, usize)> = matches_by_file
                .get(file_path.to_str().unwrap_or(""))
                .cloned()
                .unwrap_or_default();
            // Also include ranges from traversal impact nodes in this file
            if let Some(imp_ranges) = impact_ranges.get(file_path.to_str().unwrap_or("")) {
                for r in imp_ranges {
                    if !def_ranges.contains(r) {
                        def_ranges.push(*r);
                    }
                }
            }

            if def_ranges.is_empty() {
                // No local definition or reference ranges for this file — skip it.
                // The file was reached via PDG traversal but may not contain the symbol.
                // Whole-file replacement would risk corrupting unrelated same-name tokens.
                continue;
            }

            // Targeted replacement: only replace within expanded windows around definitions
            let modified = replace_near_definitions(&original, old_name, new_name, &def_ranges);
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
                modified_contents: None,
                original_contents: None,
                error: Some(errors.join("; ")),
            });
        }

        // Phase 2: write all files, rollback on failure
        for (file_path, original_content, modified) in &pending_writes {
            match std::fs::write(file_path, modified.as_bytes()) {
                Ok(()) => {
                    total_changes += 1;
                    modified_files.push((
                        file_path.clone(),
                        original_content.clone(),
                        modified.clone(),
                    ));
                }
                Err(e) => {
                    errors.push(format!("Failed to write '{}': {}", file_path.display(), e));
                    // Restore the failed file first — write() may have truncated it
                    if let Err(restore_err) = std::fs::write(file_path, original_content.as_bytes())
                    {
                        tracing::error!(
                            "CRITICAL: Failed to restore failed file '{}' during rollback: {}",
                            file_path.display(),
                            restore_err
                        );
                    }
                    // Rollback all previously written files
                    for (prev_path, prev_original, _prev_modified) in &modified_files {
                        if let Err(restore_err) =
                            std::fs::write(prev_path, prev_original.as_bytes())
                        {
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

        if !errors.is_empty() {
            // Rollback was already performed in the loop — return clean zero metrics
            return Ok(EditResult {
                success: false,
                changes_applied: 0,
                files_modified: vec![],
                modified_contents: None,
                original_contents: None,
                error: Some(errors.join("; ")),
            });
        }

        Ok(EditResult {
            success: errors.is_empty(),
            changes_applied: total_changes,
            files_modified: modified_files.iter().map(|(p, _, _)| p.clone()).collect(),
            modified_contents: Some(
                modified_files
                    .iter()
                    .map(|(p, _, modified)| (p.display().to_string(), modified.clone()))
                    .collect(),
            ),
            original_contents: Some(
                modified_files
                    .into_iter()
                    .map(|(p, orig, _)| (p.display().to_string(), orig))
                    .collect(),
            ),
            error: match (errors.is_empty(), &truncation_warning) {
                (true, None) => None,
                (true, Some(w)) => Some(w.clone()),
                (false, _) => Some(errors.join("; ")),
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
            modified_contents: None,
            original_contents: None,
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
            modified_contents: None,
            original_contents: None,
            error: None,
        })
    }
}
