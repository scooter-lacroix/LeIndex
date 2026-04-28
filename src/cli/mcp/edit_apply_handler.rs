use super::edit_preview_handler::EditPreviewHandler;
use super::helpers::{
    apply_changes_in_memory, extract_bool, extract_string, get_direct_callers,
    parse_edit_changes, validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
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
                _ => {
                    return Err(JsonRpcError::invalid_params(
                        "Provide either 'changes' array or 'old_text'+'new_text' for simple replacement"
                    ));
                }
            }
        };
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;

        // Enforce project boundary
        {
            let index = handle.lock().await;
            let _ = validate_file_within_project(&file_path, index.project_path())?;
        }

        // Read → parse (with content for text-search) → apply → write
        let original = std::fs::read_to_string(&file_path).map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        let changes = parse_edit_changes(&changes_val, Some(&original))?;
        let modified = apply_changes_in_memory(&original, &changes)?;

        if modified == original {
            let idx = handle.lock().await;
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "success": true,
                    "changes_applied": 0,
                    "files_modified": [],
                    "message": "No changes needed — content already matches"
                }),
                &idx,
            ));
        }

        std::fs::write(&file_path, modified.as_bytes()).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to write '{}': {}", file_path, e))
        })?;

        // Build verification context: show the edited region so LLM doesn't need to Read
        let modified_lines: Vec<&str> = modified.lines().collect();

        // Find the first differing line to show relevant context
        let original_lines: Vec<&str> = original.lines().collect();
        let first_diff_line = original_lines
            .iter()
            .zip(modified_lines.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(0);

        // Show ±5 lines around the edit point
        let ctx_start = first_diff_line.saturating_sub(5);
        let ctx_end = (first_diff_line + 10).min(modified_lines.len());
        let edit_region: Vec<String> = modified_lines[ctx_start..ctx_end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", ctx_start + i + 1, line))
            .collect();

        // Compact affected callers — eliminates follow-up Grep for breakage
        let affected_callers: Vec<String> = {
            let idx = handle.lock().await;
            if let Some(pdg) = idx.pdg() {
                let nodes = pdg.nodes_in_file(&file_path);
                let mut callers: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for &nid in &nodes {
                    for &cid in &get_direct_callers(pdg, nid) {
                        if let Some(cn) = pdg.get_node(cid) {
                            if cn.file_path != file_path {
                                callers.insert(format!("{}:{}", cn.file_path, cn.name));
                            }
                        }
                    }
                }
                callers.into_iter().take(15).collect()
            } else {
                Vec::new()
            }
        };

        let idx = handle.lock().await;
        Ok(wrap_with_meta(
            serde_json::json!({
                "success": true,
                "changes_applied": changes.len(),
                "files_modified": [&file_path],
                "edit_region": edit_region.join("\n"),
                "external_callers": affected_callers
            }),
            &idx,
        ))
    }
}
