use super::edit_cache::GLOBAL_EDIT_CACHE;
use super::edit_preview_handler::EditPreviewHandler;
use super::helpers::{
    apply_changes_in_memory, extract_bool, extract_string, parse_edit_changes,
    validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::{atomic_write_async, ResolvedEditChange};
use crate::validation::validation_to_json;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_edit_apply — atomic code modifications.
#[derive(Clone)]
pub struct EditApplyHandler;

#[allow(missing_docs)]
impl EditApplyHandler {
    pub fn name(&self) -> &str {
        "leindex_edit_apply"
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
                    "description": "Absolute path to the file to edit"
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
                    "description": "The token returned by a previous leindex_edit_preview call. Required if using cached preview."
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

        let file_path = extract_string(&args, "file_path")?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let provided_token = args.get("preview_token").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        // Ensure PDG is loaded
        guard
            .ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        // Enforce project boundary
        let canonical_path = validate_file_within_project(&file_path, guard.project_path())?;

        // 1. Check Edit Cache for a match
        let cached_entry = GLOBAL_EDIT_CACHE.get(guard.storage_path(), &canonical_path).await;
        
        let (original, modified, changes) = if let Some(cache) = cached_entry {
            // If cache exists, verify it matches the current request
            let token_matches = provided_token.map(|t| t == cache.preview_token).unwrap_or(false);
            
            // Freshness check: read the file from disk and compare it to cache.original_text
            let disk_content = tokio::fs::read_to_string(&canonical_path).await.map_err(|e| {
                JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
            })?;

            if token_matches && disk_content == cache.original_text {
                (cache.original_text, cache.modified_text, cache.changes)
            } else {
                // Cache stale or token mismatch - discard cache and proceed with provided args
                GLOBAL_EDIT_CACHE.clear(guard.storage_path(), &canonical_path).await;
                
                let changes_val = self.get_changes_from_args(&args)?;
                let changes = parse_edit_changes(&changes_val, Some(&disk_content))?;
                let modified = apply_changes_in_memory(&disk_content, &changes)?;
                (disk_content, modified, changes)
            }
        } else {
            // No cache - proceed with provided arguments
            let original = tokio::fs::read_to_string(&canonical_path).await.map_err(|e| {
                JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
            })?;

            let changes_val = self.get_changes_from_args(&args)?;
            let changes = parse_edit_changes(&changes_val, Some(&original))?;
            let modified = apply_changes_in_memory(&original, &changes)?;
            (original, modified, changes)
        };

        // If no changes, nothing to do
        if modified == original {
            return Ok(wrap_with_meta(serde_json::json!({
                "success": true,
                "changes_applied": 0,
                "message": "No changes to apply (content identical)"
            }), &guard));
        }

        // Run validation
        let _validation_json = match guard.create_validator() {
            Some(validator) => {
                let resolved = ResolvedEditChange::new(
                    canonical_path.clone(),
                    original.clone(),
                    modified.clone(),
                );

                match validator.validate_changes(&[resolved]) {
                    Ok(result) => Some(validation_to_json(&result)),
                    Err(e) => {
                        tracing::warn!("Validation check failed: {}", e);
                        None
                    }
                }
            }
            None => None,
        };

        // Atomic write
        atomic_write_async(canonical_path.clone(), modified.as_bytes().to_vec())
            .await
            .map_err(|e| {
                JsonRpcError::internal_error(format!(
                    "Failed to write '{}': {}",
                    canonical_path.display(),
                    e
                ))
            })?;

        // Clear cache after successful apply
        GLOBAL_EDIT_CACHE.clear(guard.storage_path(), &canonical_path).await;

        // PDG Context Enrichment
        let mut affected_nodes: Vec<String> = Vec::new();
        let mut affected_files: std::collections::HashSet<String> = std::collections::HashSet::new();
        
        if let Some(pdg) = guard.pdg() {
            for change in &changes {
                if let crate::edit::EditChange::RenameSymbol { old_name, .. } = change {
                    if let Some(node_id) = pdg.find_by_symbol(old_name).or_else(|| pdg.find_by_name(old_name)) {
                        for dep_id in pdg.forward_impact(node_id, &crate::graph::pdg::TraversalConfig::for_impact_analysis()) {
                            if let Some(dn) = pdg.get_node(dep_id) {
                                affected_nodes.push(dn.name.clone());
                                affected_files.insert(dn.file_path.to_string());
                            }
                        }
                    }
                }
            }
        }

        let response = serde_json::json!({
            "success": true,
            "changes_applied": changes.len(),
            "file_path": canonical_path.to_string_lossy(),
            "affected_symbols": affected_nodes,
            "affected_files": affected_files.into_iter().collect::<Vec<_>>(),
        });

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
}
