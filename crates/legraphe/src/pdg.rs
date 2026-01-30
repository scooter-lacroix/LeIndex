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

    /// Programming language
    pub language: String,

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
    /// A function definition
    Function,
    /// A class definition
    Class,
    /// A method definition
    Method,
    /// A variable definition
    Variable,
    /// A module or file
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

/// Serializable representation of a node with its ID
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializableNode {
    /// Original node index (for reconstruction)
    index: u32,

    /// Node data
    node: Node,
}

/// Serializable representation of an edge with its endpoints
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializableEdge {
    /// Source node index
    source: u32,

    /// Target node index
    target: u32,

    /// Edge data
    edge: Edge,
}

/// Serializable representation of the entire PDG
///
/// This struct contains all the data needed to reconstruct a PDG,
/// including nodes, edges, and indexes for fast lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDG {
    /// All nodes with their original indices
    nodes: Vec<SerializableNode>,

    /// All edges with source/target indices
    edges: Vec<SerializableEdge>,

    /// Symbol name to node index mapping
    symbol_index: HashMap<String, u32>,

    /// File path to node indices mapping
    file_index: HashMap<String, Vec<u32>>,
}

impl SerializablePDG {
    /// Convert a ProgramDependenceGraph to its serializable form
    fn from_pdg(pdg: &ProgramDependenceGraph) -> Self {
        // Collect all nodes with their indices
        let nodes: Vec<SerializableNode> = pdg
            .graph
            .node_indices()
            .map(|idx| SerializableNode {
                index: idx.index() as u32,
                node: pdg.graph[idx].clone(),
            })
            .collect();

        // Collect all edges with their source and target indices
        let edges: Vec<SerializableEdge> = pdg
            .graph
            .edge_indices()
            .map(|eidx| {
                let (source, target) = pdg
                    .graph
                    .edge_endpoints(eidx)
                    .expect("Edge should have valid endpoints");
                SerializableEdge {
                    source: source.index() as u32,
                    target: target.index() as u32,
                    edge: pdg.graph[eidx].clone(),
                }
            })
            .collect();

        // Convert symbol index: NodeId -> u32
        let symbol_index: HashMap<String, u32> = pdg
            .symbol_index
            .iter()
            .map(|(k, v)| (k.clone(), v.index() as u32))
            .collect();

        // Convert file index: Vec<NodeId> -> Vec<u32>
        let file_index: HashMap<String, Vec<u32>> = pdg
            .file_index
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();

        Self {
            nodes,
            edges,
            symbol_index,
            file_index,
        }
    }

    /// Convert the serializable form back to a ProgramDependenceGraph
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        let mut pdg = ProgramDependenceGraph::new();

        // Create a mapping from serialized indices to new NodeIds
        let mut index_map: HashMap<u32, NodeId> = HashMap::new();

        // Add all nodes
        for serializable_node in &self.nodes {
            let node = serializable_node.node.clone();
            let node_id = pdg.graph.add_node(node);

            // Store the mapping from old index to new NodeId
            index_map.insert(serializable_node.index, node_id);
        }

        // Rebuild symbol index with new NodeIds
        for (symbol, old_index) in &self.symbol_index {
            if let Some(&node_id) = index_map.get(old_index) {
                pdg.symbol_index.insert(symbol.clone(), node_id);
            }
        }

        // Rebuild file index with new NodeIds
        for (file_path, old_indices) in &self.file_index {
            let mut new_indices = Vec::new();
            for old_index in old_indices {
                if let Some(&node_id) = index_map.get(old_index) {
                    new_indices.push(node_id);
                }
            }
            if !new_indices.is_empty() {
                pdg.file_index.insert(file_path.clone(), new_indices);
            }
        }

        // Add all edges
        for serializable_edge in &self.edges {
            let source_id = index_map.get(&serializable_edge.source).ok_or_else(|| {
                format!("Missing source node index: {}", serializable_edge.source)
            })?;
            let target_id = index_map.get(&serializable_edge.target).ok_or_else(|| {
                format!("Missing target node index: {}", serializable_edge.target)
            })?;

            pdg.graph
                .add_edge(*source_id, *target_id, serializable_edge.edge.clone());
        }

