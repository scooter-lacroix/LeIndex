use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
    read_source_snippet, resolve_scope, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

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

        // Resolve scope and get project handle
        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let scope = {
            let guard = handle.read().await;
            resolve_scope(&args, guard.project_path())?
        };

        // Determine symbol list: single "symbol" or batch "symbols"
        let symbols: Vec<String> = if let Some(arr) = args.get("symbols").and_then(|v| v.as_array())
        {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .filter(|s| !s.trim().is_empty())
                .take(20)
                .collect()
        } else if let Ok(sym) = extract_string(&args, "symbol") {
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
                ) {
                    Ok(val) => results.push(val),
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
                    "results": results
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
        )?;

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
    ) -> Result<Value, JsonRpcError> {
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
        .or_else(|| {
            // 3. Fuzzy match: substring, case-insensitive — prefer non-module nodes
            let sym_lower = symbol.to_lowercase();
            let mut best: Option<crate::graph::pdg::NodeId> = None;
            let mut best_is_module = true;
            for nid in pdg.node_indices() {
                let Some(n) = pdg.get_node(nid) else { continue };
                if !in_scope(n) {
                    continue;
                }
                let matches = n.name.to_lowercase().contains(&sym_lower)
                    || n.id.to_lowercase().contains(&sym_lower);
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
        })
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

        let node = pdg
            .get_node(node_id)
            .ok_or_else(|| JsonRpcError::internal_error("PDG node disappeared after lookup"))?;

        // Callees (direct)
        let (callees, callees_truncated) = if include_callees {
            let all_callees: Vec<Value> = pdg
                .neighbors(node_id)
                .iter()
                .filter_map(|&cid| {
                    pdg.get_node(cid).map(|cn| {
                        serde_json::json!({
                            "name": cn.name,
                            "file": cn.file_path,
                            "type": node_type_str(&cn.node_type)
                        })
                    })
                })
                .collect();
            let total = all_callees.len();
            let truncated = total > 50;
            (
                all_callees.into_iter().take(50).collect::<Vec<_>>(),
                truncated,
            )
        } else {
            (Vec::new(), false)
        };

        // Callers (direct)
        let (callers, callers_truncated) = if include_callers {
            let all_callers: Vec<Value> = get_direct_callers(pdg, node_id)
                .iter()
                .filter_map(|&cid| {
                    pdg.get_node(cid).map(|cn| {
                        serde_json::json!({
                            "name": cn.name,
                            "file": cn.file_path,
                            "type": node_type_str(&cn.node_type)
                        })
                    })
                })
                .collect();
            let total = all_callers.len();
            let truncated = total > 50;
            (
                all_callers.into_iter().take(50).collect::<Vec<_>>(),
                truncated,
            )
        } else {
            (Vec::new(), false)
        };

        // Forward impact (depth-bounded transitive dependents)
        let forward = pdg.forward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig {
                max_depth: Some(depth),
                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
            },
        );
        let affected_files: std::collections::HashSet<&str> = forward
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.file_path.as_ref()))
            .collect();
        let impact_radius = serde_json::json!({
            "affected_symbols": forward.len(),
            "affected_files": affected_files.len()
        });

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
            "impact_radius": impact_radius
        });

        if include_source {
            if let Some(src) = read_source_snippet(&node.file_path, node.byte_range) {
                let truncated: String = src.chars().take(char_budget / 2).collect();
                result["source"] = Value::String(truncated);
            }
        }

        Ok(result)
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
