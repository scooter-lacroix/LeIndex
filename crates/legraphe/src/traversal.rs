// Gravity-based traversal algorithm

use crate::pdg::{NodeId, ProgramDependenceGraph};
use std::collections::BinaryHeap;
use serde::{Deserialize, Serialize};

/// Configuration for gravity traversal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalConfig {
    /// Maximum token budget
    pub max_tokens: usize,

    /// Decay factor for distance
    pub distance_decay: f64,

    /// Weight for semantic score
    pub semantic_weight: f64,

    /// Weight for complexity
    pub complexity_weight: f64,
}

impl Default for TraversalConfig {
    fn default() -> Self {
        Self {
            max_tokens: 2000,
            distance_decay: 2.0,
            semantic_weight: 1.0,
            complexity_weight: 0.5,
        }
    }
}

/// Gravity-based context traversal
///
/// Uses a priority-weighted expansion based on the formula:
/// Relevance(N) = (SemanticScore(N) * Complexity(N)) / (Distance(Entry, N)^2)
pub struct GravityTraversal {
    config: TraversalConfig,
}

impl GravityTraversal {
    /// Create a new gravity traversal with default config
    pub fn new() -> Self {
        Self {
            config: TraversalConfig::default(),
        }
    }

    /// Create with custom config
    pub fn with_config(config: TraversalConfig) -> Self {
        Self { config }
    }

    /// Expand context from entry nodes within token budget
    pub fn expand_context(
        &self,
        pdg: &ProgramDependenceGraph,
        entry_nodes: Vec<NodeId>,
    ) -> Vec<NodeId> {
        let mut pq = BinaryHeap::new();
        let mut visited = std::collections::HashSet::new();
        let mut context = Vec::new();
        let mut current_tokens = 0;

        // Initialize with entry nodes
        for &entry in &entry_nodes {
            if let Some(node) = pdg.get_node(entry) {
                let weight = self.calculate_relevance(node, 0.0, 1.0);
                pq.push(WeightedNode {
                    id: entry,
                    weight,
                    distance: 0,
                });
            }
        }

        // Expand using priority queue
        while let Some(wnode) = pq.pop() {
            if visited.contains(&wnode.id) {
                continue;
            }

            if let Some(node) = pdg.get_node(wnode.id) {
                let estimated_tokens = self.estimate_tokens(node);
                if current_tokens + estimated_tokens > self.config.max_tokens {
                    break;
                }

                visited.insert(wnode.id);
                context.push(wnode.id);
                current_tokens += estimated_tokens;

                // Add neighbors with decayed weight
                for neighbor in self.get_neighbors(pdg, wnode.id) {
                    if !visited.contains(&neighbor) {
                        let new_distance = wnode.distance + 1;
                        if let Some(nnode) = pdg.get_node(neighbor) {
                            let semantic = 1.0; // Would come from embedding
                            let weight = self.calculate_relevance(
                                nnode,
                                new_distance as f64,
                                semantic,
                            );
                            pq.push(WeightedNode {
                                id: neighbor,
                                weight,
                                distance: new_distance,
                            });
                        }
                    }
                }
            }
        }

        context
    }

    /// Calculate relevance score for a node
    fn calculate_relevance(
        &self,
        node: &crate::pdg::Node,
        distance: f64,
        semantic_score: f64,
    ) -> f64 {
        let complexity = node.complexity as f64;
        let distance_factor = distance.powf(self.config.distance_decay);

        (semantic_score * self.config.semantic_weight
            + complexity * self.config.complexity_weight)
            / distance_factor.max(1.0)
    }

    /// Estimate token count for a node
    fn estimate_tokens(&self, node: &crate::pdg::Node) -> usize {
        let range = node.byte_range.1.saturating_sub(node.byte_range.0);
        // Rough estimate: ~4 characters per token. Ensure at least 10 tokens per node.
        (range / 4).max(10)
    }

    /// Get neighboring nodes
    fn get_neighbors(
        &self,
        pdg: &ProgramDependenceGraph,
        node_id: NodeId,
    ) -> Vec<NodeId> {
        pdg.neighbors(node_id)
    }
}

impl Default for GravityTraversal {
    fn default() -> Self {
        Self::new()
    }
}

/// Node with weight for priority queue
#[derive(Debug, Clone)]
struct WeightedNode {
    id: NodeId,
    weight: f64,
    distance: usize,
}

// Implement reverse ordering for max-heap behavior
impl PartialEq for WeightedNode {
    fn eq(&self, other: &Self) -> bool {
        self.weight == other.weight
    }
}

impl Eq for WeightedNode {}

impl PartialOrd for WeightedNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WeightedNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Max-heap behavior: higher weight comes first
        self.weight.partial_cmp(&other.weight).unwrap_or(std::cmp::Ordering::Equal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_traversal_config_default() {
        let config = TraversalConfig::default();
        assert_eq!(config.max_tokens, 2000);
    }

    #[test]
    fn test_gravity_traversal_creation() {
        let traversal = GravityTraversal::new();
        let pdg = ProgramDependenceGraph::new();
        let result = traversal.expand_context(&pdg, vec![]);
        assert_eq!(result.len(), 0);
    }
}
