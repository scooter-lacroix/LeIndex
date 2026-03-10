// Cross-project PDG extension
//
// This module extends the PDG with cross-project capabilities,
// enabling tracking of external node references and merging graphs
// from multiple projects.

use crate::pdg::{EdgeId, NodeId, NodeType, ProgramDependenceGraph};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Cross-project PDG extension
///
/// Combines a root project's PDG with external project PDGs,
/// tracking the origin of each node for reference.
pub struct CrossProjectPDG {
    /// Root project ID
    pub root_project_id: String,

    /// Merged graph containing local + external nodes
    pub merged_pdg: ProgramDependenceGraph,

    /// Mapping from external node IDs to their project origin
    pub node_origins: HashMap<NodeId, String>,

    /// External project references (lazy load)
    pub external_refs: HashMap<String, Vec<NodeId>>, // project_id -> nodes

    /// Max depth of external loading
    pub max_depth: usize,
}

impl CrossProjectPDG {
    /// Create new cross-project PDG
    pub fn new(root_project_id: String, root_pdg: ProgramDependenceGraph) -> Self {
        Self {
            root_project_id,
            merged_pdg: root_pdg,
            node_origins: HashMap::new(),
            external_refs: HashMap::new(),
            max_depth: 3,
        }
    }

    /// Create with custom max depth
    pub fn with_max_depth(
        root_project_id: String,
        root_pdg: ProgramDependenceGraph,
        max_depth: usize,
    ) -> Self {
        Self {
            root_project_id,
            merged_pdg: root_pdg,
            node_origins: HashMap::new(),
            external_refs: HashMap::new(),
            max_depth,
        }
    }

    /// Merge external PDG into this one
    ///
    /// Adds all nodes and edges from the external PDG to the merged graph,
    /// tracking their origin project.
    pub fn merge_external_pdg(
        &mut self,
        project_id: &str,
        external_pdg: &ProgramDependenceGraph,
    ) -> Result<(), MergeError> {
        // Track current depth
        let current_depth = self.external_refs.len();
        if current_depth >= self.max_depth {
            return Err(MergeError::MaxDepthExceeded(self.max_depth));
        }

        // Track mapping from old node IDs to new node IDs
        let mut node_id_map: HashMap<NodeId, NodeId> = HashMap::new();

        // Track which nodes we've added from this project
        let mut added_nodes = Vec::new();

        // Add all nodes from external PDG
        for old_node_id in external_pdg.node_indices() {
            if let Some(node) = external_pdg.get_node(old_node_id) {
                // Add node to merged PDG, which returns the new NodeId
                let new_node_id = self.merged_pdg.add_node(node.clone());

                // Store the mapping from old to new
                node_id_map.insert(old_node_id, new_node_id);

                // Track origin
                self.node_origins
                    .insert(new_node_id, project_id.to_string());
                added_nodes.push(new_node_id);
            }
        }

        // Add all edges from external PDG with proper ID mapping
        for edge_id in external_pdg.edge_indices() {
            if let Some(edge) = external_pdg.get_edge(edge_id) {
                // Get the original edge endpoints
                if let Some((old_source, old_target)) = external_pdg.edge_endpoints(edge_id) {
                    // Map old node IDs to new node IDs
                    let new_source = match node_id_map.get(&old_source) {
                        Some(&id) => id,
                        None => old_source, // Keep original if not in our map (shouldn't happen)
                    };
                    let new_target = match node_id_map.get(&old_target) {
                        Some(&id) => id,
                        None => old_target,
                    };

                    // Add the edge with remapped endpoints
                    self.merged_pdg
                        .add_edge(new_source, new_target, edge.clone());
                }
            }
        }

        // Record external reference
        self.external_refs
            .insert(project_id.to_string(), added_nodes);

        Ok(())
    }

