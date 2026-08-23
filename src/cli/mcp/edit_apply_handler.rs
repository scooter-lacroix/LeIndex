use super::edit_cache::{EditCacheEntry, GLOBAL_EDIT_CACHE};
use super::edit_preview_handler::EditPreviewHandler;
use super::helpers::{
    apply_changes_in_memory, extract_bool, extract_string, parse_edit_changes,
    validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::{ResolvedEditChange, atomic_write_with_expected_async};
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
    let mut affected_files: Vec<_> = files.into_iter().collect();
    affected_files.sort();
    let mut response = serde_json::json!({
        "success": true,
        "changes_applied": changes_applied,
        "file_path": path.to_string_lossy(),
        "edit_region": region,
        "affected_symbols": nodes,
        "affected_files": affected_files,
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
        "leindex_edit_apply"
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
            // Delegate to preview, but wrap the preview payload in an
            // explicit dry-run envelope. Delegating raw made the apply
            // renderer read the preview-shaped payload (no `success` field)
            // as "Edit apply failed" with no detail — a dry-run that
            // reports failure without a reason is worse than none (N-09).
            let preview = EditPreviewHandler.execute(registry, args).await?;
            let mut envelope = serde_json::json!({
                "success": true,
                "dry_run": true,
                "changes_applied": 0,
                "message": "Dry run: no changes written. See `preview` for the diff and validation.",
            });
            if let (Some(obj), Some(preview_obj)) = (envelope.as_object_mut(), preview.as_object())
            {
                for (key, value) in preview_obj {
                    if key == "content" || key == "isError" {
                        continue;
                    }
                    obj.insert(key.clone(), value.clone());
                }
            }
            return Ok(envelope);
        }

        let (file_path, project_path_arg, provided_token) = apply_request_args(&args)?;
        let handle = registry.get_or_create(project_path_arg.as_deref()).await?;

        // 0. Best-effort PDG load. Applying a plain text edit must never be
        // blocked by a degraded/unavailable index: the atomic write + expected-
        // content guard is the safety net, and impact/validation simply report
        // as unavailable when no PDG can be loaded.
        let pdg_loaded = {
            let mut guard = handle.write().await;
            match guard.ensure_pdg_loaded_graph_only() {
                Ok(()) => guard.pdg().is_some(),
                Err(error) => {
                    tracing::warn!(
                        project = %guard.project_path().display(),
                        "PDG unavailable for edit-apply; continuing without PDG impact analysis: {error}"
                    );
                    false
                }
            }
        };

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

        // 5. Invalidate the registry's staleness cache so the next
        // read tool re-runs `is_stale_fast` instead of reusing a
        // pre-write `false` cached result. The watcher (when enabled)
        // does this on its own reindex path; this explicit call
        // covers the watcher-disabled default mode where the
        // 30-second negative-cache TTL would otherwise silently
        // mask the edit.
        let project_root = {
            let guard = handle.read().await;
            guard.project_path().to_path_buf()
        };
        registry.invalidate_stale_cache(&project_root).await;

        // 6. Build the response NOW — the edit is durable and every field
        // is already computed (impact runs against the pre-edit PDG loaded
        // above; validation ran before the write). The MCP caller must
        // receive this the moment it exists: the incremental reindex used
        // to run inline here under the project write lock, holding the
        // response for seconds-to-minutes (and queuing behind any other
        // lock holder) until MCP clients timed out while the file had in
        // fact been edited.
        let impact = {
            let guard = handle.read().await;
            edit_impact(guard.pdg(), &changes, &canonical_path)
        };

        let mut response = edit_response(
            &canonical_path,
            changes.len(),
            edit_region(&original, &modified),
            impact,
            validation_json,
        );
        if !pdg_loaded {
            if let Some(obj) = response.as_object_mut() {
                obj.insert("pdg_status".to_string(), serde_json::json!("not_loaded"));
                obj.insert(
                    "warning".to_string(),
                    serde_json::json!(
                        "Index unavailable — edit applied with file-level safety only \
                        (no PDG impact analysis or validation). Reindex (LeIndex [Index] with \
                        force_reindex=true) to restore full editing safeguards."
                    ),
                );
            }
        }

        let guard = handle.read().await;
        let response = wrap_with_meta(response, &guard);
        drop(guard);

        // 7. Index maintenance AFTER the response value exists. One-shot
        // CLI processes run it inline (the process exits once the response
        // is printed, so a spawned task would be killed mid-write); the
        // long-running MCP server/daemon spawns it so the caller's latency
        // is exactly the edit, never the reindex.
        if registry.is_one_shot() {
            let mut guard = handle.write().await;
            if let Err(e) = guard.incremental_reindex_from_watcher() {
                tracing::warn!("Failed to refresh index after edit-apply: {}", e);
            }
        } else {
            let registry = Arc::clone(registry);
            let handle = Arc::clone(&handle);
            tokio::spawn(async move {
                // Serialized behind the project write lock: concurrent
                // edit-applies queue their refreshes instead of racing.
                let mut guard = handle.write().await;
                let root = guard.project_path().to_path_buf();
                if let Err(e) = guard.incremental_reindex_from_watcher() {
                    tracing::warn!(
                        project = %root.display(),
                        "Background refresh after edit-apply failed: {e}"
                    );
                }
                drop(guard);
                registry.invalidate_stale_cache(&root).await;
                tracing::debug!(
                    project = %root.display(),
                    "Background refresh after edit-apply complete"
                );
            });
        }

        Ok(response)
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
                (Some(old), Some(new)) => Ok(serde_json::json!([{
                    "type": "replace_text",
                    "old_text": old,
                    "new_text": new
                }])),
                _ => Err(JsonRpcError::invalid_params(
                    "Provide either 'changes' array or 'old_text'+'new_text' for simple replacement",
                )),
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
