use super::helpers::*;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_rename_symbol — rename a symbol across all files.
#[derive(Clone)]
pub struct RenameSymbolHandler;

#[allow(missing_docs)]
impl RenameSymbolHandler {
    pub fn name(&self) -> &str {
        "leindex_rename_symbol"
    }

    pub fn description(&self) -> &str {
        "Rename a symbol across all files using PDG to find all reference sites. Generates a \
unified multi-file diff (preview_only=true by default for safety). Replaces manual \
Grep + multi-file Edit with a single atomic operation."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "old_name": {
                    "type": "string",
                    "description": "Current symbol name"
                },
                "new_name": {
                    "type": "string",
                    "description": "New symbol name"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "scope": {
                    "type": "string",
                    "description": "Limit rename to a file or directory path (optional)"
                },
                "preview_only": {
                    "type": "boolean",
                    "description": "If true, return diff without applying changes (default: true). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": true
                }
            },
            "required": ["old_name", "new_name"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let old_name = extract_string(&args, "old_name")?;
        let new_name = extract_string(&args, "new_name")?;
        let scope = args
            .get("scope")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let preview_only = extract_bool(&args, "preview_only", true);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.lock().await;
        index.ensure_pdg_loaded().map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;
        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;

        // Collect all files containing references to old_name
        let mut ref_files: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Resolve old_name to PDG node using multiple strategies:
        // 1. Exact ID match ("file_path:qualified_name")
        // 2. Name-based match ("health_check")
        // 3. Fuzzy case-insensitive substring match
        let node_id = pdg
            .find_by_symbol(&old_name)
            .or_else(|| pdg.find_by_name(&old_name))
            .or_else(|| pdg.find_by_name_in_file(&old_name, None));

        if let Some(node_id) = node_id {
            // The definition file
            if let Some(n) = pdg.get_node(node_id) {
                ref_files.insert(n.file_path.clone());
            }
            // Include all known incoming references, not just direct call edges.
            // This captures call, data, and transitive usage relationships.
            for ref_id in pdg.backward_impact(
                node_id,
                &crate::graph::pdg::TraversalConfig {
                    max_depth: Some(5),
                    ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
                },
            ) {
                if let Some(dn) = pdg.get_node(ref_id) {
                    ref_files.insert(dn.file_path.clone());
                }
            }
            // Also include files where the old_name appears in other symbols' IDs
            // (e.g., imports or references that aren't captured as direct callers)
            for nid in pdg.find_all_by_name(&old_name) {
                if let Some(n) = pdg.get_node(nid) {
                    ref_files.insert(n.file_path.clone());
                }
            }
        } else {
            return Err(JsonRpcError::invalid_params(format!(
                "Symbol '{}' not found in project index. The index uses short symbol names \
                (e.g., 'health_check', not 'ClassName.health_check'). \
                Try leindex_grep_symbols to find the exact name.",
                old_name
            )));
        }

        // Apply scope filter
        let filtered_files: Vec<String> = ref_files
            .into_iter()
            .filter(|f| {
                scope
                    .as_ref()
                    .map(|s| f.starts_with(s.as_str()))
                    .unwrap_or(true)
            })
            .collect();

        // Release the mutex before spawning blocking I/O.
        // All PDG data has been extracted into filtered_files above.
        drop(index);

        // Generate per-file diffs (file I/O — offload to blocking thread)
        let (diffs, files_to_modify) = tokio::task::spawn_blocking({
            let filtered_files = filtered_files;
            let old_name = old_name.clone();
            let new_name = new_name.clone();
            move || -> Result<(Vec<Value>, Vec<String>), String> {
                let mut diffs: Vec<Value> = Vec::new();
                let mut files_to_modify: Vec<String> = Vec::new();
                for file_path in &filtered_files {
                    let original = std::fs::read_to_string(file_path)
                        .map_err(|e| format!("Failed reading '{}': {}", file_path, e))?;
                    let modified = replace_whole_word(&original, &old_name, &new_name);
                    if modified != original {
                        let diff = make_diff(&original, &modified, file_path);
                        diffs.push(serde_json::json!({ "file": file_path, "diff": diff }));
                        files_to_modify.push(file_path.clone());
                    }
                }
                Ok((diffs, files_to_modify))
            }
        })
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Rename task failed: {}", e)))?
        .map_err(JsonRpcError::internal_error)?;

        if !preview_only {
            // Apply changes to all files (file I/O — offload to blocking thread)
            let old_name_c = old_name.clone();
            let new_name_c = new_name.clone();
            let apply_files = files_to_modify.clone();
            tokio::task::spawn_blocking(move || {
                for file_path in &apply_files {
                    let original = match std::fs::read_to_string(file_path) {
                        Ok(o) => o,
                        Err(e) => return Err(format!("Failed reading '{}': {}", file_path, e)),
                    };
                    let modified = replace_whole_word(&original, &old_name_c, &new_name_c);
                    if let Err(e) = std::fs::write(file_path, modified.as_bytes()) {
                        return Err(format!("Failed writing '{}': {}", file_path, e));
                    }
                }
                Ok(())
            })
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("Rename apply task failed: {}", e)))?
            .map_err(JsonRpcError::internal_error)?;
        }

        let response_data = serde_json::json!({
            "old_name": old_name,
            "new_name": new_name,
            "files_affected": files_to_modify.len(),
            "preview_only": preview_only,
            "diffs": diffs,
            "applied": !preview_only
        });

        // Re-acquire the lock for wrap_with_meta (released before spawn_blocking)
        let index = handle.lock().await;
        Ok(wrap_with_meta(
            response_data,
            &index,
        ))
    }
}
