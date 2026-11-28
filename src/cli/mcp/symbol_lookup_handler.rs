use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callees, node_type_str,
    read_source_snippet_resolved, resolve_scope, wrap_with_meta,
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

/// Per-call lookup context shared by every symbol in one request.
struct LookupEnv<'a> {
    pdg: &'a crate::graph::pdg::ProgramDependenceGraph,
    project_root: &'a std::path::Path,
    include_source: bool,
    include_callers: bool,
    include_callees: bool,
    depth: usize,
    /// Caller page to serve (brief §6.2: `+N more callers — next: <offset>`).
    caller_offset: usize,
    /// Alternate lookup body: relations (default), construct/match usage
    /// sites (§6.3) or exercising tests (§6.4).
    kind: LookupKind,
    started: Instant,
    budget: WorkBudget,
    index_freshness: &'static str,
    index_stale: bool,
    graph_has_call_edges: bool,
}

/// The `kind` discriminator: what the lookup should return *about* the symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LookupKind {
    /// Callers, callees, impact — the default body.
    Relations,
    /// Struct-literal construction sites (`Foo { .. }`, §6.3).
    Construct,
    /// Enum-variant pattern sites (`match` arms, `if let`, `matches!`, §6.3).
    Match,
    /// Tests exercising the symbol via the call graph, with run commands (§6.4).
    Tests,
}

