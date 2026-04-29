use super::helpers::{extract_bool, extract_usize, resolve_scope, wrap_with_meta, HandlerContext};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::path::PathBuf;
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
        "Project structure map — use instead of Glob/ls for directory listing. Shows files \
with symbol counts, complexity hotspots, and inter-module dependency arrows. Supports \
scoping to subdirectories, sorting, and pagination."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Subdirectory to scope to (default: project root)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "depth": {
                    "type": "integer",
                    "description": "Tree depth (default: 3, max: 10)",
                    "default": 3,
                    "minimum": 1,
                    "maximum": 10
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 2000)",
                    "default": 2000
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["complexity", "name", "dependencies", "size"],
                    "description": "Sort order (default: complexity)",
                    "default": "complexity"
                },
                "include_symbols": {
                    "type": "boolean",
                    "description": "Include top symbols per file (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N files for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of files to return (default: unlimited, subject to token_budget)",
                    "minimum": 1
                },
                "focus": {
                    "type": "string",
                    "description": "Semantic focus area — ranks files by relevance to this topic (e.g., 'authentication', 'database layer', 'payment flow')"
                }
            },
            "required": []
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let sort_by = args
            .get("sort_by")
            .and_then(|v| v.as_str())
            .unwrap_or("complexity")
            .to_owned();
        let depth = extract_usize(&args, "depth", 3)?.min(10);
        let token_budget = extract_usize(&args, "token_budget", 2000)?;
        let include_symbols = extract_bool(&args, "include_symbols", false);
        let offset = extract_usize(&args, "offset", 0)?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let focus = args.get("focus").and_then(|v| v.as_str()).map(String::from);
        let mut ctx = HandlerContext::new(registry, &args).await?;
        let project_root = ctx.project_path().to_path_buf();

        // Allow legacy "path" param; map it into "scope" for resolution.
        let mut args_with_scope = args.clone();
        if let Some(obj) = args_with_scope.as_object_mut() {
            if !obj.contains_key("scope") {
                if let Some(p) = obj.get("path").cloned() {
                    obj.insert("scope".to_string(), p);
                }
            }
        }
        let scope = resolve_scope(&args_with_scope, ctx.project_path())?;
        let scope_str = scope.unwrap_or_else(|| {
            let mut s = project_root.to_string_lossy().to_string();
            if !s.ends_with(std::path::MAIN_SEPARATOR) {
                s.push(std::path::MAIN_SEPARATOR);
            }
            s
        });
        let scope_path = PathBuf::from(&scope_str);
        let scope_base = PathBuf::from(
            scope_str.trim_end_matches(|c| c == '/' || c == std::path::MAIN_SEPARATOR),
        );

        // Use cached file stats if available, otherwise build from PDG
        // Collect source paths first to avoid borrow conflicts with file_stats()/pdg()
        let source_paths = ctx.index_mut().source_file_paths().unwrap_or_default();

        // file → (symbol_count, total_complexity, symbol_names, incoming_deps, outgoing_deps)
        let file_map: std::collections::HashMap<String, (usize, u32, Vec<String>, usize, usize)> =
            if let Some(cache) = ctx.index().file_stats() {
                // Fast path: use cached statistics (includes pre-computed dep counts)
                let mut map: std::collections::HashMap<
                    String,
                    (usize, u32, Vec<String>, usize, usize),
                > = source_paths
                    .into_iter()
                    .map(|path| (path.display().to_string(), (0, 0, Vec::new(), 0, 0)))
                    .collect();

                // Overlay cached statistics, capping symbol_names to top 5
                for (path, stats) in cache.iter() {
                    let capped: Vec<String> = stats.symbol_names.iter().take(5).cloned().collect();
                    map.insert(
                        path.clone(),
                        (
                            stats.symbol_count,
                            stats.total_complexity,
                            capped,
                            stats.incoming_deps,
                            stats.outgoing_deps,
                        ),
                    );
                }
                map
            } else {
                // Fallback: build from PDG via the same method used at index time
                ctx.index_mut().build_file_stats_cache();
                let mut map: std::collections::HashMap<
                    String,
                    (usize, u32, Vec<String>, usize, usize),
                > = source_paths
                    .into_iter()
                    .map(|path| (path.display().to_string(), (0, 0, Vec::new(), 0, 0)))
                    .collect();

                if let Some(cache) = ctx.index().file_stats() {
                    for (path, stats) in cache.iter() {
                        let capped: Vec<String> =
                            stats.symbol_names.iter().take(5).cloned().collect();
                        map.insert(
                            path.clone(),
                            (
                                stats.symbol_count,
                                stats.total_complexity,
                                capped,
                                stats.incoming_deps,
                                stats.outgoing_deps,
                            ),
                        );
                    }
                }
                map
            }; // file → (node_count, total_complexity, symbol_names)

        // Get PDG for scope filtering (no degree computation needed — cached in file_map)
        let _pdg = ctx
            .maybe_pdg()
            .ok_or_else(|| JsonRpcError::project_not_indexed(project_root.display().to_string()))?;

        // Filter to scope path and respect depth.
        // Files must either be exactly in the scope directory or in a subdirectory.
        let mut files: Vec<Value> = file_map
            .iter()
            .filter(|(fp, _)| {
                // File is in scope if its path starts with scope_str (directory prefix)
                // or if the file IS the scope path (exact match for single-file scope)
                fp.starts_with(&scope_str) || fp.as_str() == scope_path.to_str().unwrap_or("")
            })
            .filter_map(|(fp, (count, complexity, syms, in_deg, out_deg))| {
                let path = std::path::Path::new(fp);
                let rel = path.strip_prefix(&scope_base).ok()?;
                let directory_depth = rel
                    .parent()
                    .map(|parent| parent.components().count())
                    .unwrap_or(0);
                if directory_depth > depth {
                    return None;
                }

                let mut entry = serde_json::json!({
                    "path": fp,
                    "relative_path": rel.display().to_string(),
                    "symbol_count": count,
                    "total_complexity": complexity,
                    "incoming_dependencies": in_deg,
                    "outgoing_dependencies": out_deg
                });
                if include_symbols || focus.is_some() {
                    entry["top_symbols"] =
                        Value::Array(syms.iter().map(|s| Value::String(s.clone())).collect());
                }
                Some(entry)
            })
            .collect();

        // Sort
        match sort_by.as_str() {
            "complexity" => files.sort_by(|a, b| {
                b["total_complexity"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&a["total_complexity"].as_u64().unwrap_or(0))
            }),
            "name" => files.sort_by(|a, b| {
                a["relative_path"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["relative_path"].as_str().unwrap_or(""))
            }),
            "dependencies" => files.sort_by(|a, b| {
                let a_deg = a["incoming_dependencies"].as_u64().unwrap_or(0)
                    + a["outgoing_dependencies"].as_u64().unwrap_or(0);
                let b_deg = b["incoming_dependencies"].as_u64().unwrap_or(0)
                    + b["outgoing_dependencies"].as_u64().unwrap_or(0);
                b_deg.cmp(&a_deg)
            }),
            "size" => files.sort_by(|a, b| {
                b["symbol_count"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&a["symbol_count"].as_u64().unwrap_or(0))
            }),
            _ => {}
        }

        // Semantic focus ranking: when focus is provided, re-rank files by
        // cosine similarity between the focus embedding and per-file symbol embeddings.
        if let Some(ref focus_text) = focus {
            let focus_emb = ctx.index().generate_query_embedding(focus_text);
            // Cache file embeddings by symbol text to avoid recomputing
            // for files with identical symbol sets.
            let mut emb_cache: std::collections::HashMap<String, Vec<f32>> =
                std::collections::HashMap::new();
            for entry in &mut files {
                let syms = entry["top_symbols"].as_array();
                let file_text = syms
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                if file_text.is_empty() {
                    entry["relevance_score"] = serde_json::json!(0.0);
                    continue;
                }
                let file_emb = emb_cache
                    .entry(file_text.clone())
                    .or_insert_with(|| ctx.index().generate_query_embedding(&file_text));
                let score = crate::search::vector::cosine_similarity(&focus_emb, file_emb);
                entry["relevance_score"] = serde_json::json!(score);
            }
            files.sort_by(|a, b| {
                let sa = a["relevance_score"].as_f64().unwrap_or(0.0);
                let sb = b["relevance_score"].as_f64().unwrap_or(0.0);
                sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
            });
            // Remove top_symbols from output if not requested
            if !include_symbols {
                for entry in &mut files {
                    entry.as_object_mut().map(|o| o.remove("top_symbols"));
                }
            }
        }

        // Apply pagination: offset + limit
        let total_before_pagination = files.len();
        let files: Vec<Value> = files.into_iter().skip(offset).collect();
        let files: Vec<Value> = if let Some(lim) = limit {
            files.into_iter().take(lim).collect()
        } else {
            files
        };

        // Truncate to token budget
        let char_budget = token_budget * 4;
        let mut total_chars = 0;
        let mut truncated_files: Vec<Value> = Vec::new();
        for f in files {
            let s = f.to_string();
            total_chars += s.len();
            if total_chars > char_budget {
                break;
            }
            truncated_files.push(f);
        }

        Ok(wrap_with_meta(
            serde_json::json!({
                "project_root": project_root.display().to_string(),
                "scope": scope_path.display().to_string(),
                "total_files_in_scope": total_before_pagination,
                "offset": offset,
                "count": truncated_files.len(),
                "has_more": offset + truncated_files.len() < total_before_pagination,
                "files": truncated_files
            }),
            ctx.index(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_project_map_auto_indexes_empty_project() {
        // With auto-indexing, an empty project returns an empty file list (not an error)
        let dir = tempdir().unwrap();
        // Create a minimal source file so indexing has something to find
        let src = dir.path().join("main.rs");
        std::fs::write(&src, "fn main() {}\n").unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({});
        let result = ProjectMapHandler.execute(&registry, args).await;
        assert!(result.is_ok(), "auto-indexing should succeed");
    }

    #[tokio::test]
    async fn test_project_map_includes_nested_and_symbol_less_files_with_directory_depth() {
        let dir = tempdir().unwrap();
        let nested_dir = dir.path().join("src").join("nested");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src").join("empty.rs"), "\n").unwrap();
        std::fs::write(nested_dir.join("mod.rs"), "pub fn helper() {}\n").unwrap();

        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({
            "depth": 2,
            "sort_by": "name",
            "token_budget": 10_000
        });
        let result = ProjectMapHandler.execute(&registry, args).await.unwrap();
        let files = result["files"].as_array().unwrap();
        let relative_paths: Vec<String> = files
            .iter()
            .filter_map(|entry| entry["relative_path"].as_str())
            .map(|p| p.replace('\\', "/"))
            .collect();

        assert!(relative_paths.iter().any(|p| p == "main.rs"));
        assert!(relative_paths.iter().any(|p| p == "src/empty.rs"));
        assert!(relative_paths.iter().any(|p| p == "src/nested/mod.rs"));
    }

    #[test]
    fn test_project_map_schema_has_pagination() {
        let schema = ProjectMapHandler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("offset").is_some(), "should have 'offset'");
        assert!(props.get("limit").is_some(), "should have 'limit'");
        assert!(
            props.get("project_path").is_some(),
            "should have 'project_path'"
        );
    }
}
