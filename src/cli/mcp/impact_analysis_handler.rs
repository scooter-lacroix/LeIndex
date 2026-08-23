use super::helpers::{extract_string, extract_usize, get_direct_callers, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex [impact_analysis — transitive dependency impact.
#[derive(Clone)]
pub struct ImpactAnalysisHandler;

#[allow(missing_docs)]
impl ImpactAnalysisHandler {
    pub fn name(&self) -> &str {
        "leindex_impact_analysis"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Impact Analysis]"
    }

    pub fn description(&self) -> &str {
        "Analyze the transitive impact of changing a symbol: shows all symbols and files \
affected at each dependency depth level, with a risk assessment. Use before refactoring \
to understand the blast radius of your change. No equivalent in standard tools."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol to analyze impact for"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "change_type": {
                    "type": "string",
                    "enum": ["modify", "remove", "rename", "change_signature"],
                    "description": "Type of change to analyze (default: modify)",
                    "default": "modify"
                },
                "depth": {
                    "type": "integer",
                    "description": "Traversal depth (default: 3, max: 5)",
                    "default": 3,
                    "minimum": 1,
                    "maximum": 5
                }
            },
            "required": ["symbol"]
        })
    }

    /// Same-community vs cross-boundary split of the affected set, from the
    /// in-memory communities map (populated by index-time detection). Zero
    /// extra computation: membership lookups only.
    fn community_breakdown(
        &self,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        node_id: crate::graph::pdg::NodeId,
        affected: &[crate::graph::pdg::NodeId],
    ) -> Value {
        #[cfg(feature = "community")]
        {
            use std::collections::HashMap;
            let Some(&origin) = pdg.communities.get(&node_id) else {
                return Value::Null;
            };
            let mut same = 0usize;
            let mut crossing = 0usize;
            let mut boundaries: HashMap<(u32, u32), usize> = HashMap::new();
            for &affected_id in affected {
                match pdg.communities.get(&affected_id) {
                    Some(&community) if community == origin => same += 1,
                    Some(&community) => {
                        crossing += 1;
                        *boundaries.entry((origin, community)).or_default() += 1;
                    }
                    None => {}
                }
            }
            let boundaries: Vec<Value> = boundaries
                .into_iter()
                .take(10)
                .map(|((from, to), symbols)| {
                    serde_json::json!({ "from": from, "to": to, "symbols": symbols })
                })
                .collect();
            serde_json::json!({
                "same_community": same,
                "crossing": crossing,
                "boundaries": boundaries,
            })
        }
        #[cfg(not(feature = "community"))]
        {
            let _ = (pdg, node_id, affected);
            Value::Null
        }
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let symbol = extract_string(&args, "symbol")?;
        let change_type = args
            .get("change_type")
            .and_then(|v| v.as_str())
            .unwrap_or("modify")
            .to_owned();
        let depth = extract_usize(&args, "depth", 3)?.min(5);

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        // Best-effort PDG load. Transitive impact is inherently PDG-based, so
        // when the graph is unavailable the tool reports a structured degraded
        // result instead of a hard error — the caller learns the index state
        // and what to do about it.
        let pdg_available = match guard.ensure_pdg_loaded_graph_only() {
            Ok(()) => guard.pdg().is_some(),
            Err(error) => {
                tracing::warn!(
                    project = %guard.project_path().display(),
                    "PDG unavailable for impact analysis: {error}"
                );
                false
            }
        };
        if !pdg_available {
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "symbol": symbol,
                    "change_type": change_type,
                    "pdg_status": "not_loaded",
                    "direct_callers": [],
                    "transitive_affected_symbols": [],
                    "transitive_affected_files": 0,
                    "transitive_callers": 0,
                    "risk_level": "unknown",
                    "warning": "Impact analysis requires the program dependence graph, which is currently unavailable. Reindex (LeIndex [Index] with force_reindex=true) to restore transitive impact analysis.",
                    "summary": format!("Cannot analyze impact of '{}': the program dependence graph is unavailable.", symbol)
                }),
                &guard,
            ));
        }

        let pdg = guard.pdg().unwrap();

        let node_id = if let Some(nid) = pdg.find_by_symbol(&symbol) {
            nid
        } else {
            let sym_lower = symbol.to_lowercase();
            pdg.node_indices()
                .find(|&nid| {
                    pdg.get_node(nid)
                        .map(|n| n.name.to_lowercase() == sym_lower)
                        .unwrap_or(false)
                })
                .ok_or_else(|| {
                    JsonRpcError::invalid_params(format!(
                        "Symbol '{}' not found in project index",
                        symbol
                    ))
                })?
        };

        let node = pdg.get_node(node_id).unwrap();

        let direct_callers: Vec<String> = get_direct_callers(pdg, node_id)
            .iter()
            .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
            .collect();

        // Impact semantics: changing a symbol breaks its DEPENDENTS (the
        // callers upstream), so the affected set is the backward traversal.
        // The previous code used the forward traversal (callees), which
        // under-reports whenever callee resolution is incomplete (e.g.
        // cross-struct method calls) — producing "affects 0 symbols in 0
        // files" while listing 10 direct callers in the same payload.
        let affected = pdg.backward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig {
                max_depth: Some(depth),
                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
            },
        );
        let affected_symbols: Vec<String> = affected
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.name.clone()))
            .take(50)
            .collect();
        let affected_files: std::collections::HashSet<&str> = affected
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.file_path.as_ref()))
            .collect();

        let risk = match change_type.as_str() {
            "remove" | "change_signature" => {
                if affected.len() > 5 || affected_files.len() > 3 {
                    "high"
                } else if !affected.is_empty() {
                    "medium"
                } else {
                    "low"
                }
            }
            _ => {
                if affected_files.len() > 3 {
                    "high"
                } else if affected_files.len() > 1 {
                    "medium"
                } else {
                    "low"
                }
            }
        };

        let community_breakdown = self.community_breakdown(pdg, node_id, &affected);

        Ok(wrap_with_meta(
            serde_json::json!({
                "symbol": node.name,
                "file": node.file_path,
                "change_type": change_type,
                // Direction label: the audit flagged the 351-vs-9 confusion
                // against symbol-lookup; this tool measures backward
                // (dependents) reach at the requested depth.
                "direction": "backward (symbols that depend on this one)",
                "direct_callers": direct_callers,
                "transitive_affected_symbols": affected_symbols,
                "transitive_affected_files": affected_files.len(),
                "transitive_callers": affected.len(),
                "community_breakdown": community_breakdown,
                "risk_level": risk,
                "summary": format!(
                    "Changing '{}' affects {} dependent symbols in {} files (risk: {})",
                    node.name, affected.len(), affected_files.len(), risk
                )
            }),
            &guard,
        ))
    }
}
