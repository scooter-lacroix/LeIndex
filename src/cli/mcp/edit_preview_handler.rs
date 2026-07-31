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

/// Handler for LeIndex [edit_preview — dry-run for code changes.
#[derive(Clone)]
pub struct EditPreviewHandler;

#[allow(missing_docs)]
impl EditPreviewHandler {
    pub fn name(&self) -> &str {
        "leindex.edit-preview"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Edit Preview]"
    }

    pub fn description(&self) -> &str {
        "Preview a code edit: unified diff, affected symbols/files, breaking changes, and risk \
level — all before touching the filesystem. No equivalent in standard tools. Run before \
LeIndex [Edit Apply] to understand the blast radius of your change."
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
        let changes_val = resolve_changes_from_args(&args);
        // Reject before any parsing/caching: neither `changes` nor
        // old_text/new_text was supplied, so there is nothing to preview.
        let changes_empty = changes_val
            .as_array()
            .map(|arr| arr.is_empty())
            .unwrap_or(true);
        if changes_empty {
            return Err(JsonRpcError::invalid_params(
                "Provide either 'changes' or 'old_text'/'new_text' to preview.",
            ));
        }

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
        let original = tokio::fs::read_to_string(&abs_file_path)
            .await
            .map_err(|e| {
                JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
            })?;

        // 3. Parse changes and apply in memory (CPU without lock)
        let changes = parse_edit_changes(&changes_val, Some(&original))?;
        let modified = apply_changes_in_memory(&original, &changes)?;

        let preview_token = blake3::hash(
            format!(
                "{}{}",
                abs_file_path.display(),
                chrono::Utc::now().to_rfc3339()
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();

        // 4. Store in cache (best effort)
        let cache_entry = EditCacheEntry {
            file_path: abs_file_path.clone(),
            preview_token: preview_token.clone(),
            original_text: original.clone(),
            modified_text: modified.clone(),
            changes: changes.clone(),
            timestamp: chrono::Utc::now(),
        };
        store_preview_in_cache(&storage_path, cache_entry).await;

        // 5. Generate diff and compute impact
        let diff = make_diff(&original, &modified, &file_path);
        let diff_json = diff.to_json();

        let (validation_json, affected_nodes, affected_files, breaking_changes) = {
            let guard = handle.read().await;

            // Run validation
            let validation = run_validation_result(&guard, &abs_file_path, &original, &modified);

            // Compute impact from PDG (rename callers + edited-region symbol overlap)
            let (nodes, files, breaks) = match guard.pdg() {
                Some(pdg) => collect_pdg_impact(pdg, &changes, &abs_file_path, &file_path),
                None => (
                    Vec::new(),
                    std::iter::once(abs_file_path.to_string_lossy().to_string()).collect(),
                    Vec::new(),
                ),
            };

            (validation, nodes, files, breaks)
        };

        let risk = compute_risk_level(&affected_nodes, &affected_files);

        let mut response = serde_json::json!({
            "preview_token": preview_token,
            "diff": diff_json,
            "diff_text": crate::cli::mcp::output::render_unified_diff(&diff, false),
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

/// Build the `changes` JSON value from explicit `changes` or the simple-mode
/// `old_text`/`new_text` (and `old_str`/`new_str`) aliases.
fn resolve_changes_from_args(args: &Value) -> Value {
    if let Some(changes) = args.get("changes").cloned() {
        return changes;
    }
    let old_text = args
        .get("old_text")
        .or_else(|| args.get("old_str"))
        .and_then(|v| v.as_str());
    let new_text = args
        .get("new_text")
        .or_else(|| args.get("new_str"))
        .and_then(|v| v.as_str());
    match (old_text, new_text) {
        (Some(old), Some(new)) => serde_json::json!([{
            "type": "replace_text",
            "old_text": old,
            "new_text": new
        }]),
        _ => Value::Array(vec![]),
    }
}

/// Store a prepared preview in the global cache; failures are non-fatal (warn only).
async fn store_preview_in_cache(storage_path: &std::path::Path, cache_entry: EditCacheEntry) {
    match GLOBAL_EDIT_CACHE.set(storage_path, cache_entry).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("Edit preview too large for cache: {}", e),
        Err(e) => tracing::warn!("Failed to store edit in cache: {}", e),
    }
}

/// Classify preview risk from the breadth of affected symbols/files.
fn compute_risk_level(
    affected_nodes: &[String],
    affected_files: &std::collections::HashSet<String>,
) -> &'static str {
    if affected_nodes.len() > 5 || affected_files.len() > 3 {
        "high"
    } else if affected_nodes.len() > 1 || affected_files.len() > 1 {
        "medium"
    } else {
        "low"
    }
}

/// Validate the resolved edit under a read guard, returning a JSON payload (or
/// `None` when no validator is available). A validator error degrades to a
/// `validation_warning` object rather than failing the whole preview.
fn run_validation_result(
    guard: &crate::cli::registry::ProjectReadGuard<'_>,
    abs_file_path: &std::path::Path,
    original: &str,
    modified: &str,
) -> Option<Value> {
    let validator = guard.create_validator()?;
    let resolved = ResolvedEditChange::new(
        abs_file_path.to_path_buf(),
        original.to_string(),
        modified.to_string(),
    );
    match validator.validate_changes(&[resolved]) {
        Ok(result) => Some(validation_to_json(&result)),
        Err(e) => Some(serde_json::json!({
            "is_valid": Value::Null,
            "has_errors": false,
            "syntax_errors": [],
            "reference_issues": [],
            "semantic_drift": [],
            "impact_report": null,
            "validation_warning": format!("Validation check failed: {}", e)
        })),
    }
}

/// Collect PDG impact: rename callers/dependents plus symbols overlapping the
/// edited byte regions. `files` always seeds with the edited file.
fn collect_pdg_impact(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    changes: &[crate::edit::EditChange],
    abs_file_path: &std::path::Path,
    file_path: &str,
) -> (Vec<String>, std::collections::HashSet<String>, Vec<String>) {
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
    files.insert(abs_file_path.to_string_lossy().to_string());

    let (nodes, breaks) = collect_rename_impact(pdg, changes, abs_file_path, &mut files);

    // For text replacements, find symbols in the edited file whose byte ranges
    // overlap with the changed region(s). Only runs when no rename impact was
    // found — avoids over-reporting symbols unrelated to the rename.
    let nodes = if nodes.is_empty() {
        collect_text_overlap_symbols(pdg, changes, file_path, abs_file_path)
    } else {
        nodes
    };

    (nodes, files, breaks)
}

/// For each rename change, record forward-impact symbol/file names and flag
/// any backward (caller) impact as a potential breaking change.
fn collect_rename_impact(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    changes: &[crate::edit::EditChange],
    abs_file_path: &std::path::Path,
    files: &mut std::collections::HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut nodes: Vec<String> = Vec::new();
    let mut breaks: Vec<String> = Vec::new();
    for change in changes {
        let crate::edit::EditChange::RenameSymbol {
            old_name,
            new_name: _,
        } = change
        else {
            continue;
        };
        // Try name-based lookup for PDG impact analysis
        let found_id = pdg
            .find_by_symbol(old_name)
            .or_else(|| pdg.find_by_name(old_name))
            .or_else(|| pdg.find_by_name_in_file(old_name, Some(&abs_file_path.to_string_lossy())));
        let Some(node_id) = found_id else {
            continue;
        };
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
    (nodes, breaks)
}

/// Find symbols in the edited file whose byte range overlaps any ReplaceText
/// edit region. Tries the user-provided path (often relative) then the absolute.
fn collect_text_overlap_symbols(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    changes: &[crate::edit::EditChange],
    file_path: &str,
    abs_file_path: &std::path::Path,
) -> Vec<String> {
    let edit_ranges: Vec<(usize, usize)> = changes
        .iter()
        .filter_map(|c| match c {
            crate::edit::EditChange::ReplaceText { start, end, .. } => Some((*start, *end)),
            _ => None,
        })
        .collect();

    let file_nodes = pdg.nodes_in_file(file_path);
    let file_nodes = if file_nodes.is_empty() {
        pdg.nodes_in_file(&abs_file_path.to_string_lossy())
    } else {
        file_nodes
    };

    let mut nodes = Vec::new();
    for nid in file_nodes {
        let Some(n) = pdg.get_node(nid) else {
            continue;
        };
        // Only include symbols whose byte range overlaps at least one edit region.
        let overlaps = edit_ranges.iter().any(|&(es, ee)| {
            if es == ee {
                n.byte_range.0 <= es && es <= n.byte_range.1
            } else {
                // Two ranges [s1, e1) and [s2, e2) overlap when s1 < e2 && s2 < e1
                n.byte_range.0 < ee && es < n.byte_range.1
            }
        });
        if overlaps {
            nodes.push(n.name.clone());
        }
    }
    nodes
}
