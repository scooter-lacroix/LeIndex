// PDG analytics (generation-layer-backed)

use crate::graph::pdg::ProgramDependenceGraph;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Analytics for graph metrics, computed from the resident PDG.
pub struct Analytics {
    pdg: ProgramDependenceGraph,
}

impl Analytics {
    /// Create analytics over a hydrated graph.
    pub fn new(pdg: ProgramDependenceGraph) -> Self {
        Self { pdg }
    }

    /// Get node count grouped by the persisted storage-vocabulary type.
    pub fn count_nodes_by_type(&self) -> Vec<NodeTypeCount> {
        let mut counts = HashMap::<String, i64>::new();
        for index in self.pdg.node_indices() {
            if let Some(node) = self.pdg.get_node(index) {
                *counts
                    .entry(
                        crate::storage::generation::graph_codec::graph_node_type_str(
                            &node.node_type,
                        )
                        .to_string(),
                    )
                    .or_default() += 1;
            }
        }
        let mut counts: Vec<NodeTypeCount> = counts
            .into_iter()
            .map(|(node_type, count)| NodeTypeCount { node_type, count })
            .collect();
        counts.sort_by(|a, b| a.node_type.cmp(&b.node_type));
        counts
    }

    /// Get complexity distribution using the SQL CASE buckets and lexical
    /// bucket ordering: complex, moderate, simple, very_complex.
    pub fn complexity_distribution(&self) -> Vec<ComplexityBucket> {
        let mut counts = HashMap::<&'static str, i64>::new();
        for index in self.pdg.node_indices() {
            if let Some(node) = self.pdg.get_node(index) {
                let bucket = match node.complexity {
                    complexity if complexity < 5 => "simple",
                    complexity if complexity < 10 => "moderate",
                    complexity if complexity < 20 => "complex",
                    _ => "very_complex",
                };
                *counts.entry(bucket).or_default() += 1;
            }
        }
        let mut buckets: Vec<ComplexityBucket> = counts
            .into_iter()
            .map(|(bucket, count)| ComplexityBucket {
                bucket: bucket.to_string(),
                count,
            })
            .collect();
        buckets.sort_by(|a, b| a.bucket.cmp(&b.bucket));
        buckets
    }

    /// Get edge count grouped by the persisted storage-vocabulary type.
    pub fn count_edges_by_type(&self) -> Vec<EdgeTypeCount> {
        let mut counts = HashMap::<String, i64>::new();
        for edge_index in self.pdg.edge_indices() {
            if let Some(edge) = self.pdg.get_edge(edge_index) {
                *counts
                    .entry(
                        crate::storage::generation::graph_codec::graph_edge_type_str(
                            &edge.edge_type,
                        )
                        .to_string(),
                    )
                    .or_default() += 1;
            }
        }
        let mut counts: Vec<EdgeTypeCount> = counts
            .into_iter()
            .map(|(edge_type, count)| EdgeTypeCount { edge_type, count })
            .collect();
        counts.sort_by(|a, b| a.edge_type.cmp(&b.edge_type));
        counts
    }

    /// Get hotspots matching the legacy SQL semantics: node complexity >=
    /// `threshold`, outgoing edge fan-out > threshold / 2, ordered by
    /// complexity DESC then fan-out DESC. Fan-out includes all edge types.
    pub fn get_hotspots(&self, threshold: i32) -> Vec<Hotspot> {
        let mut fan_out_by_node = HashMap::<petgraph::graph::NodeIndex, i64>::new();
        for edge_index in self.pdg.edge_indices() {
            if let Some((source, _)) = self.pdg.edge_endpoints(edge_index) {
                *fan_out_by_node.entry(source).or_default() += 1;
            }
        }

        let threshold = i64::from(threshold);
        let mut hotspots = Vec::new();
        for node_index in self.pdg.node_indices() {
            let Some(node) = self.pdg.get_node(node_index) else {
                continue;
            };
            let complexity = i64::from(node.complexity);
            let fan_out = fan_out_by_node.get(&node_index).copied().unwrap_or(0);
            if complexity >= threshold && fan_out > threshold / 2 {
                hotspots.push(Hotspot {
                    node_id: node.id.clone(),
                    symbol_name: node.name.clone(),
                    file_path: node.file_path.to_string(),
                    complexity: node.complexity as i32,
                    fan_out,
                });
            }
        }
        hotspots.sort_by(|a, b| {
            b.complexity
                .cmp(&a.complexity)
                .then_with(|| b.fan_out.cmp(&a.fan_out))
        });
        hotspots
    }
}

