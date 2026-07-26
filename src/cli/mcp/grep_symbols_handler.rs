use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
    read_source_snippet, resolve_scope, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use super::read_symbol_handler::{catalog_is_fresh, parse_live_file, read_live_bytes};
use super::request_meta::WorkBudget;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::graph::pdg::{NodeId, ProgramDependenceGraph};
use crate::search::query_route::{classify, QueryRoute, RequestedMode};
use crate::search::ranking::Score;
use crate::storage::{CatalogReader, CatalogSymbol};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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

fn build_catalog_symbol_entry(
    symbol: CatalogSymbol,
    opts: &SymbolEntryOpts,
    content: Option<&str>,
) -> Value {
    let source = content.and_then(|content| {
        let (start, end) = symbol.byte_range;
        (start <= end
            && end <= content.len()
            && content.is_char_boundary(start)
            && content.is_char_boundary(end))
        .then(|| &content[start..end])
    });
    let mut entry = serde_json::json!({
        "node_id": symbol.node_id,
        "name": symbol.symbol_name,
        "type": symbol.node_type,
        "file": symbol.file_path,
        "byte_range": symbol.byte_range,
        "complexity": symbol.complexity,
        "caller_count": 0,
        "dependency_count": 0,
        "callers": [],
        "callees": [],
        "language": symbol.language,
    });
    if opts.context_lines > 0 {
        if let Some(source) = &source {
            entry["context"] = Value::String(
                source
                    .lines()
                    .take(opts.context_lines)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
    if opts.include_source {
        if let Some(source) = source {
            let truncated: String = source.chars().take(4000).collect();
            let was_truncated = source.char_indices().nth(4000).is_some();
            entry["source"] = Value::String(truncated);
            if was_truncated {
                entry["source_truncated"] = Value::Bool(true);
            }
        }
    }
    entry
}

async fn catalog_exact_response(
    registry: &Arc<ProjectRegistry>,
    args: &Value,
    pattern: &str,
    project_path: Option<&std::path::Path>,
    type_filter: &str,
    token_budget: usize,
    max_results: usize,
    offset: usize,
    context_lines: usize,
    include_source: bool,
) -> Result<Option<Value>, JsonRpcError> {
    let Some(project_path) = project_path else {
        return Ok(None);
    };
    let live = LiveProject::resolve(&project_path.to_string_lossy()).map_err(|error| {
        JsonRpcError::invalid_params(format!(
            "Cannot resolve project_path '{}': {}",
            project_path.display(),
            error
        ))
    })?;
    let db_path = live.active_storage().join("leindex.db");
    let catalog = if db_path.is_file() {
        CatalogReader::open(&db_path, live.root())
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    // A complete catalog built from the current Git tree already covers every
    // unchanged tracked file. Limit a miss fallback to live edits; when the
    // generation tree cannot be proven current, retain the full Git grep path
    // so committed changes cannot be hidden by an old snapshot.
    let catalog_tree_current = catalog.as_ref().is_some_and(|_| {
        crate::cli::index_freshness::load_health(&live.active_storage()).is_some_and(|health| {
            health.phase == crate::cli::leindex::IndexPhase::Complete
                && health.status == crate::cli::leindex::ComponentStatus::Fresh
                && health.tree_oid.is_some()
                && health.tree_oid == crate::cli::git::tree_oid(live.root()).ok().flatten()
        })
    });
    let scope = resolve_scope(args, live.root())?;
    let pattern_lower = pattern.to_lowercase();
    let symbols = match &catalog {
        Some(catalog) if is_code_pattern(pattern) => {
            // Identifier-shaped exact requests should use the indexed equality
            // path. A leading-wildcard LIKE scan over every catalog row is
            // the scale cliff on large projects; the live Git candidate path
            // below handles substring misses without hydrating the catalog.
            catalog.find_symbol(pattern, None).await.unwrap_or_default()
        }
        Some(catalog) => catalog
            .find_symbols_matching(pattern)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let mut fresh = Vec::new();
    let mut stale_paths = HashSet::new();
    let mut symbol_index_miss = false;
    for mut symbol in symbols {
        symbol.file_path = live
            .file(&symbol.file_path.to_string_lossy())
            .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
        let in_scope = scope.as_ref().map_or(true, |scope| {
            symbol.file_path.starts_with(scope)
                || symbol.file_path
                    == std::path::Path::new(scope.trim_end_matches(std::path::MAIN_SEPARATOR))
        });
        let type_matches = type_filter == "all" || symbol.node_type == type_filter;
        let name_matches = symbol.symbol_name.to_lowercase().contains(&pattern_lower)
            || symbol
                .qualified_name
                .to_lowercase()
                .contains(&pattern_lower);
        if !(in_scope && type_matches && name_matches) {
            continue;
        }
        let bytes = read_live_bytes(symbol.file_path.clone()).await?;
        let is_fresh = match &catalog {
            Some(catalog) => catalog_is_fresh(catalog, &symbol.file_path, &bytes).await,
            None => false,
        };
        if is_fresh {
            fresh.push((symbol, bytes));
        } else {
            stale_paths.insert(symbol.file_path);
        }
    }

    // A stale catalog's paths are safe candidates; an empty catalog falls
    // back to the Git-authoritative live inventory, never registry hydration.
    let paths: Vec<PathBuf> = if stale_paths.is_empty() {
        if fresh.is_empty() {
            symbol_index_miss = true;
            tokio::task::spawn_blocking({
                let root = live.root().to_path_buf();
                let pattern = pattern.to_owned();
                move || match if catalog_tree_current {
                    crate::cli::git::changed_source_candidates(&root)
                } else {
                    crate::cli::git::source_candidates(&root, &pattern)
                } {
                    Ok(paths) => Ok(paths),
                    Err(crate::cli::git::GitInventoryError::NotRepository) => {
                        crate::cli::index_builder::scan_project_files(&root)
                            .map(|scan| scan.source_paths)
                    }
                    Err(error) => Err(anyhow::anyhow!(error.to_string())),
                }
            })
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("live scan task failed: {}", e)))?
            .map_err(|e| JsonRpcError::invalid_params(format!("Cannot scan live project: {}", e)))?
        } else {
            Vec::new()
        }
    } else {
        symbol_index_miss = true;
        stale_paths.into_iter().collect()
    };
    if !paths.is_empty() {
        for path in paths {
            // Avoid invoking Tree-sitter for every source file when a catalog
            // is missing. A cheap byte-level prefilter keeps the live fallback
            // bounded by files that can actually contain the requested anchor.
            let bytes = read_live_bytes(path.clone()).await?;
            if !live_text_might_match(&bytes, pattern) {
                continue;
            }
            let parsed = match parse_live_file(path).await {
                Ok(parsed) => parsed,
                Err(_) => continue,
            };
            let content = match std::str::from_utf8(&parsed.bytes) {
                Ok(content) => content.to_owned(),
                Err(_) => continue,
            };
            for symbol in parsed.symbols {
                let in_scope = scope
                    .as_ref()
                    .map_or(true, |scope| symbol.file_path.starts_with(scope));
                let type_matches = type_filter == "all" || symbol.node_type == type_filter;
                let name_matches = symbol.symbol_name.to_lowercase().contains(&pattern_lower)
                    || symbol
                        .qualified_name
                        .to_lowercase()
                        .contains(&pattern_lower);
                if in_scope && type_matches && name_matches {
                    fresh.push((symbol, content.as_bytes().to_vec()));
                }
            }
        }
    }

    let total_matches = fresh.len();
    let char_budget = token_budget * 4;
    let mut results = Vec::new();
    let mut used_chars = 0;
    for (symbol, bytes) in fresh.into_iter().skip(offset).take(max_results) {
        let content = std::str::from_utf8(&bytes).ok();
        let entry = build_catalog_symbol_entry(
            symbol,
            &SymbolEntryOpts {
                context_lines,
                include_source,
                score: None,
            },
            content,
        );
        let entry_chars = entry.to_string().len();
        if used_chars + entry_chars > char_budget {
            break;
        }
        used_chars += entry_chars;
        results.push(entry);
    }
    let shown = results.len();
    let mut pdg_status = "not_loaded";
    if let Some(handle) = registry.try_get_loaded(live.root()).await {
        let guard = handle.read().await;
        if let Some(pdg) = guard.pdg() {
            pdg_status = "fresh";
            for result in &mut results {
                let Some(node_id) = result.get("node_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(node_index) = pdg.find_by_id(node_id) else {
                    continue;
                };
                let callers = get_direct_callers(pdg, node_index);
                let callees = pdg.neighbors(node_index);
                result["caller_count"] = Value::from(callers.len());
                result["dependency_count"] = Value::from(callees.len());
                result["callers"] = Value::Array(
                    callers
                        .iter()
                        .take(50)
                        .filter_map(|id| {
                            pdg.get_node(*id)
                                .map(|node| Value::String(node.name.clone()))
                        })
                        .collect(),
                );
                result["callees"] = Value::Array(
                    callees
                        .iter()
                        .take(50)
                        .filter_map(|id| {
                            pdg.get_node(*id)
                                .map(|node| Value::String(node.name.clone()))
                        })
                        .collect(),
                );
            }
        }
    }
    Ok(Some(serde_json::json!({
        "results": results,
        "total_matches": total_matches,
        "shown": shown,
        "offset": offset,
        "mode": "exact",
        "truncated": total_matches.saturating_sub(offset).min(max_results) > shown,
        "pdg_status": pdg_status,
        "symbol_index_miss": symbol_index_miss,
        "retrieval": {
            "tfidf_status": "not_used_exact",
            "neural_status": "not_used_exact"
        },
    })))
}

fn is_code_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | ':' | '.' | '$' | '-'))
}

