use super::helpers::{
    byte_range_to_line_range, extract_bool, extract_string, extract_usize, get_direct_callers,
    node_type_str, validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::graph::pdg::ProgramDependenceGraph;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

/// Handler for leindex_read_file — PDG-annotated file read.
#[derive(Clone)]
pub struct ReadFileHandler;

fn line_byte_offsets(content: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(content.lines().count() + 1);
    offsets.push(0);
    let mut acc: usize = 0;
    for chunk in content.split_inclusive('\n') {
        acc += chunk.len();
        offsets.push(acc);
    }
    offsets
}

fn build_pdg_enrichment(
    pdg: &ProgramDependenceGraph,
    file_path: &str,
    content: &str,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    include_symbol_map: bool,
) -> (Vec<Value>, Option<Value>) {
    let line_byte_offsets = line_byte_offsets(content);
    let nodes = pdg.nodes_in_file(file_path);

    let symbol_map = if include_symbol_map {
        let visible_start_byte = line_byte_offsets.get(start_line - 1).copied().unwrap_or(0);
        let visible_end_byte = line_byte_offsets
            .get(end_line.min(total_lines))
            .copied()
            .unwrap_or(content.len());

        let mut symbols: Vec<Value> = Vec::new();
        for &nid in &nodes {
            let Some(node) = pdg.get_node(nid) else {
                continue;
            };
            let (sym_start, sym_end) = node.byte_range;

            if sym_end <= visible_start_byte || sym_start >= visible_end_byte {
                continue;
            }

            let line_start = line_byte_offsets
                .iter()
                .position(|&off| off > sym_start)
                .unwrap_or(1);
            let line_end = line_byte_offsets
                .iter()
                .position(|&off| off >= sym_end)
                .unwrap_or(total_lines);

            let caller_count = get_direct_callers(pdg, nid).len();
            let dep_count = pdg.neighbors(nid).len();
            let callers: Vec<String> = get_direct_callers(pdg, nid)
                .iter()
                .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
                .take(5)
                .collect();
            let callees: Vec<String> = pdg
                .neighbors(nid)
                .iter()
                .filter_map(|&did| pdg.get_node(did).map(|n| n.name.clone()))
                .take(5)
                .collect();

            symbols.push(serde_json::json!({
                "name": node.name,
                "type": node_type_str(&node.node_type),
                "line_start": line_start,
                "line_end": line_end,
                "complexity": node.complexity,
                "caller_count": caller_count,
                "dependency_count": dep_count,
                "callers": callers,
                "callees": callees,
            }));
        }

        symbols.sort_by_key(|s| s.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0));
        symbols
    } else {
        Vec::new()
    };

    let context = {
        let visible_start_byte = line_byte_offsets.get(start_line - 1).copied().unwrap_or(0);
        let visible_end_byte = line_byte_offsets
            .get(end_line.min(total_lines))
            .copied()
            .unwrap_or(content.len());

        let mut symbols_here: Vec<String> = Vec::new();
        let mut imports_from: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut used_by: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for &nid in &nodes {
            let Some(node) = pdg.get_node(nid) else {
                continue;
            };
            let (sym_start, sym_end) = node.byte_range;

            if sym_end > visible_start_byte && sym_start < visible_end_byte {
                let ls = line_byte_offsets
                    .iter()
                    .position(|&off| off > sym_start)
                    .unwrap_or(1);
                let le = line_byte_offsets
                    .iter()
                    .position(|&off| off >= sym_end)
                    .unwrap_or(total_lines);
                symbols_here.push(format!("{}(L{}-L{})", node.name, ls, le));
            }

            for &did in &pdg.neighbors(nid) {
                if let Some(dep) = pdg.get_node(did) {
                    if dep.file_path != node.file_path {
                        let dep_content =
                            std::fs::read_to_string(&*dep.file_path).unwrap_or_default();
                        let dep_line = byte_range_to_line_range(&dep_content, dep.byte_range).0;
                        imports_from
                            .insert(format!("{}:{} (L{})", dep.file_path, dep.name, dep_line));
                    }
                }
            }

            for &cid in &get_direct_callers(pdg, nid) {
                if let Some(caller) = pdg.get_node(cid) {
                    if caller.file_path != node.file_path {
                        used_by.insert(format!("{}:{}", caller.file_path, caller.name));
                    }
                }
            }
        }

        let imports_vec: Vec<String> = imports_from.into_iter().take(10).collect();
        let used_by_vec: Vec<String> = used_by.into_iter().take(10).collect();

        Some(serde_json::json!({
            "symbols_on_visible_lines": symbols_here,
            "imports_from": imports_vec,
            "used_by": used_by_vec
        }))
    };

    (symbol_map, context)
}

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

        // Try to get project handle for boundary validation and PDG, but don't require it
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let maybe_handle = if project_path.is_some() {
            Some(registry.get_or_create(project_path).await?)
        } else {
            registry.get_or_create(project_path).await.ok()
        };

        // Validate file within project when indexed
        if let Some(ref handle) = maybe_handle {
            let guard = handle.read().await;
            validate_file_within_project(&file_path, guard.project_path())?;
        }

        // Read file content — works for any text file
        let content = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{}': {}", file_path, e))
        })?;

        let total_lines = content.lines().count();

        // Resolve end_line
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

        if end_line < start_line {
            return Err(JsonRpcError::invalid_params(format!(
                "end_line {} precedes start_line {}",
                end_line_raw, start_line
            )));
        }

        // Build numbered content (1-indexed)
        let visible_lines: Vec<String> = content
            .lines()
            .skip(start_line - 1)
            .take(end_line.min(total_lines) - (start_line - 1))
            .enumerate()
            .map(|(i, line)| format!("{}: {}", start_line + i, line))
            .collect();
        let content_str = visible_lines.join("\n");

        // Detect language from extension (case-insensitive)
        let ext_lower = Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| ext.to_ascii_lowercase());
        let language = ext_lower
            .as_deref()
            .map(|ext| match ext {
                "rs" => "rust",
                "py" => "python",
                "js" | "mjs" | "cjs" => "javascript",
                "ts" | "mts" | "cts" => "typescript",
                "tsx" => "typescriptreact",
                "jsx" => "javascriptreact",
                "go" => "go",
                "java" => "java",
                "c" | "h" => "c",
                "cpp" | "hpp" | "cc" => "cpp",
                "rb" => "ruby",
                "php" => "php",
                "swift" => "swift",
                "kt" => "kotlin",
                "cs" => "csharp",
                "lua" => "lua",
                "zig" => "zig",
                "md" => "markdown",
                "json" => "json",
                "yaml" | "yml" => "yaml",
                "toml" => "toml",
                "html" => "html",
                "css" => "css",
                "scss" => "scss",
                "sql" => "sql",
                "sh" | "bash" => "shell",
                other => other,
            })
            .unwrap_or("text");

        let pdg_snapshot = if let Some(ref handle) = maybe_handle {
            let mut guard = handle.write().await;
            if let Err(e) = guard.ensure_pdg_loaded() {
                tracing::warn!(
                    "PDG load failed for enrichment, degrading gracefully: {}",
                    e
                );
                None
            } else {
                guard.pdg().cloned()
            }
        } else {
            None
        };

        let enrichment_file_path = file_path.clone();
        let (symbol_map, context) = if let Some(pdg) = pdg_snapshot {
            tokio::task::spawn_blocking(move || {
                build_pdg_enrichment(
                    &pdg,
                    &enrichment_file_path,
                    &content,
                    start_line,
                    end_line,
                    total_lines,
                    include_symbol_map,
                )
            })
            .await
            .map_err(|e| {
                JsonRpcError::internal_error(format!(
                    "Failed to build PDG enrichment for '{}': {}",
                    file_path, e
                ))
            })?
        } else {
            (Vec::new(), None)
        };

        let mut result = serde_json::json!({
            "file_path": file_path,
            "language": language,
            "total_lines": total_lines,
            "start_line": start_line,
            "end_line": end_line.min(total_lines),
            "content": content_str,
        });

        // Always attach compact context when available
        if let Some(ctx) = context {
            result["context"] = ctx;
        }

        // Verbose symbol map only when explicitly requested
        if include_symbol_map && !symbol_map.is_empty() {
            result["symbol_map"] = serde_json::json!(symbol_map);
        }

        // Add staleness warning only if we have an indexed project
        if let Some(ref handle) = maybe_handle {
            let guard = handle.read().await;
            result = wrap_with_meta(result, &guard);
        }

        Ok(result)
    }
}
