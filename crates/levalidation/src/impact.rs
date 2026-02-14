//! Impact analysis for edit changes

use crate::edit_change::EditChange;
use crate::ValidationError;
use legraphe::{ProgramDependenceGraph};
use legraphe::pdg::{NodeType, NodeId, EdgeType, EdgeMetadata};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Location in source code
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// Line number (1-indexed)
    pub line: usize,
    /// Column number (1-indexed, in bytes)
    pub column: usize,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

impl Location {
    /// Create a new location
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// Risk level of an impact
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    /// Low risk - local changes only
    Low = 0,
    /// Medium risk - affects same module
    Medium = 1,
    /// High risk - affects multiple modules
    High = 2,
    /// Critical risk - affects public API
    Critical = 3,
}

impl RiskLevel {
    /// Get a description of this risk level
    pub fn description(&self) -> &str {
        match self {
            Self::Low => "Local changes only",
            Self::Medium => "Affects same module",
            Self::High => "Affects multiple modules",
            Self::Critical => "Affects public API",
        }
    }

    /// Get the color code for terminal output
    pub fn color_code(&self) -> &str {
        match self {
            Self::Low => "\x1b[32m",      // Green
            Self::Medium => "\x1b[33m",    // Yellow
            Self::High => "\x1b[31m",      // Red
            Self::Critical => "\x1b[35m",  // Magenta
        }
    }
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// Impact analysis report
#[derive(Debug, Clone)]
pub struct ImpactReport {
    /// Risk level of the change
    pub risk_level: RiskLevel,
    /// Number of affected nodes
    pub affected_nodes: usize,
    /// Files affected by the change
    pub affected_files: Vec<PathBuf>,
    /// Public APIs affected
    pub affected_apis: Vec<String>,
    /// Detailed description
    pub description: String,
}

impl ImpactReport {
    /// Create a new impact report
    pub fn new(
        risk_level: RiskLevel,
        affected_nodes: usize,
        affected_files: Vec<PathBuf>,
        affected_apis: Vec<String>,
    ) -> Self {
        let description = format!(
            "Risk: {}, Affected nodes: {}, Files: {}, APIs: {}",
            risk_level,
            affected_nodes,
            affected_files.len(),
            affected_apis.len()
        );

        Self {
            risk_level,
            affected_nodes,
            affected_files,
            affected_apis,
            description,
        }
    }

    /// Create a minimal (low risk) impact report
    pub fn minimal() -> Self {
        Self::new(RiskLevel::Low, 0, vec![], vec![])
    }
}

/// Impact analyzer using PDG
#[derive(Clone)]
pub struct ImpactAnalyzer {
    /// PDG for impact analysis
    pdg: Arc<ProgramDependenceGraph>,
}

impl ImpactAnalyzer {
    /// Create a new impact analyzer
    pub fn new(pdg: Arc<ProgramDependenceGraph>) -> Self {
        Self { pdg }
    }

    /// Analyze impact for edit changes
    ///
    /// # Arguments
    /// * `changes` - Edit changes to analyze
    ///
    /// # Returns
    /// Impact report with analysis results
    pub fn analyze_impact(&self, changes: &[EditChange]) -> Result<ImpactReport, ValidationError> {
        if changes.is_empty() {
            return Ok(ImpactReport::minimal());
        }

        let mut affected_files = HashSet::new();
        let mut affected_apis = HashSet::new();
        let mut total_affected_nodes = 0;

        for change in changes {
            // Find nodes in the changed file
            let file_path = change.file_path.to_string_lossy().to_string();
            let nodes_in_file = self.pdg.nodes_in_file(&file_path);

            // For each node, calculate forward and backward impact
            let mut affected = HashSet::new();

            for node_id in &nodes_in_file {
                // Forward impact (nodes that depend on this)
                let forward = self.pdg.get_forward_impact(*node_id);
                affected.extend(forward);

                // Backward impact (nodes this depends on)
                let backward = self.pdg.get_backward_impact(*node_id);
                affected.extend(backward);

                affected.insert(*node_id);
            }

            total_affected_nodes += affected.len();

            // Collect affected files
            for node_id in &affected {
                if let Some(node) = self.pdg.get_node(*node_id) {
                    affected_files.insert(PathBuf::from(&node.file_path));

                    // Check if this is a public API
                    if matches!(
                        node.node_type,
                        NodeType::Function | NodeType::Method | NodeType::Class
                    ) {
                        affected_apis.insert(node.name.clone());
                    }
                }
            }

            // Always include the changed file
            affected_files.insert(change.file_path.clone());
        }

        // Calculate risk level
        let risk_level = self.calculate_risk_level(
            total_affected_nodes,
            &affected_files,
            &affected_apis,
        );

        Ok(ImpactReport::new(
            risk_level,
            total_affected_nodes,
            affected_files.into_iter().collect(),
            affected_apis.into_iter().collect(),
        ))
    }

