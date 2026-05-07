use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
    read_source_snippet, resolve_scope, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::graph::pdg::{NodeId, ProgramDependenceGraph};
use crate::search::ranking::Score;
use regex::RegexBuilder;
use serde_json::Value;
use std::sync::Arc;

/// Options for building a symbol entry in grep results.
struct SymbolEntryOpts {
    context_lines: usize,
    include_source: bool,
    score: Option<Score>,
}

/// Build a JSON symbol entry from a PDG node.
///
/// Extracts common logic shared by all three search modes (semantic, exact, regex)
/// into a single helper. Produces identical output to the original inline code.
fn build_symbol_entry(pdg: &ProgramDependenceGraph, nid: NodeId, opts: &SymbolEntryOpts) -> Value {
    let node = match pdg.get_node(nid) {
        Some(n) => n,
        None => return Value::Null,
    };

    let caller_ids = get_direct_callers(pdg, nid);
    let callers: Vec<String> = caller_ids
        .iter()
        .take(50)
        .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
        .collect();
    let callee_ids = pdg.neighbors(nid);
    let callees: Vec<String> = callee_ids
        .iter()
        .take(50)
        .filter_map(|id| pdg.get_node(*id).map(|n| n.name.clone()))
        .collect();

    let mut entry = serde_json::json!({
        "name": node.name,
        "type": node_type_str(&node.node_type),
        "file": node.file_path,
        "byte_range": node.byte_range,
        "complexity": node.complexity,
        "caller_count": caller_ids.len(),
        "dependency_count": callees.len(),
        "callers": callers,
        "callees": callees,
        "language": node.language,
    });

    if let Some(score) = opts.score {
        entry["score"] = serde_json::json!(score);
    }

    let source = if opts.context_lines > 0 || opts.include_source {
        read_source_snippet(&node.file_path, node.byte_range)
    } else {
        None
    };

    if opts.context_lines > 0 {
        if let Some(ref src) = source {
            let snippet: String = src
                .lines()
                .take(opts.context_lines)
                .collect::<Vec<_>>()
                .join("\n");
            entry["context"] = Value::String(snippet);
        }
    }

    if opts.include_source {
        if let Some(ref src) = source {
            let truncated: String = src.chars().take(4000).collect();
            let was_truncated = src.char_indices().nth(4000).is_some();
            entry["source"] = Value::String(truncated);
            if was_truncated {
                entry["source_truncated"] = Value::Bool(true);
            }
        }
    }

    entry
}

/// Handler for LeIndex [grep_symbols — structurally-aware symbol search.
#[derive(Clone)]
pub struct GrepSymbolsHandler;