    /// Add external node reference (lazy load)
    pub fn add_external_ref(&mut self, node_id: NodeId, project_id: &str) {
        self.external_refs
            .entry(project_id.to_string())
            .or_default()
            .push(node_id);
        self.node_origins.insert(node_id, project_id.to_string());
    }

    /// Check if node is external
    pub fn is_external_node(&self, node_id: &NodeId) -> bool {
        self.node_origins
            .get(node_id)
            .map(|origin| origin != &self.root_project_id)
            .unwrap_or(false)
    }

    /// Get origin project for a node
    pub fn get_node_origin(&self, node_id: &NodeId) -> Option<&String> {
        self.node_origins.get(node_id)
    }

    /// Get all external projects referenced
    pub fn get_referenced_projects(&self) -> Vec<String> {
        self.external_refs
            .keys()
            .filter(|project_id| **project_id != self.root_project_id)
            .cloned()
            .collect()
    }

    /// Filter to only local nodes
    pub fn local_nodes(&self) -> Vec<NodeId> {
        self.merged_pdg
            .node_indices()
            .filter(|id| !self.is_external_node(id))
            .collect()
    }

    /// Filter to only external nodes
    pub fn external_nodes(&self) -> Vec<NodeId> {
        self.merged_pdg
            .node_indices()
            .filter(|id| self.is_external_node(id))
            .collect()
    }

    /// Get total node count
    pub fn node_count(&self) -> usize {
        self.merged_pdg.node_count()
    }

    /// Get total edge count
    pub fn edge_count(&self) -> usize {
        self.merged_pdg.edge_count()
    }

    /// Get reference to the merged PDG
    pub fn pdg(&self) -> &ProgramDependenceGraph {
        &self.merged_pdg
    }

    /// Get mutable reference to the merged PDG
    pub fn pdg_mut(&mut self) -> &mut ProgramDependenceGraph {
        &mut self.merged_pdg
    }
}

/// External node reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalNodeRef {
    /// Serialized node index
    pub node_id: u32,
    /// Origin project ID
    pub project_id: String,
    /// Symbol name of the node
    pub symbol_name: String,
    /// Type of the node
    pub node_type: NodeType,
}

/// Serializable cross-project PDG for storage
///
/// Note: This is a simplified serialization format that captures
/// the essential information for persisting cross-project relationships.
/// The full PDG serialization is handled separately by the pdg module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableCrossProjectPDG {
    /// Root project ID
    pub root_project_id: String,
    /// List of node origins as (node_index, project_id)
    pub node_origins: Vec<(u32, String)>,
    /// Map of external project IDs to their node references
    pub external_refs: HashMap<String, Vec<ExternalNodeRef>>,
}

impl CrossProjectPDG {
    /// Convert to serializable format for storage
    pub fn to_serializable(&self) -> SerializableCrossProjectPDG {
        // Convert node origins to serializable format
        let node_origins: Vec<(u32, String)> = self
            .node_origins
            .iter()
            .map(|(id, project)| (id.index() as u32, project.clone()))
            .collect();

        // Convert external refs to serializable format
        let external_refs_serializable: HashMap<String, Vec<ExternalNodeRef>> = self
            .external_refs
            .iter()
            .map(|(project, nodes)| {
                let refs: Vec<ExternalNodeRef> = nodes
                    .iter()
                    .filter_map(|node_id| {
                        self.merged_pdg
                            .get_node(*node_id)
                            .map(|node| ExternalNodeRef {
                                node_id: node_id.index() as u32,
                                project_id: project.clone(),
                                symbol_name: node.name.clone(),
                                node_type: node.node_type.clone(),
                            })
                    })
                    .collect();
                (project.clone(), refs)
            })
            .collect();

        SerializableCrossProjectPDG {
            root_project_id: self.root_project_id.clone(),
            node_origins,
            external_refs: external_refs_serializable,
        }
    }

