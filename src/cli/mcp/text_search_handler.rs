use super::helpers::{
    extract_bool, extract_string, extract_usize, glob_match, node_type_str, resolve_scope,
    wrap_live_with_meta, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use super::request_meta::WorkBudget;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::{ProjectHandle, ProjectRegistry};
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Handler for LeIndex [text_search — raw text/regex search across files.
#[derive(Clone)]
pub struct TextSearchHandler;

fn strip_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

type PdgSpans = HashMap<String, Vec<((usize, usize), String, String)>>;

#[derive(Clone)]
struct TextSearchParams {
    query: String,
    is_regex: bool,
    case_sensitive: bool,
    max_results: usize,
    offset: usize,
    context_lines: usize,
    budget: WorkBudget,
    include_globs: Vec<String>,
    exclude_globs: Vec<String>,
    regex: Option<std::sync::Arc<Regex>>,
    search_query: String,
}

struct LiveSearchContext {
    project_root: PathBuf,
    scope: Option<String>,
    source_paths: Vec<PathBuf>,
    pdg_spans: PdgSpans,
    handle: Option<ProjectHandle>,
}

fn string_array(args: &Value, name: &str) -> Vec<String> {
    args.get(name)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_text_search_params(args: &Value) -> Result<TextSearchParams, JsonRpcError> {
    let query = extract_string(args, "query")?;
    let is_regex = extract_bool(args, "is_regex", false);
    let case_sensitive = extract_bool(args, "case_sensitive", false);
    let regex = if is_regex {
        Some(std::sync::Arc::new(
            RegexBuilder::new(&query)
                .case_insensitive(!case_sensitive)
                .build()
                .map_err(|error| {
                    JsonRpcError::invalid_params(format!("Invalid regex '{}': {}", query, error))
                })?,
        ))
    } else {
        None
    };
    let search_query = if case_sensitive {
        query.clone()
    } else {
        query.to_lowercase()
    };

    Ok(TextSearchParams {
        query,
        is_regex,
        case_sensitive,
        max_results: extract_usize(args, "max_results", 100)?.min(1000),
        offset: extract_usize(args, "offset", 0)?,
        context_lines: extract_usize(args, "context_lines", 2)?.min(10),
        budget: WorkBudget {
            max_latency_ms: extract_usize(args, "max_latency_ms", 150)?.min(60000) as u64,
            allow_partial: extract_bool(args, "allow_partial", true),
        },
        include_globs: string_array(args, "include_globs"),
        exclude_globs: string_array(args, "exclude_globs"),
        regex,
        search_query,
    })
}

async fn live_source_inventory(project_root: &Path) -> Result<Vec<PathBuf>, JsonRpcError> {
    let project_root = project_root.to_path_buf();
    tokio::task::spawn_blocking(
        move || match crate::cli::git::source_inventory(&project_root) {
            Ok(paths) => Ok(paths),
            Err(crate::cli::git::GitInventoryError::NotRepository) => {
                let mut paths = Vec::new();
                for entry in walkdir::WalkDir::new(&project_root)
                    .follow_links(false)
                    .into_iter()
                    .filter_entry(|entry| {
                        let name = entry.file_name().to_string_lossy();
                        !crate::skip_dirs::SKIP_DIRS.iter().any(|skip| name == *skip)
                    })
                {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => continue,
                    };
                    if entry.file_type().is_file() {
                        paths.push(entry.path().to_path_buf());
                    }
                }
                Ok(paths)
            }
            Err(error) => Err(anyhow::anyhow!(error.to_string())),
        },
    )
    .await
    .map_err(|error| JsonRpcError::internal_error(format!("live text inventory failed: {error}")))?
    .map_err(|error| JsonRpcError::internal_error(format!("live text inventory failed: {error}")))
}

async fn source_paths_for_scope(
    project_root: &Path,
    scope: Option<&str>,
) -> Result<Vec<PathBuf>, JsonRpcError> {
    let Some(scope) = scope else {
        return live_source_inventory(project_root).await;
    };
    let path = Path::new(scope.trim_end_matches(std::path::MAIN_SEPARATOR));
    if path.is_file() {
        Ok(vec![path.to_path_buf()])
    } else {
        live_source_inventory(project_root).await
    }
}

async fn snapshot_pdg_spans(handle: Option<&ProjectHandle>, source_paths: &[PathBuf]) -> PdgSpans {
    let Some(handle) = handle else {
        return HashMap::new();
    };
    let guard = handle.read().await;
    guard
        .pdg()
        .map(|pdg| {
            source_paths
                .iter()
                .map(|file_path| {
                    let key = file_path.to_string_lossy().into_owned();
                    let spans = pdg
                        .nodes_in_file(&key)
                        .into_iter()
                        .filter_map(|node_id| {
                            let node = pdg.get_node(node_id)?;
                            Some((
                                node.byte_range,
                                node.name.clone(),
                                node_type_str(&node.node_type).to_owned(),
                            ))
                        })
                        .collect();
                    (key, spans)
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn resolve_live_search_context(
    registry: &Arc<ProjectRegistry>,
    args: &Value,
) -> Result<LiveSearchContext, JsonRpcError> {
    let project_root = if let Some(project_path) = args.get("project_path").and_then(Value::as_str)
    {
        LiveProject::resolve(project_path)
            .map_err(|error| JsonRpcError::invalid_params(error.to_string()))?
            .root()
            .to_path_buf()
    } else {
        registry.default_project_path().await?
    };
    let scope = resolve_scope(args, &project_root)?;
    let handle = registry.try_get_loaded(&project_root).await;
    let source_paths = source_paths_for_scope(&project_root, scope.as_deref()).await?;
    let pdg_spans = snapshot_pdg_spans(handle.as_ref(), &source_paths).await;

    Ok(LiveSearchContext {
        project_root,
        scope,
        source_paths,
        pdg_spans,
        handle,
    })
}

fn path_matches_filters(
    file_path: &Path,
    scope: Option<&str>,
    include_globs: &[String],
    exclude_globs: &[String],
) -> bool {
    let file_path = file_path.to_string_lossy();
    scope.iter().all(|scope| file_path.starts_with(scope))
        && (include_globs.is_empty()
            || include_globs
                .iter()
                .any(|glob| glob_match(&file_path, glob)))
        && !exclude_globs
            .iter()
            .any(|glob| glob_match(&file_path, glob))
}

fn line_matches(line: &str, params: &TextSearchParams) -> bool {
    if let Some(regex) = &params.regex {
        regex.is_match(line)
    } else if params.case_sensitive {
        line.contains(&params.search_query)
    } else {
        line.to_lowercase().contains(&params.search_query)
    }
}

fn match_entry(
    file_path: &str,
    line_index: usize,
    lines: &[&str],
    line_byte_offsets: &[usize],
    params: &TextSearchParams,
    pdg_spans: &PdgSpans,
) -> Value {
    let line = strip_line_ending(lines[line_index]);
    let before: Vec<String> = (line_index.saturating_sub(params.context_lines)..line_index)
        .map(|index| format!("{}: {}", index + 1, strip_line_ending(lines[index])))
        .collect();
    let after: Vec<String> = ((line_index + 1)
        ..((line_index + 1 + params.context_lines).min(lines.len())))
        .map(|index| format!("{}: {}", index + 1, strip_line_ending(lines[index])))
        .collect();
    let (in_symbol, symbol_type) = pdg_spans
        .get(file_path)
        .and_then(|spans| {
            let byte_offset = line_byte_offsets[line_index];
            spans
                .iter()
                .filter(|((start, end), _, _)| byte_offset >= *start && byte_offset < *end)
                .min_by_key(|((start, end), _, _)| end.saturating_sub(*start))
                .map(|(_, name, typ)| (name.clone(), typ.clone()))
        })
        .map(|(name, typ)| (Some(name), Some(typ)))
        .unwrap_or((None, None));
    let mut entry = serde_json::json!({
        "file": file_path,
        "line": line_index + 1,
        "content": line,
    });
    if !before.is_empty() {
        entry["before"] = serde_json::json!(before);
    }
    if !after.is_empty() {
        entry["after"] = serde_json::json!(after);
    }
    if let Some(symbol) = in_symbol {
        entry["in_symbol"] = Value::String(symbol);
    }
    if let Some(symbol_type) = symbol_type {
        entry["symbol_type"] = Value::String(symbol_type);
    }
    entry
}

fn scan_file(
    file_path: &Path,
    params: &TextSearchParams,
    pdg_spans: &PdgSpans,
    started: Instant,
    results: &mut Vec<Value>,
) -> bool {
    let content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(_) => return false,
    };
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let line_byte_offsets: Vec<usize> = lines
        .iter()
        .scan(0usize, |offset, line| {
            let current = *offset;
            *offset += line.len();
            Some(current)
        })
        .collect();
    let file_path = file_path.to_string_lossy();

    for (line_index, line) in lines.iter().enumerate() {
        if results.len()
            >= params
                .offset
                .saturating_add(params.max_results)
                .saturating_add(1)
        {
            break;
        }
        if params.budget.elapsed(started) {
            return true;
        }
        if line_matches(strip_line_ending(line), params) {
            results.push(match_entry(
                &file_path,
                line_index,
                &lines,
                &line_byte_offsets,
                params,
                pdg_spans,
            ));
        }
    }
    false
}

fn scan_source_paths(
    source_paths: &[PathBuf],
    scope: Option<&str>,
    params: &TextSearchParams,
    pdg_spans: &PdgSpans,
    started: Instant,
) -> (Vec<Value>, bool) {
    let mut results = Vec::new();
    for file_path in source_paths {
        if results.len()
            >= params
                .offset
                .saturating_add(params.max_results)
                .saturating_add(1)
        {
            break;
        }
        if params.budget.elapsed(started) {
            return (results, true);
        }
        if path_matches_filters(
            file_path,
            scope,
            &params.include_globs,
            &params.exclude_globs,
        ) && scan_file(file_path, params, pdg_spans, started, &mut results)
        {
            return (results, true);
        }
    }
    (results, false)
}

#[allow(missing_docs)]
impl TextSearchHandler {
    pub fn name(&self) -> &str {
        "leindex.text-search"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Text Search]"
    }

    pub fn description(&self) -> &str {
        "PRIMARY text search — use instead of Grep/rg. Returns exact matching lines with \
file:line and the owning symbol name+type for each match. One call replaces Grep + Read \
to understand match context. Supports regex, globs, scope, and context_lines."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Text pattern to search for (literal or regex)"
                },
                "is_regex": {
                    "type": "boolean",
                    "description": "Treat query as regex (default: false = literal match)",
                    "default": false
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default: false)",
                    "default": false
                },
                "include_globs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only search files matching these globs, e.g. [\"*.rs\", \"*.ts\"]"
                },
                "exclude_globs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exclude files matching these globs, e.g. [\"*_test.rs\"]"
                },
                "scope": {
                    "type": "string",
                    "description": "Restrict search to a directory path"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (default: 100)",
                    "default": 100,
                    "minimum": 1,
                    "maximum": 1000
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context above/below each match (default: 2)",
                    "default": 2,
                    "minimum": 0,
                    "maximum": 10
                },
                "max_latency_ms": {
                    "type": "integer",
                    "description": "Optional live scan budget; returns matches found so far (default: 150)",
                    "default": 150,
                    "minimum": 0,
                    "maximum": 60000
                },
                "allow_partial": {
                    "type": "boolean",
                    "description": "Return matches found before the live scan budget is reached",
                    "default": true
                }
            },
            "required": ["query"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let params = parse_text_search_params(&args)?;
        let started = Instant::now();
        let context = resolve_live_search_context(registry, &args).await?;
        let source_paths = context.source_paths.clone();
        let scope = context.scope.clone();
        let scan_params = params.clone();
        let pdg_spans = context.pdg_spans.clone();
        let (results, partial) = tokio::task::spawn_blocking(move || {
            scan_source_paths(
                &source_paths,
                scope.as_deref(),
                &scan_params,
                &pdg_spans,
                started,
            )
        })
        .await
        .map_err(|error| {
            JsonRpcError::internal_error(format!("text search task failed: {error}"))
        })?;
        let total = results.len();
        let paginated: Vec<Value> = results
            .into_iter()
            .skip(params.offset)
            .take(params.max_results)
            .collect();
        let count = paginated.len();

        let mut response = serde_json::json!({
            "query": params.query,
            "is_regex": params.is_regex,
            "offset": params.offset,
            "count": count,
            "total_matched": total,
            "has_more": total > params.offset.saturating_add(params.max_results),
            "results": paginated,
            "retrieval": {
                "tfidf_status": "not_used_exact",
                "pdg_status": if partial {
                    "partial"
                } else if context.pdg_spans.is_empty() {
                    "not_loaded"
                } else {
                    "resident"
                },
                "neural_status": "not_used_exact",
                "max_latency_ms": params.budget.max_latency_ms,
                "allow_partial": params.budget.allow_partial,
                "partial": partial
            }
        });
        if let Some(handle) = context.handle.as_ref() {
            let guard = handle.read().await;
            response = wrap_with_meta(response, &guard);
        } else {
            response = wrap_live_with_meta(response, &context.project_root);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_text_search_live_result_shape_and_pagination() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.rs");
        std::fs::write(&source, "needle first\nneedle second\nneedle third\n").unwrap();
        let registry = test_registry_for(directory.path());

        let response = TextSearchHandler
            .execute(
                &registry,
                serde_json::json!({
                    "query": "needle",
                    "max_results": 2,
                    "offset": 1,
                    "context_lines": 0,
                    "allow_partial": false,
                }),
            )
            .await
            .unwrap();

        assert_eq!(response["query"], "needle");
        assert_eq!(response["offset"], 1);
        assert_eq!(response["total_matched"], 3);
        assert_eq!(response["count"], 2);
        assert_eq!(response["has_more"], false);
        assert_eq!(
            response["results"][0]["file"],
            source.to_string_lossy().as_ref()
        );
        assert_eq!(response["results"][0]["line"], 2);
        assert_eq!(response["results"][0]["content"], "needle second");
        assert_eq!(response["results"][1]["line"], 3);
        assert_eq!(response["results"][1]["content"], "needle third");
        assert!(response["results"][0].get("before").is_none());
        assert!(response["results"][0].get("after").is_none());
        assert_eq!(response["retrieval"]["partial"], false);
    }
}
