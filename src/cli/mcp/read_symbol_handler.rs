use super::helpers::{
    byte_range_to_line_range, extract_bool, extract_string, extract_usize, get_direct_callers,
    node_type_str, read_source_snippet, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_read_symbol — targeted symbol source read.
#[derive(Clone)]
pub struct ReadSymbolHandler;

#[allow(missing_docs)]
impl ReadSymbolHandler {
    pub fn name(&self) -> &str {
        "leindex_read_symbol"
    }

    pub fn description(&self) -> &str {
        "PRIMARY symbol reader — returns exact source code with line numbers, doc comments, \
and compact caller/callee locations (file:line). Use instead of Read for specific \
functions, methods, classes, or types. Set include_dependencies=true for full signatures."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to read source for"
                },
                "file_path": {
                    "type": "string",
                    "description": "Disambiguate when symbol exists in multiple files (optional)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "include_dependencies": {
                    "type": "boolean",
                    "description": "Include dependency signatures (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 8000)",
                    "default": 8000
                }
            },
            "required": ["symbol"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let symbol = extract_string(&args, "symbol")?;
        let file_path_hint = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let include_dependencies = extract_bool(&args, "include_dependencies", false);
        let token_budget = extract_usize(&args, "token_budget", 8000)?;

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

        let pdg = guard.pdg().unwrap();

        let symbol_lower = symbol.to_lowercase();
        let node_id = if let Some(ref fp_hint) = file_path_hint {
            pdg.nodes_in_file(fp_hint).into_iter().find(|&nid| {
                pdg.get_node(nid)
                    .map(|n| n.name.to_lowercase() == symbol_lower)
                    .unwrap_or(false)
            })
        } else {
            pdg.find_by_symbol(&symbol).or_else(|| {
                pdg.node_indices().find(|&nid| {
                    pdg.get_node(nid)
                        .map(|n| n.name.to_lowercase() == symbol_lower)
                        .unwrap_or(false)
                })
            })
        };

        let node_id = node_id.ok_or_else(|| {
            JsonRpcError::invalid_params(format!("Symbol '{}' not found in project index", symbol))
        })?;

        let node = pdg.get_node(node_id).unwrap();
        let char_budget = token_budget * 4;

        let source = read_source_snippet(&node.file_path, node.byte_range)
            .map(|s| s.chars().take(char_budget).collect::<String>());

        let doc_comment = (|| {
            let file_bytes = std::fs::read(&*node.file_path).ok()?;
            let end = node.byte_range.0.min(file_bytes.len());
            let up_to_def = String::from_utf8_lossy(&file_bytes[..end]).into_owned();
            let mut comment_lines: Vec<&str> = Vec::new();
            let mut in_doc_block = false;
            let mut saw_doc_start = false;

            for line in up_to_def.lines().rev().take(20) {
                let t = line.trim_start();
                if t.starts_with("///") || t.starts_with("//!") {
                    comment_lines.push(line);
                    continue;
                }
                if t.starts_with("*/") {
                    in_doc_block = true;
                    comment_lines.push(line);
                    continue;
                }
                if in_doc_block {
                    if t.starts_with("/**") {
                        saw_doc_start = true;
                        comment_lines.push(line);
                        in_doc_block = false;
                        continue;
                    }
                    if t.starts_with('*') || t.is_empty() {
                        comment_lines.push(line);
                        continue;
                    }
                    comment_lines.clear();
                    break;
                }
                if t.is_empty() && !comment_lines.is_empty() {
                    comment_lines.push(line);
                    continue;
                }
                break;
            }

            if in_doc_block
                || (!saw_doc_start
                    && comment_lines
                        .iter()
                        .any(|l| l.trim_start().starts_with("*/")))
            {
                comment_lines.clear();
            }

            if comment_lines.is_empty() {
                None
            } else {
                let reversed: Vec<&str> = comment_lines.into_iter().rev().collect();
                Some(reversed.join("\n"))
            }
        })();

        let dep_signatures: Vec<Value> = if include_dependencies {
            pdg.neighbors(node_id)
                .iter()
                .filter_map(|&did| {
                    let dn = pdg.get_node(did)?;
                    let sig = read_source_snippet(&dn.file_path, dn.byte_range)
                        .and_then(|s| s.lines().next().map(str::to_owned));
                    Some(serde_json::json!({
                        "name": dn.name,
                        "type": node_type_str(&dn.node_type),
                        "file": dn.file_path,
                        "signature": sig
                    }))
                })
                .take(20)
                .collect()
        } else {
            Vec::new()
        };

        let callers: Vec<Value> = get_direct_callers(pdg, node_id)
            .iter()
            .filter_map(|&cid| {
                let cn = pdg.get_node(cid)?;
                let caller_line = {
                    let fc = std::fs::read_to_string(&*cn.file_path).unwrap_or_default();
                    byte_range_to_line_range(&fc, cn.byte_range).0
                };
                Some(serde_json::json!({
                    "name": cn.name,
                    "file": cn.file_path,
                    "line": caller_line
                }))
            })
            .take(15)
            .collect();

        let callees: Vec<Value> = pdg
            .neighbors(node_id)
            .iter()
            .filter_map(|&did| {
                let dn = pdg.get_node(did)?;
                let callee_line = {
                    let fc = std::fs::read_to_string(&*dn.file_path).unwrap_or_default();
                    byte_range_to_line_range(&fc, dn.byte_range).0
                };
                Some(serde_json::json!({
                    "name": dn.name,
                    "file": dn.file_path,
                    "line": callee_line
                }))
            })
            .take(15)
            .collect();

        let (line_start, line_end) = {
            let file_content = std::fs::read_to_string(&*node.file_path).unwrap_or_default();
            byte_range_to_line_range(&file_content, node.byte_range)
        };

        Ok(wrap_with_meta(
            serde_json::json!({
                "symbol": node.name,
                "type": node_type_str(&node.node_type),
                "file": node.file_path,
                "language": node.language,
                "complexity": node.complexity,
                "line_start": line_start,
                "line_end": line_end,
                "doc_comment": doc_comment,
                "source": source,
                "callers": callers,
                "callees": callees,
                "dependencies": dep_signatures
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
    async fn test_read_symbol_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbol": "my_func" });
        let result = ReadSymbolHandler.execute(&registry, args).await;
        assert!(result.is_err());
    }
}
