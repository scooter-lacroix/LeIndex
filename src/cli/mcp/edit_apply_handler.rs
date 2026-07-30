use super::edit_cache::{EditCacheEntry, GLOBAL_EDIT_CACHE};
use super::edit_preview_handler::EditPreviewHandler;
use super::helpers::{
    apply_changes_in_memory, extract_bool, extract_string, parse_edit_changes,
    validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::{atomic_write_with_expected_async, ResolvedEditChange};
use crate::validation::validation_to_json;
use serde_json::Value;
use std::sync::Arc;

type EditImpact = (Vec<String>, std::collections::HashSet<String>, Vec<String>);

type PreparedEdit = (String, String, Vec<crate::edit::EditChange>);

fn apply_request_args(
    args: &Value,
) -> Result<(String, Option<String>, Option<String>), JsonRpcError> {
    Ok((
        extract_string(args, "file_path")?,
        args.get("project_path")
            .and_then(Value::as_str)
            .map(str::to_owned),
        args.get("preview_token")
            .and_then(Value::as_str)
            .map(str::to_owned),
    ))
}

fn cached_edit(entry: Option<EditCacheEntry>, token: &str) -> Result<PreparedEdit, JsonRpcError> {
    let entry = entry.ok_or_else(|| {
        JsonRpcError::invalid_params(
            "No cached preview found for this file — request a new preview",
        )
    })?;
    if entry.preview_token != token {
        return Err(JsonRpcError::invalid_params(
            "preview token mismatch — request a new preview",
        ));
    }
    Ok((entry.original_text, entry.modified_text, entry.changes))
}

async fn apply_atomic(
    path: &std::path::Path,
    original: &str,
    modified: &str,
) -> Result<bool, JsonRpcError> {
    atomic_write_with_expected_async(
        path.to_path_buf(),
        modified.as_bytes().to_vec(),
        original.as_bytes().to_vec(),
    )
    .await
    .map_err(|error| {
        JsonRpcError::internal_error(format!("Failed to write '{}': {}", path.display(), error))
    })
}

async fn ensure_write_succeeded(
    success: bool,
    storage_path: &std::path::Path,
    canonical_path: &std::path::Path,
) -> Result<(), JsonRpcError> {
    if success {
        return Ok(());
    }
    GLOBAL_EDIT_CACHE.clear(storage_path, canonical_path).await;
    Err(JsonRpcError::invalid_params(
        "Edit rejected: file content changed on disk since preview was generated. \
        Please call LeIndex [Edit Preview] again (tool: leindex.edit-preview).",
    ))
}

fn validate_edit(
    validator: Option<crate::validation::LogicValidator>,
    path: &std::path::Path,
    original: &str,
    modified: &str,
) -> Option<Value> {
    let validator = validator?;
    let change =
        ResolvedEditChange::new(path.to_path_buf(), original.to_owned(), modified.to_owned());
    match validator.validate_changes(&[change]) {
        Ok(result) => Some(validation_to_json(&result)),
        Err(error) => {
            tracing::warn!("Validation check failed: {}", error);
            None
        }
    }
}

fn edit_impact(
    pdg: Option<&crate::graph::pdg::ProgramDependenceGraph>,
    changes: &[crate::edit::EditChange],
    path: &std::path::Path,
) -> EditImpact {
    let mut nodes = Vec::new();
    let mut files = std::collections::HashSet::new();
    files.insert(path.to_string_lossy().to_string());
    let mut breaking = Vec::new();
    let Some(pdg) = pdg else {
        return (nodes, files, breaking);
    };
    for change in changes {
        let crate::edit::EditChange::RenameSymbol { old_name, .. } = change else {
            continue;
        };
        let node_id = pdg
            .find_by_symbol(old_name)
            .or_else(|| pdg.find_by_name(old_name))
            .or_else(|| pdg.find_by_name_in_file(old_name, Some(&path.to_string_lossy())));
        let Some(node_id) = node_id else {
            continue;
        };
        for dependency in pdg.forward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
        ) {
            if let Some(node) = pdg.get_node(dependency) {
                nodes.push(node.name.clone());
                files.insert(node.file_path.to_string());
            }
        }
        let callers = pdg.backward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
        );
        if !callers.is_empty() {
            breaking.push(format!(
                "Renaming '{}' may break {} caller(s)",
                old_name,
                callers.len()
            ));
        }
    }
    (nodes, files, breaking)
}

