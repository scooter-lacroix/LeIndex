//! Leiden community detection over the PDG (roadmap Part IV).
//!
//! Communities approximate module/feature boundaries the codebase itself may
//! never have named. Leiden (not Louvain) because it guarantees internally
//! connected communities — a disconnected "community" on a code graph is
//! actively misleading (the plan's §IV.1).
//!
//! RAM discipline: the projection is a TRANSIENT petgraph::Graph built from
//! SORTED node/edge iteration (determinism), Leiden runs on its compact CSR
//! form, and both are dropped before returning — steady-state memory is the
//! returned `HashMap<NodeId, u32>` (~24 bytes/node) plus stats. Edge types
//! projected: Call, DataDependency, Containment — Import edges are excluded
//! (file-level noise that blurs module shape).

use std::collections::HashMap;

use crate::graph::pdg::{EdgeType, NodeId, NodeType, ProgramDependenceGraph};

/// Algorithm + quality identity persisted alongside results: the cache key
/// (the plan's retrofit warning honored from day one — a community cache
/// keyed on content alone would serve stale partitions after any algorithm
/// or parameter change).
/// Algorithm name persisted with results (cache identity).
pub const COMMUNITY_ALGORITHM: &str = "leiden";
/// Quality function persisted with results (cache identity).
pub const COMMUNITY_QUALITY: &str = "modularity";
/// Resolution parameter persisted with results (cache identity).
pub const COMMUNITY_RESOLUTION: f64 = 1.0;
/// Fixed seed: same graph → same partition (up to community relabeling).
const COMMUNITY_SEED: u64 = 42;

/// Summary of one detection run.
#[derive(Debug, Clone, Copy)]
pub struct CommunityStats {
    /// Number of communities found.
    pub community_count: usize,
    /// Modularity quality of the partition.
    pub quality: f64,
    /// Wall-clock milliseconds spent in Leiden (projection excluded).
    pub recompute_ms: u64,
}

/// Detect communities over the PDG's structural edges.
///
/// Deterministic: nodes are projected in sorted index order and edges in
/// sorted (source, target) order, with a fixed Leiden seed — identical graphs
/// produce identical partitions up to community ID relabeling.
pub fn detect_communities(pdg: &ProgramDependenceGraph) -> (HashMap<NodeId, u32>, CommunityStats) {
    use petgraph::Graph as PGraph;

    // Sorted, deterministic node projection: PDG indices are insertion-order
    // (parse order); sorting by node id string stabilizes the projection.
    let mut node_ids: Vec<NodeId> = pdg
        .node_indices()
        .filter(|&idx| {
            pdg.get_node(idx)
                .is_some_and(|node| community_relevant(&node.node_type))
        })
        .collect();
    node_ids.sort_unstable_by_key(|&idx| {
        pdg.get_node(idx)
            .map(|node| node.id.clone())
            .unwrap_or_default()
    });
    let mut index_map: HashMap<NodeId, usize> = HashMap::with_capacity(node_ids.len());
    let mut graph: PGraph<u32, f64, petgraph::Undirected> = PGraph::new_undirected();
    for (position, &node_idx) in node_ids.iter().enumerate() {
        graph.add_node(position as u32);
        index_map.insert(node_idx, position);
    }

    // Deterministic edge projection: structural types only, deduplicated
    // (multi-edges between the same pair collapse to a summed weight).
    let mut edge_list: Vec<(usize, usize)> = Vec::new();
    for edge_idx in pdg.edge_indices() {
        let Some(edge) = pdg.get_edge(edge_idx) else {
            continue;
        };
        if !matches!(
            edge.edge_type,
            EdgeType::Call | EdgeType::DataDependency | EdgeType::Containment
        ) {
            continue;
        }
        let Some((source, target)) = pdg.edge_endpoints(edge_idx) else {
            continue;
        };
        let (Some(&from), Some(&to)) = (index_map.get(&source), index_map.get(&target)) else {
            continue;
        };
        if from != to {
            edge_list.push((from, to));
        }
    }
    edge_list.sort_unstable();
    edge_list.dedup();
    for &(from, to) in &edge_list {
        graph.add_edge(
            petgraph::graph::NodeIndex::new(from),
            petgraph::graph::NodeIndex::new(to),
            1.0,
        );
    }
    let edge_count = edge_list.len();
    drop(edge_list);

    // Leiden over the compact CSR projection; drop it immediately after.
    let started = std::time::Instant::now();
    let mut result: Option<(Vec<usize>, f64)> = None;
    if graph.node_count() >= 2 && edge_count >= 1 {
        if let Ok(data) = leiden_rs::convert::petgraph::from_petgraph(&graph) {
            let config = leiden_rs::leiden::LeidenConfig::builder()
                .quality(leiden_rs::leiden::QualityType::Modularity)
                .resolution(COMMUNITY_RESOLUTION)
                .seed(COMMUNITY_SEED)
                .build();
            let leiden = leiden_rs::leiden::Leiden::new(config);
            if let Ok(output) = leiden.run(&data) {
                result = Some((output.partition.as_slice().to_vec(), output.quality));
            }
        }
    }
    drop(graph);
    let recompute_ms = started.elapsed().as_millis() as u64;

    let (membership, quality) = result.unwrap_or_else(|| {
        // No graph structure (or Leiden unavailable): every node is its own
        // community — the trivially correct partition.
        (Vec::new(), 0.0)
    });

    let mut communities: HashMap<NodeId, u32> = HashMap::with_capacity(node_ids.len());
    if membership.is_empty() {
        for (position, &node_idx) in node_ids.iter().enumerate() {
            communities.insert(node_idx, position as u32);
        }
    } else {
        for (position, &node_idx) in node_ids.iter().enumerate() {
            let community = membership.get(position).copied().unwrap_or(position) as u32;
            communities.insert(node_idx, community);
        }
    }

    let community_count = communities
        .values()
        .copied()
        .max()
        .map_or(0, |max| max as usize + 1);
    (
        communities,
        CommunityStats {
            community_count,
            quality,
            recompute_ms,
        },
    )
}