/// Node type count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTypeCount {
    /// Type of the node (as string)
    pub node_type: String,
    /// Number of nodes of this type
    pub count: i64,
}

/// Complexity bucket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityBucket {
    /// Complexity category (e.g., 'simple', 'moderate', etc.)
    pub bucket: String,
    /// Number of nodes in this complexity bucket
    pub count: i64,
}

/// Edge type count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeTypeCount {
    /// Type of the edge (as string)
    pub edge_type: String,
    /// Number of edges of this type
    pub count: i64,
}

/// Hotspot node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotspot {
    /// Stable graph node identifier
    pub node_id: String,
    /// Name of the symbol
    pub symbol_name: String,
    /// Path to the file containing the symbol
    pub file_path: String,
    /// Complexity score of the node
    pub complexity: i32,
    /// Number of outgoing edges (fan-out)
    pub fan_out: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::pdg::{Edge, EdgeMetadata, EdgeType, Node, NodeType};
    use std::sync::Arc;

    fn node(id: &str, name: &str, kind: NodeType, complexity: u32) -> Node {
        Node {
            id: id.to_string(),
            node_type: kind,
            name: name.to_string(),
            file_path: Arc::from("src/lib.rs"),
            byte_range: (0, 10),
            complexity,
            language: "rust".to_string(),
        }
    }

    #[test]
    fn test_analytics_graph_queries_preserve_grouping_buckets_and_hotspot_order() {
        let mut pdg = ProgramDependenceGraph::new();
        let simple = pdg.add_node(node("src/lib.rs:simple", "simple", NodeType::Function, 4));
        let moderate = pdg.add_node(node(
            "src/lib.rs:moderate",
            "moderate",
            NodeType::Function,
            7,
        ));
        let complex = pdg.add_node(node("src/lib.rs:complex", "complex", NodeType::Class, 12));
        let very_complex = pdg.add_node(node(
            "src/lib.rs:very_complex",
            "very_complex",
            NodeType::Method,
            20,
        ));
        for (source, target, edge_type) in [
            (moderate, simple, EdgeType::Call),
            (moderate, complex, EdgeType::Call),
            (moderate, very_complex, EdgeType::DataDependency),
            (complex, simple, EdgeType::Import),
            (very_complex, simple, EdgeType::Call),
        ] {
            pdg.add_edge(
                source,
                target,
                Edge {
                    edge_type,
                    metadata: EdgeMetadata::empty(),
                },
            );
        }

        let analytics = Analytics::new(pdg);
        assert_eq!(
            analytics
                .count_nodes_by_type()
                .into_iter()
                .map(|item| (item.node_type, item.count))
                .collect::<Vec<_>>(),
            vec![
                ("class".to_string(), 1),
                ("function".to_string(), 2),
                ("method".to_string(), 1),
            ]
        );
        assert_eq!(
            analytics
                .complexity_distribution()
                .into_iter()
                .map(|item| (item.bucket, item.count))
                .collect::<Vec<_>>(),
            vec![
                ("complex".to_string(), 1),
                ("moderate".to_string(), 1),
                ("simple".to_string(), 1),
                ("very_complex".to_string(), 1),
            ]
        );
        assert_eq!(
            analytics
                .count_edges_by_type()
                .into_iter()
                .map(|item| (item.edge_type, item.count))
                .collect::<Vec<_>>(),
            vec![
                ("call".to_string(), 3),
                ("data_dependency".to_string(), 1),
                ("import".to_string(), 1),
            ]
        );
        let hotspots = analytics.get_hotspots(4);
        assert_eq!(hotspots.len(), 1);
        assert_eq!(hotspots[0].node_id, "src/lib.rs:moderate");
        assert_eq!(hotspots[0].fan_out, 3);
    }

    #[test]
    fn test_analytics_creation() {
        let analytics = Analytics::new(ProgramDependenceGraph::new());
        assert!(analytics.count_nodes_by_type().is_empty());
        assert!(analytics.complexity_distribution().is_empty());
        assert!(analytics.count_edges_by_type().is_empty());
        assert!(analytics.get_hotspots(5).is_empty());
    }
}
