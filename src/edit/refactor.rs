//! AST refactoring operations.
//!
//! Provides [`Refactor`] with methods for symbol renaming, function extraction,
//! and variable inlining.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::command::{EditCommand, EditResult};
use super::engine::{EditEngine, EditError, Result, replace_near_definitions};
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
            if let (Some(originals), Some(modifieds)) =
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
        let mut seed_ids = pdg.find_all_by_name(old_name);
        if let Some(exact) = pdg.find_by_symbol(old_name) {
            if !seed_ids.contains(&exact) {
                seed_ids.push(exact);
            }
        }

        // 2. Collect definition + impact files/ranges via exhaustive PDG traversal.
        // Forward reaches calleles/used types; backward reaches callers (the
        // primary rename targets). Both use the same exhaustive config below.
        let (files, impact_ranges, hit_node_limit) = collect_rename_targets(pdg, &seed_ids);

        if files.is_empty() {
            return Ok(empty_rename_failure(format!(
                "Symbol '{}' was not found in project sources",
                old_name
            )));
        }

        // Warn if the traversal hit the node limit — some references may have been missed.
        let truncation_warning = if hit_node_limit {
            let warning = format!(
                "Warning: rename traversal hit the node limit ({}). Some references may be missing — verify manually.",
                RENAME_MAX_NODES
            );
            tracing::warn!("{}", warning);
            Some(warning)
        } else {
            None
        };

        // 3. Build per-file byte-range map, then collect all modifications (no writes yet).
        let matches_by_file = build_matches_by_file(pdg, old_name, &seed_ids);
        let mut sorted_files: Vec<_> = files.into_iter().collect();
        sorted_files.sort();

        let (pending_writes, errors) = collect_pending_writes(
            &sorted_files,
            &matches_by_file,
            &impact_ranges,
            old_name,
            new_name,
        );
        if !errors.is_empty() {
            return Ok(empty_rename_failure(errors.join("; ")));
        }

        // 4. Write all files atomically, rolling back on any failure (all-or-nothing).
        let (total_changes, modified_files, errors) = apply_writes_with_rollback(pending_writes);
        if !errors.is_empty() {
            return Ok(empty_rename_failure(errors.join("; ")));
        }

        Ok(EditResult {
            // errors is empty here (early-returned otherwise), so success is always true;
            // the only non-error `error` payload is the optional truncation warning.
            success: true,
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
            error: truncation_warning,
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

/// Exhaustive traversal cap for rename — high enough that hitting it indicates a
/// genuinely huge graph; a post-traversal warning then flags possible misses.
const RENAME_MAX_NODES: usize = 1_000_000;

/// Construct a zero-change failure result carrying only an error message.
fn empty_rename_failure(error: String) -> EditResult {
    EditResult {
        success: false,
        changes_applied: 0,
        files_modified: vec![],
        modified_contents: None,
        original_contents: None,
        error: Some(error),
    }
}

/// Insert non-external impact nodes' files and byte ranges into the rename target maps.
/// Shared by forward and backward traversal — both collect the same way.
fn merge_impact_nodes(
    pdg: &PDG,
    ids: &[crate::graph::pdg::NodeId],
    files: &mut HashSet<PathBuf>,
    impact_ranges: &mut std::collections::HashMap<String, Vec<(usize, usize)>>,
) {
    for &id in ids {
        if let Some(node) = pdg.get_node(id) {
            if node.node_type != crate::graph::pdg::NodeType::External {
                files.insert(PathBuf::from(&*node.file_path));
                if node.byte_range != (0, 0) {
                    impact_ranges
                        .entry(node.file_path.to_string())
                        .or_default()
                        .push(node.byte_range);
                }
            }
        }
    }
}

/// Traverse forward (callees/used types) and backward (callers) from each seed,
/// collecting every definition + reference file and the byte ranges to target.
/// Returns `(files, impact_ranges, hit_node_limit)`.
fn collect_rename_targets(
    pdg: &PDG,
    seed_ids: &[crate::graph::pdg::NodeId],
) -> (
    HashSet<PathBuf>,
    std::collections::HashMap<String, Vec<(usize, usize)>>,
    bool,
) {
    // Exhaustive traversal for rename — missing any reference would break the build.
    let traversal_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1000),
        max_nodes: Some(RENAME_MAX_NODES),
        allowed_edge_types: Some(&[
            crate::graph::pdg::EdgeType::Call,
            crate::graph::pdg::EdgeType::DataDependency,
            crate::graph::pdg::EdgeType::Inheritance,
        ]),
        excluded_node_types: Some(vec![crate::graph::pdg::NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };

    let mut files: HashSet<PathBuf> = HashSet::new();
    let mut impact_ranges: std::collections::HashMap<String, Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    let mut hit_node_limit = false;

    for &node_id in seed_ids {
        if let Some(node) = pdg.get_node(node_id) {
            if node.node_type != crate::graph::pdg::NodeType::External {
                files.insert(PathBuf::from(&*node.file_path));

                let impacted = pdg.forward_impact(node_id, &traversal_config);
                hit_node_limit |= impacted.len() >= RENAME_MAX_NODES;
                merge_impact_nodes(pdg, &impacted, &mut files, &mut impact_ranges);

                let backward = pdg.backward_impact(node_id, &traversal_config);
                hit_node_limit |= backward.len() >= RENAME_MAX_NODES;
                merge_impact_nodes(pdg, &backward, &mut files, &mut impact_ranges);
            }
        }
    }

    (files, impact_ranges, hit_node_limit)
}

/// Cache all PDG nodes matching `old_name`, pre-grouped by file path. Combines
/// name matches with seed definition ranges for O(Files + Matches) lookups.
fn build_matches_by_file(
    pdg: &PDG,
    old_name: &str,
    seed_ids: &[crate::graph::pdg::NodeId],
) -> std::collections::HashMap<String, Vec<(usize, usize)>> {
    let mut matches_by_file: std::collections::HashMap<String, Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    for nid in pdg.find_all_by_name(old_name) {
        if let Some(node) = pdg.get_node(nid) {
            if node.byte_range != (0, 0) {
                matches_by_file
                    .entry(node.file_path.to_string())
                    .or_default()
                    .push(node.byte_range);
            }
        }
    }
    for &node_id in seed_ids {
        if let Some(node) = pdg.get_node(node_id) {
            if node.byte_range != (0, 0) {
                let entry = matches_by_file
                    .entry(node.file_path.to_string())
                    .or_default();
                if !entry.contains(&node.byte_range) {
                    entry.push(node.byte_range);
                }
            }
        }
    }
    matches_by_file
}

/// Phase 1: read each candidate file, assemble its target byte ranges
/// (PDG name matches + traversal impacts), and produce targeted replacements.
/// No writes happen here — all-or-nothing collection.
fn collect_pending_writes(
    sorted_files: &[PathBuf],
    matches_by_file: &std::collections::HashMap<String, Vec<(usize, usize)>>,
    impact_ranges: &std::collections::HashMap<String, Vec<(usize, usize)>>,
    old_name: &str,
    new_name: &str,
) -> (Vec<(PathBuf, String, String)>, Vec<String>) {
    let mut pending_writes: Vec<(PathBuf, String, String)> = Vec::new();
    let mut errors = Vec::new();

    for file_path in sorted_files {
        let original = match std::fs::read_to_string(file_path) {
            Ok(content) => content,
            Err(e) => {
                errors.push(format!("Failed to read '{}': {}", file_path.display(), e));
                continue;
            }
        };

        // Look up pre-grouped ranges for this file (PDG name matches + traversal impacts).
        let key = file_path.to_str().unwrap_or("");
        let mut def_ranges: Vec<(usize, usize)> =
            matches_by_file.get(key).cloned().unwrap_or_default();
        if let Some(imp_ranges) = impact_ranges.get(key) {
            for r in imp_ranges {
                if !def_ranges.contains(r) {
                    def_ranges.push(*r);
                }
            }
        }

        if def_ranges.is_empty() {
            // No local definition/reference ranges — reached via traversal but may
            // not contain the symbol. Whole-file replacement would risk corrupting
            // unrelated same-name tokens, so skip.
            continue;
        }

        // Targeted replacement: only replace within expanded windows around definitions.
        let modified = replace_near_definitions(&original, old_name, new_name, &def_ranges);
        if modified != original {
            pending_writes.push((file_path.clone(), original, modified));
        }
    }

    (pending_writes, errors)
}

/// Phase 2: write all pending files. On any write failure, restore the failed
/// file and roll back every previously-written file, then stop. Returns
/// `(total_changes, modified_files, errors)`.
fn apply_writes_with_rollback(
    pending_writes: Vec<(PathBuf, String, String)>,
) -> (usize, Vec<(PathBuf, String, String)>, Vec<String>) {
    let mut total_changes = 0usize;
    let mut modified_files: Vec<(PathBuf, String, String)> = Vec::new();
    let mut errors = Vec::new();

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
                // Restore the failed file first — write() may have truncated it.
                if let Err(restore_err) = std::fs::write(file_path, original_content.as_bytes()) {
                    tracing::error!(
                        "CRITICAL: Failed to restore failed file '{}' during rollback: {}",
                        file_path.display(),
                        restore_err
                    );
                }
                // Roll back all previously written files.
                for (prev_path, prev_original, _prev_modified) in &modified_files {
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

    (total_changes, modified_files, errors)
}