fn edit_region(original: &str, modified: &str) -> String {
    let modified_lines: Vec<&str> = modified.lines().collect();
    let original_lines: Vec<&str> = original.lines().collect();
    let shared_len = original_lines.len().min(modified_lines.len());
    let first_diff = original_lines
        .iter()
        .zip(modified_lines.iter())
        .position(|(old, new)| old != new)
        .unwrap_or(shared_len);
    let start = first_diff.saturating_sub(5);
    let end = (first_diff + 10).min(modified_lines.len());
    modified_lines[start..end]
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{}: {}", start + index + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn edit_response(
    path: &std::path::Path,
    changes_applied: usize,
    region: String,
    impact: EditImpact,
    validation: Option<Value>,
) -> Value {
    let (nodes, files, breaking) = impact;
    let mut response = serde_json::json!({
        "success": true,
        "changes_applied": changes_applied,
        "file_path": path.to_string_lossy(),
        "edit_region": region,
        "affected_symbols": nodes,
        "affected_files": files.into_iter().collect::<Vec<_>>(),
        "breaking_changes": breaking,
    });
    if let (Some(validation), Some(object)) = (validation, response.as_object_mut()) {
        object.insert("validation".to_string(), validation);
    }
    response
}

/// Handler for LeIndex [edit_apply — atomic code modifications.
#[derive(Clone)]
pub struct EditApplyHandler;

