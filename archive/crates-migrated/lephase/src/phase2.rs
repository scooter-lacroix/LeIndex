use crate::context::PhaseExecutionContext;
use legraphe::pdg::{EdgeType, NodeType};
use serde::{Deserialize, Serialize};

/// Dependency-map output.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase2Summary {
    /// Number of import edges targeting internal nodes.
    pub internal_import_edges: usize,
    /// Number of import edges targeting synthetic external module nodes.
    pub external_import_edges: usize,
    /// Number of unique unresolved external modules.
    pub unresolved_modules: usize,
    /// Confidence counters `(exact, heuristic, external)`.
    pub confidence_bands: (usize, usize, usize),
}

/// Run phase 2 dependency-map analysis.
pub fn run(context: &PhaseExecutionContext) -> Phase2Summary {
    let mut internal = 0usize;
    let mut external = 0usize;
    let mut unresolved_modules = std::collections::HashSet::new();
    let mut exact = 0usize;
    let mut heuristic = 0usize;

    for edge_idx in context.pdg.edge_indices() {
        let Some(edge) = context.pdg.get_edge(edge_idx) else {
            continue;
        };
        if edge.edge_type != EdgeType::Import {
            continue;
        }

        let Some((_, to)) = context.pdg.edge_endpoints(edge_idx) else {
            continue;
        };
        let Some(target) = context.pdg.get_node(to) else {
            continue;
        };

        if target.node_type == NodeType::Module && target.language == "external" {
            external += 1;
            unresolved_modules.insert(target.name.clone());
        } else {
            internal += 1;
            // heuristic fallback: if this looks like a synthetic local heuristic module marker
            if target.id.contains("__heuristic__") {
                heuristic += 1;
            } else {
                exact += 1;
            }
        }
    }

    Phase2Summary {
        internal_import_edges: internal,
        external_import_edges: external,
        unresolved_modules: unresolved_modules.len(),
        confidence_bands: (exact, heuristic, external),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::PhaseExecutionContext;
    use legraphe::pdg::{Edge, EdgeMetadata, Node};
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn counts_internal_and_external_import_edges() {
        let temp = tempdir().expect("tempdir");
        let storage_path = temp.path().join(".leindex").join("leindex.db");
        std::fs::create_dir_all(storage_path.parent().expect("parent")).expect("mkdir");
        let storage = lestockage::schema::Storage::open(storage_path).expect("storage");

        let mut pdg = legraphe::pdg::ProgramDependenceGraph::new();
        let importer = pdg.add_node(Node {
            id: "src/main.rs:__module__".to_string(),
            node_type: NodeType::Module,
            name: "__module__".to_string(),
            file_path: "src/main.rs".to_string(),
            byte_range: (0, 0),
            complexity: 1,
            language: "rust".to_string(),
            embedding: None,
        });
        let internal_target = pdg.add_node(Node {
            id: "src/lib.rs:pkg::helper".to_string(),
            node_type: NodeType::Function,
            name: "helper".to_string(),
            file_path: "src/lib.rs".to_string(),
            byte_range: (0, 0),
            complexity: 1,
            language: "rust".to_string(),
            embedding: None,
        });
        let external_target = pdg.add_node(Node {
            id: "src/main.rs:__external__:third.party.lib".to_string(),
            node_type: NodeType::Module,
            name: "third.party.lib".to_string(),
            file_path: "src/main.rs".to_string(),
            byte_range: (0, 0),
            complexity: 1,
            language: "external".to_string(),
            embedding: None,
        });

        pdg.add_edge(
            importer,
            internal_target,
            Edge {
                edge_type: EdgeType::Import,
                metadata: EdgeMetadata {
                    call_count: None,
                    variable_name: None,
                },
            },
        );
        pdg.add_edge(
            importer,
            external_target,
            Edge {
                edge_type: EdgeType::Import,
                metadata: EdgeMetadata {
                    call_count: None,
                    variable_name: None,
                },
            },
        );

        let context = PhaseExecutionContext {
            root: temp.path().to_path_buf(),
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
        };

        let summary = run(&context);
        assert_eq!(summary.internal_import_edges, 1);
        assert_eq!(summary.external_import_edges, 1);
        assert_eq!(summary.unresolved_modules, 1);
        assert_eq!(summary.confidence_bands.0, 1);
        assert_eq!(summary.confidence_bands.2, 1);
    }
}
