use super::helpers::*;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

/// Handler for leindex_read_file — PDG-annotated file read.
#[derive(Clone)]
pub struct ReadFileHandler;

#[allow(missing_docs)]
impl ReadFileHandler {
    pub fn name(&self) -> &str {
        "leindex_read_file"
    }

    pub fn description(&self) -> &str {
        "PRIMARY file reader — returns exact file contents with line numbers PLUS context \
showing symbols, imports, and dependents. One call replaces Read + Grep for imports. \
Works for any text file including configs and docs."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to file to read"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Start line, 1-indexed (default: 1)",
                    "default": 1,
                    "minimum": 1
                },
                "end_line": {
                    "type": "integer",
                    "description": "End line, 1-indexed inclusive (default: end of file)",
                    "minimum": 1
                },
                "max_lines": {
                    "type": "integer",
                    "description": "Maximum lines to return (default: 500, safety cap)",
                    "default": 500,
                    "minimum": 1,
                    "maximum": 2000
                },
                "include_symbol_map": {
                    "type": "boolean",
                    "description": "Include PDG symbol annotations (default: false). \
        Set true when structural context is useful.",
                    "default": false
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
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
        let start_line = extract_usize(&args, "start_line", 1)?.max(1);
        let max_lines = extract_usize(&args, "max_lines", 500)?.min(2000);
        let include_symbol_map = extract_bool(&args, "include_symbol_map", false);
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let maybe_handle = registry.get_or_create(project_path).await.ok();
        if let Some(ref handle) = maybe_handle {
            let index = handle.lock().await;
            let _ = validate_file_within_project(&file_path, index.project_path());
        }

        let content = std::fs::read_to_string(&file_path).map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        let end_line_raw = extract_usize(&args, "end_line", total_lines)?;
        let end_line = end_line_raw
            .min(total_lines)
            .min(start_line + max_lines - 1);

        if start_line > total_lines {
            return Err(JsonRpcError::invalid_params(format!(
                "start_line {} exceeds total lines {}",
                start_line, total_lines
            )));
        }

        let visible_lines: Vec<String> = all_lines[(start_line - 1)..end_line.min(total_lines)]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", start_line + i, line))
            .collect();
        let content_str = visible_lines.join("\n");

        let ext_lower = Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| ext.to_ascii_lowercase());
        let language = ext_lower.as_deref()
            .map(|ext| match ext {
                "rs" => "rust",
                "py" => "python",
                "js" | "mjs" | "cjs" => "javascript",
                "ts" | "mts" | "cts" => "typescript",
                "tsx" => "typescriptreact",
                "jsx" => "javascriptreact",
                other => other,
            })
            .unwrap_or("text");

        let symbol_map: Vec<Value> = if include_symbol_map {
            if let Some(ref handle) = maybe_handle {
                let index = handle.lock().await;
                if let Some(pdg) = index.pdg() {
                    let nodes = pdg.nodes_in_file(&file_path);
                    let line_byte_offsets: Vec<usize> = {
                        let mut offsets = Vec::with_capacity(all_lines.len() + 1);
                        offsets.push(0);
                        let mut acc: usize = 0;
                        for line in &all_lines {
                            acc += line.len() + 1;
                            offsets.push(acc);
                        }
                        offsets
                    };
                    let visible_start_byte = line_byte_offsets.get(start_line - 1).copied().unwrap_or(0);
                    let visible_end_byte = line_byte_offsets.get(end_line.min(total_lines)).copied().unwrap_or(content.len());

                    let mut symbols = Vec::new();
                    for nid in nodes {
                        let node = pdg.get_node(nid).unwrap();
                        let (sym_start, sym_end) = node.byte_range;
                        if sym_end <= visible_start_byte || sym_start >= visible_end_byte { continue; }
                        let s_start = line_byte_offsets.iter().position(|&off| off > sym_start).unwrap_or(1);
                        let s_end = line_byte_offsets.iter().position(|&off| off >= sym_end).unwrap_or(total_lines);
                        symbols.push(serde_json::json!({
                            "name": node.name,
                            "type": node_type_str(&node.node_type),
                            "line_start": s_start,
                            "line_end": s_end,
                            "complexity": node.complexity
                        }));
                    }
                    symbols
                } else { Vec::new() }
            } else { Vec::new() }
        } else { Vec::new() };

        let index = maybe_handle.as_ref().unwrap().lock().await;

        Ok(wrap_with_meta(serde_json::json!({
            "file_path": file_path,
            "language": language,
            "total_lines": total_lines,
            "start_line": start_line,
            "end_line": end_line,
            "content": content_str,
            "symbol_map": symbol_map
        }), &index))
    }
}
