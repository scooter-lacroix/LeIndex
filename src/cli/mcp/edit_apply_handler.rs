use super::edit_cache::GLOBAL_EDIT_CACHE;
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

        let file_path = extract_string(&args, "file_path")?;
        let project_path_arg = args.get("project_path").and_then(|v| v.as_str());
        let provided_token = args.get("preview_token").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path_arg).await?;

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

        let (original, modified, changes) = if let Some(provided_token) = provided_token {
            // Strict token enforcement: if token is provided, it MUST be valid and fresh
            let cache = cached_entry.ok_or_else(|| {
                JsonRpcError::invalid_params(
                    "No cached preview found for this file — request a new preview",
                )
            })?;

            if cache.preview_token != provided_token {
                return Err(JsonRpcError::invalid_params(
                    "preview token mismatch — request a new preview",
                ));
            }

            // Freshness check: compare expected to disk content handled by atomic_write_with_expected_async
            (cache.original_text, cache.modified_text, cache.changes)
        } else {
            // No token provided - we need to parse changes (PDG already loaded above)
            let original = tokio::fs::read_to_string(&canonical_path)
                .await
                .map_err(|e| {
                    JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
                })?;

            let changes_val = self.get_changes_from_args(&args)?;
            let changes = parse_edit_changes(&changes_val, Some(&original))?;
            let modified = apply_changes_in_memory(&original, &changes)?;
            (original, modified, changes)
        };

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

        // 2. Validation (if validator available)
        let validation_json = {
            let guard = handle.read().await;
            match guard.create_validator() {
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
            }
        };

        // 3. Atomic write with compare-and-swap semantics (Drop all locks for IO)
        let success: bool = atomic_write_with_expected_async(
            canonical_path.clone(),
            modified.as_bytes().to_vec(),
            original.as_bytes().to_vec(),
        )
        .await
        .map_err(|e| {
            JsonRpcError::internal_error(format!(
                "Failed to write '{}': {}",
                canonical_path.display(),
                e
            ))
        })?;

        if !success {
            GLOBAL_EDIT_CACHE
                .clear(&storage_path, &canonical_path)
                .await;
            return Err(JsonRpcError::invalid_params(
                "Edit rejected: file content changed on disk since preview was generated. \
                Please call LeIndex [Edit Preview] again (tool: leindex.edit-preview).",
            ));
        }

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

        // 6. PDG Context Enrichment
        let mut affected_nodes: Vec<String> = Vec::new();
        let mut affected_files: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        affected_files.insert(canonical_path.to_string_lossy().to_string());
        let mut breaking_changes: Vec<String> = Vec::new();

        {
            let guard = handle.read().await;
            if let Some(pdg) = guard.pdg() {
                for change in &changes {
                    if let crate::edit::EditChange::RenameSymbol { old_name, .. } = change {
                        let found_id = pdg
                            .find_by_symbol(old_name)
                            .or_else(|| pdg.find_by_name(old_name))
                            .or_else(|| {
                                pdg.find_by_name_in_file(
                                    old_name,
                                    Some(&canonical_path.to_string_lossy()),
                                )
                            });

                        if let Some(node_id) = found_id {
                            for dep_id in pdg.forward_impact(
                                node_id,
                                &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                            ) {
                                if let Some(dn) = pdg.get_node(dep_id) {
                                    affected_nodes.push(dn.name.clone());
                                    affected_files.insert(dn.file_path.to_string());
                                }
                            }
                            let backward = pdg.backward_impact(
                                node_id,
                                &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                            );
                            if !backward.is_empty() {
                                breaking_changes.push(format!(
                                    "Renaming '{}' may break {} caller(s)",
                                    old_name,
                                    backward.len()
                                ));
                            }
                        }
                    }
                }
            }
        }

        // 6. Generate edit_region for LLM verification
        let modified_lines: Vec<&str> = modified.lines().collect();
        let original_lines: Vec<&str> = original.lines().collect();
        let shared_len = original_lines.len().min(modified_lines.len());
        let first_diff_line = original_lines
            .iter()
            .zip(modified_lines.iter())
            .position(|(old, new)| old != new)
            .unwrap_or(shared_len);

        let ctx_start = first_diff_line.saturating_sub(5);
        let ctx_end = (first_diff_line + 10).min(modified_lines.len());
        let edit_region: String = modified_lines[ctx_start..ctx_end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", ctx_start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        let mut response = serde_json::json!({
            "success": true,
            "changes_applied": changes.len(),
            "file_path": canonical_path.to_string_lossy(),
            "edit_region": edit_region,
            "affected_symbols": affected_nodes,
            "affected_files": affected_files.into_iter().collect::<Vec<_>>(),
            "breaking_changes": breaking_changes,
        });

        if let Some(val) = validation_json {
            if let Some(obj) = response.as_object_mut() {
                obj.insert("validation".to_string(), val);
            }
        }

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
}
