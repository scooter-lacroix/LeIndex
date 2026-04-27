use super::helpers::*;
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
        "Apply code changes to a file and trigger incremental re-indexing. Supports \
        multiple simultaneous edits (search/replace, symbol rename) in one atomic operation."
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
                            "new_text": { "type": "string" }
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
        let mut index = handle.lock().await;

        let canonical = validate_file_within_project(&file_path, index.project_path())?;
        let content = std::fs::read_to_string(&canonical).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to read file: {}", e))
        })?;

        let changes = parse_edit_changes(changes_val, Some(&content))?;
        let modified = apply_changes_in_memory(&content, &changes)?;
        
        std::fs::write(&canonical, &modified).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to write file: {}", e))
        })?;

        // Incremental re-indexing
        index.index_project(false).ok();

        Ok(wrap_with_meta(serde_json::json!({
            "file_path": file_path,
            "status": "success"
        }), &index))
    }
}