/// Human-scannable label for a community: the longest common directory
/// prefix of its member files, falling back to the alphabetically first
/// member's stem.
///
/// The paths are sorted first: membership arrives in randomized HashMap
/// iteration order, and the fallback used to take whichever file happened
/// to be first — the same community persisted as `a` or `b` across
/// otherwise identical runs.
pub fn community_label(pdg: &ProgramDependenceGraph, members: &[NodeId]) -> String {
    let mut paths: Vec<&str> = members
        .iter()
        .filter_map(|&node_id| pdg.get_node(node_id).map(|node| node.file_path.as_ref()))
        .collect();
    if paths.is_empty() {
        return "community".to_string();
    }
    paths.sort_unstable();
    let first = paths[0];
    let mut prefix_len = first.rfind('/').map(|idx| idx + 1).unwrap_or(0);
    for path in &paths[1..] {
        let common = first
            .bytes()
            .zip(path.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        // The byte-wise common length can end in the MIDDLE of a multi-byte
        // UTF-8 character (two distinct characters sharing a lead byte, e.g.
        // `src/é/a.rs` and `src/ê/b.rs`): retreat to a character boundary
        // before slicing, or `first[..bounded]` panics and takes the whole
        // indexing run down with it.
        let mut bounded = common.min(prefix_len);
        while bounded > 0 && !first.is_char_boundary(bounded) {
            bounded -= 1;
        }
        // Snap to a directory boundary.
        prefix_len = first[..bounded].rfind('/').map(|idx| idx + 1).unwrap_or(0);
        if prefix_len == 0 {
            break;
        }
    }
    if prefix_len > 0 {
        first[..prefix_len].trim_end_matches('/').to_string()
    } else {
        // No shared prefix: name by the alphabetically first member's stem.
        first
            .rsplit('/')
            .next()
            .unwrap_or(first)
            .split('.')
            .next()
            .unwrap_or("community")
            .to_string()
    }
}

/// Filter for community relevance: doc sections and external markers do not
/// participate in module shape.
pub fn community_relevant(node_type: &NodeType) -> bool {
    !matches!(node_type, NodeType::External | NodeType::DocSection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::pdg::{Edge, EdgeMetadata, Node};
    use std::sync::Arc;

    fn add_function(pdg: &mut ProgramDependenceGraph, id: &str, file: &str) -> NodeId {
        pdg.add_node(Node {
            id: id.to_string(),
            node_type: NodeType::Function,
            name: id.rsplit(':').next().unwrap_or(id).to_string(),
            file_path: Arc::from(file),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".to_string(),
        })
    }

    fn call_edge() -> Edge {
        Edge {
            edge_type: EdgeType::Call,
            metadata: EdgeMetadata {
                call_count: None,
                variable_name: None,
                confidence: None,
                channel: None,
                position: None,
            },
        }
    }

    fn two_module_graph() -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();
        let a1 = add_function(&mut pdg, "src/alpha/a1", "src/alpha/a1.rs");
        let a2 = add_function(&mut pdg, "src/alpha/a2", "src/alpha/a2.rs");
        let b1 = add_function(&mut pdg, "src/beta/b1", "src/beta/b1.rs");
        let b2 = add_function(&mut pdg, "src/beta/b2", "src/beta/b2.rs");
        pdg.add_edge(a1, a2, call_edge());
        pdg.add_edge(a2, a1, call_edge());
        pdg.add_edge(b1, b2, call_edge());
        pdg.add_edge(b2, b1, call_edge());
        // One bridge edge joins the modules.
        pdg.add_edge(a2, b1, call_edge());
        pdg
    }

    #[test]
    fn test_two_modules_split_into_two_communities() {
        let pdg = two_module_graph();
        let (communities, stats) = detect_communities(&pdg);
        assert_eq!(stats.community_count, 2, "stats: {stats:?}");
        let ids = ["src/alpha/a1", "src/alpha/a2", "src/beta/b1", "src/beta/b2"];
        let resolved: Vec<Option<NodeId>> = ids.iter().map(|id| pdg.find_by_id(id)).collect();
        let membership: Vec<u32> = resolved.iter().map(|n| communities[&n.unwrap()]).collect();
        assert_eq!(membership[0], membership[1], "alpha cluster together");
        assert_eq!(membership[2], membership[3], "beta cluster together");
        assert_ne!(membership[0], membership[2], "clusters separate");
    }

    #[test]
    fn test_deterministic_partition() {
        // Same graph built twice → same membership vector.
        let (c1, _) = detect_communities(&two_module_graph());
        let pdg2 = two_module_graph();
        let ids = ["src/alpha/a1", "src/alpha/a2", "src/beta/b1", "src/beta/b2"];
        let m1: Vec<u32> = ids
            .iter()
            .map(|id| c1[&pdg2.find_by_id(id).unwrap()])
            .collect();
        let (c2, _) = detect_communities(&two_module_graph());
        let m2: Vec<u32> = ids
            .iter()
            .map(|id| c2[&pdg2.find_by_id(id).unwrap()])
            .collect();
        // Equal up to relabeling: the partition structure matches.
        assert_eq!(
            (m1[0] == m1[1], m1[2] == m1[3], m1[0] != m1[2]),
            (m2[0] == m2[1], m2[2] == m2[3], m2[0] != m2[2])
        );
    }

    #[test]
    fn test_empty_graph_singletons() {
        let mut pdg = ProgramDependenceGraph::new();
        add_function(&mut pdg, "x", "x.rs");
        add_function(&mut pdg, "y", "y.rs");
        let (communities, stats) = detect_communities(&pdg);
        assert_eq!(stats.community_count, 2);
        assert!(communities.values().all(|&c| c < 2));
    }

    #[test]
    fn test_label_is_common_directory() {
        let pdg = two_module_graph();
        let members: Vec<NodeId> = ["src/alpha/a1", "src/alpha/a2"]
            .iter()
            .filter_map(|id| pdg.find_by_id(id))
            .collect();
        assert_eq!(community_label(&pdg, &members), "src/alpha");
    }

    #[test]
    fn test_label_does_not_panic_on_partial_utf8_common_prefix() {
        // é and ê share their UTF-8 lead byte, so the byte-wise common
        // prefix ends mid-character: slicing there used to panic.
        let mut pdg = ProgramDependenceGraph::new();
        add_function(&mut pdg, "a", "src/\u{e9}/a.rs"); // src/é/a.rs
        add_function(&mut pdg, "b", "src/\u{ea}/b.rs"); // src/ê/b.rs
        let members: Vec<NodeId> = ["a", "b"]
            .iter()
            .filter_map(|id| pdg.find_by_id(id))
            .collect();
        let label = community_label(&pdg, &members);
        assert_eq!(
            label, "src",
            "prefix retreats to the shared character boundary"
        );
    }

    #[test]
    fn test_label_fallback_is_order_independent() {
        // No shared directory prefix: the label must not depend on the
        // (randomized) membership iteration order.
        let build = |members: Vec<&str>| -> String {
            let mut pdg = ProgramDependenceGraph::new();
            add_function(&mut pdg, "a", "src/a.rs");
            add_function(&mut pdg, "b", "tests/b.rs");
            let member_ids: Vec<NodeId> =
                members.iter().filter_map(|id| pdg.find_by_id(id)).collect();
            community_label(&pdg, &member_ids)
        };
        assert_eq!(
            build(vec!["a", "b"]),
            build(vec!["b", "a"]),
            "identical communities must label identically across runs"
        );
    }
}
