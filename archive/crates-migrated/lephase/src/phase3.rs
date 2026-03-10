use crate::context::PhaseExecutionContext;
use crate::options::PhaseOptions;
use serde::{Deserialize, Serialize};

/// Logic-flow output.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase3Summary {
    /// Deterministic entry points selected for traversal.
    pub entry_points: Vec<String>,
    /// Unique impacted nodes across all entry points.
    pub impacted_nodes: usize,
    /// Focus files sorted by impact score.
    pub focus_files: Vec<String>,
}

/// Run phase 3 logic-flow analysis.
pub fn run(context: &PhaseExecutionContext, options: &PhaseOptions) -> Phase3Summary {
    let mut ranked_nodes = Vec::new();
    for node_idx in context.pdg.node_indices() {
        if let Some(node) = context.pdg.get_node(node_idx) {
            ranked_nodes.push((node_idx, node.complexity, node.id.clone()));
        }
    }

    ranked_nodes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    let selected = ranked_nodes
        .into_iter()
        .take(options.top_n.max(1))
        .collect::<Vec<_>>();

    let mut impacted = std::collections::HashSet::new();
    let mut file_impact: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut entry_points = Vec::new();

    for (node_idx, _, node_id) in &selected {
        entry_points.push(node_id.clone());
        for impacted_node in context.pdg.get_forward_impact(*node_idx) {
            impacted.insert(impacted_node);
            if let Some(node) = context.pdg.get_node(impacted_node) {
                *file_impact.entry(node.file_path.clone()).or_insert(0) += 1;
            }
        }
    }

    let mut focus_files = file_impact.into_iter().collect::<Vec<_>>();
    focus_files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Phase3Summary {
        entry_points,
        impacted_nodes: impacted.len(),
        focus_files: focus_files
            .into_iter()
            .take(options.max_focus_files)
            .map(|(file, _)| file)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use legraphe::pdg::{Edge, EdgeMetadata, EdgeType, Node, NodeType, ProgramDependenceGraph};
    use std::collections::HashMap;

    fn make_context() -> PhaseExecutionContext {
        let unique = format!(
            "lephase-phase3-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(root.join(".leindex")).expect("mkdir");
        let db_path = root.join(".leindex").join("leindex.db");
        let storage = lestockage::schema::Storage::open(db_path).expect("storage");

        let mut pdg = ProgramDependenceGraph::new();
        let a = pdg.add_node(Node {
            id: "src/a.rs:a".to_string(),
            node_type: NodeType::Function,
            name: "a".to_string(),
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 1),
            complexity: 9,
            language: "rust".to_string(),
            embedding: None,
        });
        let b = pdg.add_node(Node {
            id: "src/b.rs:b".to_string(),
            node_type: NodeType::Function,
            name: "b".to_string(),
            file_path: "src/b.rs".to_string(),
            byte_range: (0, 1),
            complexity: 2,
            language: "rust".to_string(),
            embedding: None,
        });
        pdg.add_edge(
            a,
            b,
            Edge {
                edge_type: EdgeType::Call,
                metadata: EdgeMetadata {
                    call_count: None,
                    variable_name: None,
                },
            },
        );

        PhaseExecutionContext {
            root: root.clone(),
            project_id: "test".to_string(),
            storage,
            file_inventory: Vec::new(),
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg,
            docs_summary: None,
            generation_hash: "gen".to_string(),
        }
    }

    #[test]
    fn phase3_selects_entry_points_and_focus_files() {
        let context = make_context();
        let summary = run(
            &context,
            &PhaseOptions {
                root: context.root.clone(),
                top_n: 1,
                max_focus_files: 1,
                ..PhaseOptions::default()
            },
        );

        assert_eq!(summary.entry_points.len(), 1);
        assert_eq!(summary.focus_files.len(), 1);
        assert!(summary.impacted_nodes >= 1);
    }

    #[test]
    fn phase3_enforces_minimum_top_n_of_one() {
        let context = make_context();
        let summary = run(
            &context,
            &PhaseOptions {
                root: context.root.clone(),
                top_n: 0,
                max_focus_files: 10,
                ..PhaseOptions::default()
            },
        );

        assert_eq!(summary.entry_points.len(), 1);
    }
}
