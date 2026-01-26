// Program Dependence Graph implementation

use petgraph::stable_graph::StableGraph;
use petgraph::visit::Dfs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Node ID type (using u32 for memory efficiency)
pub type NodeId = petgraph::stable_graph::NodeIndex;

/// Edge ID type
pub type EdgeId = petgraph::stable_graph::EdgeIndex;

/// Node in the PDG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Unique identifier
    pub id: String,

    /// Node type
    pub node_type: NodeType,

    /// Symbol name
    pub name: String,

    /// File path
    pub file_path: String,

    /// Byte range in source
    pub byte_range: (usize, usize),

    /// Complexity score
    pub complexity: u32,

    /// Node-level embedding (optional)
    pub embedding: Option<Vec<f32>>,
}

/// Edge in the PDG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    /// Edge type
    pub edge_type: EdgeType,

    /// Metadata
    pub metadata: EdgeMetadata,
}

/// Node type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NodeType {
    Function,
    Class,
    Method,
    Variable,
    Module,
}

/// Edge type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EdgeType {
    /// Function call
    Call,

    /// Data dependency
    DataDependency,

    /// Inheritance
    Inheritance,

    /// Import/dependency
    Import,
}

/// Edge metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeMetadata {
    /// Call frequency (for call edges)
    pub call_count: Option<usize>,

    /// Variable name (for data dependency)
    pub variable_name: Option<String>,
}

/// Program Dependence Graph
///
/// Combines Call Graph, Data Flow Graph, and Inheritance Graph into a unified structure.
pub struct ProgramDependenceGraph {
    /// Internal graph structure
    graph: StableGraph<Node, Edge>,

    /// Symbol name to node ID mapping
    symbol_index: HashMap<String, NodeId>,

    /// File path to node IDs mapping
    file_index: HashMap<String, Vec<NodeId>>,
}

impl ProgramDependenceGraph {
    /// Create a new empty PDG
    pub fn new() -> Self {
        Self {
            graph: StableGraph::new(),
            symbol_index: HashMap::new(),
            file_index: HashMap::new(),
        }
    }

    /// Add a node to the graph
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = self.graph.add_node(node.clone());
        self.symbol_index.insert(node.id.clone(), id);
        self.file_index
            .entry(node.file_path.clone())
            .or_insert_with(Vec::new)
            .push(id);
        id
    }

    /// Add an edge between two nodes
    pub fn add_edge(&mut self, from: NodeId, to: NodeId, edge: Edge) -> Option<EdgeId> {
        Some(self.graph.add_edge(from, to, edge))
    }

    /// Get node by ID
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        self.graph.node_weight(id)
    }

    /// Get edge by ID
    pub fn get_edge(&self, id: EdgeId) -> Option<&Edge> {
        self.graph.edge_weight(id)
    }

    /// Find node by symbol name
    pub fn find_by_symbol(&self, symbol: &str) -> Option<NodeId> {
        self.symbol_index.get(symbol).copied()
    }

    /// Get all nodes in a file
    pub fn nodes_in_file(&self, file_path: &str) -> Vec<NodeId> {
        self.file_index.get(file_path).cloned().unwrap_or_default()
    }

    /// Get node count
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get edge count
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get neighbors of a node (outgoing)
    pub fn neighbors(&self, node_id: NodeId) -> Vec<NodeId> {
        self.graph.neighbors(node_id).collect()
    }

    /// Add multiple call edges
    ///
    /// # Arguments
    /// * `calls` - Vector of (caller_id, callee_id) tuples
    pub fn add_call_graph_edges(&mut self, calls: Vec<(NodeId, NodeId)>) {
        for (caller, callee) in calls {
            self.add_edge(
                caller,
                callee,
                Edge {
                    edge_type: EdgeType::Call,
                    metadata: EdgeMetadata {
                        call_count: None,
                        variable_name: None,
                    },
                },
            );
        }
    }

    /// Add multiple data flow edges
    ///
    /// # Arguments
    /// * `flows` - Vector of (from_id, to_id, variable_name) tuples
    pub fn add_data_flow_edges(&mut self, flows: Vec<(NodeId, NodeId, String)>) {
        for (from, to, var_name) in flows {
            self.add_edge(
                from,
                to,
                Edge {
                    edge_type: EdgeType::DataDependency,
                    metadata: EdgeMetadata {
                        call_count: None,
                        variable_name: Some(var_name),
                    },
                },
            );
        }
    }

    /// Add multiple inheritance edges
    ///
    /// # Arguments
    /// * `inheritances` - Vector of (child_id, parent_id) tuples
    pub fn add_inheritance_edges(&mut self, inheritances: Vec<(NodeId, NodeId)>) {
        for (child, parent) in inheritances {
            self.add_edge(
                child,
                parent,
                Edge {
                    edge_type: EdgeType::Inheritance,
                    metadata: EdgeMetadata {
                        call_count: None,
                        variable_name: None,
                    },
                },
            );
        }
    }

    /// Get forward impact (nodes reachable from this node)
    ///
    /// # Arguments
    /// * `node_id` - Starting node for reachability analysis
    pub fn get_forward_impact(&self, node_id: NodeId) -> Vec<NodeId> {
        let mut impact = Vec::new();
        let mut dfs = Dfs::new(&self.graph, node_id);
        while let Some(nx) = dfs.next(&self.graph) {
            if nx != node_id {
                impact.push(nx);
            }
        }
        impact
    }

    /// Get backward impact (nodes that can reach this node)
    ///
    /// # Arguments
    /// * `node_id` - Target node for reachability analysis
    pub fn get_backward_impact(&self, node_id: NodeId) -> Vec<NodeId> {
        let mut impact = Vec::new();
        // Use neighbors_directed with Incoming to traverse backwards
        let reversed_graph = petgraph::visit::Reversed(&self.graph);
        let mut dfs = Dfs::new(reversed_graph, node_id);
        while let Some(nx) = dfs.next(reversed_graph) {
            if nx != node_id {
                impact.push(nx);
            }
        }
        impact
    }

    /// Serialize the graph (placeholder - needs proper implementation for StableGraph)
    pub fn serialize(&self) -> Result<Vec<u8>, String> {
        // TODO: Implement proper serialization for StableGraph
        // StableGraph doesn't support serde directly, need to convert nodes/edges
        Err("Serialization not yet implemented".to_string())
    }

    /// Deserialize a graph (placeholder - needs proper implementation for StableGraph)
    pub fn deserialize(_data: &[u8]) -> Result<Self, String> {
        // TODO: Implement proper deserialization for StableGraph
        Err("Deserialization not yet implemented".to_string())
    }
}