#[allow(missing_docs)]
impl GrepSymbolsHandler {
    pub fn name(&self) -> &str {
        "leindex.grep-symbols"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Grep Symbols]"
    }

    pub fn description(&self) -> &str {
        "Search for symbols across the codebase with structural awareness. Supports \
        substring and regex patterns. Results include symbol type (function/class) and \
        its role in the dependency graph."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Symbol name or substring to search for"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "scope": {
                    "type": "string",
                    "description": "Limit results to a file or directory path (optional)"
                },
                "type_filter": {
                    "type": "string",
                    "enum": ["function", "class", "method", "variable", "module", "external", "all"],
                    "description": "Filter by symbol type (default: all)",
                    "default": "all"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1500)",
                    "default": 1500
                },
                "include_context_lines": {
                    "type": "integer",
                    "description": "Source context lines around each match (default: 0, max: 10)",
                    "default": 0,
                    "minimum": 0,
                    "maximum": 10
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results (default: 20, max: 200)",
                    "default": 20,
                    "minimum": 1,
                    "maximum": 200
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include up to 4000 chars of symbol source code in results (default: false)",
                    "default": false
                },
                "mode": {
                    "type": "string",
                    "enum": ["exact", "semantic"],
                    "description": "Search mode: 'exact' for name substring matching (default), 'semantic' for concept-based similarity search using TF-IDF embeddings",
                    "default": "exact"
                }
            },
            "required": ["pattern"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let pattern = extract_string(&args, "pattern")?;
        let type_filter = args
            .get("type_filter")
            .and_then(|v| v.as_str())
            .unwrap_or("all")
            .to_owned();
        let token_budget = extract_usize(&args, "token_budget", 1500)?;
        let max_results = extract_usize(&args, "max_results", 20)?.min(200);
        let context_lines = extract_usize(&args, "include_context_lines", 0)?.min(10);
        let offset = extract_usize(&args, "offset", 0)?;
        let include_source = extract_bool(&args, "include_source", false);
        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("exact")
            .to_owned();
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.write().await;
        let scope = resolve_scope(&args, index.project_path())?;

        const MAX_CANDIDATE_LIMIT: usize = 1000;
        let effective_fetch = (max_results + offset).min(MAX_CANDIDATE_LIMIT);
        let mut candidate_limit = effective_fetch
            .saturating_mul(5)
            .clamp(50, MAX_CANDIDATE_LIMIT);
        let mut candidate_results = index
            .search(&pattern, candidate_limit, None)
            .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;

        index
            .ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;
        if index.pdg().is_none() {
            return Err(JsonRpcError::project_not_indexed(
                index.project_path().display().to_string(),
            ));
        }

        let pattern_lower = pattern.to_lowercase();
        let char_budget = token_budget * 4;

        let fetch_limit = (max_results + offset).min(MAX_CANDIDATE_LIMIT);

        // `seen_ids` uses `String` keys because `PdgNode.id` is a `String`, so
        // `node.id.clone()` is already optimal (single heap allocation, no indirection).
        //
        // `seen_locations` uses `Arc<str>` keys because `PdgNode.file_path` is `Arc<str>`,
        // so `node.file_path.clone()` is a cheap atomic refcount increment — avoiding the
        // heap allocation that `to_string()` would incur for every node visited.
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut seen_locations: std::collections::HashSet<(Arc<str>, (usize, usize))> =
            std::collections::HashSet::new();
        let mut all_matches: Vec<Value> = Vec::new();

        let scope_prefix: Option<String> = scope.as_ref().map(|s| {
            let base = s.trim_end_matches(std::path::MAIN_SEPARATOR);
            format!("{}{}", base, std::path::MAIN_SEPARATOR)
        });
        let scope_exact: Option<String> = scope
            .as_ref()
            .map(|s| s.trim_end_matches(std::path::MAIN_SEPARATOR).to_string());

        if mode == "semantic" {
            'semantic_retry: for _attempt in 0..2 {
                all_matches.clear();
                seen_ids.clear();
                seen_locations.clear();

                index.ensure_pdg_loaded().map_err(|e| {
                    JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e))
                })?;

                for result in &candidate_results {
                    if all_matches.len() >= fetch_limit {
                        break;
                    }
                    let nid = match index.pdg().and_then(|pdg| pdg.find_by_id(&result.node_id)) {
                        Some(id) => id,
                        None => continue,
                    };
                    let node = match index.pdg().and_then(|pdg| pdg.get_node(nid).cloned()) {
                        Some(n) => n,
                        None => continue,
                    };

                    if let Some(ref prefix) = scope_prefix {
                        if !(node.file_path.starts_with(prefix)
                            || node.file_path.as_ref() == scope_exact.as_ref().unwrap().as_str())
                        {
                            continue;
                        }
                    }
                    if type_filter != "all" && node_type_str(&node.node_type) != type_filter {
                        continue;
                    }

                    if matches!(node.node_type, crate::graph::pdg::NodeType::External)
                        && type_filter != "external"
                    {
                        continue;
                    }

                    let entry = build_symbol_entry(
                        index.pdg().unwrap(),
                        nid,
                        &SymbolEntryOpts {
                            context_lines,
                            include_source,
                            score: Some(result.score),
                        },
                    );
                    all_matches.push(entry);
                }

                if all_matches.is_empty() && !candidate_results.is_empty() {
                    let expanded = (candidate_limit * 10).min(1000);
                    if expanded > candidate_limit {
                        candidate_limit = expanded;
                        candidate_results =
                            index.search(&pattern, candidate_limit, None).map_err(|e| {
                                JsonRpcError::search_failed(format!("Search error: {}", e))
                            })?;
                        continue 'semantic_retry;
                    }
                }
                break 'semantic_retry;
            }

            let total_matches = all_matches.len();
            let paginated: Vec<Value> = all_matches
                .into_iter()
                .skip(offset)
                .take(max_results)
                .collect();

            let mut truncated_results: Vec<Value> = Vec::new();
            let mut used_chars: usize = 0;
            for entry in paginated {
                let entry_chars = entry.to_string().len();
                if used_chars + entry_chars > char_budget {
                    break;
                }
                used_chars += entry_chars;
                truncated_results.push(entry);
            }
            let shown = truncated_results.len();

            let mut response = serde_json::json!({
                "results": truncated_results,
                "total_matches": total_matches,
                "shown": shown,
                "offset": offset,
                "mode": "semantic",
                "truncated": total_matches.saturating_sub(offset).min(max_results) > shown,
            });
            response = wrap_with_meta(response, &index);
            return Ok(response);
        }

        let pdg = index.pdg().unwrap();

        for sr in candidate_results {
            if all_matches.len() >= fetch_limit {
                break;
            }

            let Some(nid) = pdg.find_by_id(&sr.node_id) else {
                continue;
            };
            let node = match pdg.get_node(nid) {
                Some(n) => n,
                None => continue,
            };

            if matches!(node.node_type, crate::graph::pdg::NodeType::External)
                && type_filter != "external"
            {
                continue;
            }

            if type_filter != "all" && node_type_str(&node.node_type) != type_filter.as_str() {
                continue;
            }

            if let Some(ref s) = scope {
                if !node.file_path.starts_with(s.as_str())
                    && node.file_path.as_ref() != s.trim_end_matches(std::path::MAIN_SEPARATOR)
                {
                    continue;
                }
            }

            let matches = node.name.to_lowercase().contains(&pattern_lower)
                || node.id.to_lowercase().contains(&pattern_lower);

            let location_key = (node.file_path.clone(), node.byte_range);
            let is_duplicate_location =
                node.byte_range != (0, 0) && seen_locations.contains(&location_key);
            if !matches || seen_ids.contains(&node.id) || is_duplicate_location {
                continue;
            }
            seen_ids.insert(node.id.clone());
            if node.byte_range != (0, 0) {
                seen_locations.insert(location_key);
            }

            let entry = build_symbol_entry(
                pdg,
                nid,
                &SymbolEntryOpts {
                    context_lines,
                    include_source,
                    score: None,
                },
            );
            all_matches.push(entry);
        }

        if all_matches.len() < fetch_limit {
            let re = RegexBuilder::new(&pattern)
                .case_insensitive(true)
                .build()
                .ok();

            for nid in pdg.node_indices() {
                if all_matches.len() >= fetch_limit {
                    break;
                }

                let Some(node) = pdg.get_node(nid) else {
                    continue;
                };

                if matches!(node.node_type, crate::graph::pdg::NodeType::External)
                    && type_filter != "external"
                {
                    continue;
                }

                if type_filter != "all" && node_type_str(&node.node_type) != type_filter.as_str() {
                    continue;
                }

                if let Some(ref s) = scope {
                    if !node.file_path.starts_with(s.as_str())
                        && node.file_path.as_ref() != s.trim_end_matches(std::path::MAIN_SEPARATOR)
                    {
                        continue;
                    }
                }

                let matches = if let Some(ref re) = re {
                    re.is_match(&node.name) || re.is_match(&node.id)
                } else {
                    node.name.to_lowercase().contains(&pattern_lower)
                        || node.id.to_lowercase().contains(&pattern_lower)
                };

                let location_key = (node.file_path.clone(), node.byte_range);
                let is_duplicate_location =
                    node.byte_range != (0, 0) && seen_locations.contains(&location_key);
                if !matches || seen_ids.contains(&node.id) || is_duplicate_location {
                    continue;
                }
                seen_ids.insert(node.id.clone());
                if node.byte_range != (0, 0) {
                    seen_locations.insert(location_key);
                }

                let entry = build_symbol_entry(
                    pdg,
                    nid,
                    &SymbolEntryOpts {
                        context_lines,
                        include_source,
                        score: None,
                    },
                );
                all_matches.push(entry);
            }
        }

        let total_matches = all_matches.len();
        let paginated: Vec<Value> = all_matches
            .into_iter()
            .skip(offset)
            .take(max_results)
            .collect();

        let mut truncated_results: Vec<Value> = Vec::new();
        let mut used_chars: usize = 0;
        for entry in paginated {
            let entry_chars = entry.to_string().len();
            if used_chars + entry_chars > char_budget {
                break;
            }
            used_chars += entry_chars;
            truncated_results.push(entry);
        }
        let shown = truncated_results.len();

        let mut response = serde_json::json!({
            "results": truncated_results,
            "total_matches": total_matches,
            "shown": shown,
            "offset": offset,
            "mode": "exact",
            "truncated": total_matches.saturating_sub(offset).min(max_results) > shown,
        });

        response = wrap_with_meta(response, &index);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_grep_symbols_auto_indexes_returns_empty() {
        // With auto-indexing, a project with no matching symbols returns empty results
        let dir = tempdir().unwrap();
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "pub fn greet() {}\n").unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "pattern": "nonexistent" });
        let result = GrepSymbolsHandler.execute(&registry, args).await;
        // Should succeed (auto-index happens) but with 0 matches
        assert!(result.is_ok(), "auto-indexing should succeed");
        let val = result.unwrap();
        assert_eq!(val["total_matches"].as_u64().unwrap_or(0), 0);
        assert_eq!(val["shown"].as_u64().unwrap_or(1), 0);
    }

    #[test]
    fn test_grep_symbols_schema_has_pagination() {
        let schema = GrepSymbolsHandler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(
            props.get("offset").is_some(),
            "should have 'offset' for pagination"
        );
        assert!(
            props.get("project_path").is_some(),
            "should have 'project_path'"
        );
    }
}
