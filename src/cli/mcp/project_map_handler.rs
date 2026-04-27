use super::helpers::*;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_project_map — annotated project tree replacing Glob/ls.
#[derive(Clone)]
pub struct ProjectMapHandler;

#[allow(missing_docs)]
impl ProjectMapHandler {
    pub fn name(&self) -> &str {
        "leindex_project_map"
    }

    pub fn description(&self) -> &str {
        "High-level structural map of the project. Returns a tree of files annotated with \
major symbols and complexity scores. Replaces Glob/ls with semantic awareness."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "scope": {
                    "type": "string",
                    "description": "Optional subdirectory to map (absolute or relative to project root)"
                },
                "depth": {
                    "type": "integer",
                    "description": "Directory traversal depth (default: 2)",
                    "default": 2
                },
                "offset": {
                     "type": "integer",
                     "description": "Pagination offset for files (default: 0)",
                     "default": 0
                },
                "limit": {
                     "type": "integer",
                     "description": "Max files to return in map (default: 100)",
                     "default": 100
                }
            }
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let _depth = extract_usize(&args, "depth", 2)?;
        let offset = extract_usize(&args, "offset", 0)?;
        let limit = extract_usize(&args, "limit", 100)?;

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.lock().await;

        let scope = resolve_scope(&args, index.project_path())?;

        let mut files = index.source_file_paths().map(|p| p.clone()).unwrap_or_default();

        index.ensure_pdg_loaded().map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;
        let pdg = index.pdg().ok_or_else(|| {
            JsonRpcError::project_not_indexed(index.project_path().display().to_string())
        })?;
        files.sort();

        // Scope filter
        let filtered: Vec<_> = if let Some(ref s) = scope {
            files.into_iter().filter(|p| p.starts_with(s)).collect()
        } else {
            files
        };

        let total = filtered.len();
        let page: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();

        let mut results = Vec::new();
        for file in page {
            let file_str = file.to_string_lossy();
            let node_ids = pdg.nodes_in_file(&*file_str);
            let mut symbols = Vec::new();
            let mut total_complexity = 0;
            for &nid in &node_ids {
                if let Some(node) = pdg.get_node(nid) {
                    total_complexity += node.complexity;
                    if symbols.len() < 5 {
                        symbols.push(serde_json::json!({
                            "name": node.name,
                            "type": node_type_str(&node.node_type),
                            "complexity": node.complexity
                        }));
                    }
                }
            }

            results.push(serde_json::json!({
                "file": file,
                "symbol_count": node_ids.len(),
                "total_complexity": total_complexity,
                "major_symbols": symbols
            }));
        }

        Ok(wrap_with_meta(serde_json::json!({
            "project_path": index.project_path().display().to_string(),
            "total_files": total,
            "offset": offset,
            "count": results.len(),
            "has_more": offset + results.len() < total,
            "map": results
        }), &index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::leindex::LeIndex;
    use tempfile::tempdir;

    fn test_registry_for(path: &std::path::Path) -> Arc<ProjectRegistry> {
        let leindex = LeIndex::new(path).expect("leindex");
        Arc::new(ProjectRegistry::with_initial_project(5, leindex))
    }

    #[tokio::test]
    async fn test_project_map_auto_indexes_empty_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let handler = ProjectMapHandler;
        let args = serde_json::json!({ "project_path": dir.path().to_str().unwrap() });
        let result = futures::executor::block_on(handler.execute(&registry, args)).unwrap();
        assert_eq!(result.get("total_files").unwrap().as_u64().unwrap(), 0);
    }
}