    /// Load from storage with existing PDG
    ///
    /// This reconstructs the cross-project metadata from the serializable format
    /// and applies it to an existing PDG.
    pub fn from_serializable_with_pdg(
        serializable: SerializableCrossProjectPDG,
        pdg: ProgramDependenceGraph,
    ) -> Self {
        // Convert node origins back from serializable format
        let mut node_origins = HashMap::new();
        for (node_index, project_id) in serializable.node_origins {
            node_origins.insert(NodeId::new(node_index as usize), project_id);
        }

        // Note: external_refs would need to be reconstructed, but for now
        // we leave it empty since the node IDs in the PDG may have changed

        Self {
            root_project_id: serializable.root_project_id,
            merged_pdg: pdg,
            node_origins,
            external_refs: HashMap::new(),
            max_depth: 3,
        }
    }
}

/// Error for PDG merging
#[derive(Debug, Error)]
pub enum MergeError {
    /// Conflict between local and external node IDs
    #[error("Node ID conflict: {0:?} exists in both local and external")]
    NodeConflict(NodeId),

    /// Conflict between local and external edge IDs
    #[error("Edge ID conflict: {0:?} exists in both local and external")]
    EdgeConflict(EdgeId),

    /// Merging exceeded the maximum allowed depth
    #[error("Max depth exceeded: {0}")]
    MaxDepthExceeded(usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdg::Node;

    fn create_test_node(name: &str) -> Node {
        Node {
            id: name.to_string(),
            node_type: NodeType::Function,
            name: name.to_string(),
            file_path: format!("src/{}.rs", name),
            byte_range: (0, 100),
            complexity: 5,
            language: "rust".to_string(),
            embedding: None,
        }
    }

    fn create_test_pdg(nodes: Vec<&str>) -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();
        for name in nodes {
            let node = create_test_node(name);
            pdg.add_node(node);
        }
        pdg
    }

    #[test]
    fn test_cross_project_pdg_creation() {
        let root_pdg = create_test_pdg(vec!["func_a", "func_b"]);
        let cross_pdg = CrossProjectPDG::new("root_project".to_string(), root_pdg);

        assert_eq!(cross_pdg.root_project_id, "root_project");
        assert_eq!(cross_pdg.node_count(), 2);
        assert_eq!(cross_pdg.local_nodes().len(), 2);
        assert_eq!(cross_pdg.external_nodes().len(), 0);
    }

    #[test]
    fn test_merge_external_pdg() {
        let root_pdg = create_test_pdg(vec!["root_func"]);
        let mut cross_pdg = CrossProjectPDG::new("root_project".to_string(), root_pdg);

        let ext_pdg = create_test_pdg(vec!["ext_func_a", "ext_func_b"]);
        cross_pdg
            .merge_external_pdg("external_project", &ext_pdg)
            .unwrap();

        assert_eq!(cross_pdg.node_count(), 3);
        assert_eq!(cross_pdg.local_nodes().len(), 1);
        assert_eq!(cross_pdg.external_nodes().len(), 2);

        // Check that external nodes are correctly identified
        for node_id in cross_pdg.external_nodes() {
            assert!(cross_pdg.is_external_node(&node_id));
            assert_eq!(
                cross_pdg.get_node_origin(&node_id),
                Some(&"external_project".to_string())
            );
        }
    }

    #[test]
    fn test_max_depth_exceeded() {
        let root_pdg = create_test_pdg(vec!["root_func"]);
        let mut cross_pdg =
            CrossProjectPDG::with_max_depth("root_project".to_string(), root_pdg, 1);

        let ext_pdg = create_test_pdg(vec!["ext_func"]);

        // First merge should succeed
        cross_pdg
            .merge_external_pdg("ext_project_1", &ext_pdg)
            .unwrap();

        // Second merge should fail due to max depth
        let result = cross_pdg.merge_external_pdg("ext_project_2", &ext_pdg);
        assert!(matches!(result, Err(MergeError::MaxDepthExceeded(1))));
    }

