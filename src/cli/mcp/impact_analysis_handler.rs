use super::helpers::{
    extract_string, extract_usize, get_direct_callers, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_impact_analysis — transitive dependency impact.
#[derive(Clone)]
pub struct ImpactAnalysisHandler;

#[allow(missing_docs)]
impl ImpactAnalysisHandler {
    pub fn name(&self) -> &str {
        "leindex_impact_analysis"
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

        guard.ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        if guard.pdg().is_none() {
            return Err(JsonRpcError::project_not_indexed(
                guard.project_path().display().to_string(),
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

        let forward = pdg.forward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig {
                max_depth: Some(depth),
                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
            },
        );
        let affected_symbols: Vec<String> = forward
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.name.clone()))
            .take(50)
            .collect();
        let affected_files: std::collections::HashSet<&str> = forward
            .iter()
            .filter_map(|&nid| pdg.get_node(nid).map(|n| n.file_path.as_ref()))
            .collect();

        let backward = pdg.backward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig {
                max_depth: Some(depth),
                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
            },
        );

        let risk = match change_type.as_str() {
            "remove" | "change_signature" => {
                if forward.len() > 5 || affected_files.len() > 3 {
                    "high"
                } else if !forward.is_empty() {
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

        Ok(wrap_with_meta(
            serde_json::json!({
                "symbol": node.name,
                "file": node.file_path,
                "change_type": change_type,
                "direct_callers": direct_callers,
                "transitive_affected_symbols": affected_symbols,
                "transitive_affected_files": affected_files.len(),
                "transitive_callers": backward.len(),
                "risk_level": risk,
                "summary": format!(
                    "Changing '{}' directly affects {} symbols in {} files (risk: {})",
                    node.name, forward.len(), affected_files.len(), risk
                )
            }),
            &guard,
        ))
    }
}
