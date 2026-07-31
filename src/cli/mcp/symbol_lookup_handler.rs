use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
    read_source_snippet, resolve_scope, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use super::request_meta::WorkBudget;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

/// Handler for LeIndex [symbol_lookup — full call graph for any symbol.
#[derive(Clone)]
pub struct SymbolLookupHandler;

#[allow(missing_docs)]
impl SymbolLookupHandler {
    pub fn name(&self) -> &str {
        "leindex.symbol-lookup"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Symbol Lookup]"
    }

    pub fn description(&self) -> &str {
        "Symbol relationship lookup: callers, callees, data dependencies, and impact radius. \
Use for understanding how a symbol connects to the rest of the codebase. \
For the exact source implementation use LeIndex [Read Symbol]."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to look up (single symbol)"
                },
                "symbols": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Batch mode: look up multiple symbols in one call (max 20)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "token_budget": {
                    "type": "integer",
                    "description": "Max tokens for response (default: 1500)",
                    "default": 1500
                },
                "scope": {
                    "type": "string",
                    "description": "Optional path to limit lookup (absolute or relative to project root)"
                },
                "include_source": {
                    "type": "boolean",
                    "description": "Include source code of definition (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "include_callers": {
                    "type": "boolean",
                    "description": "Include callers (default: true). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": true
                },
                "include_callees": {
                    "type": "boolean",
                    "description": "Include callees (default: true). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": true
                },
                "depth": {
                    "type": "integer",
                    "description": "Call graph traversal depth (default: 2, max: 5)",
                    "default": 2,
                    "minimum": 1,
                    "maximum": 5
                },
                "max_latency_ms": {
                    "type": "integer",
                    "description": "Optional caller/callee enrichment budget (default: 250)",
                    "default": 250,
                    "minimum": 0,
                    "maximum": 60000
                },
                "allow_partial": {
                    "type": "boolean",
                    "default": true
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
        let is_batch = args
            .get("symbols")
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.len() > 1);
        let include_source = extract_bool(&args, "include_source", !is_batch);
        let include_callers = extract_bool(&args, "include_callers", true);
        let include_callees = extract_bool(&args, "include_callees", true);
        let depth = extract_usize(&args, "depth", 2)?.min(5);
        let token_budget = extract_usize(&args, "token_budget", 1500)?;
        let budget = WorkBudget {
            max_latency_ms: extract_usize(&args, "max_latency_ms", 250)?.min(60000) as u64,
            allow_partial: extract_bool(&args, "allow_partial", true),
        };
        let started = Instant::now();

        // Resolve scope and get project handle
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let scope = {
            let guard = handle.read().await;
            resolve_scope(&args, guard.project_path())?
        };

        let symbols = parse_symbols(&args)?;

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

        // For batch mode, collect results for each symbol
        if symbols.len() > 1 {
            let char_budget = token_budget * 4;
            let per_symbol_budget = char_budget / symbols.len();
            let mut results: Vec<Value> = Vec::new();

            for symbol in &symbols {
                match self.lookup_single_symbol(
                    pdg,
                    symbol,
                    &scope,
                    include_source,
                    include_callers,
                    include_callees,
                    depth,
                    per_symbol_budget,
                    started,
                    budget,
                ) {
                    Ok(mut val) => {
                        add_retrieval_meta(&mut val, budget, started);
                        results.push(val);
                    }
                    Err(e) => results.push(serde_json::json!({
                        "symbol": symbol,
                        "error": format!("{}", e)
                    })),
                }
            }

            return Ok(wrap_with_meta(
                serde_json::json!({
                    "batch": true,
                    "count": results.len(),
                    "results": results,
                    "retrieval": retrieval_meta(budget, started)
                }),
                &guard,
            ));
        }

        // Single symbol mode
        let char_budget = token_budget * 4;
        let single = self.lookup_single_symbol(
            pdg,
            &symbols[0],
            &scope,
            include_source,
            include_callers,
            include_callees,
            depth,
            char_budget,
            started,
            budget,
        )?;

        let mut single = single;
        add_retrieval_meta(&mut single, budget, started);
        Ok(wrap_with_meta(single, &guard))
    }

    /// Resolve and return full structural context for a single symbol.
    #[allow(clippy::too_many_arguments)]
    fn lookup_single_symbol(
        &self,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        symbol: &str,
        scope: &Option<String>,
        include_source: bool,
        include_callers: bool,
        include_callees: bool,
        depth: usize,
        char_budget: usize,
        started: Instant,
        budget: WorkBudget,
    ) -> Result<Value, JsonRpcError> {
        let node_id = resolve_symbol_node(pdg, symbol, scope)?;

        let node = pdg
            .get_node(node_id)
            .ok_or_else(|| JsonRpcError::internal_error("PDG node disappeared after lookup"))?;
        let mut partial = budget.elapsed(started);

        // Callees (direct)
        let (callees, callees_truncated) = if include_callees && !partial {
            summarize_nodes(pdg, pdg.neighbors(node_id))
        } else {
            (Vec::new(), false)
        };
        partial |= budget.elapsed(started);

        // Callers (direct)
        let (callers, callers_truncated) = if include_callers && !partial {
            summarize_nodes(pdg, get_direct_callers(pdg, node_id))
        } else {
            (Vec::new(), false)
        };
        partial |= budget.elapsed(started);

        // Forward impact (depth-bounded transitive dependents)
        let forward = if partial {
            Vec::new()
        } else {
            pdg.forward_impact(
                node_id,
                &crate::graph::pdg::TraversalConfig {
                    max_depth: Some(depth),
                    ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
                },
            )
        };
        let affected_files: std::collections::HashSet<&str> = forward
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.file_path.as_ref()))
            .collect();
        let impact_radius = serde_json::json!({
            "affected_symbols": forward.len(),
            "affected_files": affected_files.len()
        });

        partial |= budget.elapsed(started);
        let mut result = serde_json::json!({
            "symbol": node.name,
            "type": node_type_str(&node.node_type),
            "file": node.file_path,
            "byte_range": node.byte_range,
            "complexity": node.complexity,
            "language": node.language,
            "callers": callers,
            "callees": callees,
            "callers_truncated": callers_truncated,
            "callees_truncated": callees_truncated,
            "impact_radius": impact_radius,
            "pdg_status": if partial { "partial" } else { "fresh" },
            "retrieval": retrieval_meta_with_partial(budget, partial)
        });

        if include_source && !partial {
            if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                let truncated: String = src.chars().take(char_budget / 2).collect();
                result["source"] = Value::String(truncated);
            }
        }

        Ok(result)
    }
}

