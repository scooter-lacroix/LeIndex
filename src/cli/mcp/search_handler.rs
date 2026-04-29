use super::helpers::{
    extract_string, extract_usize, resolve_scope, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_search
///
/// Performs semantic search on the indexed code.
#[derive(Clone)]
pub struct SearchHandler;

impl SearchHandler {
    /// Returns the name of this RPC method
    pub fn name(&self) -> &str {
        "leindex_search"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Semantic code search. Finds symbols by meaning, not just name. Returns ranked \
results with composite scores (semantic + text + structural). Accepts project_path \
to auto-switch/auto-index projects."
    }

    /// Returns the JSON schema for the arguments of this RPC method
    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (e.g., 'authentication', 'database connection')"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "top_k": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 10)",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 100
                },
                "scope": {
                    "type": "string",
                    "description": "Optional path to limit results (absolute or relative to project root)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "search_mode": {
                    "type": "string",
                    "enum": ["code", "prose", "auto"],
                    "description": "Scoring mode: 'code' (default) emphasizes semantic/structural similarity, \
        'prose' boosts text-match weight for natural-language queries (e.g. roadmap, README content), \
        'auto' detects based on query shape.",
                    "default": "code"
                }
            },
            "required": ["query"]
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let top_k = extract_usize(&args, "top_k", 10)?;
        let offset = extract_usize(&args, "offset", 0)?;
        let search_mode = args
            .get("search_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("code");

        // Resolve query type
        let query_type = match search_mode {
            "prose" => Some(crate::search::ranking::QueryType::Text),
            "code" => Some(crate::search::ranking::QueryType::Semantic),
            "auto" => {
                let q_lower = query.to_lowercase();
                let prose_keywords = [
                    "how", "what", "where", "why", "who", "when", "can", "is", "explain",
                    "describe", "find", "show",
                ];
                let is_natural_language = q_lower.split_whitespace().count() > 3
                    || prose_keywords.iter().any(|k| q_lower.contains(k));

                if is_natural_language {
                    Some(crate::search::ranking::QueryType::Text)
                } else {
                    Some(crate::search::ranking::QueryType::Semantic)
                }
            }
            _ => Some(crate::search::ranking::QueryType::Semantic),
        };

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        guard
            .ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        let scope = resolve_scope(&args, guard.project_path())?;

        if guard.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        const MAX_FETCH_K: usize = 1000;
        let mut fetch_k = (top_k + offset).min(MAX_FETCH_K);
        let mut all_results = guard
            .search(&query, fetch_k, query_type)
            .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;

        let in_scope = |file_path: &str| match &scope {
            Some(s) => {
                let scope_str = s.trim_end_matches(std::path::MAIN_SEPARATOR);
                if std::path::Path::new(scope_str).extension().is_some() {
                    file_path == scope_str
                } else {
                    file_path.starts_with(&format!("{}{}", scope_str, std::path::MAIN_SEPARATOR))
                        || file_path == scope_str
                }
            }
            None => true,
        };

        let mut filtered: Vec<_> = all_results
            .iter()
            .filter(|r| in_scope(&r.file_path))
            .cloned()
            .collect();

        if filtered.is_empty() && scope.is_some() && !all_results.is_empty() {
            fetch_k = (fetch_k * 10).min(MAX_FETCH_K * 10);
            if fetch_k > top_k + offset {
                all_results = guard
                    .search(&query, fetch_k, query_type)
                    .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;
                filtered = all_results
                    .iter()
                    .filter(|r| in_scope(&r.file_path))
                    .cloned()
                    .collect();
            }
        }

        let total_filtered = filtered.len();
        let page: Vec<_> = filtered.into_iter().skip(offset).take(top_k).collect();
        let total_returned = page.len();

        if total_filtered == 0 {
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "results": [],
                    "offset": offset,
                    "count": 0,
                    "has_more": false,
                    "suggestion": format!(
                        "No semantic matches found for '{}'. The project contains {} indexed files. \
                        Try: rephrase query, use different keywords, or try leindex_grep_symbols for exact symbol names.",
                        query,
                        guard.source_file_paths().map(|p| p.len()).unwrap_or(0)
                    )
                }),
                &guard,
            ));
        }

        Ok(wrap_with_meta(
            serde_json::json!({
                "results": serde_json::to_value(&page).map_err(|e|
                    JsonRpcError::internal_error(format!("Serialization error: {}", e)))?,
                "offset": offset,
                "count": total_returned,
                "has_more": offset + total_returned < total_filtered
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
    async fn test_search_handler_zero_results_includes_suggestion() {
        // Test that semantic search with no matches returns helpful suggestion
        let dir = tempdir().unwrap();
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "pub fn hello() {}\n").unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "query": "nonexistent_function_xyz" });
        let result = SearchHandler.execute(&registry, args).await;
        // Should succeed but with 0 matches
        assert!(result.is_ok(), "search should succeed");
        let val = result.unwrap();
        assert_eq!(val["count"].as_i64().unwrap_or(0), 0);
        // Verify suggestion field is present for zero results
        assert!(
            val.get("suggestion").is_some(),
            "zero results should include suggestion"
        );
    }

    #[test]
    fn test_search_schema_has_pagination() {
        let handler = SearchHandler;
        let schema = handler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("offset").is_some());
    }
}