impl Default for ProgramDependenceGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pdg_creation() {
        let pdg = ProgramDependenceGraph::new();
        assert_eq!(pdg.node_count(), 0);
        assert_eq!(pdg.edge_count(), 0);
    }

    #[test]
    fn test_add_node() {
        let mut pdg = ProgramDependenceGraph::new();
        let node = Node {
            id: "test_func".to_string(),
            node_type: NodeType::Function,
            name: "test_func".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            embedding: None,
        };

        let id = pdg.add_node(node);
        assert_eq!(pdg.node_count(), 1);
        assert!(pdg.get_node(id).is_some());
    }

    #[test]
    fn test_find_by_symbol() {
        let mut pdg = ProgramDependenceGraph::new();
        let node = Node {
            id: "my_func".to_string(),
            node_type: NodeType::Function,
            name: "my_func".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            embedding: None,
        };

        pdg.add_node(node);
        let found = pdg.find_by_symbol("my_func");
        assert!(found.is_some());
    }

    #[test]
    fn test_impact_analysis() {
        let mut pdg = ProgramDependenceGraph::new();
        let n1 = pdg.add_node(Node {
            id: "n1".to_string(),
            node_type: NodeType::Function,
            name: "n1".to_string(),
            file_path: "f1.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            embedding: None,
        });
        let n2 = pdg.add_node(Node {
            id: "n2".to_string(),
            node_type: NodeType::Function,
            name: "n2".to_string(),
            file_path: "f1.py".to_string(),
            byte_range: (20, 30),
            complexity: 1,
            embedding: None,
        });
        let n3 = pdg.add_node(Node {
            id: "n3".to_string(),
            node_type: NodeType::Function,
            name: "n3".to_string(),
            file_path: "f1.py".to_string(),
            byte_range: (40, 50),
            complexity: 1,
            embedding: None,
        });

        pdg.add_call_graph_edges(vec![(n1, n2), (n2, n3)]);

        let forward = pdg.get_forward_impact(n1);
        assert_eq!(forward.len(), 2);
        assert!(forward.contains(&n2));
        assert!(forward.contains(&n3));

        let backward = pdg.get_backward_impact(n3);
        assert_eq!(backward.len(), 2);
        assert!(backward.contains(&n2));
        assert!(backward.contains(&n1));
    }
}
