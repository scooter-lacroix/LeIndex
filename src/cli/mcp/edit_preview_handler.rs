use super::helpers::{
    apply_changes_in_memory, extract_string, make_diff, parse_edit_changes,
    validate_file_within_project, wrap_with_meta, HandlerContext,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::ResolvedEditChange;
use crate::validation::validation_to_json;
use serde_json::Value;
use std::path::PathBuf;
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
                    "description": "Absolute path to the file to edit"
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

        let mut ctx = HandlerContext::new(registry, &args).await?;

        // Enforce project boundary
        let _ = validate_file_within_project(&file_path, ctx.project_path())?;

        // Read file content
        let original = std::fs::read_to_string(&file_path).map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        // Parse changes with content available for text-search resolution
        let changes = parse_edit_changes(&changes_val, Some(&original))?;

        // Apply changes in memory
        let modified = apply_changes_in_memory(&original, &changes)?;

        // Generate diff
        let diff = make_diff(&original, &modified, &file_path);

        // Run validation via LogicValidator (if PDG available)
        let validation_json = match ctx.index_mut().create_validator() {
            Some(validator) => {
                // Convert parsed EditChanges to ResolvedEditChanges for validation
                let resolved: Vec<ResolvedEditChange> = changes
                    .iter()
                    .map(|_c| {
                        ResolvedEditChange::new(
                            PathBuf::from(&file_path),
                            original.clone(),
                            modified.clone(),
                        )
                    })
                    .collect();

                match validator.validate_changes(&resolved) {
                    Ok(result) => Some(validation_to_json(&result)),
                    Err(e) => {
                        // Validation itself failed — include as a warning, don't block preview
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
        let mut affected_nodes: Vec<String> = Vec::new();
        let mut affected_files: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        affected_files.insert(file_path.clone());
        let mut breaking_changes: Vec<String> = Vec::new();

        {
            let pdg = ctx.pdg();
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
                                affected_nodes.push(dn.name.clone());
                                affected_files.insert(dn.file_path.clone());
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

        let risk = if affected_nodes.len() > 5 || affected_files.len() > 3 {
            "high"
        } else if affected_nodes.len() > 1 || affected_files.len() > 1 {
            "medium"
        } else {
            "low"
        };

        let mut response = serde_json::json!({
            "diff": diff,
            "affected_symbols": affected_nodes,
            "affected_files": affected_files.into_iter().collect::<Vec<_>>(),
            "breaking_changes": breaking_changes,
            "risk_level": risk,
            "change_count": changes.len()
        });

        // Include validation results (warnings only — preview is never blocked)
        if let Some(validation) = validation_json {
            if let Some(obj) = response.as_object_mut() {
                obj.insert("validation".to_string(), validation);
            }
        }

        Ok(wrap_with_meta(response, ctx.index()))
    }
}
