use super::helpers::{extract_bool, extract_string, make_diff, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::{atomic_write, replace_whole_word, ResolvedEditChange};
use crate::validation::validation_to_json;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

/// Handler for LeIndex [rename_symbol — rename a symbol across all files.
#[derive(Clone)]
pub struct RenameSymbolHandler;

#[allow(missing_docs)]
impl RenameSymbolHandler {
    pub fn name(&self) -> &str {
        "leindex.rename-symbol"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Rename Symbol]"
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
        let mut index = handle.write().await;
        index
            .ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;
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
            // --- Name conflict check ---
            // Reject rename if new_name already exists as a symbol in the PDG,
            // which would create an ambiguous binding or break references.
            let name_conflict = pdg
                .find_by_symbol(&new_name)
                .or_else(|| pdg.find_by_name(&new_name))
                .or_else(|| pdg.find_by_name_in_file(&new_name, None));
            if name_conflict.is_some() {
                return Err(JsonRpcError::invalid_params(format!(
                    "Rename conflict: symbol '{}' already exists in the project index. \
                    Renaming '{}' to '{}' would create a duplicate. \
                    Use LeIndex [Grep Symbols] to inspect '{}'.",
                    new_name, old_name, new_name, new_name
                )));
            }

            // The definition file
            if let Some(n) = pdg.get_node(node_id) {
                ref_files.insert(n.file_path.to_string());
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
                    ref_files.insert(dn.file_path.to_string());
                }
            }
            // Also include files where the old_name appears in other symbols' IDs
            // (e.g., imports or references that aren't captured as direct callers)
            for nid in pdg.find_all_by_name(&old_name) {
                if let Some(n) = pdg.get_node(nid) {
                    ref_files.insert(n.file_path.to_string());
                }
            }
        } else {
            return Err(JsonRpcError::invalid_params(format!(
                "Symbol '{}' not found in project index. The index uses short symbol names \
                (e.g., 'health_check', not 'ClassName.health_check'). \
                Try LeIndex [Grep Symbols] to find the exact name.",
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
        let (diffs, files_to_modify, file_contents) = tokio::task::spawn_blocking({
            let old_name = old_name.clone();
            let new_name = new_name.clone();
            #[allow(clippy::type_complexity)]
            move || -> Result<(Vec<Value>, Vec<String>, Vec<(String, String, String)>), String> {
                let mut diffs: Vec<Value> = Vec::new();
                let mut files_to_modify: Vec<String> = Vec::new();
                let mut file_contents: Vec<(String, String, String)> = Vec::new(); // (path, original, modified)
                for file_path in &filtered_files {
                    let original = std::fs::read_to_string(file_path)
                        .map_err(|e| format!("Failed reading '{}': {}", file_path, e))?;
                    let modified = replace_whole_word(&original, &old_name, &new_name);
                    if modified != original {
                        let diff = make_diff(&original, &modified, file_path);
                        diffs.push(serde_json::json!({
                            "file": file_path,
                            "diff": diff.to_json(),
                            "diff_text": crate::cli::mcp::output::render_unified_diff(&diff, false),
                        }));
                        files_to_modify.push(file_path.clone());
                        file_contents.push((file_path.clone(), original, modified));
                    }
                }
                Ok((diffs, files_to_modify, file_contents))
            }
        })
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Rename task failed: {}", e)))?
        .map_err(JsonRpcError::internal_error)?;

        // --- Syntax validation via LogicValidator ---
        // Validate the proposed file contents for syntax correctness.
        // For non-preview renames, reject if validation finds errors.
        // For preview renames, include validation results as warnings.
        let validation_json = {
            let idx = handle.read().await;
            match idx.create_validator() {
                Some(validator) => {
                    // Build ResolvedEditChanges from the proposed file modifications
                    let resolved: Vec<ResolvedEditChange> = file_contents
                        .iter()
                        .map(|(path, original, modified)| {
                            let mut change = ResolvedEditChange::new(
                                PathBuf::from(path),
                                original.clone(),
                                modified.clone(),
                            );
                            change = change.with_edit_type(crate::edit::EditType::Rename);
                            change
                        })
                        .collect();

                    match validator.validate_changes(&resolved) {
                        Ok(result) => {
                            let has_errors = result.has_errors();
                            let v_json = validation_to_json(&result);

                            if has_errors && !preview_only {
                                // Build detailed error response — reject the rename
                                let syn_errs = v_json["syntax_errors"]
                                    .as_array()
                                    .map(|a| a.len())
                                    .unwrap_or(0);
                                let ref_issues = v_json["reference_issues"]
                                    .as_array()
                                    .map(|a| a.len())
                                    .unwrap_or(0);
                                let drift = v_json["semantic_drift"]
                                    .as_array()
                                    .map(|a| a.len())
                                    .unwrap_or(0);
                                return Err(JsonRpcError::invalid_params(format!(
                                    "Rename rejected — validation found errors. Files unchanged.\n\
                                     Syntax errors: {}\nReference issues: {}\nSemantic drift: {}\n\
                                     Details: {}",
                                    syn_errs, ref_issues, drift, v_json
                                )));
                            }

                            // Include validation in response (warnings or preview mode)
                            Some(v_json)
                        }
                        Err(e) => {
                            // Validation itself failed — log warning but don't block
                            tracing::warn!("Rename validation check failed: {}", e);
                            None
                        }
                    }
                }
                None => None,
            }
        };

        if !preview_only {
            // Apply changes to all files (file I/O — offload to blocking thread)
            // IMPORTANT: Write the validated buffers from file_contents instead of recomputing.
            // If files change between validation and write, recomputing would corrupt data.
            let validated_contents = file_contents;
            tokio::task::spawn_blocking(move || {
                let mut written: Vec<(String, String)> = Vec::new();
                for (file_path, original, modified) in validated_contents {
                    if let Err(e) =
                        atomic_write(std::path::Path::new(&file_path), modified.as_bytes())
                    {
                        for (written_path, original_content) in written.into_iter().rev() {
                            let _ = atomic_write(
                                std::path::Path::new(&written_path),
                                original_content.as_bytes(),
                            );
                        }
                        return Err(format!("Failed writing '{}': {}", file_path, e));
                    }
                    written.push((file_path, original));
                }
                Ok(())
            })
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("Rename apply task failed: {}", e)))?
            .map_err(JsonRpcError::internal_error)?;

            // Invalidate the registry's staleness cache so the next
            // read tool re-runs `is_stale_fast` instead of reusing
            // a pre-write `false` cached result. The watcher (when
            // enabled via `LEINDEX_WATCHER=1`) does this on its
            // own reindex path; this explicit call covers the
            // watcher-disabled default mode where the 30-second
            // negative-cache TTL would otherwise silently mask the
            // rename. Preview-only runs (the default) skip this
            // — no files were written, so the cache value is
            // still accurate and re-running `is_stale_fast` on
            // the next read would be wasted work.
            let project_root = {
                let guard = handle.read().await;
                guard.project_path().to_path_buf()
            };
            registry.invalidate_stale_cache(&project_root).await;
        }

        let mut response_data = serde_json::json!({
            "old_name": old_name,
            "new_name": new_name,
            "files_affected": files_to_modify.len(),
            "preview_only": preview_only,
            "diffs": diffs,
            "applied": !preview_only
        });

        // Include validation results in response
        if let Some(validation) = validation_json {
            if let Some(obj) = response_data.as_object_mut() {
                obj.insert("validation".to_string(), validation);
            }
        }

        // Re-acquire the lock for wrap_with_meta (released before spawn_blocking)
        let index = handle.read().await;
        Ok(wrap_with_meta(response_data, &index))
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
    async fn test_rename_missing_old_name_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = test_registry_for(dir.path());

        let handler = RenameSymbolHandler;
        let args = serde_json::json!({
            "new_name": "bar",
        });

        let result = handler.execute(&registry, args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("old_name"),
            "Expected missing old_name error, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn test_rename_missing_new_name_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = test_registry_for(dir.path());

        let handler = RenameSymbolHandler;
        let args = serde_json::json!({
            "old_name": "foo",
        });

        let result = handler.execute(&registry, args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("new_name"),
            "Expected missing new_name error, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn test_rename_symbol_not_found_returns_error() {
        // Without a PDG (empty project), the symbol lookup fails
        let (_dir, _file_path, registry) =
            setup_test_file("fn hello() { println!(\"world\"); }\n", "test.rs").await;

        let handler = RenameSymbolHandler;
        let args = serde_json::json!({
            "old_name": "nonexistent_symbol",
            "new_name": "new_name",
            "preview_only": true,
        });

        let result = handler.execute(&registry, args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("not found"),
            "Expected 'not found' error, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn test_rename_returns_project_not_indexed_for_empty_project() {
        // Empty project with no indexed files — PDG is None, so pdg() returns error
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = test_registry_for(dir.path());

        let handler = RenameSymbolHandler;
        let args = serde_json::json!({
            "old_name": "foo",
            "new_name": "bar",
            "preview_only": true,
        });

        let result = handler.execute(&registry, args).await;
        assert!(result.is_err());
        // With no PDG loaded and no indexed files, handler returns project not indexed
        // or symbol not found depending on ensure_pdg_loaded behavior
        let err = result.unwrap_err();
        assert!(
            err.message.contains("not indexed")
                || err.message.contains("not found")
                || err.message.contains("Failed to load PDG"),
            "Expected project not indexed or symbol not found error, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn test_rename_preview_only_does_not_modify_file() {
        // Even if the project auto-indexes, the rename in preview_only mode
        // should NOT modify files. Since the symbol won't be in the PDG for a
        // simple test file, this will return "not found" — but the key invariant
        // is that files are never modified.
        let (_dir, file_path, registry) =
            setup_test_file("fn hello() { println!(\"world\"); }\n", "test.rs").await;

        let original_content = std::fs::read_to_string(&file_path).unwrap();

        let handler = RenameSymbolHandler;
        let args = serde_json::json!({
            "old_name": "hello",
            "new_name": "greet",
            "preview_only": true,
        });

        let _ = handler.execute(&registry, args).await;

        // File must be unchanged regardless of outcome
        let content_after = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(
            content_after, original_content,
            "File must not be modified in preview_only mode"
        );
    }

    /// Regression for codex round 16 (`3344884534`-followup):
    /// the round-15 `invalidate_stale_cache` call ran
    /// unconditionally, but `preview_only` defaults to `true`, so on
    /// the default path no files are written — yet the call still
    /// acquired a read lock and forced the next read to recompute
    /// `is_stale_fast`. The fix moves the invalidation inside the
    /// `if !preview_only` block. This test verifies the
    /// structural contract by reading the source file (the
    /// invalidation call is reachable only from inside the
    /// `if !preview_only { … }` block).
    #[tokio::test]
    async fn test_rename_preview_only_invalidation_is_gated() {
        // Read the source and confirm the `invalidate_stale_cache`
        // call is nested inside the `if !preview_only` block —
        // not at the top level of `execute`. This is a static
        // structural check; the existing
        // `test_rename_preview_only_does_not_modify_file` test
        // covers the runtime contract that no files are written
        // in preview mode, and the apply-path invalidation is
        // exercised by the existing
        // `test_invalidate_stale_cache_removes_entry` test in
        // `src/cli/registry.rs`.
        let source = include_str!("rename_symbol_handler.rs");
        let apply_block_start = source
            .find("if !preview_only {")
            .expect("if !preview_only block must exist in the handler");
        let apply_block_open_brace = source[apply_block_start..]
            .find('{')
            .map(|i| apply_block_start + i)
            .expect("if !preview_only block must have an opening brace");
        let invalidation_pos = source
            .find("registry.invalidate_stale_cache(&project_root).await")
            .expect("invalidate_stale_cache call must exist in the handler");
        assert!(
            invalidation_pos > apply_block_open_brace,
            "invalidate_stale_cache must be inside the if !preview_only block; \
             apply block opens at byte {} but invalidation is at byte {}",
            apply_block_open_brace,
            invalidation_pos
        );
    }

    #[tokio::test]
    async fn test_rename_apply_does_not_modify_on_symbol_not_found() {
        // When the symbol is not found, the file should remain unchanged
        let (_dir, file_path, registry) =
            setup_test_file("fn hello() { println!(\"world\"); }\n", "test.rs").await;

        let original_content = std::fs::read_to_string(&file_path).unwrap();

        let handler = RenameSymbolHandler;
        let args = serde_json::json!({
            "old_name": "nonexistent",
            "new_name": "something",
            "preview_only": false,
        });

        let _ = handler.execute(&registry, args).await;

        // File must be unchanged since symbol was not found
        let content_after = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(
            content_after, original_content,
            "File must not be modified when symbol not found"
        );
    }

    #[tokio::test]
    async fn test_rename_schema_has_required_fields() {
        let handler = RenameSymbolHandler;
        let schema = handler.argument_schema();

        // Verify required fields
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("old_name".to_string())));
        assert!(required.contains(&serde_json::Value::String("new_name".to_string())));

        // Verify properties exist
        let props = schema.get("properties").unwrap();
        assert!(props.get("old_name").is_some());
        assert!(props.get("new_name").is_some());
        assert!(props.get("preview_only").is_some());
        assert!(props.get("scope").is_some());
        assert!(props.get("project_path").is_some());
    }
}
