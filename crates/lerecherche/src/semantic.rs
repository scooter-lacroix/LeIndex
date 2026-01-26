// Semantic search and processing

use crate::search::{SemanticEntry, NodeInfo};
use legraphe::pdg::ProgramDependenceGraph;
use legraphe::traversal::{GravityTraversal, TraversalConfig};
use std::collections::HashMap;

/// Semantic processor for vector-AST synergy
pub struct SemanticProcessor {}

impl SemanticProcessor {
    /// Create a new semantic processor
    pub fn new() -> Self {
        Self {}
    }

    /// Process semantic entry and expand context using PDG
    pub async fn process_entry(
        &self,
        pdg: &ProgramDependenceGraph,
        nodes: &[NodeInfo],
        entry: SemanticEntry,
        token_budget: usize,
    ) -> Result<String, Error> {
        let mut config = TraversalConfig::default();
        config.max_tokens = token_budget;
        
        let traversal = GravityTraversal::with_config(config);
        
        // Map node_id string to graph NodeId
        let entry_node_id = pdg.find_by_symbol(&entry.node_id)
            .ok_or_else(|| Error::ContextExpansionFailed(format!("Entry node {} not found in PDG", entry.node_id)))?;
            
        let expanded_ids = traversal.expand_context(pdg, vec![entry_node_id]);
        
        // Create a lookup for node content
        let content_lookup: HashMap<String, &NodeInfo> = nodes.iter()
            .map(|n| (n.node_id.clone(), n))
            .collect();
            
        let mut formatted_context = String::new();
        
        for id in expanded_ids {
            if let Some(graph_node) = pdg.get_node(id) {
                if let Some(node_info) = content_lookup.get(&graph_node.id) {
                    formatted_context.push_str(&format!("// File: {}\n", node_info.file_path));
                    formatted_context.push_str(&format!("// Symbol: {}\n", node_info.symbol_name));
                    formatted_context.push_str(&node_info.content);
                    formatted_context.push_str("\n\n");
                }
            }
        }
        
        Ok(formatted_context)
    }
}

impl Default for SemanticProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// Semantic processing errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Processing failed: {0}")]
    ProcessingFailed(String),

    #[error("Context expansion failed: {0}")]
    ContextExpansionFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::EntryType;
    use legraphe::pdg::{Node as GraphNode, NodeType};

    #[tokio::test]
    async fn test_semantic_context_expansion() {
        let mut pdg = ProgramDependenceGraph::new();
        let processor = SemanticProcessor::new();
        
        // Add some nodes to PDG
        let n1 = pdg.add_node(GraphNode {
            id: "func1".to_string(),
            node_type: NodeType::Function,
            name: "func1".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (0, 50),
            complexity: 2,
            embedding: None,
        });
        let n2 = pdg.add_node(GraphNode {
            id: "func2".to_string(),
            node_type: NodeType::Function,
            name: "func2".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (60, 100),
            complexity: 3,
            embedding: None,
        });
        
        pdg.add_call_graph_edges(vec![(n1, n2)]);
        
        let nodes = vec![
            NodeInfo {
                node_id: "func1".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func1".to_string(),
                content: "fn func1() { func2(); }".to_string(),
                byte_range: (0, 50),
                embedding: None,
                complexity: 2,
            },
            NodeInfo {
                node_id: "func2".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func2".to_string(),
                content: "fn func2() { println!(\"world\"); }".to_string(),
                byte_range: (60, 100),
                embedding: None,
                complexity: 3,
            },
        ];
        
        let entry = SemanticEntry {
            node_id: "func1".to_string(),
            relevance: 1.0,
            entry_type: EntryType::Function,
        };
        
        let context = processor.process_entry(&pdg, &nodes, entry, 1000).await.unwrap();
        
        assert!(context.contains("func1"));
        assert!(context.contains("func2"));
    }
}