    /// Calculate risk level based on impact metrics
    fn calculate_risk_level(
        &self,
        affected_nodes: usize,
        affected_files: &HashSet<PathBuf>,
        affected_apis: &HashSet<String>,
    ) -> RiskLevel {
        // If public APIs are affected, it's at least high risk
        if !affected_apis.is_empty() {
            // Check if any of the affected APIs are in a public location
            // (e.g., not in test files, not in private modules)
            for api in affected_apis {
                if self.is_public_api(api) {
                    return RiskLevel::Critical;
                }
            }
            return RiskLevel::High;
        }

        // If multiple files are affected, it's high risk
        if affected_files.len() > 3 {
            return RiskLevel::High;
        }

        // If multiple files are affected, it's medium risk
        if affected_files.len() > 1 {
            return RiskLevel::Medium;
        }

        // If many nodes are affected in the same file, it's medium risk
        if affected_nodes > 10 {
            return RiskLevel::Medium;
        }

        // Default to low risk
        RiskLevel::Low
    }

    /// Check if an API is public (exported)
    fn is_public_api(&self, api_name: &str) -> bool {
        // Check if the API is in a public location
        if let Some(node_id) = self.pdg.find_by_symbol(api_name) {
            if let Some(node) = self.pdg.get_node(node_id) {
                // Not in a test file
                if !node.file_path.contains("test") && !node.file_path.contains("spec") {
                    // Not in a private/internal module
                    return !node.file_path.contains("internal")
                        && !node.file_path.contains("private");
                }
            }
        }
        false
    }

    /// Get the forward impact (nodes reachable from a node)
    pub fn get_forward_impact(&self, node_id: NodeId) -> Vec<NodeId> {
        self.pdg.get_forward_impact(node_id)
    }

    /// Get the backward impact (nodes that can reach a node)
    pub fn get_backward_impact(&self, node_id: NodeId) -> Vec<NodeId> {
        self.pdg.get_backward_impact(node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use legraphe::Node;

    #[test]
    fn test_location_new() {
        let loc = Location::new(10, 5);
        assert_eq!(loc.line, 10);
        assert_eq!(loc.column, 5);
    }

    #[test]
    fn test_location_display() {
        let loc = Location::new(10, 5);
        assert_eq!(loc.to_string(), "10:5");
    }

    #[test]
    fn test_risk_level_ordering() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::Medium < RiskLevel::High);
        assert!(RiskLevel::High < RiskLevel::Critical);
    }

    #[test]
    fn test_risk_level_description() {
        assert_eq!(RiskLevel::Low.description(), "Local changes only");
        assert_eq!(RiskLevel::Medium.description(), "Affects same module");
        assert_eq!(RiskLevel::High.description(), "Affects multiple modules");
        assert_eq!(RiskLevel::Critical.description(), "Affects public API");
    }

    #[test]
    fn test_risk_level_display() {
        assert_eq!(RiskLevel::Low.to_string(), "Local changes only");
        assert_eq!(RiskLevel::Critical.to_string(), "Affects public API");
    }

    #[test]
    fn test_risk_level_equality() {
        assert_eq!(RiskLevel::Low, RiskLevel::Low);
        assert_ne!(RiskLevel::Low, RiskLevel::High);
    }

    #[test]
    fn test_impact_report_new() {
        let report = ImpactReport::new(
            RiskLevel::Low,
            5,
            vec![PathBuf::from("test.py")],
            vec!["my_func".to_string()],
        );

        assert_eq!(report.risk_level, RiskLevel::Low);
        assert_eq!(report.affected_nodes, 5);
        assert_eq!(report.affected_files.len(), 1);
        assert_eq!(report.affected_apis.len(), 1);
    }

    #[test]
    fn test_impact_report_minimal() {
        let report = ImpactReport::minimal();
        assert_eq!(report.risk_level, RiskLevel::Low);
        assert_eq!(report.affected_nodes, 0);
        assert!(report.affected_files.is_empty());
        assert!(report.affected_apis.is_empty());
    }

    #[test]
    fn test_impact_analyzer_new() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let _analyzer = ImpactAnalyzer::new(pdg);
        // Just verify it was created
        assert!(true);
    }