    #[test]
    fn test_add_external_ref() {
        let root_pdg = create_test_pdg(vec!["root_func"]);
        let mut cross_pdg = CrossProjectPDG::new("root_project".to_string(), root_pdg);

        // Add a fake external node reference
        let fake_node_id = NodeId::new(100);
        cross_pdg.add_external_ref(fake_node_id, "external_project");

        assert!(cross_pdg.is_external_node(&fake_node_id));
        assert_eq!(
            cross_pdg.get_node_origin(&fake_node_id),
            Some(&"external_project".to_string())
        );
    }

    #[test]
    fn test_get_referenced_projects() {
        let root_pdg = create_test_pdg(vec!["root_func"]);
        let mut cross_pdg = CrossProjectPDG::new("root_project".to_string(), root_pdg);

        let ext_pdg_1 = create_test_pdg(vec!["ext_func_a"]);
        let ext_pdg_2 = create_test_pdg(vec!["ext_func_b"]);

        cross_pdg
            .merge_external_pdg("ext_project_1", &ext_pdg_1)
            .unwrap();
        cross_pdg
            .merge_external_pdg("ext_project_2", &ext_pdg_2)
            .unwrap();

        let referenced = cross_pdg.get_referenced_projects();
        assert_eq!(referenced.len(), 2);
        assert!(referenced.contains(&"ext_project_1".to_string()));
        assert!(referenced.contains(&"ext_project_2".to_string()));
    }

    #[test]
    fn test_merge_with_edges() {
        // Create root PDG with two connected nodes
        let mut root_pdg = ProgramDependenceGraph::new();
        let node_a = create_test_node("root_func_a");
        let node_b = create_test_node("root_func_b");
        let id_a = root_pdg.add_node(node_a);
        let id_b = root_pdg.add_node(node_b);

        // Add an edge between them
        let edge = crate::pdg::Edge {
            edge_type: crate::pdg::EdgeType::Call,
            metadata: crate::pdg::EdgeMetadata {
                call_count: Some(1),
                variable_name: None,
            },
        };
        root_pdg.add_edge(id_a, id_b, edge);

        let mut cross_pdg = CrossProjectPDG::new("root_project".to_string(), root_pdg);

        // Create external PDG with connected nodes
        let mut ext_pdg = ProgramDependenceGraph::new();
        let node_x = create_test_node("ext_func_x");
        let node_y = create_test_node("ext_func_y");
        let id_x = ext_pdg.add_node(node_x);
        let id_y = ext_pdg.add_node(node_y);

        // Add edge in external PDG
        let ext_edge = crate::pdg::Edge {
            edge_type: crate::pdg::EdgeType::DataDependency,
            metadata: crate::pdg::EdgeMetadata {
                call_count: None,
                variable_name: Some("data".to_string()),
            },
        };
        ext_pdg.add_edge(id_x, id_y, ext_edge);

        // Merge the external PDG
        cross_pdg
            .merge_external_pdg("external_project", &ext_pdg)
            .unwrap();

        // Verify all nodes and edges were merged
        assert_eq!(cross_pdg.node_count(), 4);
        assert_eq!(cross_pdg.edge_count(), 2); // Both local and external edges
    }

    #[test]
    fn test_serialization() {
        let root_pdg = create_test_pdg(vec!["root_func"]);
        let mut cross_pdg = CrossProjectPDG::new("root_project".to_string(), root_pdg);

        let ext_pdg = create_test_pdg(vec!["ext_func"]);
        cross_pdg
            .merge_external_pdg("external_project", &ext_pdg)
            .unwrap();

        // Serialize
        let serializable = cross_pdg.to_serializable();
        assert_eq!(serializable.root_project_id, "root_project");
        assert_eq!(serializable.node_origins.len(), 1); // Only external nodes are tracked

        // Verify project origin is the external project
        assert_eq!(serializable.node_origins[0].1, "external_project");
    }
}
