use super::helpers::{extract_bool, extract_usize, resolve_scope, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

type FileMap = std::collections::HashMap<String, (usize, u32, Vec<String>, usize, usize)>;

/// Handler for LeIndex [project_map — annotated project tree replacing Glob/ls.
#[derive(Clone)]
pub struct ProjectMapHandler;

#[allow(missing_docs)]
impl ProjectMapHandler {
    pub fn name(&self) -> &str {
        "leindex.project-map"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Project Map]"
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

        let project_root = guard.project_path().to_path_buf();

        let (scope_str, scope_path, scope_base) =
            Self::scope_paths(&args, guard.project_path(), &project_root)?;

        let file_map = Self::build_file_map(&mut guard);

        // Get PDG for scope filtering (no degree computation needed — cached in file_map)
        let _pdg = guard
            .pdg()
            .ok_or_else(|| JsonRpcError::project_not_indexed(project_root.display().to_string()))?;

        let mut files = Self::files_in_scope(
            &file_map,
            &scope_str,
            &scope_path,
            &scope_base,
            depth,
            include_symbols || focus.is_some(),
        );
        Self::sort_and_rank_files(
            &mut guard,
            &mut files,
            &sort_by,
            focus.as_deref(),
            include_symbols,
        );

        let (total_before_pagination, truncated_files) =
            Self::paginate_and_truncate(files, offset, limit, token_budget);

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
            &guard,
        ))
    }

    fn scope_paths(
        args: &Value,
        project_path: &Path,
        project_root: &Path,
    ) -> Result<(String, PathBuf, PathBuf), JsonRpcError> {
        // Allow legacy "path" param; map it into "scope" for resolution.
        let mut args_with_scope = args.clone();
        if let Some(obj) = args_with_scope.as_object_mut() {
            if !obj.contains_key("scope") {
                if let Some(path) = obj.get("path").cloned() {
                    obj.insert("scope".to_string(), path);
                }
            }
        }
        let scope = resolve_scope(&args_with_scope, project_path)?;
        let scope_str = scope.unwrap_or_else(|| {
            let mut root = project_root.to_string_lossy().to_string();
            if !root.ends_with(std::path::MAIN_SEPARATOR) {
                root.push(std::path::MAIN_SEPARATOR);
            }
            root
        });
        let scope_path = PathBuf::from(&scope_str);
        let scope_base =
            PathBuf::from(scope_str.trim_end_matches(['/', std::path::MAIN_SEPARATOR]));
        Ok((scope_str, scope_path, scope_base))
    }

    fn paginate_and_truncate(
        files: Vec<Value>,
        offset: usize,
        limit: Option<usize>,
        token_budget: usize,
    ) -> (usize, Vec<Value>) {
        let total_before_pagination = files.len();
        let files: Vec<Value> = files
            .into_iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .collect();

        let char_budget = token_budget * 4;
        let mut total_chars = 0;
        let mut truncated_files = Vec::new();
        for file in files {
            total_chars += file.to_string().len();
            if total_chars > char_budget {
                break;
            }
            truncated_files.push(file);
        }
        (total_before_pagination, truncated_files)
    }

    fn build_file_map(guard: &mut crate::cli::leindex::LeIndex) -> FileMap {
        // Collect source paths first to avoid borrow conflicts with file_stats()/pdg().
        let source_paths = guard.source_file_paths().unwrap_or_default();
        if guard.file_stats().is_none() {
            guard.build_file_stats_cache();
        }

        // file → (symbol_count, total_complexity, symbol_names, incoming_deps, outgoing_deps)
        let mut file_map: FileMap = source_paths
            .into_iter()
            .map(|path| (path.display().to_string(), (0, 0, Vec::new(), 0, 0)))
            .collect();

        // Overlay cached statistics, capping symbol_names to top 5.
        if let Some(cache) = guard.file_stats() {
            for (path, stats) in cache.iter() {
                let capped = stats.symbol_names.iter().take(5).cloned().collect();
                file_map.insert(
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

        file_map
    }

    fn files_in_scope(
        file_map: &FileMap,
        scope_str: &str,
        scope_path: &Path,
        scope_base: &Path,
        depth: usize,
        include_symbols: bool,
    ) -> Vec<Value> {
        file_map
            .iter()
            .filter(|(fp, _)| {
                fp.starts_with(scope_str) || fp.as_str() == scope_path.to_str().unwrap_or("")
            })
            .filter_map(|(fp, (count, complexity, syms, in_deg, out_deg))| {
                let path = std::path::Path::new(fp);
                let rel = path.strip_prefix(scope_base).ok()?;
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
                if include_symbols {
                    entry["top_symbols"] =
                        Value::Array(syms.iter().map(|s| Value::String(s.clone())).collect());
                }
                Some(entry)
            })
            .collect()
    }

    fn sort_and_rank_files(
        guard: &mut crate::cli::leindex::LeIndex,
        files: &mut [Value],
        sort_by: &str,
        focus: Option<&str>,
        include_symbols: bool,
    ) {
        match sort_by {
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

        let Some(focus_text) = focus else {
            return;
        };
        let focus_emb = guard.generate_query_embedding(focus_text);
        let mut emb_cache = std::collections::HashMap::new();
        for entry in files.iter_mut() {
            let file_text = entry["top_symbols"]
                .as_array()
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
                .or_insert_with(|| guard.generate_query_embedding(&file_text));
            let score = crate::search::vector::cosine_similarity(&focus_emb, file_emb);
            entry["relevance_score"] = serde_json::json!(score);
        }
        files.sort_by(|a, b| {
            let sa = a["relevance_score"].as_f64().unwrap_or(0.0);
            let sb = b["relevance_score"].as_f64().unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        if !include_symbols {
            for entry in files.iter_mut() {
                entry.as_object_mut().map(|o| o.remove("top_symbols"));
            }
        }
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
    async fn test_project_map_cache_preserves_nested_and_symbol_less_files_with_directory_depth() {
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
        let result = ProjectMapHandler
            .execute(&registry, args.clone())
            .await
            .unwrap();
        let files = result["files"].as_array().unwrap();
        let relative_paths: Vec<String> = files
            .iter()
            .filter_map(|entry| entry["relative_path"].as_str())
            .map(|p| p.replace('\\', "/"))
            .collect();

        assert!(relative_paths.iter().any(|p| p == "main.rs"));
        assert!(relative_paths.iter().any(|p| p == "src/empty.rs"));
        assert!(relative_paths.iter().any(|p| p == "src/nested/mod.rs"));

        let cached_result = ProjectMapHandler.execute(&registry, args).await.unwrap();
        assert_eq!(cached_result["files"], result["files"]);
    }

    #[test]
    fn test_project_map_pagination_applies_offset_and_limit() {
        let files = vec![
            serde_json::json!({"relative_path": "first.rs"}),
            serde_json::json!({"relative_path": "second.rs"}),
            serde_json::json!({"relative_path": "third.rs"}),
        ];

        let (total, files) = ProjectMapHandler::paginate_and_truncate(files, 1, Some(1), 10_000);

        assert_eq!(total, 3);
        assert_eq!(
            files,
            vec![serde_json::json!({"relative_path": "second.rs"})]
        );
    }

    #[test]
    fn test_project_map_truncation_respects_token_budget() {
        let (_, files) = ProjectMapHandler::paginate_and_truncate(
            vec![serde_json::json!({"relative_path": "file.rs"})],
            0,
            None,
            1,
        );

        assert!(files.is_empty());
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
