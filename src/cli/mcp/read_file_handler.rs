use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
    validate_file_within_project, wrap_live_with_meta, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::graph::pdg::ProgramDependenceGraph;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Handler for LeIndex [read_file — PDG-annotated file read.
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

fn byte_to_line_start(offsets: &[usize], byte_pos: usize, total_lines: usize) -> usize {
    offsets
        .partition_point(|&off| off <= byte_pos)
        .min(total_lines.max(1))
}

fn byte_to_line_end(offsets: &[usize], byte_pos: usize, total_lines: usize) -> usize {
    offsets
        .partition_point(|&off| off < byte_pos)
        .min(total_lines.max(1))
}

const LANGUAGE_BY_EXTENSION: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("py", "python"),
    ("js", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("ts", "typescript"),
    ("mts", "typescript"),
    ("cts", "typescript"),
    ("tsx", "typescriptreact"),
    ("jsx", "javascriptreact"),
    ("go", "go"),
    ("java", "java"),
    ("c", "c"),
    ("h", "c"),
    ("cpp", "cpp"),
    ("hpp", "cpp"),
    ("cc", "cpp"),
    ("rb", "ruby"),
    ("php", "php"),
    ("swift", "swift"),
    ("kt", "kotlin"),
    ("cs", "csharp"),
    ("lua", "lua"),
    ("zig", "zig"),
    ("md", "markdown"),
    ("json", "json"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("html", "html"),
    ("css", "css"),
    ("scss", "scss"),
    ("sql", "sql"),
    ("sh", "shell"),
    ("bash", "shell"),
];

fn detect_language(extension: &str) -> &str {
    LANGUAGE_BY_EXTENSION
        .iter()
        .find(|(known_extension, _)| *known_extension == extension)
        .map_or(extension, |(_, language)| *language)
}

async fn resolve_project_root(
    registry: &Arc<ProjectRegistry>,
    args: &Value,
) -> Result<PathBuf, JsonRpcError> {
    if let Some(project_path) = args.get("project_path").and_then(|value| value.as_str()) {
        return LiveProject::resolve(project_path)
            .map(|project| project.root().to_path_buf())
            .map_err(|error| JsonRpcError::invalid_params(error.to_string()));
    }

    registry.default_project_path().await
}

async fn read_visible_content(
    resolved_file_path: &Path,
    args: &Value,
    start_line: usize,
    max_lines: usize,
) -> Result<(String, usize, usize, String), JsonRpcError> {
    let content = tokio::fs::read_to_string(resolved_file_path)
        .await
        .map_err(|error| {
            JsonRpcError::invalid_params(format!(
                "Cannot read file '{}': {}",
                resolved_file_path.display(),
                error
            ))
        })?;
    let total_lines = content.lines().count();
    let end_line_raw = extract_usize(args, "end_line", total_lines)?;
    let end_line = end_line_raw
        .min(total_lines)
        .min(start_line + max_lines - 1);

    if total_lines > 0 && start_line > total_lines {
        return Err(JsonRpcError::invalid_params(format!(
            "start_line {} exceeds total lines {}",
            start_line, total_lines
        )));
    }
    if total_lines > 0 && end_line < start_line {
        return Err(JsonRpcError::invalid_params(format!(
            "end_line {} precedes start_line {}",
            end_line_raw, start_line
        )));
    }

    let visible_lines: Vec<String> = if total_lines == 0 {
        Vec::new()
    } else {
        content
            .lines()
            .skip(start_line - 1)
            .take(end_line.min(total_lines) - (start_line - 1))
            .enumerate()
            .map(|(index, line)| format!("{}: {}", start_line + index, line))
            .collect()
    };
    Ok((content, total_lines, end_line, visible_lines.join("\n")))
}

fn visible_byte_range(
    line_offsets: &[usize],
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    content_len: usize,
) -> (usize, usize) {
    (
        line_offsets.get(start_line - 1).copied().unwrap_or(0),
        line_offsets
            .get(end_line.min(total_lines))
            .copied()
            .unwrap_or(content_len),
    )
}

fn build_symbol_map(
    pdg: &ProgramDependenceGraph,
    file_path: &str,
    line_offsets: &[usize],
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    content_len: usize,
    include_symbol_map: bool,
) -> Vec<Value> {
    if !include_symbol_map {
        return Vec::new();
    }

    let (visible_start_byte, visible_end_byte) =
        visible_byte_range(line_offsets, start_line, end_line, total_lines, content_len);
    let mut symbols = Vec::new();

    for nid in pdg.nodes_in_file(file_path) {
        let Some(node) = pdg.get_node(nid) else {
            continue;
        };
        let (sym_start, sym_end) = node.byte_range;
        if sym_end <= visible_start_byte || sym_start >= visible_end_byte {
            continue;
        }

        let line_start = byte_to_line_start(line_offsets, sym_start, total_lines);
        let line_end = byte_to_line_end(line_offsets, sym_end, total_lines);
        let caller_count = get_direct_callers(pdg, nid).len();
        let dep_count = pdg.neighbors(nid).len();
        let callers = get_direct_callers(pdg, nid)
            .iter()
            .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
            .take(5)
            .collect::<Vec<_>>();
        let callees = pdg
            .neighbors(nid)
            .iter()
            .filter_map(|&did| pdg.get_node(did).map(|n| n.name.clone()))
            .take(5)
            .collect::<Vec<_>>();

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

    symbols.sort_by_key(|symbol| {
        symbol
            .get("line_start")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
    });
    symbols
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
    let file_line_offsets = line_byte_offsets(content);
    let nodes = pdg.nodes_in_file(file_path);
    let mut dep_line_offsets_cache: HashMap<Arc<str>, Vec<usize>> = HashMap::new();

    let symbol_map = build_symbol_map(
        pdg,
        file_path,
        &file_line_offsets,
        start_line,
        end_line,
        total_lines,
        content.len(),
        include_symbol_map,
    );

    let context = {
        let (visible_start_byte, visible_end_byte) = visible_byte_range(
            &file_line_offsets,
            start_line,
            end_line,
            total_lines,
            content.len(),
        );

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
                let ls = byte_to_line_start(&file_line_offsets, sym_start, total_lines);
                let le = byte_to_line_end(&file_line_offsets, sym_end, total_lines);
                symbols_here.push(format!("{}(L{}-L{})", node.name, ls, le));
            }

            for &did in &pdg.neighbors(nid) {
                if let Some(dep) = pdg.get_node(did) {
                    if dep.file_path != node.file_path {
                        let dep_offsets = dep_line_offsets_cache
                            .entry(dep.file_path.clone())
                            .or_insert_with(|| {
                                std::fs::read_to_string(&*dep.file_path)
                                    .map(|c| line_byte_offsets(&c))
                                    .unwrap_or_default()
                            });
                        if dep_offsets.is_empty() {
                            continue;
                        }
                        let dep_total_lines = dep_offsets.len().saturating_sub(1);
                        let dep_line =
                            byte_to_line_start(dep_offsets, dep.byte_range.0, dep_total_lines);
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
        "leindex.read-file"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Read File]"
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

        // Resolve and validate against the live project without creating or
        // hydrating a registry entry. Resident PDG data is enrichment only;
        // an exact file read must remain useful before any index exists.
        let project_root = resolve_project_root(registry, &args).await?;
        let resolved_file_path = validate_file_within_project(&file_path, &project_root)?;
        let maybe_handle = registry.try_get_loaded(&project_root).await;

        let (content, total_lines, end_line, content_str) =
            read_visible_content(&resolved_file_path, &args, start_line, max_lines).await?;

        // Detect language from extension (case-insensitive)
        let ext_lower = Path::new(&file_path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase());
        let language = ext_lower.as_deref().map(detect_language).unwrap_or("text");

        let pdg_snapshot = if let Some(ref handle) = maybe_handle {
            let guard = handle.read().await;
            guard.pdg().cloned()
        } else {
            None
        };

        let pdg_status = if pdg_snapshot.is_some() {
            "fresh"
        } else {
            "not_loaded"
        };
        let enrichment_file_path = resolved_file_path.to_string_lossy().to_string();
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

        result["retrieval"] = serde_json::json!({
            "tfidf_status": "not_used_exact",
            "pdg_status": pdg_status,
            "neural_status": "not_used_exact"
        });

        // Verbose symbol map only when explicitly requested
        if !symbol_map.is_empty() {
            result["symbol_map"] = serde_json::json!(symbol_map);
        }

        // Add staleness warning only if we have an indexed project
        if let Some(ref handle) = maybe_handle {
            let guard = handle.read().await;
            result = wrap_with_meta(result, &guard);
        } else {
            result = wrap_live_with_meta(result, &project_root);
        }

        Ok(result)
    }
}