#[allow(missing_docs)]
impl EditApplyHandler {
    pub fn name(&self) -> &str {
        "leindex.edit-apply"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Edit Apply]"
    }

    pub fn description(&self) -> &str {
        "PRIMARY file editor — use instead of edit_file. Simple mode: provide file_path + \
old_text + new_text for exact replacement. Advanced mode: use changes[] array for \
multiple or byte-offset edits. Supports dry_run=true for preview."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or project-relative path. Relative paths resolve against the project root."
                },
                "old_text": {
                    "type": "string",
                    "description": "Simple mode: text to find and replace (exact match)"
                },
                "old_str": {
                    "type": "string",
                    "description": "Alias for old_text (compatibility with edit_file)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Simple mode: replacement text"
                },
                "new_str": {
                    "type": "string",
                    "description": "Alias for new_text (compatibility with edit_file)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "changes": {
                    "type": "array",
                    "description": "Advanced mode: list of changes to apply. Each has type (replace_text/rename_symbol) and type-specific fields.",
                    "items": { "type": "object" }
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, return preview without modifying files (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "preview_token": {
                    "type": "string",
                    "description": "The token returned by a previous LeIndex [Edit Preview] (tool: leindex.edit-preview) call. Required if using cached preview."
                }
            },
            "required": ["file_path"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let dry_run = extract_bool(&args, "dry_run", false);

        if dry_run {
            // Delegate to preview
            return EditPreviewHandler.execute(registry, args).await;
        }

        let (file_path, project_path_arg, provided_token) = apply_request_args(&args)?;
        let handle = registry.get_or_create(project_path_arg.as_deref()).await?;

        // 0. Ensure PDG is loaded for BOTH branches (parsing and impact analysis)
        {
            let mut guard = handle.write().await;
            guard
                .ensure_pdg_loaded()
                .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;
        }

        // 1. Resolve path and check cache (avoid awaiting while holding lock)
        let (canonical_path, storage_path) = {
            let guard = handle.read().await;
            let canonical = validate_file_within_project(&file_path, guard.project_path())?;
            (canonical, guard.storage_path().to_path_buf())
        };

        let cached_entry = GLOBAL_EDIT_CACHE.get(&storage_path, &canonical_path).await;

        let (original, modified, changes) = self
            .get_edit_content(
                provided_token,
                cached_entry,
                &canonical_path,
                &file_path,
                &args,
            )
            .await?;

        // If no changes, nothing to do
        if modified == original {
            GLOBAL_EDIT_CACHE
                .clear(&storage_path, &canonical_path)
                .await;
            let guard = handle.read().await;
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "success": true,
                    "changes_applied": 0,
                    "message": "No changes to apply (content identical)"
                }),
                &guard,
            ));
        }

        let validation_json = {
            let guard = handle.read().await;
            validate_edit(
                guard.create_validator(),
                &canonical_path,
                &original,
                &modified,
            )
        };

        let success = apply_atomic(&canonical_path, &original, &modified).await?;

        ensure_write_succeeded(success, &storage_path, &canonical_path).await?;

        // 4. Clear cache after successful apply
        GLOBAL_EDIT_CACHE
            .clear(&storage_path, &canonical_path)
            .await;

        // 5. Incremental reindex to refresh the index with the edited file changes
        // This ensures the index is fresh so subsequent tool calls don't show stale warnings
        let mut guard = handle.write().await;
        if let Err(e) = guard.incremental_reindex_from_watcher() {
            tracing::warn!("Failed to refresh index after edit-apply: {}", e);
            // Continue despite reindex failure - edit was applied successfully
        }
        let project_root = guard.project_path().to_path_buf();
        drop(guard); // Release write lock before continuing

        // 5a. Invalidate the registry's staleness cache so the next
        // read tool re-runs `is_stale_fast` instead of reusing a
        // pre-write `false` cached result. The watcher (when enabled)
        // does this on its own reindex path; this explicit call
        // covers the watcher-disabled default mode where the
        // 30-second negative-cache TTL would otherwise silently
        // mask the edit.
        registry.invalidate_stale_cache(&project_root).await;

        let impact = {
            let guard = handle.read().await;
            edit_impact(guard.pdg(), &changes, &canonical_path)
        };

        let response = edit_response(
            &canonical_path,
            changes.len(),
            edit_region(&original, &modified),
            impact,
            validation_json,
        );

        let guard = handle.read().await;
        Ok(wrap_with_meta(response, &guard))
    }

    fn get_changes_from_args(&self, args: &Value) -> Result<Value, JsonRpcError> {
        if let Some(changes) = args.get("changes").cloned() {
            Ok(changes)
        } else {
            let old_text = args
                .get("old_text")
                .or_else(|| args.get("old_str"))
                .and_then(|v| v.as_str());
            let new_text = args
                .get("new_text")
                .or_else(|| args.get("new_str"))
                .and_then(|v| v.as_str());
            match (old_text, new_text) {
                (Some(old), Some(new)) => {
                    Ok(serde_json::json!([{
                        "type": "replace_text",
                        "old_text": old,
                        "new_text": new
                    }]))
                }
                _ => {
                    Err(JsonRpcError::invalid_params(
                        "Provide either 'changes' array or 'old_text'+'new_text' for simple replacement"
                    ))
                }
            }
        }
    }

    async fn get_edit_content(
        &self,
        provided_token: Option<String>,
        cached_entry: Option<EditCacheEntry>,
        canonical_path: &std::path::Path,
        file_path: &str,
        args: &Value,
    ) -> Result<PreparedEdit, JsonRpcError> {
        if let Some(provided_token) = provided_token {
            cached_edit(cached_entry, &provided_token)
        } else {
            let original = tokio::fs::read_to_string(canonical_path)
                .await
                .map_err(|e| {
                    JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
                })?;

            let changes_val = self.get_changes_from_args(args)?;
            let changes = parse_edit_changes(&changes_val, Some(&original))?;
            let modified = apply_changes_in_memory(&original, &changes)?;
            Ok((original, modified, changes))
        }
    }
}