        Ok(pdg)
    }
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
            .or_default()
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

    /// Remove a node and its associated edges
    pub fn remove_node(&mut self, node_id: NodeId) -> Option<Node> {
        if let Some(node) = self.graph.remove_node(node_id) {
            self.symbol_index.remove(&node.id);
            if let Some(nodes) = self.file_index.get_mut(&node.file_path) {
                nodes.retain(|&id| id != node_id);
            }
            Some(node)
        } else {
            None
        }
    }

    /// Remove all nodes and edges for a specific file
    pub fn remove_file(&mut self, file_path: &str) {
        let node_ids = self.nodes_in_file(file_path);
        for node_id in node_ids {
            self.remove_node(node_id);
        }
        self.file_index.remove(file_path);
    }

    /// Iterate over all node indices
    ///
    /// This provides access to all nodes in the graph for iteration.
    pub fn node_indices(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.graph.node_indices()
    }

    /// Iterate over all edge indices
    ///
    /// This provides access to all edges in the graph for iteration.
    pub fn edge_indices(&self) -> impl Iterator<Item = EdgeId> + '_ {
        self.graph.edge_indices()
    }

    /// Get edge endpoints
    ///
    /// Returns the source and target nodes for the given edge ID.
    pub fn edge_endpoints(&self, edge_id: EdgeId) -> Option<(NodeId, NodeId)> {
        self.graph.edge_endpoints(edge_id)
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

    /// Serialize the graph to a byte vector
    ///
    /// This method serializes the entire PDG including:
    /// - All nodes with their metadata
    /// - All edges with their types and metadata
    /// - Symbol index for fast lookup
    /// - File index for file-based queries
    ///
    /// # Returns
    ///
    /// A `Vec<u8>` containing the serialized graph data using bincode format
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pdg = ProgramDependenceGraph::new();
    /// // ... add nodes and edges ...
    /// let serialized = pdg.serialize()?;
    /// std::fs::write("pdg.bin", serialized)?;
    /// ```
    pub fn serialize(&self) -> Result<Vec<u8>, String> {
        // Create a serializable representation of the graph
        let serializable = SerializablePDG::from_pdg(self);

        // Use bincode for efficient binary serialization
        bincode::serialize(&serializable).map_err(|e| format!("Failed to serialize PDG: {}", e))
    }

    /// Deserialize a graph from a byte vector
    ///
    /// This method reconstructs a PDG from previously serialized data,
    /// including all nodes, edges, and indexes.
    ///
    /// # Arguments
    ///
    /// * `data` - Byte vector containing serialized PDG data
    ///
    /// # Returns
    ///
    /// A reconstructed `ProgramDependenceGraph`
    ///
    /// # Errors
    ///
    /// Returns an error if deserialization fails or data is corrupted
    ///
    /// # Example
    ///
    /// ```ignore
    /// let data = std::fs::read("pdg.bin")?;
    /// let pdg = ProgramDependenceGraph::deserialize(&data)?;
    /// ```
    pub fn deserialize(data: &[u8]) -> Result<Self, String> {
        // Deserialize the serializable representation
        let serializable: SerializablePDG =
            bincode::deserialize(data).map_err(|e| format!("Failed to deserialize PDG: {}", e))?;

        // Convert back to ProgramDependenceGraph
        serializable.to_pdg()
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
            language: "python".to_string(),
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
            language: "python".to_string(),
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
            language: "python".to_string(),
            embedding: None,
        });
        let n2 = pdg.add_node(Node {
            id: "n2".to_string(),
            node_type: NodeType::Function,
            name: "n2".to_string(),
            file_path: "f1.py".to_string(),
            byte_range: (20, 30),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });
        let n3 = pdg.add_node(Node {
            id: "n3".to_string(),
            node_type: NodeType::Function,
            name: "n3".to_string(),
            file_path: "f1.py".to_string(),
            byte_range: (40, 50),
            complexity: 1,
            language: "python".to_string(),
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

    #[test]
    fn test_serialize_empty_graph() {
        let pdg = ProgramDependenceGraph::new();
        let serialized = pdg.serialize().expect("Serialization should succeed");
        assert!(!serialized.is_empty());

        let deserialized = ProgramDependenceGraph::deserialize(&serialized)
            .expect("Deserialization should succeed");
        assert_eq!(deserialized.node_count(), 0);
        assert_eq!(deserialized.edge_count(), 0);
    }

    #[test]
    fn test_serialize_single_node() {
        let mut pdg = ProgramDependenceGraph::new();
        let node = Node {
            id: "test_func".to_string(),
            node_type: NodeType::Function,
            name: "test_func".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            language: "python".to_string(),
            embedding: None,
        };
        pdg.add_node(node);

        let serialized = pdg.serialize().expect("Serialization should succeed");
        let deserialized = ProgramDependenceGraph::deserialize(&serialized)
            .expect("Deserialization should succeed");

        assert_eq!(deserialized.node_count(), 1);
        assert_eq!(deserialized.edge_count(), 0);

        let node_id = deserialized
            .find_by_symbol("test_func")
            .expect("Node should be found");
        let retrieved_node = deserialized.get_node(node_id).expect("Node should exist");
        assert_eq!(retrieved_node.name, "test_func");
        assert_eq!(retrieved_node.complexity, 5);
        assert_eq!(retrieved_node.file_path, "test.py");
    }

    #[test]
    fn test_serialize_graph_with_edges() {
        let mut pdg = ProgramDependenceGraph::new();
        let n1 = pdg.add_node(Node {
            id: "n1".to_string(),
            node_type: NodeType::Function,
            name: "n1".to_string(),
            file_path: "f1.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });
        let n2 = pdg.add_node(Node {
            id: "n2".to_string(),
            node_type: NodeType::Function,
            name: "n2".to_string(),
            file_path: "f1.py".to_string(),
            byte_range: (20, 30),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        pdg.add_call_graph_edges(vec![(n1, n2)]);

        let serialized = pdg.serialize().expect("Serialization should succeed");
        let deserialized = ProgramDependenceGraph::deserialize(&serialized)
            .expect("Deserialization should succeed");

        assert_eq!(deserialized.node_count(), 2);
        assert_eq!(deserialized.edge_count(), 1);
    }

    #[test]
    fn test_serialize_preserves_symbol_index() {
        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "func_a".to_string(),
            node_type: NodeType::Function,
            name: "func_a".to_string(),
            file_path: "a.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });
        pdg.add_node(Node {
            id: "func_b".to_string(),
            node_type: NodeType::Function,
            name: "func_b".to_string(),
            file_path: "b.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        let serialized = pdg.serialize().expect("Serialization should succeed");
        let deserialized = ProgramDependenceGraph::deserialize(&serialized)
            .expect("Deserialization should succeed");

        assert!(deserialized.find_by_symbol("func_a").is_some());
        assert!(deserialized.find_by_symbol("func_b").is_some());
        assert!(deserialized.find_by_symbol("nonexistent").is_none());
    }

    #[test]
    fn test_serialize_preserves_file_index() {
        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "func1".to_string(),
            node_type: NodeType::Function,
            name: "func1".to_string(),
            file_path: "file1.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });
        pdg.add_node(Node {
            id: "func2".to_string(),
            node_type: NodeType::Function,
            name: "func2".to_string(),
            file_path: "file1.py".to_string(),
            byte_range: (20, 30),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });
        pdg.add_node(Node {
            id: "func3".to_string(),
            node_type: NodeType::Function,
            name: "func3".to_string(),
            file_path: "file2.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        let serialized = pdg.serialize().expect("Serialization should succeed");
        let deserialized = ProgramDependenceGraph::deserialize(&serialized)
            .expect("Deserialization should succeed");

        let file1_nodes = deserialized.nodes_in_file("file1.py");
        assert_eq!(file1_nodes.len(), 2);

        let file2_nodes = deserialized.nodes_in_file("file2.py");
        assert_eq!(file2_nodes.len(), 1);
    }

    #[test]
    fn test_serialize_with_different_edge_types() {
        let mut pdg = ProgramDependenceGraph::new();
        let n1 = pdg.add_node(Node {
            id: "child".to_string(),
            node_type: NodeType::Class,
            name: "Child".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });
        let n2 = pdg.add_node(Node {
            id: "parent".to_string(),
            node_type: NodeType::Class,
            name: "Parent".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (20, 30),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        pdg.add_inheritance_edges(vec![(n1, n2)]);

        let serialized = pdg.serialize().expect("Serialization should succeed");
        let deserialized = ProgramDependenceGraph::deserialize(&serialized)
            .expect("Deserialization should succeed");

        assert_eq!(deserialized.node_count(), 2);
        assert_eq!(deserialized.edge_count(), 1);
    }

    #[test]
    fn test_serialize_roundtrip_complexity() {
        let mut pdg = ProgramDependenceGraph::new();
        let node = Node {
            id: "complex_func".to_string(),
            node_type: NodeType::Function,
            name: "complex_func".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (0, 1000),
            complexity: 42,
            language: "python".to_string(),
            embedding: Some(vec![0.1, 0.2, 0.3]),
        };
        pdg.add_node(node);

        let serialized = pdg.serialize().expect("Serialization should succeed");
        let deserialized = ProgramDependenceGraph::deserialize(&serialized)
            .expect("Deserialization should succeed");

        let node_id = deserialized.find_by_symbol("complex_func").unwrap();
        let retrieved_node = deserialized.get_node(node_id).unwrap();

        assert_eq!(retrieved_node.complexity, 42);
        assert_eq!(retrieved_node.byte_range, (0, 1000));
        assert_eq!(retrieved_node.embedding, Some(vec![0.1, 0.2, 0.3]));
    }

    #[test]
    fn test_deserialize_invalid_data() {
        let invalid_data = b"not a valid pdg";
        let result = ProgramDependenceGraph::deserialize(invalid_data);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_empty_data() {
        let empty_data = b"";
        let result = ProgramDependenceGraph::deserialize(empty_data);
        assert!(result.is_err());
    }
}
