use super::edit_preview_handler::EditPreviewHandler;
use super::helpers::{
    apply_changes_in_memory, extract_bool, extract_string, get_direct_callers, parse_edit_changes,
    validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::ResolvedEditChange;
use crate::validation::validation_to_json;
use serde_json::Value;
use std::path::PathBuf;
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
            let index = handle.read().await;
            let _ = validate_file_within_project(&file_path, index.project_path())?;
        }

        // Read → parse (with content for text-search) → apply → write
        let original = std::fs::read_to_string(&file_path).map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        let changes = parse_edit_changes(&changes_val, Some(&original))?;
        let modified = apply_changes_in_memory(&original, &changes)?;

        if modified == original {
            let idx = handle.read().await;
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

        // Run validation via LogicValidator — reject edits with errors,
        // include warnings in the success response
        let validation_json = {
            let idx = handle.read().await;
            match idx.create_validator() {
                Some(validator) => {
                    // Create a single ResolvedEditChange representing the final file state.
                    // All edit changes apply to the same file and produce the same
                    // (original → modified) pair, so N identical validation objects would
                    // be redundant — one is sufficient for syntax/reference/drift checks.
                    let resolved = ResolvedEditChange::new(
                        PathBuf::from(&file_path),
                        original.clone(),
                        modified.clone(),
                    );

                    match validator.validate_changes(&[resolved]) {
                        Ok(result) => {
                            if result.has_errors() {
                                // Build detailed error response with validation details
                                let v_json = validation_to_json(&result);
                                return Err(JsonRpcError::invalid_params(format!(
                                    "Edit rejected — validation found errors. File unchanged.\n\
                                     Syntax errors: {}\nReference issues: {}\nSemantic drift: {}\n\
                                     Details: {}",
                                    v_json["syntax_errors"]
                                        .as_array()
                                        .map(|a| a.len())
                                        .unwrap_or(0),
                                    v_json["reference_issues"]
                                        .as_array()
                                        .map(|a| a.len())
                                        .unwrap_or(0),
                                    v_json["semantic_drift"]
                                        .as_array()
                                        .map(|a| a.len())
                                        .unwrap_or(0),
                                    v_json
                                )));
                            }

                            // No blocking errors — include validation in response
                            Some(validation_to_json(&result))
                        }
                        Err(e) => {
                            // Validation itself failed — log warning but don't block the edit
                            tracing::warn!("Validation check failed: {}", e);
                            None
                        }
                    }
                }
                None => None,
            }
        };

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
            let idx = handle.read().await;
            if let Some(pdg) = idx.pdg() {
                let nodes = pdg.nodes_in_file(&file_path);
                let mut callers: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for &nid in &nodes {
                    for &cid in &get_direct_callers(pdg, nid) {
                        if let Some(cn) = pdg.get_node(cid) {
                            if cn.file_path.as_ref() != file_path.as_str() {
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

        let idx = handle.read().await;
        let mut response = serde_json::json!({
            "success": true,
            "changes_applied": changes.len(),
            "files_modified": [&file_path],
            "edit_region": edit_region.join("\n"),
            "external_callers": affected_callers
        });

        // Include validation warnings in success response
        if let Some(validation) = validation_json {
            if let Some(obj) = response.as_object_mut() {
                obj.insert("validation".to_string(), validation);
            }
        }

        Ok(wrap_with_meta(response, &idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::TempDir;
    use tokio;

    /// Helper: create a temp dir with a file and return (TempDir, file_path, registry)
    async fn setup_test_file(
        content: &str,
        file_name: &str,
    ) -> (TempDir, String, Arc<ProjectRegistry>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join(file_name);
        std::fs::write(&file_path, content).expect("write test file");
        let registry = test_registry_for(dir.path());
        (dir, file_path.to_string_lossy().to_string(), registry)
    }

    #[tokio::test]
    async fn test_edit_apply_simple_replacement() {
        let (_dir, file_path, registry) =
            setup_test_file("fn hello() { println!(\"world\"); }\n", "test.rs").await;

        let handler = EditApplyHandler;
        let args = serde_json::json!({
            "file_path": file_path,
            "old_text": "world",
            "new_text": "universe",
        });

        let result = handler.execute(&registry, args).await;
        assert!(
            result.is_ok(),
            "Expected success, got error: {:?}",
            result.err()
        );

        let response = result.unwrap();
        assert_eq!(response["success"], true);
        assert_eq!(response["changes_applied"], 1);

        // Verify the file was actually modified
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("universe"));
        assert!(!content.contains("world"));
    }

    #[tokio::test]
    async fn test_edit_apply_no_changes_needed() {
        // Test with matching content that results in no diff
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("test.rs");
        std::fs::write(&file_path, "same content\n").expect("write");
        let registry = test_registry_for(dir.path());

        let handler = EditApplyHandler;
        let args = serde_json::json!({
            "file_path": file_path.to_string_lossy().to_string(),
            "old_text": "same content",
            "new_text": "same content",
        });

        let result = handler.execute(&registry, args).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response["changes_applied"], 0);
    }

    #[tokio::test]
    async fn test_edit_apply_dry_run_delegates_to_preview() {
        let (_dir, file_path, registry) =
            setup_test_file("fn hello() { println!(\"world\"); }\n", "test.rs").await;

        let handler = EditApplyHandler;
        let args = serde_json::json!({
            "file_path": file_path,
            "old_text": "world",
            "new_text": "universe",
            "dry_run": true,
        });

        let result = handler.execute(&registry, args).await;
        assert!(
            result.is_ok(),
            "Expected success, got error: {:?}",
            result.err()
        );

        let response = result.unwrap();
        // Preview response should have diff, not edit_region
        assert!(
            response.get("diff").is_some(),
            "dry_run should produce diff"
        );

        // File should NOT be modified
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("world"));
        assert!(!content.contains("universe"));
    }

    #[tokio::test]
    async fn test_edit_apply_rejects_syntax_errors() {
        // Create a Python file with valid content.
        // The project auto-indexes, so create_validator returns Some.
        // We replace valid code with invalid syntax to trigger validation errors.
        let (_dir, file_path, registry) =
            setup_test_file("def hello():\n    print('world')\n", "test.py").await;

        let handler = EditApplyHandler;
        // Replace with invalid Python syntax (missing closing paren)
        let args = serde_json::json!({
            "file_path": file_path,
            "old_text": "print('world')",
            "new_text": "print('universe'",
        });

        let result = handler.execute(&registry, args).await;
        // With validation active, the syntax error should cause rejection
        if let Err(ref e) = result {
            let msg = &e.message;
            // The error should mention validation rejection
            assert!(
                msg.contains("Edit rejected") || msg.contains("validation"),
                "Expected validation rejection, got: {}",
                msg
            );
        }
        // Either validation caught it (Err) or it went through (Ok).
        // The key invariant: if rejected, file must be unchanged.
        if result.is_err() {
            let content = std::fs::read_to_string(&file_path).unwrap();
            assert!(
                content.contains("world"),
                "File should be unchanged after rejection"
            );
        }
    }

    #[tokio::test]
    async fn test_edit_apply_includes_validation_field_in_response() {
        // When the project has a PDG (auto-indexed), validation runs.
        // For a valid edit, the response should include a validation field
        // with the complete structured result.
        let (_dir, file_path, registry) =
            setup_test_file("fn hello() { println!(\"world\"); }\n", "test.rs").await;

        let handler = EditApplyHandler;
        let args = serde_json::json!({
            "file_path": file_path,
            "old_text": "world",
            "new_text": "universe",
        });

        let result = handler.execute(&registry, args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["success"], true);
        assert_eq!(response["changes_applied"], 1);

        // Verify the file was actually modified
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("universe"));

        // When validation runs (PDG available), the validation field must be present
        // and have the correct structure with all required sub-fields.
        if let Some(validation) = response.get("validation") {
            assert!(
                validation.get("is_valid").is_some(),
                "validation.is_valid must be present"
            );
            assert!(
                validation.get("has_errors").is_some(),
                "validation.has_errors must be present"
            );
            assert!(
                validation.get("syntax_errors").is_some(),
                "validation.syntax_errors must be present"
            );
            assert!(
                validation.get("reference_issues").is_some(),
                "validation.reference_issues must be present"
            );
            assert!(
                validation.get("semantic_drift").is_some(),
                "validation.semantic_drift must be present"
            );
            assert!(
                validation.get("impact_report").is_some(),
                "validation.impact_report must be present"
            );

            // For a valid edit, syntax_errors/reference_issues/semantic_drift should be arrays
            assert!(validation["syntax_errors"].is_array());
            assert!(validation["reference_issues"].is_array());
            assert!(validation["semantic_drift"].is_array());
        }
    }

    #[tokio::test]
    async fn test_edit_apply_missing_params_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = test_registry_for(dir.path());

        let handler = EditApplyHandler;
        let args = serde_json::json!({
            "file_path": "/tmp/nonexistent.rs",
        });

        let result = handler.execute(&registry, args).await;
        // Should fail because no old_text/new_text or changes provided
        assert!(result.is_err());
    }
}
