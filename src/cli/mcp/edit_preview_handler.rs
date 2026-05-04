use super::edit_cache::{EditCacheEntry, GLOBAL_EDIT_CACHE};
use super::helpers::{
    apply_changes_in_memory, extract_string, make_diff, parse_edit_changes,
    validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::ResolvedEditChange;
use crate::validation::validation_to_json;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_edit_preview — dry-run for code changes.
#[derive(Clone)]
pub struct EditPreviewHandler;

#[allow(missing_docs)]
impl EditPreviewHandler {
    pub fn name(&self) -> &str {
        "leindex_edit_preview"
    }

    pub fn description(&self) -> &str {
        "Preview a code edit: unified diff, affected symbols/files, breaking changes, and risk \
level — all before touching the filesystem. No equivalent in standard tools. Run before \
leindex_edit_apply to understand the blast radius of your change."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or project-relative path to the file to edit. Relative paths resolve against the project root."
                },
                "old_text": {
                    "type": "string",
                    "description": "Simple mode: text to find and replace (exact match)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Simple mode: replacement text"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "changes": {
                    "type": "array",
                    "description": "Advanced mode: list of changes to preview. Each has 'type' (replace_text/rename_symbol) and type-specific fields.",
                    "items": { "type": "object" }
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
        let file_path = extract_string(&args, "file_path")?;

        // Support simple mode: top-level old_text/new_text (or old_str/new_str aliases)
        let changes_val = if let Some(changes) = args.get("changes").cloned() {
            changes
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
                    serde_json::json!([{
                        "type": "replace_text",
                        "old_text": old,
                        "new_text": new
                    }])
                }
                _ => Value::Array(vec![]),
            }
        };

        let project_path_arg = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path_arg).await?;

        // 1. Resolve path and ensure PDG loaded with WRITE lock (if needed)
        let (abs_file_path, storage_path) = {
            let mut guard = handle.write().await;
            guard
                .ensure_pdg_loaded()
                .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

            if guard.pdg().is_none() {
                return Err(JsonRpcError::project_not_indexed(
                    guard.project_path().display().to_string(),
                ));
            }

            let abs = validate_file_within_project(&file_path, guard.project_path())?;
            (abs, guard.storage_path().to_path_buf())
        };
        // Write lock dropped

        // 2. Read file content (IO without lock)
        let original = tokio::fs::read_to_string(&abs_file_path).await.map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        // 3. Parse changes and apply in memory (CPU without lock)
        let changes = parse_edit_changes(&changes_val, Some(&original))?;
        let modified = apply_changes_in_memory(&original, &changes)?;

        let preview_token = blake3::hash(
            format!("{}{}", abs_file_path.display(), chrono::Utc::now().to_rfc3339()).as_bytes(),
        )
        .to_hex()
        .to_string();

        // 4. Store in cache (Best effort)
        let cache_entry = EditCacheEntry {
            file_path: abs_file_path.clone(),
            preview_token: preview_token.clone(),
            original_text: original.clone(),
            modified_text: modified.clone(),
            changes: changes.clone(),
            timestamp: chrono::Utc::now(),
        };

        if let Err(e) = GLOBAL_EDIT_CACHE.set(&storage_path, cache_entry).await {
            tracing::warn!("Failed to store edit in cache: {}", e);
        }

        // 5. Generate diff and compute impact
        let diff = make_diff(&original, &modified, &file_path);

        let (validation_json, affected_nodes, affected_files, breaking_changes) = {
            let guard = handle.read().await;
            
            // Run validation
            let validation = match guard.create_validator() {
                Some(validator) => {
                    let resolved = ResolvedEditChange::new(
                        abs_file_path.clone(),
                        original.clone(),
                        modified.clone(),
                    );

                    match validator.validate_changes(&[resolved]) {
                        Ok(result) => Some(validation_to_json(&result)),
                        Err(e) => {
                            Some(serde_json::json!({
                                "is_valid": true,
                                "has_errors": false,
                                "syntax_errors": [],
                                "reference_issues": [],
                                "semantic_drift": [],
                                "impact_report": null,
                                "validation_warning": format!("Validation check failed: {}", e),
                            }))
                        }
                    }
                }
                None => None,
            };

            // Compute impact from PDG
            let mut nodes: Vec<String> = Vec::new();
            let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
            files.insert(file_path.clone());
            let mut breaks: Vec<String> = Vec::new();

            if let Some(pdg) = guard.pdg() {
                for change in &changes {
                    if let crate::edit::EditChange::RenameSymbol {
                        old_name,
                        new_name: _,
                    } = change
                    {
                        // Try name-based lookup for PDG impact analysis
                        let found_id = pdg
                            .find_by_symbol(old_name)
                            .or_else(|| pdg.find_by_name(old_name))
                            .or_else(|| pdg.find_by_name_in_file(old_name, None));
                        if let Some(node_id) = found_id {
                            let forward = pdg.forward_impact(
                                node_id,
                                &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                            );
                            for dep_id in &forward {
                                if let Some(dn) = pdg.get_node(*dep_id) {
                                    nodes.push(dn.name.clone());
                                    files.insert(dn.file_path.to_string());
                                }
                            }
                            let backward = pdg.backward_impact(
                                node_id,
                                &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
                            );
                            if !backward.is_empty() {
                                breaks.push(format!(
                                    "Renaming '{}' may break {} caller(s)",
                                    old_name,
                                    backward.len()
                                ));
                            }
                        }
                    }
                }
            }
            (validation, nodes, files, breaks)
        };

        let risk = if affected_nodes.len() > 5 || affected_files.len() > 3 {
            "high"
        } else if affected_nodes.len() > 1 || affected_files.len() > 1 {
            "medium"
        } else {
            "low"
        };

        let mut response = serde_json::json!({
            "preview_token": preview_token,
            "diff": diff,
            "affected_symbols": affected_nodes,
            "affected_files": affected_files.into_iter().collect::<Vec<_>>(),
            "breaking_changes": breaking_changes,
            "risk_level": risk,
            "change_count": changes.len()
        });

        // Include validation results
        if let Some(validation) = validation_json {
            if let Some(obj) = response.as_object_mut() {
                obj.insert("validation".to_string(), validation);
            }
        }

        let guard = handle.read().await;
        Ok(wrap_with_meta(response, &guard))
    }
}