fn resolve_symbol_node(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    symbol: &str,
    scope: &Option<String>,
) -> Result<crate::graph::pdg::NodeId, JsonRpcError> {
    let in_scope = |node: &crate::graph::pdg::Node| match scope {
        Some(s) => node.file_path.starts_with(s),
        None => true,
    };

    // 1. Exact symbol lookup (by node.id in symbol_index)
    let node_id = if let Some(nid) = pdg.find_by_symbol(symbol) {
        pdg.get_node(nid).filter(|n| in_scope(n)).map(|_| nid)
    } else {
        None
    }
    // 2. Exact name lookup (by node.name in name_index) — prefer non-module nodes
    .or_else(|| {
        let candidates = pdg.find_all_by_name(symbol);
        // Prefer class/function/method over module nodes
        candidates
            .iter()
            .copied()
            .find(|&nid| {
                pdg.get_node(nid)
                    .map(|n| n.node_type != crate::graph::pdg::NodeType::Module && in_scope(n))
                    .unwrap_or(false)
            })
            .or_else(|| {
                candidates
                    .iter()
                    .copied()
                    .find(|&nid| pdg.get_node(nid).is_some_and(&in_scope))
            })
    })
    .or_else(|| find_fuzzy_node(pdg, symbol, &in_scope))
    .ok_or_else(|| {
        let total_symbols = pdg.node_count();
        let total_files = pdg.file_count();
        let suggestion = format!(
            "Symbol '{}' not found among {} indexed symbols across {} files. Try: \
            check spelling, use LeIndex [Grep Symbols] for partial matches, \
            or LeIndex [Text Search] for raw content search.",
            symbol, total_symbols, total_files
        );
        JsonRpcError::invalid_params_with_suggestion(
            format!("Symbol '{}' not found in project index", symbol),
            &suggestion,
        )
    })?;

    Ok(node_id)
}

fn find_fuzzy_node(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    symbol: &str,
    in_scope: &impl Fn(&crate::graph::pdg::Node) -> bool,
) -> Option<crate::graph::pdg::NodeId> {
    // 3. Fuzzy match: substring, case-insensitive — prefer non-module nodes
    let sym_lower = symbol.to_lowercase();
    let mut best: Option<crate::graph::pdg::NodeId> = None;
    let mut best_is_module = true;
    for nid in pdg.node_indices() {
        let Some(n) = pdg.get_node(nid) else {
            continue;
        };
        if !in_scope(n) {
            continue;
        }
        let matches =
            n.name.to_lowercase().contains(&sym_lower) || n.id.to_lowercase().contains(&sym_lower);
        if !matches {
            continue;
        }
        let is_module = n.node_type == crate::graph::pdg::NodeType::Module;
        // Always prefer non-module; only accept module if it's the first match
        if best.is_none() || (best_is_module && !is_module) {
            best = Some(nid);
            best_is_module = is_module;
            if !is_module {
                break;
            } // non-module is best, stop early
        }
    }
    best
}

