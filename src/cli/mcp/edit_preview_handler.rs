use super::helpers::*;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
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
        "Preview code changes without applying them. Returns a unified diff and \
        validates that the targets exist in the source."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "changes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["replace_text", "rename_symbol"] },
                            "old_text": { "type": "string" },
                            "new_text": { "type": "string" },
                            "start_byte": { "type": "integer" },
                            "end_byte": { "type": "integer" }
                        }
                    }
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (optional)"
                }
            },
            "required": ["file_path", "changes"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let file_path = extract_string(&args, "file_path")?;
        let changes_val = args.get("changes").ok_or_else(|| JsonRpcError::invalid_params("Missing 'changes'"))?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;

        let canonical = validate_file_within_project(&file_path, index.project_path())?;
        let content = std::fs::read_to_string(&canonical).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to read file: {}", e))
        })?;

        let changes = parse_edit_changes(changes_val, Some(&content))?;
        let modified = apply_changes_in_memory(&content, &changes)?;
        let diff = make_diff(&content, &modified, &file_path);

        Ok(wrap_with_meta(serde_json::json!({
            "file_path": file_path,
            "diff": diff,
            "status": "ok"
        }), &index))
    }
}