#[allow(missing_docs)]
impl SymbolLookupHandler {
    pub fn name(&self) -> &str {
        "leindex_symbol_lookup"
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
                    "description": "Project directory (omit to use current project). Indexing is NOT automatic: an unindexed project answers with a 'project not indexed' error until you run the index action."
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
                "kind": {
                    "type": "string",
                    "enum": ["relations", "construct", "match", "tests"],
                    "description": "What to return: relations (default: callers/callees/impact), construct (struct-literal sites), match (enum-pattern sites), tests (exercising tests + run commands)",
                    "default": "relations"
                },
                "caller_offset": {
                    "type": "integer",
                    "description": "Callers to skip before collecting; take it from callers_next_offset of the previous response",
                    "default": 0,
                    "minimum": 0
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
        let caller_offset = extract_usize(&args, "caller_offset", 0)?;
        let kind = extract_lookup_kind(&args)?;
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

        // Graph-only hydration: this tool never queries the search engine,
        // so skip snapshot/embedding-mmap restoration (~1s per cold call).
        guard
            .ensure_pdg_loaded_graph_only()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        if guard.pdg().is_none() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
            ));
        }

        let pdg = guard.pdg().unwrap();
        let project_root = guard.project_path().to_path_buf();

        // Index-freshness context for honest degradation (N-15): a zero-impact
        // result served from a stale index or a degraded graph must say so,
        // not present "0 symbols / 0 files" as fact.
        let storage_root = crate::cli::leindex::resolve_existing_storage_path(&project_root)
            .unwrap_or_else(|| project_root.join(".leindex"));
        let health = crate::cli::index_freshness::load_health(&storage_root);
        // One worktree delta, threaded into the staleness check, so this
        // response does not fork `git status` separately from the meta wrapper.
        let live_status = crate::cli::git::status(&project_root).ok();
        let status_known = live_status.is_some();
        let index_stale = super::helpers::is_index_genuinely_stale(
            &health,
            &project_root,
            live_status.as_ref(),
            status_known,
        );
        let index_freshness = match health.as_ref().map(|h| h.status) {
            Some(crate::cli::leindex::ComponentStatus::Fresh) => "fresh",
            Some(crate::cli::leindex::ComponentStatus::Stale) => "stale",
            Some(crate::cli::leindex::ComponentStatus::Failed) => "failed",
            Some(crate::cli::leindex::ComponentStatus::Partial) => "partial",
            _ => "unknown",
        };
        let graph_has_call_edges = pdg.edge_indices().any(|edge| {
            pdg.get_edge(edge)
                .is_some_and(|e| e.edge_type == crate::graph::pdg::EdgeType::Call)
        });
        let env = LookupEnv {
            pdg,
            project_root: &project_root,
            include_source,
            include_callers,
            include_callees,
            depth,
            caller_offset,
            kind,
            started,
            budget,
            index_freshness,
            index_stale,
            graph_has_call_edges,
        };

        // For batch mode, collect results for each symbol
        if symbols.len() > 1 {
            let char_budget = token_budget * 4;
            let per_symbol_budget = char_budget / symbols.len();
            let mut results: Vec<Value> = Vec::new();

            for symbol in &symbols {
                match self.lookup_single_symbol(&env, symbol, &scope, per_symbol_budget) {
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
        let single = self.lookup_single_symbol(&env, &symbols[0], &scope, char_budget)?;

        let mut single = single;
        add_retrieval_meta(&mut single, budget, started);
        Ok(wrap_with_meta(single, &guard))
    }

    /// Resolve and return full structural context for a single symbol.
    fn lookup_single_symbol(
        &self,
        env: &LookupEnv<'_>,
        symbol: &str,
        scope: &Option<String>,
        char_budget: usize,
    ) -> Result<Value, JsonRpcError> {
        let node_id = resolve_symbol_node(env.pdg, symbol, scope)?;

        let node = env
            .pdg
            .get_node(node_id)
            .ok_or_else(|| JsonRpcError::internal_error("PDG node disappeared after lookup"))?;

        // §6.3 / §6.4 alternate bodies share symbol resolution with the
        // default relations view but replace the payload.
        match env.kind {
            LookupKind::Construct => {
                return Ok(usage_sites_body(env, node, symbol, char_budget));
            }
            LookupKind::Match => return Ok(usage_sites_body(env, node, symbol, char_budget)),
            LookupKind::Tests => return Ok(tests_body(env, node_id, node, char_budget)),
            LookupKind::Relations => {}
        }

        let mut cache = crate::cli::mcp::call_sites::SourceCache::new(env.project_root);
        let relations = collect_relations(
            env.pdg,
            node_id,
            env.depth,
            env.include_callers,
            env.include_callees,
            env.caller_offset,
            &mut cache,
            char_budget,
            env.started,
            env.budget,
        );
        let partial = relations.partial;

        let affected_files: std::collections::HashSet<&str> = relations
            .forward
            .iter()
            .filter_map(|&nid| env.pdg.get_node(nid).map(|n| n.file_path.as_ref()))
            .collect();
        let impact_radius = serde_json::json!({
            // Direction label: the audit flagged 351-vs-9 confusion against
            // impact-analysis; this figure is forward (dependents) reach.
            "direction": "forward (symbols that depend on this one)",
            "affected_symbols": relations.forward.len(),
            "affected_files": affected_files.len()
        });

        let impact_note = impact_degradation_note(
            relations_are_empty(&relations),
            env.index_stale,
            env.graph_has_call_edges,
            partial,
        );
        let mut result = serde_json::json!({
            "symbol": node.name,
            "type": node_type_str(&node.node_type),
            "file": node.file_path,
            "byte_range": node.byte_range,
            "complexity": node.complexity,
            "language": node.language,
            "callers": relations.callers,
            "callers_total": relations.callers_total,
            "callers_next_offset": relations.callers_next_offset,
            "callees": relations.callees,
            "callers_truncated": relations.callers_truncated,
            "callees_truncated": relations.callees_truncated,
            "impact_radius": impact_radius,
            "pdg_status": if partial { "partial" } else { "fresh" },
            "index_freshness": env.index_freshness,
            "retrieval": retrieval_meta_with_partial(env.budget, partial)
        });
        if let Some(note) = impact_note {
            result["impact_note"] = Value::String(note.to_string());
        }

        if env.include_source && !partial {
            attach_source(&mut result, node, env.project_root, char_budget);
        }

        Ok(result)
    }
}

/// Honest-degradation note for empty lookup relations (N-15).
///
/// A zero-impact result is only a fact when the index is fresh AND the graph
/// actually carries call edges. Otherwise "Impact: 0 symbols / 0 files" is a
/// data-availability statement, and saying nothing silently misled callers
/// (the stress-test audit found ground-truth call sites rendered as zero
/// impact with no warning). Precedence: budget truncation > degraded graph >
/// stale index.
fn impact_degradation_note(
    relations_empty: bool,
    index_stale: bool,
    graph_has_call_edges: bool,
    partial: bool,
) -> Option<&'static str> {
    if !relations_empty {
        return None;
    }
    if partial {
        return Some(
            "relations were skipped because the latency budget was exhausted \
             (allow_partial); the zero impact figure is not authoritative — \
             retry with a larger max_latency_ms",
        );
    }
    if !graph_has_call_edges {
        return Some(
            "the loaded PDG has no call edges — the graph is degraded \
             (incomplete or corrupted index); the zero impact figure reflects \
             missing graph data, not isolation; re-index with force_reindex=true",
        );
    }
    if index_stale {
        return Some(
            "the index is stale relative to the worktree; the zero impact \
             figure may reflect missing data — re-index for authoritative impact",
        );
    }
    None
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
                    .find(|&nid| pdg.get_node(nid).is_some_and(in_scope))
            })
    })
    .or_else(|| find_fuzzy_node(pdg, symbol, &in_scope))
    .ok_or_else(|| {
        let total_symbols = pdg.node_count();
        let total_files = pdg.file_count();
        let suggestion = format!(
            "Symbol '{}' not found among {} indexed symbols across {} files. Try: \
            check spelling, use leindex_explore mode=find target=symbols for partial matches, \
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

/// Structural relations collected for one symbol lookup, plus the sticky
/// latency-budget flag observed while collecting them.
struct SymbolRelations {
    callers: Vec<Value>,
    callers_truncated: bool,
    /// All PDG callers, before the budget page: the total the `+N more`
    /// line reports against.
    callers_total: usize,
    /// Offset to pass as `caller_offset` for the next page; `None` when
    /// every caller already fits (brief §6.2 `+N more callers — next:`).
    callers_next_offset: Option<usize>,
    callees: Vec<Value>,
    callees_truncated: bool,
    forward: Vec<crate::graph::pdg::NodeId>,
    /// Once the budget is exhausted every later relation step is skipped and
    /// the result is reported as partial.
    partial: bool,
}

/// How many caller entries fit in the caller share of the response budget,
/// by [`crate::cli::mcp::call_sites::CallerEntry::render_cost`]. The first
/// entry always renders (an empty caller list is a fact worth stating);
/// later entries are dropped whole — a half-truncated line serves nobody.
fn caller_budget(
    char_budget: usize,
    entries: &[crate::cli::mcp::call_sites::CallerEntry],
) -> usize {
    let mut chars = char_budget / 2;
    let mut count = 0usize;
    for entry in entries {
        let cost = entry.render_cost();
        if count > 0 && cost > chars {
            break;
        }
        chars -= cost.min(chars);
        count += 1;
    }
    count
}

/// Collect a symbol's direct callees and direct callers, plus depth-bounded
/// forward impact, each only while the latency budget still allows work. The
/// flag is re-evaluated between steps and ORs forward, so a step that runs
/// long suppresses the steps after it.
///
/// Callers are the §6.2 upgrade over the bare name/file/type list: every
/// entry resolves to its call-site line through [`SourceCache`], tests rank
/// last, the page fills by [`caller_budget`] and overflows into
/// `callers_next_offset` (served later via the `caller_offset` argument).
#[allow(clippy::too_many_arguments)]
fn collect_relations(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    node_id: crate::graph::pdg::NodeId,
    depth: usize,
    include_callers: bool,
    include_callees: bool,
    caller_offset: usize,
    cache: &mut crate::cli::mcp::call_sites::SourceCache,
    char_budget: usize,
    started: Instant,
    budget: WorkBudget,
) -> SymbolRelations {
    let mut partial = budget.elapsed(started);
    let callee_name = pdg
        .get_node(node_id)
        .map(|n| n.name.to_string())
        .unwrap_or_default();

    // Callees (direct)
    let (callees, callees_truncated) = if include_callees && !partial {
        summarize_nodes(pdg, get_direct_callees(pdg, node_id))
    } else {
        (Vec::new(), false)
    };
    partial |= budget.elapsed(started);

    // Callers (direct): call-site lines, tests last, budget page, next offset.
    let (callers, callers_truncated, callers_total, callers_next_offset) =
        if include_callers && !partial {
            let entries = crate::cli::mcp::call_sites::caller_entries(
                pdg,
                node_id,
                true,
                callee_name.as_str(),
                cache,
            );
            let total = entries.len();
            let page = caller_budget(char_budget, &entries);
            let (page, next) = page_callers(&entries, page, caller_offset);
            let callers = page.iter().map(|entry| entry.to_json()).collect();
            (callers, next.is_some(), total, next)
        } else {
            (Vec::new(), false, 0, None)
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
    partial |= budget.elapsed(started);

    SymbolRelations {
        callers,
        callers_truncated,
        callers_total,
        callers_next_offset,
        callees,
        callees_truncated,
        forward,
        partial,
    }
}

/// Offset paging over ranked caller entries (§6.2 `+N more callers —
/// next: <offset>`). Pure so the boundary cases (offset past the end,
/// zero-sized pages) are testable without a PDG. A `skip` at or past the
/// page end yields an empty page with no next offset — the terminal page.
fn page_callers(
    entries: &[crate::cli::mcp::call_sites::CallerEntry],
    page_size: usize,
    skip: usize,
) -> (&[crate::cli::mcp::call_sites::CallerEntry], Option<usize>) {
    let start = skip.min(entries.len());
    let end = (start + page_size).min(entries.len());
    let next = (end < entries.len()).then_some(end);
    (&entries[start..end], next)
}

/// The §6.3 alternate body: every construct/match site of the symbol across
/// the project's first-party sources. Sites are budget-filled and overflow is
/// disclosed with `sites_truncated`.
fn usage_sites_body(
    env: &LookupEnv<'_>,
    node: &crate::graph::pdg::Node,
    symbol: &str,
    char_budget: usize,
) -> Value {
    let kind = match env.kind {
        LookupKind::Construct => crate::cli::mcp::usages::UsageKind::Construct,
        _ => crate::cli::mcp::usages::UsageKind::Match,
    };
    let query = crate::cli::mcp::usages::UsageQuery::from_symbol(kind, symbol);
    let (sites, scanned, more) =
        crate::cli::mcp::usages::sites_in_project(env.project_root, &query);
    let mut chars = char_budget.saturating_sub(160);
    let mut body: Vec<Value> = Vec::with_capacity(sites.len());
    for site in &sites {
        let cost = 32 + site.file.len() + site.site.site_line.len();
        if !body.is_empty() && cost > chars {
            break;
        }
        chars -= cost.min(chars);
        body.push(serde_json::json!({
            "file": site.file,
            "line": site.site.line,
            "site_line": site.site.site_line,
            "form": site.site.form,
            "enclosing": site.site.enclosing,
            "language": site.language,
        }));
    }
    let languages: std::collections::BTreeSet<&str> =
        sites.iter().map(|site| site.language).collect();
    let mut result = serde_json::json!({
        "symbol": node.name,
        "file": node.file_path,
        "kind": match kind {
            crate::cli::mcp::usages::UsageKind::Construct => "construct",
            crate::cli::mcp::usages::UsageKind::Match => "match",
        },
        "sites": body,
        "sites_total": sites.len(),
        "sites_truncated": more || body.len() < sites.len(),
        "languages_covered": languages,
        "files_scanned": scanned,
        "language_coverage": "rust, python, typescript, javascript (tsx/jsx have no wired grammar and are skipped)",
        "index_freshness": env.index_freshness,
        "retrieval": retrieval_meta(env.budget, env.started)
    });
    if env.include_source {
        attach_source(&mut result, node, env.project_root, char_budget);
    }
    result
}

/// The §6.4 alternate body: tests exercising the symbol (direct callers
/// first, then transitive), each with the exact command that runs it.
fn tests_body(
    env: &LookupEnv<'_>,
    node_id: crate::graph::pdg::NodeId,
    node: &crate::graph::pdg::Node,
    char_budget: usize,
) -> Value {
    let mut cache = crate::cli::mcp::call_sites::SourceCache::new(env.project_root);
    let (targets, truncated) = crate::cli::mcp::tests_mode::tests_for(
        env.pdg,
        node_id,
        env.depth,
        &mut cache,
        crate::cli::mcp::tests_mode::MAX_TEST_TARGETS,
    );
    let mut chars = char_budget.saturating_sub(160);
    let mut body: Vec<Value> = Vec::with_capacity(targets.len());
    for target in &targets {
        let cost = target.render_cost();
        if !body.is_empty() && cost > chars {
            break;
        }
        chars -= cost.min(chars);
        body.push(target.to_json());
    }
    let mut result = serde_json::json!({
        "symbol": node.name,
        "file": node.file_path,
        "kind": "tests",
        "tests": body,
        "tests_total": targets.len(),
        "tests_truncated": truncated || body.len() < targets.len(),
        "index_freshness": env.index_freshness,
        "retrieval": retrieval_meta(env.budget, env.started)
    });
    if env.include_source {
        attach_source(&mut result, node, env.project_root, char_budget);
    }
    result
}

/// No relations at all — the case where a zero impact figure must be disclosed
/// as a data-availability statement rather than a fact.
fn relations_are_empty(relations: &SymbolRelations) -> bool {
    relations.callers.is_empty() && relations.callees.is_empty() && relations.forward.is_empty()
}

/// Attach the source snippet for a resolved node, truncated to half the char
/// budget (the other half is reserved for the structural payload).
fn attach_source(
    result: &mut Value,
    node: &crate::graph::pdg::Node,
    project_root: &std::path::Path,
    char_budget: usize,
) {
    if let Some(src) =
        read_source_snippet_resolved(&node.file_path, node.byte_range, Some(project_root))
    {
        let half = char_budget / 2;
        if half == 0 {
            // A zero-share batch (e.g. 20 symbols on a tiny token_budget) would
            // otherwise attach `"source": ""`, which reads as "empty file"
            // rather than "budget too thin" (same disclosure rule as N-15).
            result["source_omitted"] = Value::String(
                "per-symbol share of token_budget is too small for a source snippet; \
                 raise token_budget or look up symbols individually"
                    .to_string(),
            );
            return;
        }
        let truncated: String = src.chars().take(half).collect();
        result["source"] = Value::String(truncated);
    }
}

fn summarize_nodes(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    node_ids: Vec<crate::graph::pdg::NodeId>,
) -> (Vec<Value>, bool) {
    // Neighbors/predecessors yield one entry per edge; a related node with
    // several edge types to the anchor must render once (N-01).
    let mut seen = std::collections::HashSet::new();
    let capped_nodes: Vec<Value> = node_ids
        .into_iter()
        .filter(|node_id| seen.insert(*node_id))
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

/// Parse the `kind` discriminator; unknown values are a named error, not a
/// silent fallback to relations.
fn extract_lookup_kind(args: &Value) -> Result<LookupKind, JsonRpcError> {
    let Some(raw) = args.get("kind") else {
        return Ok(LookupKind::Relations);
    };
    let Value::String(text) = raw else {
        return Err(JsonRpcError::invalid_params(
            "'kind' must be a string (relations | construct | match | tests)".to_string(),
        ));
    };
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "relations" | "callers" | "calls" => Ok(LookupKind::Relations),
        "construct" | "constructs" | "construction" => Ok(LookupKind::Construct),
        "match" | "matches" | "pattern" | "patterns" => Ok(LookupKind::Match),
        "tests" | "test" => Ok(LookupKind::Tests),
        other => Err(JsonRpcError::invalid_params_with_suggestion(
            format!("Unknown kind: '{other}'"),
            "Use kind: relations (default) | construct (struct-literal sites) | \
             match (enum-pattern sites) | tests (exercising tests + run commands)",
        )),
    }
}

fn parse_symbols(args: &Value) -> Result<Vec<String>, JsonRpcError> {
    // Determine symbol list: single "symbol" or batch "symbols"
    let symbols = if let Some(arr) = args.get("symbols").and_then(|v| v.as_array()) {
        // The schema documents maxItems: 20 — enforce it with a clear error
        // instead of silently dropping the tail (a caller batching 21
        // symbols previously got 20 results with no indication anything was
        // truncated).
        if arr.len() > 20 {
            return Err(JsonRpcError::invalid_params(format!(
                "'symbols' accepts at most 20 entries; got {}. Split into multiple calls.",
                arr.len()
            )));
        }
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .filter(|s| !s.trim().is_empty())
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
    fn test_batch_source_budget_remains_bounded_and_discloses_zero_share() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("source.rs");
        let source = "fn example() { println!(\"hello\"); }";
        std::fs::write(&path, source).unwrap();
        let mut node = crate::graph::pdg::Node::new_file_summary(path.to_str().unwrap(), "rust");
        node.byte_range = (0, source.len());
        for budget in [0, 1, 20] {
            let mut result = serde_json::json!({});
            attach_source(&mut result, &node, dir.path(), budget);
            if budget < 2 {
                assert!(result.get("source").is_none());
                assert!(
                    result["source_omitted"]
                        .as_str()
                        .unwrap()
                        .contains("raise token_budget")
                );
            } else {
                assert_eq!(
                    result["source"].as_str().unwrap().chars().count(),
                    budget / 2
                );
            }
        }
    }

    #[test]
    fn test_symbol_lookup_schema_supports_batch() {
        let handler = SymbolLookupHandler;
        let schema = handler.argument_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("symbol").is_some());
        assert!(props.get("symbols").is_some());
    }

    #[test]
    fn test_impact_note_absent_when_relations_present() {
        assert!(impact_degradation_note(false, true, false, false).is_none());
    }

    #[test]
    fn test_impact_note_absent_when_index_fresh_and_graph_sound() {
        // Zero relations with a fresh index and a live call graph are a fact,
        // not degradation.
        assert!(impact_degradation_note(true, false, true, false).is_none());
    }

    #[test]
    fn test_impact_note_flags_degraded_graph_before_staleness() {
        let note = impact_degradation_note(true, true, false, false).unwrap();
        assert!(note.contains("no call edges"), "note: {note}");
    }

    #[test]
    fn test_impact_note_flags_stale_index() {
        let note = impact_degradation_note(true, true, true, false).unwrap();
        assert!(note.contains("stale"), "note: {note}");
    }

    #[test]
    fn test_impact_note_flags_budget_truncation_first() {
        let note = impact_degradation_note(true, true, false, true).unwrap();
        assert!(note.contains("latency budget"), "note: {note}");
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

    // ---- §6.2 caller paging and budget ----

    fn caller_entry(
        name: &str,
        file: &str,
        is_test: bool,
    ) -> crate::cli::mcp::call_sites::CallerEntry {
        crate::cli::mcp::call_sites::CallerEntry {
            name: name.to_string(),
            file: file.to_string(),
            kind: "function".to_string(),
            is_test,
            line: Some(7),
            site_line: Some(format!("{name}(1);")),
        }
    }

    #[test]
    fn test_page_callers_paging_boundaries() {
        let entries: Vec<_> = (0..5)
            .map(|i| caller_entry(&format!("caller_{i}"), "src/lib.rs", false))
            .collect();
        let (page, next) = page_callers(&entries, 2, 0);
        assert_eq!(page.len(), 2);
        assert_eq!(next, Some(2));
        let (page, next) = page_callers(&entries, 2, 2);
        assert_eq!(page[0].name, "caller_2");
        assert_eq!(next, Some(4));
        let (page, next) = page_callers(&entries, 2, 4);
        assert_eq!(page.len(), 1);
        assert_eq!(next, None, "last page is terminal");
        // Skip past the end: empty terminal page, no phantom offset.
        let (page, next) = page_callers(&entries, 2, 9);
        assert!(page.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn test_caller_budget_caps_entries_but_always_keeps_first() {
        let small: Vec<_> = (0..10)
            .map(|i| caller_entry(&format!("c{i}"), "a.rs", false))
            .collect();
        assert_eq!(caller_budget(10_000, &small), 10, "everything fits");
        let big: Vec<_> = (0..4)
            .map(|i| {
                caller_entry(
                    &format!("caller_number_{i}_with_long_name"),
                    "deeply/nested/path/file.rs",
                    false,
                )
            })
            .collect();
        let fits = caller_budget(120, &big);
        assert!((1..4).contains(&fits), "budget must cap entries: {fits}");
        // Even a zero budget renders the first entry: an empty caller list is
        // a fact worth stating, silence is not.
        let one = [caller_entry("only", "a.rs", false)];
        assert_eq!(caller_budget(0, &one), 1);
    }

    // ---- End-to-end on a fixture project ----

    /// A real tempdir crate exercising every §6.2 caller shape (plain call,
    /// call inside a macro, direct test, transitive test) with a true
    /// Cargo.toml for exact `-p` derivation.
    fn fixture_project() -> (tempfile::TempDir, crate::graph::pdg::ProgramDependenceGraph) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        let source = "pub fn leaf_fn(x: u32) -> u32 {\n    x + 1\n}\n\nfn prod_caller() -> u32 {\n    leaf_fn(1)\n}\n\nfn wrapper() -> u32 {\n    prod_caller()\n}\n\nfn macro_caller() -> bool {\n    assert_eq!(leaf_fn(3), 3);\n    true\n}\n\n#[test]\nfn test_direct() {\n    assert_eq!(leaf_fn(2), 2);\n}\n\n#[test]\nfn test_via_wrapper() {\n    assert_eq!(wrapper(), 1);\n}\n";
        let path = dir.path().join("src/lib.rs");
        std::fs::write(&path, source).expect("lib.rs");
        let file = path.to_str().expect("utf8 path").to_string();

        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let add = |pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
                   name: &str|
         -> crate::graph::pdg::NodeId {
            let node = crate::graph::pdg::Node {
                id: format!("{file}::{name}"),
                node_type: crate::graph::pdg::NodeType::Function,
                name: name.to_string(),
                file_path: std::sync::Arc::from(file.as_str()),
                byte_range: (
                    source.find(&format!("fn {name}")).expect("fixture fn"),
                    source.len(),
                ),
                complexity: 1,
                language: "rust".to_string(),
            };
            pdg.add_node(node)
        };
        let leaf = add(&mut pdg, "leaf_fn");
        let prod = add(&mut pdg, "prod_caller");
        let wrap = add(&mut pdg, "wrapper");
        let mac = add(&mut pdg, "macro_caller");
        let direct = add(&mut pdg, "test_direct");
        let via = add(&mut pdg, "test_via_wrapper");
        pdg.add_call_edges(vec![
            (prod, leaf),
            (wrap, prod),
            (mac, leaf),
            (direct, leaf),
            (via, wrap),
        ]);
        (dir, pdg)
    }

    fn env_for<'a>(
        pdg: &'a crate::graph::pdg::ProgramDependenceGraph,
        root: &'a std::path::Path,
        kind: LookupKind,
    ) -> LookupEnv<'a> {
        LookupEnv {
            pdg,
            project_root: root,
            include_source: true,
            include_callers: true,
            include_callees: true,
            depth: 5,
            caller_offset: 0,
            kind,
            started: Instant::now(),
            budget: WorkBudget {
                max_latency_ms: 30_000,
                allow_partial: true,
            },
            index_freshness: "fresh",
            index_stale: false,
            graph_has_call_edges: true,
        }
    }

    #[test]
    fn test_relations_callers_carry_call_sites_ranked_and_paged() {
        let (dir, pdg) = fixture_project();
        let root = dir.path().to_path_buf();
        let env = env_for(&pdg, &root, LookupKind::Relations);
        let result = SymbolLookupHandler
            .lookup_single_symbol(&env, "leaf_fn", &None, 20_000)
            .expect("lookup ok");

        // include_source still attaches the definition.
        assert!(
            result["source"]
                .as_str()
                .expect("source attached")
                .contains("fn leaf_fn"),
            "include_source body: {result}"
        );

        let callers = result["callers"].as_array().expect("callers array");
        assert_eq!(result["callers_total"].as_u64(), Some(3));
        assert_eq!(
            result["callers_next_offset"],
            Value::Null,
            "all callers fit: {callers:?}"
        );
        let names: Vec<&str> = callers
            .iter()
            .map(|c| c["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names.len(), 3, "every PDG caller reported: {names:?}");
        // Tests rank after non-tests; within each class PDG edge order holds.
        let test_flags: Vec<bool> = callers
            .iter()
            .map(|c| c["is_test"].as_bool().expect("is_test"))
            .collect();
        assert!(
            test_flags.windows(2).all(|w| w[0] <= w[1]),
            "non-tests first: {test_flags:?}"
        );
        // Every entry carries its resolved call site — including the call
        // that only exists inside a macro token tree.
        for caller in callers {
            assert!(caller["line"].is_u64(), "line resolved: {caller}");
            assert!(
                caller["call_site"].as_str().is_some(),
                "call site resolved: {caller}"
            );
        }
        let macro_entry = callers
            .iter()
            .find(|c| c["name"] == "macro_caller")
            .unwrap();
        assert!(
            macro_entry["call_site"]
                .as_str()
                .unwrap()
                .contains("leaf_fn(3)"),
            "macro-internal call: {macro_entry}"
        );
        let prod_entry = callers.iter().find(|c| c["name"] == "prod_caller").unwrap();
        assert!(
            prod_entry["call_site"]
                .as_str()
                .unwrap()
                .contains("leaf_fn(1)"),
            "plain call: {prod_entry}"
        );

        // A small budget paginates; walking caller_offset pages reaches every
        // caller exactly once and terminates.
        let mut env = env_for(&pdg, &root, LookupKind::Relations);
        let mut seen: Vec<String> = Vec::new();
        let mut pages = 0usize;
        loop {
            let page = SymbolLookupHandler
                .lookup_single_symbol(&env, "leaf_fn", &None, 400)
                .expect("page ok");
            let callers = page["callers"].as_array().expect("callers array");
            for caller in callers {
                seen.push(caller["name"].as_str().expect("name").to_string());
            }
            pages += 1;
            match page["callers_next_offset"].as_u64() {
                Some(next) => env.caller_offset = next as usize,
                None => break,
            }
            assert!(pages < 8, "paging must terminate: {seen:?}");
        }
        assert!(pages > 1, "small budget must paginate: {seen:?}");
        seen.sort();
        assert_eq!(
            seen,
            vec!["macro_caller", "prod_caller", "test_direct"],
            "paged walk covers every caller exactly once"
        );
    }

    #[test]
    fn test_usage_body_scans_project_and_excludes_decoys() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
        let source = "pub struct Widget {\n    pub id: u32,\n}\n\npub fn make() -> Widget {\n    Widget { id: 1 }\n}\n\nfn decoys() {\n    let _c = 0; // Widget { id: 9 } in a comment\n    let _s = \"Widget { id: 9 }\";\n    let _t: fn() -> Widget = make;\n}\n";
        std::fs::write(dir.path().join("src/lib.rs"), source).expect("lib.rs");

        let pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let node = crate::graph::pdg::Node {
            id: "anchor".to_string(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "make".to_string(),
            file_path: std::sync::Arc::from(dir.path().join("src/lib.rs").to_str().expect("utf8")),
            byte_range: (source.find("fn make").expect("fixture fn"), source.len()),
            complexity: 1,
            language: "rust".to_string(),
        };
        let env = env_for(&pdg, dir.path(), LookupKind::Construct);
        let body = usage_sites_body(&env, &node, "Widget", 8000);

        let sites = body["sites"].as_array().expect("sites array");
        assert_eq!(sites.len(), 1, "only the true literal: {sites:?}");
        assert_eq!(sites[0]["line"].as_u64(), Some(6));
        assert_eq!(sites[0]["form"], "struct literal");
        assert!(
            sites[0]["site_line"]
                .as_str()
                .unwrap()
                .contains("Widget { id: 1 }"),
            "site: {sites:?}"
        );
        assert!(
            body["sites_truncated"].as_bool() == Some(false),
            "nothing withheld: {body}"
        );
        assert!(
            body["languages_covered"]
                .as_array()
                .expect("languages")
                .iter()
                .any(|l| l == "rust"),
            "coverage reported: {body}"
        );
    }

    #[test]
    fn test_tests_body_direct_and_transitive_with_exact_commands() {
        let (dir, pdg) = fixture_project();
        let root = dir.path().to_path_buf();
        let env = env_for(&pdg, &root, LookupKind::Tests);
        let leaf = pdg
            .find_all_by_name("leaf_fn")
            .into_iter()
            .next()
            .expect("fixture symbol");
        let node = pdg.get_node(leaf).expect("fixture node");
        let body = tests_body(&env, leaf, node, 8000);

        let tests = body["tests"].as_array().expect("tests array");
        let names: Vec<&str> = tests
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert!(names.contains(&"test_direct"), "direct test: {names:?}");
        assert!(
            names.contains(&"test_via_wrapper"),
            "transitive test through wrapper: {names:?}"
        );
        // Direct callers first: depths are non-decreasing in the list.
        let depths: Vec<u64> = tests
            .iter()
            .map(|t| t["call_depth"].as_u64().expect("depth"))
            .collect();
        assert!(
            depths.windows(2).all(|w| w[0] <= w[1]),
            "direct before transitive: {depths:?}"
        );
        let direct = tests
            .iter()
            .find(|t| t["name"] == "test_direct")
            .expect("direct entry");
        assert_eq!(direct["evidence"], "#[test]");
        assert_eq!(
            direct["command"], "cargo test -p proj --lib test_direct -- --exact",
            "exact unit command: {direct}"
        );
    }
}