    #[test]
    fn test_analyze_impact_empty_changes() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let analyzer = ImpactAnalyzer::new(pdg);
        let changes: &[EditChange] = &[];
        let report = analyzer.analyze_impact(changes).unwrap();
        assert_eq!(report.risk_level, RiskLevel::Low);
        assert_eq!(report.affected_nodes, 0);
    }

    #[test]
    fn test_analyze_impact_single_change() {
        let mut pdg = ProgramDependenceGraph::new();

        // Add a node
        let node = Node {
            id: "my_func".to_string(),
            node_type: NodeType::Function,
            name: "my_func".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (0, 100),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        };
        pdg.add_node(node);

        let analyzer = ImpactAnalyzer::new(Arc::new(pdg));

        let change = EditChange::new(
            PathBuf::from("test.py"),
            "old content".to_string(),
            "new content".to_string(),
        );

        let report = analyzer.analyze_impact(&[change]).unwrap();
        assert!(!report.affected_files.is_empty());
    }

    #[test]
    fn test_analyze_impact_with_dependencies() {
        let mut pdg = ProgramDependenceGraph::new();

        // Create a dependency chain: func_a -> func_b -> func_c
        let node_a = pdg.add_node(Node {
            id: "func_a".to_string(),
            node_type: NodeType::Function,
            name: "func_a".to_string(),
            file_path: "a.py".to_string(),
            byte_range: (0, 100),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        let node_b = pdg.add_node(Node {
            id: "func_b".to_string(),
            node_type: NodeType::Function,
            name: "func_b".to_string(),
            file_path: "b.py".to_string(),
            byte_range: (0, 100),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        let node_c = pdg.add_node(Node {
            id: "func_c".to_string(),
            node_type: NodeType::Function,
            name: "func_c".to_string(),
            file_path: "c.py".to_string(),
            byte_range: (0, 100),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        // Add edges: a calls b, b calls c
        pdg.add_edge(
            node_a,
            node_b,
            legraphe::Edge {
                edge_type: EdgeType::Call,
                metadata: EdgeMetadata {
                    call_count: None,
                    variable_name: None,
                },
            },
        );
        pdg.add_edge(
            node_b,
            node_c,
            legraphe::Edge {
                edge_type: EdgeType::Call,
                metadata: EdgeMetadata {
                    call_count: None,
                    variable_name: None,
                },
            },
        );

        let analyzer = ImpactAnalyzer::new(Arc::new(pdg));

        // Change to func_c should show impact
        let change = EditChange::new(
            PathBuf::from("c.py"),
            "old".to_string(),
            "new".to_string(),
        );

        let report = analyzer.analyze_impact(&[change]).unwrap();
        // func_c is in c.py, and func_b and func_a depend on it
        assert!(report.affected_nodes >= 1);
    }

    #[test]
    fn test_is_public_api() {
        let mut pdg = ProgramDependenceGraph::new();

        let node = Node {
            id: "public_func".to_string(),
            node_type: NodeType::Function,
            name: "public_func".to_string(),
            file_path: "src/lib.rs".to_string(),
            byte_range: (0, 100),
            complexity: 1,
            language: "rust".to_string(),
            embedding: None,
        };
        pdg.add_node(node);

        let analyzer = ImpactAnalyzer::new(Arc::new(pdg));
        assert!(analyzer.is_public_api("public_func"));
    }

    #[test]
    fn test_is_public_api_test_file() {
        let mut pdg = ProgramDependenceGraph::new();

        let node = Node {
            id: "test_func".to_string(),
            node_type: NodeType::Function,
            name: "test_func".to_string(),
            file_path: "tests/test_lib.rs".to_string(),
            byte_range: (0, 100),
            complexity: 1,
            language: "rust".to_string(),
            embedding: None,
        };
        pdg.add_node(node);

        let analyzer = ImpactAnalyzer::new(Arc::new(pdg));
        assert!(!analyzer.is_public_api("test_func"));
    }

    #[test]
    fn test_is_public_api_internal_module() {
        let mut pdg = ProgramDependenceGraph::new();

        let node = Node {
            id: "internal_func".to_string(),
            node_type: NodeType::Function,
            name: "internal_func".to_string(),
            file_path: "src/internal/mod.rs".to_string(),
            byte_range: (0, 100),
            complexity: 1,
            language: "rust".to_string(),
            embedding: None,
        };
        pdg.add_node(node);

        let analyzer = ImpactAnalyzer::new(Arc::new(pdg));
        assert!(!analyzer.is_public_api("internal_func"));
    }
}