fn live_text_might_match(bytes: &[u8], pattern: &str) -> bool {
    let haystack = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let pattern = pattern.trim().to_ascii_lowercase();
    if pattern.is_empty() || haystack.contains(&pattern) {
        return true;
    }
    pattern
        .rsplit_once([':', '.'])
        .map(|(_, tail)| !tail.is_empty() && haystack.contains(tail))
        .unwrap_or(false)
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
                },
                "max_latency_ms": {
                    "type": "integer",
                    "description": "Optional enrichment budget (default: 250)",
                    "default": 250,
                    "minimum": 0,
                    "maximum": 60000
                },
                "allow_partial": {
                    "type": "boolean",
                    "default": true
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
        let budget = WorkBudget {
            max_latency_ms: extract_usize(&args, "max_latency_ms", 250)?.min(60000) as u64,
            allow_partial: extract_bool(&args, "allow_partial", true),
        };
        let started = Instant::now();

        let requested_mode = match mode.as_str() {
            "auto" => RequestedMode::Auto,
            "semantic" => RequestedMode::Semantic,
            "deep" => RequestedMode::Deep,
            _ => RequestedMode::Exact,
        };
        let route = classify(&pattern, requested_mode);
        if route == QueryRoute::DeepPdg {
            return Err(JsonRpcError::invalid_params(
                "deep mode is available through deep-analyze or context, not grep-symbols",
            ));
        }

        if matches!(route, QueryRoute::ExactSymbol | QueryRoute::ExactText) {
            let default_project_path = if project_path.is_none() {
                registry.default_project_path().await.ok()
            } else {
                None
            };
            let catalog_project_path = project_path
                .map(std::path::Path::new)
                .or(default_project_path.as_deref());
            if let Some(response) = catalog_exact_response(
                registry,
                &args,
                &pattern,
                catalog_project_path,
                &type_filter,
                token_budget,
                max_results,
                offset,
                context_lines,
                include_source,
            )
            .await?
            {
                return Ok(annotate_retrieval(
                    response,
                    "exact",
                    "not_used_exact",
                    budget,
                    started,
                ));
            }
        }

        let handle = registry.get_or_create(project_path).await?;
        let mut index = handle.write().await;
        let scope = resolve_scope(&args, index.project_path())?;

        const MAX_CANDIDATE_LIMIT: usize = 1000;

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

        if route == QueryRoute::Semantic {
            let effective_fetch = (max_results + offset).min(MAX_CANDIDATE_LIMIT);
            let mut candidate_limit = effective_fetch
                .saturating_mul(5)
                .clamp(50, MAX_CANDIDATE_LIMIT);
            let mut candidate_results = index
                .search(&pattern, candidate_limit, None)
                .map_err(|e| JsonRpcError::search_failed(format!("Search error: {}", e)))?;
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
            return Ok(annotate_retrieval(
                response,
                "semantic",
                index.neural_status(),
                budget,
                started,
            ));
        }

        let pdg = index.pdg().unwrap();

        let mut candidates = pdg
            .node_indices()
            .filter_map(|nid| {
                let node = pdg.get_node(nid)?;
                if (matches!(node.node_type, crate::graph::pdg::NodeType::External)
                    && type_filter != "external")
                    || (type_filter != "all"
                        && node_type_str(&node.node_type) != type_filter.as_str())
                    || scope.as_ref().is_some_and(|scope| {
                        !node.file_path.starts_with(scope.as_str())
                            && node.file_path.as_ref()
                                != scope.trim_end_matches(std::path::MAIN_SEPARATOR)
                    })
                {
                    return None;
                }
                let rank = if node.id == pattern {
                    0
                } else if node.name == pattern {
                    1
                } else if node.id.eq_ignore_ascii_case(&pattern) {
                    2
                } else if node.name.to_lowercase().contains(&pattern_lower)
                    || node.id.to_lowercase().contains(&pattern_lower)
                {
                    3
                } else {
                    return None;
                };
                Some((rank, node.id.clone(), nid))
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));

        for (_, _, nid) in candidates {
            if all_matches.len() >= fetch_limit {
                break;
            }
            let Some(node) = pdg.get_node(nid) else {
                continue;
            };
            let location_key = (node.file_path.clone(), node.byte_range);
            let is_duplicate_location =
                node.byte_range != (0, 0) && seen_locations.contains(&location_key);
            if seen_ids.contains(&node.id) || is_duplicate_location {
                continue;
            }
            seen_ids.insert(node.id.clone());
            if node.byte_range != (0, 0) {
                seen_locations.insert(location_key);
            }
            all_matches.push(build_symbol_entry(
                pdg,
                nid,
                &SymbolEntryOpts {
                    context_lines,
                    include_source,
                    score: None,
                },
            ));
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
        Ok(annotate_retrieval(
            response,
            "exact",
            "not_used_exact",
            budget,
            started,
        ))
    }
}

fn annotate_retrieval(
    mut response: Value,
    route: &str,
    neural_status: &'static str,
    budget: WorkBudget,
    started: Instant,
) -> Value {
    let partial = budget.elapsed(started);
    let pdg_status = response
        .get("pdg_status")
        .and_then(Value::as_str)
        .unwrap_or(if partial { "partial" } else { "resident" });
    response["pdg_status"] = Value::String(if partial {
        "partial".to_string()
    } else {
        pdg_status.to_string()
    });
    response["retrieval"] = serde_json::json!({
        "tfidf_status": if route == "semantic" { "fresh" } else { "not_used_exact" },
        "pdg_status": response["pdg_status"],
        "neural_status": neural_status,
        "partial": partial,
        "max_latency_ms": budget.max_latency_ms,
        "allow_partial": budget.allow_partial
    });
    response
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
