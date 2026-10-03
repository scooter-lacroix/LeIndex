// PDG graph-merge primitives extracted from `index_builder`.
//
// *Le Pont* — the bridge between per-file graphs and the resident project
// graph: exact-id deduping merge, per-file removal, external normalization.

use super::ProgramDependenceGraph;
use crate::graph::pdg::NodeType;
use anyhow::Result;
use tracing::{info, warn};

/// Merge a source PDG into a target PDG.
///
/// The merged target holds at most one node per node id: a source node whose
/// id already exists in the target is not re-added — its edges are remapped
/// onto the existing node. Per-file extraction legitimately produces
/// duplicate ids for its external placeholders (`external::{target}` is
/// created once per file pass), and concatenating them gave the graph
/// several nodes sharing one id; `save_pdg` then keys every copy onto the
/// single `intel_nodes` row `(project_id, node_id)` and a reload collapses
/// them. Exact-id dedup preserves overloaded methods that share a qualified
/// name: their ids differ (file-prefixed, or `@start..end`-suffixed within
/// a file by the extraction's duplicate guard).
pub(crate) fn merge_pdgs(target: &mut ProgramDependenceGraph, source: ProgramDependenceGraph) {
    let mut id_map: std::collections::HashMap<
        petgraph::graph::NodeIndex,
        petgraph::graph::NodeIndex,
    > = std::collections::HashMap::with_capacity(source.node_count());

    // Consume the source graph by value so node/edge weights are *moved*, not
    // cloned, into the target. `into_nodes_edges_iters()` yields owned weights
    // (with vacant slots filtered out), avoiding a per-element `clone()`.
    let (nodes_iter, edges_iter) = source.graph.into_nodes_edges_iters();

    for mut node in nodes_iter {
        if node.weight.node_type == NodeType::External {
            node.weight.file_path =
                std::sync::Arc::from(crate::graph::pdg::EXTERNAL_NODE_FILE_PATH);
        }
        let new_idx = match target.find_by_id(&node.weight.id) {
            // Duplicate id (per-file external placeholder): fold onto the
            // existing node; the moved weight is dropped. A non-external
            // duplicate means a caller merged without removing the file's
            // old nodes first (a caller-side invariant) — stay visible so a
            // future id scheme that produces real symbol collisions is
            // diagnosed, not silently absorbed.
            Some(existing_idx) => {
                if node.weight.node_type != NodeType::External {
                    warn!(
                        id = %node.weight.id,
                        "merge_pdgs folded a non-external duplicate node id (first wins)"
                    );
                }
                existing_idx
            }
            None => target.add_node(node.weight),
        };
        id_map.insert(node.index, new_idx);
    }

    for edge in edges_iter {
        if let (Some(&si), Some(&ti)) = (id_map.get(&edge.source), id_map.get(&edge.target)) {
            target.add_edge(si, ti, edge.weight);
        }
    }
}

/// Remove all nodes and edges for a file from the PDG.
pub(crate) fn remove_file_from_pdg(
    pdg: &mut ProgramDependenceGraph,
    file_path: &str,
) -> Result<()> {
    pdg.remove_file(file_path);
    Ok(())
}

/// Normalize external nodes: ensure any node with `language == "external"`
/// also has `NodeType::External`.
pub(crate) fn normalize_external_nodes(pdg: &mut ProgramDependenceGraph) {
    let mut migrated = 0usize;
    for node in pdg.node_weights_mut() {
        let is_external = node.language == "external" || node.language.starts_with("external:");
        if is_external && node.node_type != NodeType::External {
            node.node_type = NodeType::External;
            migrated += 1;
        }
    }
    if migrated > 0 {
        info!(
            "Normalized {} external nodes to NodeType::External",
            migrated
        );
    }
}
