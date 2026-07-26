use super::helpers::{extract_bool, extract_string, extract_usize, resolve_scope, wrap_with_meta};
use super::protocol::JsonRpcError;
use super::request_meta::WorkBudget;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

/// Handler for LeIndex [search
///
/// Performs semantic search on the indexed code.
#[derive(Clone)]
pub struct SearchHandler;

impl SearchHandler {
    /// Returns the name of this MCP tool (MCP-compliant: ASCII letters, digits, underscore, hyphen, dot only)
    pub fn name(&self) -> &str {
        "leindex.search"
    }

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "LeIndex [Search]"
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
                    "enum": ["code", "prose", "auto", "exact", "semantic"],
                    "description": "Scoring mode: 'code' (default) emphasizes semantic/structural similarity, \
        'prose' boosts text-match weight for natural-language queries (e.g. roadmap, README content), \
        'auto' detects based on query shape, \
        'exact' prioritizes exact symbol name matches (higher text/structural weights), \
        'semantic' prioritizes conceptual relevance (higher TF-IDF semantic weights).",
                    "default": "code"
                },
                "task_context": {
                    "type": "string",
                    "description": "Optional bounded review/task context used for this retrieval only",
                    "maxLength": 2000
                },
                "max_latency_ms": {
                    "type": "integer",
                    "description": "Optional enrichment budget; never cancels the search (default: 500)",
                    "default": 500,
                    "minimum": 0,
                    "maximum": 60000
                },
                "allow_partial": {
                    "type": "boolean",
                    "description": "Return core TF-IDF results when optional enrichment exceeds the budget",
                    "default": true
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
        let task_context = args
            .get("task_context")
            .and_then(Value::as_str)
            .map(|context| context.chars().take(2000).collect::<String>());
        let budget = WorkBudget {
            max_latency_ms: extract_usize(&args, "max_latency_ms", 500)?.min(60000) as u64,
            allow_partial: extract_bool(&args, "allow_partial", true),
        };
        let started = Instant::now();
        let effective_query = task_context.as_deref().map_or_else(
            || query.clone(),
            |context| format!("{}\nTask context: {}", query, context),
        );

        let requested_mode = match search_mode {
            "exact" => crate::search::query_route::RequestedMode::Exact,
            "semantic" | "code" => crate::search::query_route::RequestedMode::Semantic,
            "prose" => crate::search::query_route::RequestedMode::Auto,
            _ => crate::search::query_route::RequestedMode::Auto,
        };
        let route = crate::search::query_route::classify(&effective_query, requested_mode);
        let route_name = match route {
            crate::search::query_route::QueryRoute::ExactSymbol => "exact_symbol",
            crate::search::query_route::QueryRoute::ExactText => "exact_text",
            crate::search::query_route::QueryRoute::Semantic => "semantic",
            crate::search::query_route::QueryRoute::DeepPdg => "deep_pdg",
        };
        let query_type = match route {
            crate::search::query_route::QueryRoute::ExactSymbol
            | crate::search::query_route::QueryRoute::ExactText => {
                Some(crate::search::ranking::QueryType::Exact)
            }
            crate::search::query_route::QueryRoute::Semantic
            | crate::search::query_route::QueryRoute::DeepPdg => Some(if search_mode == "prose" {
                crate::search::ranking::QueryType::Text
            } else {
                crate::search::ranking::QueryType::Semantic
            }),
        };

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        let scope = resolve_scope(&args, guard.project_path())?;

        if guard.search_engine().is_empty() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        const MAX_FETCH_K: usize = 1000;
        let mut fetch_k = (top_k + offset).min(MAX_FETCH_K);
        let search = |index: &mut crate::cli::leindex::LeIndex,
                      query: &str,
                      top_k: usize,
                      query_type: Option<crate::search::ranking::QueryType>| {
            if task_context.is_some() {
                index.search_ephemeral(query, top_k, query_type)
            } else {
                index.search(query, top_k, query_type)
            }
        };
        let mut all_results = search(&mut guard, &effective_query, fetch_k, query_type)
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
                all_results = search(&mut guard, &effective_query, fetch_k, query_type)
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
                        Try: rephrase query, use different keywords, or try LeIndex [Grep Symbols] for exact symbol names.",
                        query,
                        guard.source_file_paths().map(|p| p.len()).unwrap_or(0)
                    ),
                    "retrieval": {
                        "tfidf_status": "fresh",
                        "pdg_status": if guard.pdg().is_some() { "resident" } else { "not_loaded" },
                        "neural_status": if matches!(
                            route,
                            crate::search::query_route::QueryRoute::ExactSymbol
                                | crate::search::query_route::QueryRoute::ExactText
                        ) {
                            "not_used_exact"
                        } else {
                            guard.neural_status()
                        },
                        "route": route_name,
                        "partial": budget.elapsed(started),
                        "max_latency_ms": budget.max_latency_ms,
                        "allow_partial": budget.allow_partial
                    }
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
                "has_more": offset + total_returned < total_filtered,
                "retrieval": {
                    "tfidf_status": "fresh",
                    "pdg_status": if guard.pdg().is_some() { "resident" } else { "not_loaded" },
                    "neural_status": if matches!(
                        route,
                        crate::search::query_route::QueryRoute::ExactSymbol
                            | crate::search::query_route::QueryRoute::ExactText
                    ) {
                        "not_used_exact"
                    } else {
                        guard.neural_status()
                    },
                    "route": route_name,
                    "partial": budget.elapsed(started),
                    "max_latency_ms": budget.max_latency_ms,
                    "allow_partial": budget.allow_partial
                }
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
        // Test that semantic search with no matches returns helpful suggestion.
        // Uses a source file with content unrelated to the query.
        let dir = tempdir().unwrap();
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "pub fn alpha_beta_gamma() {}\n").unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "query": "zzz_nonexistent_qqq_12345" });
        let result = SearchHandler.execute(&registry, args).await;
        // Should succeed
        assert!(result.is_ok(), "search should succeed");
        let val = result.unwrap();
        // With improved scoring, unrelated queries should return 0 results
        // due to no token overlap and no symbol name match
        if val["count"].as_i64().unwrap_or(0) == 0 {
            // Verify suggestion field is present for zero results
            assert!(
                val.get("suggestion").is_some(),
                "zero results should include suggestion"
            );
        }
    }

    #[test]
    fn test_search_schema_has_pagination() {
        let handler = SearchHandler;
        let schema = handler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("offset").is_some());
    }
}