fn summarize_nodes(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    node_ids: Vec<crate::graph::pdg::NodeId>,
) -> (Vec<Value>, bool) {
    let capped_nodes: Vec<Value> = node_ids
        .into_iter()
        .filter_map(|node_id| {
            pdg.get_node(node_id).map(|node| {
                serde_json::json!({
                    "name": node.name,
                    "file": node.file_path,
                    "type": node_type_str(&node.node_type)
                })
            })
        })
        .take(51)
        .collect();
    let truncated = capped_nodes.len() > 50;
    (capped_nodes.into_iter().take(50).collect(), truncated)
}

fn parse_symbols(args: &Value) -> Result<Vec<String>, JsonRpcError> {
    // Determine symbol list: single "symbol" or batch "symbols"
    let symbols = if let Some(arr) = args.get("symbols").and_then(|v| v.as_array()) {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .filter(|s| !s.trim().is_empty())
            .take(20)
            .collect()
    } else if let Ok(sym) = extract_string(args, "symbol") {
        if sym.trim().is_empty() {
            return Err(JsonRpcError::invalid_params(
                "'symbol' must be a non-empty string".to_string(),
            ));
        }
        vec![sym]
    } else {
        return Err(JsonRpcError::invalid_params(
            "Provide either 'symbol' (string) or 'symbols' (array of strings)".to_string(),
        ));
    };

    // Validate symbols is non-empty (after filtering blanks)
    if symbols.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "'symbols' array must contain at least one non-blank string".to_string(),
        ));
    }

    Ok(symbols)
}

fn retrieval_meta(budget: WorkBudget, started: Instant) -> Value {
    retrieval_meta_with_partial(budget, budget.elapsed(started))
}

fn retrieval_meta_with_partial(budget: WorkBudget, partial: bool) -> Value {
    serde_json::json!({
        "tfidf_status": "not_used_exact",
        "pdg_status": if partial { "partial" } else { "fresh" },
        "neural_status": "not_used_exact",
        "partial": partial,
        "max_latency_ms": budget.max_latency_ms,
        "allow_partial": budget.allow_partial
    })
}

fn add_retrieval_meta(value: &mut Value, budget: WorkBudget, started: Instant) {
    if let Some(object) = value.as_object_mut() {
        object.insert("retrieval".to_string(), retrieval_meta(budget, started));
        object.insert(
            "pdg_status".to_string(),
            Value::String(if budget.elapsed(started) {
                "partial".to_string()
            } else {
                "fresh".to_string()
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[test]
    fn test_symbol_lookup_schema_supports_batch() {
        let handler = SymbolLookupHandler;
        let schema = handler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("symbol").is_some());
        assert!(props.get("symbols").is_some());
    }

    #[tokio::test]
    async fn test_symbol_lookup_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbol": "my_func" });
        let result = SymbolLookupHandler.execute(&registry, args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_blank_single_symbol_rejected() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbol": "" });
        let result = SymbolLookupHandler.execute(&registry, args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("non-empty"),
            "Expected 'non-empty' in error message, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn test_whitespace_only_single_symbol_rejected() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbol": "   " });
        let result = SymbolLookupHandler.execute(&registry, args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("non-empty"),
            "Expected 'non-empty' in error message, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn test_all_blank_batch_symbols_rejected() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbols": ["", ""] });
        let result = SymbolLookupHandler.execute(&registry, args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("non-blank"),
            "Expected 'non-blank' in error message, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn test_batch_with_mixed_blank_and_valid_symbols() {
        // Blank strings should be filtered out; valid symbols should proceed
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbols": ["", "my_func", "  "] });
        let result = SymbolLookupHandler.execute(&registry, args).await;
        // Should not return invalid_params — the blank strings are filtered out,
        // leaving ["my_func"] which is a valid single-symbol lookup (just not indexed)
        assert!(result.is_err());
        // The error should NOT be about blank symbols — it should be about indexing
        let err = result.unwrap_err();
        assert!(
            !err.message.contains("non-blank"),
            "Should not reject for blank symbols when valid ones exist, got: {}",
            err.message
        );
    }
}
