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
/// also has `NodeType::External` — and carries the graph-level external
/// path. The type flip alone would leave the creating file's path in place,
/// keeping the node in that file's `file_index` entry: a later
/// `remove_file`/`delete_file_data` for the file would then delete a shared
/// placeholder other files' edges point at (the hazard
/// [`crate::graph::pdg::EXTERNAL_NODE_FILE_PATH`] exists to prevent).
pub(crate) fn normalize_external_nodes(pdg: &mut ProgramDependenceGraph) {
    let mut migrated = 0usize;
    for node in pdg.node_weights_mut() {
        let is_external = node.language == "external" || node.language.starts_with("external:");
        if is_external && node.node_type != NodeType::External {
            node.node_type = NodeType::External;
            node.file_path = std::sync::Arc::from(crate::graph::pdg::EXTERNAL_NODE_FILE_PATH);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::pdg::{Edge, EdgeMetadata, Node};

    fn function(id: &str, file: &str, name: &str, language: &str) -> Node {
        Node {
            id: id.to_string(),
            node_type: NodeType::Function,
            name: name.to_string(),
            file_path: std::sync::Arc::from(file),
            byte_range: (0, 0),
            complexity: 0,
            language: language.to_string(),
        }
    }

    /// A node that only becomes external via the language migration (round-8
    /// Kilo) must also be re-pathed to the graph-level external vocabulary:
    /// keeping the creating file's path would leave it in that file's
    /// file_index entry, so a later remove/delete for the file would destroy
    /// a shared placeholder other files' edges point at.
    #[test]
    fn test_normalize_external_nodes_repaths_migrated_nodes() {
        let mut pdg = ProgramDependenceGraph::new();
        let idx = pdg.add_node(function(
            "a.rs:ext_thing",
            "a.rs",
            "ext_thing",
            "external:crate::thing",
        ));
        normalize_external_nodes(&mut pdg);
        let node = pdg.get_node(idx).unwrap();
        assert_eq!(node.node_type, NodeType::External);
        assert_eq!(
            node.file_path.as_ref(),
            crate::graph::pdg::EXTERNAL_NODE_FILE_PATH,
            "migrated externals leave the per-file namespace"
        );
    }

    /// merge_pdgs canonicalizes externals on entry too — the merge path and
    /// the normalize pass must agree on the file_path convention.
    #[test]
    fn test_merge_pdgs_canonicalizes_external_paths() {
        let mut target = ProgramDependenceGraph::new();
        let mut source = ProgramDependenceGraph::new();
        let mut external = function("external::log", "a.rs", "log", "rust");
        external.node_type = NodeType::External;
        source.add_node(external);
        merge_pdgs(&mut target, source);
        let node = target
            .node_indices()
            .find_map(|idx| {
                let node = target.get_node(idx)?;
                (node.id == "external::log").then_some(node)
            })
            .unwrap();
        assert_eq!(
            node.file_path.as_ref(),
            crate::graph::pdg::EXTERNAL_NODE_FILE_PATH
        );
        // And a merged edge survives the fold onto the canonical node.
        let mut target = ProgramDependenceGraph::new();
        target.add_node(function("b.rs:caller", "b.rs", "caller", "rust"));
        let mut source = ProgramDependenceGraph::new();
        let mut external = function("external::log", "a.rs", "log", "rust");
        external.node_type = NodeType::External;
        let ext_idx = source.add_node(external);
        let caller_idx = source.add_node(function("a.rs:main", "a.rs", "main", "rust"));
        source.add_edge(
            caller_idx,
            ext_idx,
            Edge {
                edge_type: crate::graph::pdg::EdgeType::Call,
                metadata: EdgeMetadata::empty(),
            },
        );
        merge_pdgs(&mut target, source);
        assert_eq!(target.node_count(), 3);
    }
}
