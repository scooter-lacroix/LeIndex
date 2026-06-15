use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
    read_source_snippet, validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex [file_summary — structured file analysis replacing Read.
#[derive(Clone)]
pub struct FileSummaryHandler;

#[allow(missing_docs)]
impl FileSummaryHandler {
    pub fn name(&self) -> &str {
        "leindex.file-summary"
    }

    pub fn title(&self) -> &str {
        "LeIndex [File Summary]"
    }

    pub fn description(&self) -> &str {
        "File overview: symbol inventory, complexity scores, cross-file dependencies, \
and module role. Use for understanding structure without reading raw content. \
For exact file contents use LeIndex [Read File]; for a specific implementation \
use LeIndex [Read Symbol]."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to analyze"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1000)",
                    "default": 1000
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include source snippets for key symbols (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "focus_symbol": {
                    "type": "string",
                    "description": "Focus analysis on a specific symbol name (optional)"
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
        let include_source = extract_bool(&args, "include_source", false);
        let focus_symbol = args
            .get("focus_symbol")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let token_budget = extract_usize(&args, "token_budget", 1000)?;

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        guard
            .ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        if guard.pdg().is_none() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        // Enforce project boundary
        let _ = validate_file_within_project(&file_path, guard.project_path())?;

        let pdg = guard.pdg().unwrap();

        // Collect all nodes in this file
        let node_ids = pdg.nodes_in_file(&file_path);

        if node_ids.is_empty() {
            return Err(JsonRpcError::invalid_params(format!(
                "No symbols found for file '{}'. Is the project indexed?",
                file_path
            )));
        }

        // Determine line count from file
        let line_count = std::fs::read_to_string(&file_path)
            .map(|s| s.lines().count())
            .unwrap_or(0);

        let language = pdg
            .get_node(node_ids[0])
            .map(|n| n.language.clone())
            .unwrap_or_default();

        // Build symbol list — filter out nodes with empty names (use
        // statements, module declarations, anonymous blocks, etc.)
        let mut symbols: Vec<Value> = Vec::new();
        let mut total_chars = 0usize;
        let chars_per_token = 4usize;
        let char_budget = token_budget * chars_per_token;
        let mut total_named_symbols = 0usize;
        let mut budget_exhausted = false;

        for &nid in &node_ids {
            let node = match pdg.get_node(nid) {
                Some(n) => n,
                None => continue,
            };

            // Skip nodes with empty names — they are structural nodes
            // (use statements, module decls, anonymous blocks) that
            // add noise to the symbol inventory.
            if node.name.is_empty() {
                continue;
            }

            // Apply focus filter
            if let Some(ref focus) = focus_symbol {
                if !node.name.to_lowercase().contains(&focus.to_lowercase()) {
                    continue;
                }
            }

            total_named_symbols += 1;

            // If we already hit the budget, stop adding but keep
            // counting for the truncation indicator.
            if budget_exhausted {
                continue;
            }

            // Outgoing edges = dependencies
            let callees = pdg.neighbors(nid);
            let dependencies: Vec<String> = callees
                .iter()
                .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
                .collect();

            // Incoming edges = dependents (callers)
            let caller_ids = get_direct_callers(pdg, nid);
            let dependents: Vec<String> = caller_ids
                .iter()
                .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
                .collect();

            // Cross-file references (edges to nodes in different files)
            let cross_file_refs: Vec<Value> = callees
                .iter()
                .filter_map(|&cid| {
                    let cn = pdg.get_node(cid)?;
                    if cn.file_path != node.file_path {
                        Some(serde_json::json!({
                            "symbol": cn.name,
                            "file": cn.file_path,
                            "relationship": "dependency"
                        }))
                    } else {
                        None
                    }
                })
                .collect();

            let mut sym = serde_json::json!({
                "name": node.name,
                "type": node_type_str(&node.node_type),
                "byte_range": node.byte_range,
                "complexity": node.complexity,
                "dependencies": dependencies,
                "dependents": dependents,
                "cross_file_refs": cross_file_refs
            });

            if include_source {
                if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                    // Trim to avoid blowing up token budget
                    let truncated: String = src.chars().take(500).collect();
                    sym["source"] = Value::String(truncated);
                }
            }

            let sym_str = sym.to_string();
            total_chars += sym_str.len();
            if total_chars > char_budget {
                budget_exhausted = true;
            }
            symbols.push(sym);
        }

        // Determine module role from node type distribution
        let func_count = symbols.iter().filter(|s| s["type"] == "function").count();
        let class_count = symbols.iter().filter(|s| s["type"] == "class").count();
        let module_role = if class_count > func_count {
            format!(
                "Class definitions ({} classes, {} functions)",
                class_count, func_count
            )
        } else {
            format!(
                "Function module ({} functions, {} classes)",
                func_count, class_count
            )
        };

        Ok(wrap_with_meta(
            serde_json::json!({
                "file_path": file_path,
                "language": language,
                "line_count": line_count,
                "symbol_count": total_named_symbols,
                "symbols_shown": symbols.len(),
                "symbols_truncated": budget_exhausted,
                "symbols": symbols,
                "module_role": module_role
            }),
            &guard,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_summary_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "file_path": "/some/file.rs" });
        let result = FileSummaryHandler.execute(&registry, args).await;
        assert!(result.is_err());
    }
}
