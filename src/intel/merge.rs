//! Merge compact SCIP facts into the resident PDG.

use crate::graph::pdg::{Edge, EdgeMetadata, EdgeType, NodeId, ProgramDependenceGraph};
use crate::intel::scip_ingest::{CompactScipFacts, DefinitionFact};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Accounting returned by one precision merge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrecisionReport {
    /// Number of SCIP definitions inspected.
    pub definitions_seen: usize,
    /// Number of definitions matched to canonical Tier-0 nodes.
    pub definitions_matched: usize,
    /// Definitions with no canonical Tier-0 match.
    pub definitions_unmatched: usize,
    /// Number of SCIP relationships inspected.
    pub relationships_seen: usize,
    /// Existing heuristic relationships upgraded to confidence 1.0.
    pub relationships_upgraded: usize,
    /// New precise relationships added to the PDG.
    pub relationships_added: usize,
    /// Relationships whose endpoint symbols could not be matched.
    pub relationships_unmatched: usize,
}

/// Merge compact SCIP facts into `pdg` without creating unmatched nodes.
pub fn merge_facts(pdg: &mut ProgramDependenceGraph, facts: &CompactScipFacts) -> PrecisionReport {
    let mut report = PrecisionReport {
        definitions_seen: facts.definitions.len(),
        relationships_seen: facts.relationships.len(),
        ..PrecisionReport::default()
    };

    // `files` identifies the complete file snapshot represented by the facts.
    // Without it, a missing definition cannot be distinguished from an omitted
    // fact, so leave marker lifecycle untouched for graph/storage to reconcile.
    clear_precision_markers_for_files(pdg, &facts.files);

    let mut symbol_nodes: HashMap<String, NodeId> = HashMap::new();

    for definition in &facts.definitions {
        if let Some(node_id) = match_definition(pdg, definition) {
            symbol_nodes.insert(definition.symbol.clone(), node_id);
            if let Some(node) = pdg.get_node(node_id) {
                pdg.mark_precision_symbol(node.id.clone());
            }
            report.definitions_matched += 1;
        } else {
            report.definitions_unmatched += 1;
        }
    }

    for relationship in &facts.relationships {
        let Some(&source) = symbol_nodes.get(&relationship.source) else {
            report.relationships_unmatched += 1;
            continue;
        };
        let Some(&target) = symbol_nodes.get(&relationship.target) else {
            report.relationships_unmatched += 1;
            continue;
        };
        let Some(edge_type) = relationship_edge_type(&relationship.kind) else {
            report.relationships_unmatched += 1;
            continue;
        };
        let mut upgraded = false;
        let edge_ids: Vec<_> = pdg
            .edge_indices()
            .filter(|&edge_id| {
                pdg.edge_endpoints(edge_id) == Some((source, target))
                    && pdg
                        .get_edge(edge_id)
                        .is_some_and(|edge| edge.edge_type == edge_type)
            })
            .collect();
        for edge_id in edge_ids {
            if let Some(edge) = pdg.get_edge_mut(edge_id) {
                edge.metadata.confidence = Some(1.0);
                upgraded = true;
            }
        }
        if upgraded {
            report.relationships_upgraded += 1;
        } else {
            pdg.add_edge(
                source,
                target,
                Edge {
                    edge_type,
                    metadata: EdgeMetadata::with_confidence(1.0),
                },
            );
            report.relationships_added += 1;
        }
    }

    report
}

fn relationship_edge_type(kind: &str) -> Option<EdgeType> {
    match kind {
        "call" => Some(EdgeType::Call),
        "inheritance" | "implements" | "extends" => Some(EdgeType::Inheritance),
        "type_of" | "type-definition" => Some(EdgeType::TypeOf),
        _ => None,
    }
}

fn clear_precision_markers_for_files(pdg: &mut ProgramDependenceGraph, files: &[String]) {
    if files.is_empty() || pdg.precision_symbols.is_empty() {
        return;
    }

    let affected_node_ids: HashSet<String> = pdg
        .node_indices()
        .filter_map(|node_id| {
            let node = pdg.get_node(node_id)?;
            files
                .iter()
                .any(|file_path| same_path(&node.file_path, file_path))
                .then(|| node.id.clone())
        })
        .collect();
    pdg.precision_symbols
        .retain(|node_id| !affected_node_ids.contains(node_id));
}

fn match_definition(pdg: &ProgramDependenceGraph, definition: &DefinitionFact) -> Option<NodeId> {
    let candidates = pdg.nodes_in_file(&definition.file_path);
    let candidates = if candidates.is_empty() {
        pdg.node_indices()
            .filter(|&node_id| {
                pdg.get_node(node_id)
                    .is_some_and(|node| same_path(&node.file_path, &definition.file_path))
            })
            .collect()
    } else {
        candidates
    };

    // A source range is the authoritative identity when it is available. Do
    // not make a range match compete with the weaker name fallback: generated
    // graphs can contain duplicate names (and, defensively, duplicate ranges).
    if let Some(range) = definition.byte_range {
        if let Some(node_id) = candidates.iter().copied().find(|&node_id| {
            pdg.get_node(node_id)
                .is_some_and(|node| node.byte_range == range)
        }) {
            return Some(node_id);
        }
    }

    // SCIP display names are often only the short name. A qualified-name
    // fallback is therefore useful, but it must never silently select the
    // first overloaded candidate. Keep the set of matching node IDs and only
    // accept the fallback when exactly one candidate remains.
    let name_matches: HashSet<NodeId> = candidates
        .into_iter()
        .filter(|&node_id| {
            pdg.get_node(node_id).is_some_and(|node| {
                matches_name_or_qualified_name(node, &definition.qualified_name)
            })
        })
        .collect();
    (name_matches.len() == 1).then(|| name_matches.into_iter().next().unwrap())
}

fn matches_name_or_qualified_name(node: &crate::graph::pdg::Node, qualified_name: &str) -> bool {
    // A short display name is a valid fallback only when the SCIP value is
    // short too. A qualified SCIP name must match the node's qualified name,
    // otherwise two namespaces containing the same short name are ambiguous.
    if node.name == qualified_name {
        return true;
    }
    if qualified_name.is_empty() {
        return false;
    }

    // Duplicate qualified names are suffixed with `@start..end` by PDG
    // extraction. Remove that suffix before comparing the canonical qname.
    let id_without_range = node
        .id
        .split_once('@')
        .map_or(node.id.as_str(), |(id, _)| id);
    if id_without_range.ends_with(&format!(":{qualified_name}")) {
        return true;
    }

    // A qname's final component is useful only for a short SCIP display name;
    // this is deliberately not used when `qualified_name` contains a scope.
    let is_short_name = !qualified_name.contains("::") && !qualified_name.contains('.');
    is_short_name
        && id_without_range
            .rsplit_once(':')
            .is_some_and(|(_, short_name)| short_name == qualified_name)
}

fn same_path(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let left = Path::new(left);
    let right = Path::new(right);
    left.ends_with(right) || right.ends_with(left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::pdg::{Node, NodeType};
    use crate::intel::scip_ingest::RelationshipFact;
    use std::sync::Arc;

    fn fixture() -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "src/lib.rs:Thing".to_string(),
            node_type: NodeType::Class,
            name: "Thing".to_string(),
            file_path: Arc::from("src/lib.rs"),
            byte_range: (0, 5),
            complexity: 1,
            language: "rust".to_string(),
        });
        pdg.add_node(Node {
            id: "src/lib.rs:value".to_string(),
            node_type: NodeType::Variable,
            name: "value".to_string(),
            file_path: Arc::from("src/lib.rs"),
            byte_range: (6, 11),
            complexity: 0,
            language: "rust".to_string(),
        });
        pdg
    }

    #[test]
    fn test_merge_marks_definition_and_adds_type_edge() {
        let mut pdg = fixture();
        let facts = CompactScipFacts {
            definitions: vec![
                DefinitionFact {
                    symbol: "thing".to_string(),
                    file_path: "src/lib.rs".to_string(),
                    byte_range: Some((0, 5)),
                    qualified_name: "Thing".to_string(),
                    signature: None,
                },
                DefinitionFact {
                    symbol: "value".to_string(),
                    file_path: "src/lib.rs".to_string(),
                    byte_range: Some((6, 11)),
                    qualified_name: "value".to_string(),
                    signature: None,
                },
            ],
            relationships: vec![RelationshipFact {
                source: "thing".to_string(),
                target: "value".to_string(),
                kind: "type_of".to_string(),
            }],
            ..CompactScipFacts::default()
        };
        let report = merge_facts(&mut pdg, &facts);
        assert_eq!(report.definitions_matched, 2);
        assert_eq!(report.relationships_added, 1);
        assert_eq!(pdg.edge_count(), 1);
        assert!(pdg.is_precision_symbol("src/lib.rs:Thing"));
    }

    #[test]
    fn test_merge_upgrades_existing_edge_without_duplicate() {
        let mut pdg = fixture();
        let source = pdg.find_by_id("src/lib.rs:Thing").unwrap();
        let target = pdg.find_by_id("src/lib.rs:value").unwrap();
        pdg.add_edge(
            source,
            target,
            Edge {
                edge_type: EdgeType::Call,
                metadata: EdgeMetadata::with_confidence(0.4),
            },
        );
        let facts = CompactScipFacts {
            definitions: vec![
                DefinitionFact {
                    symbol: "thing".to_string(),
                    file_path: "src/lib.rs".to_string(),
                    byte_range: Some((0, 5)),
                    qualified_name: "Thing".to_string(),
                    signature: None,
                },
                DefinitionFact {
                    symbol: "value".to_string(),
                    file_path: "src/lib.rs".to_string(),
                    byte_range: Some((6, 11)),
                    qualified_name: "value".to_string(),
                    signature: None,
                },
            ],
            relationships: vec![RelationshipFact {
                source: "thing".to_string(),
                target: "value".to_string(),
                kind: "call".to_string(),
            }],
            ..CompactScipFacts::default()
        };
        let report = merge_facts(&mut pdg, &facts);
        assert_eq!(report.relationships_upgraded, 1);
        assert_eq!(pdg.edge_count(), 1);
        let edge_id = pdg.edge_indices().next().unwrap();
        assert_eq!(
            pdg.get_edge(edge_id).unwrap().metadata.confidence,
            Some(1.0)
        );
    }

    #[test]
    fn test_merge_leaves_ambiguous_name_fallback_unmatched() {
        let mut pdg = ProgramDependenceGraph::new();
        for (id, byte_range) in [
            ("src/lib.rs:first:overloaded", (0, 10)),
            ("src/lib.rs:second:overloaded", (20, 30)),
        ] {
            pdg.add_node(Node {
                id: id.to_string(),
                node_type: NodeType::Function,
                name: "overloaded".to_string(),
                file_path: Arc::from("src/lib.rs"),
                byte_range,
                complexity: 1,
                language: "rust".to_string(),
            });
        }

        let facts = CompactScipFacts {
            definitions: vec![DefinitionFact {
                symbol: "overloaded".to_string(),
                file_path: "src/lib.rs".to_string(),
                byte_range: None,
                qualified_name: "overloaded".to_string(),
                signature: None,
            }],
            ..CompactScipFacts::default()
        };
        let report = merge_facts(&mut pdg, &facts);

        assert_eq!(report.definitions_matched, 0);
        assert_eq!(report.definitions_unmatched, 1);
        assert!(!pdg.is_precision_symbol("src/lib.rs:first:overloaded"));
        assert!(!pdg.is_precision_symbol("src/lib.rs:second:overloaded"));
    }

    #[test]
    fn test_merge_range_match_wins_for_ambiguous_name() {
        let mut pdg = ProgramDependenceGraph::new();
        for (id, byte_range) in [
            ("src/lib.rs:first:overloaded", (0, 10)),
            ("src/lib.rs:second:overloaded", (20, 30)),
        ] {
            pdg.add_node(Node {
                id: id.to_string(),
                node_type: NodeType::Function,
                name: "overloaded".to_string(),
                file_path: Arc::from("src/lib.rs"),
                byte_range,
                complexity: 1,
                language: "rust".to_string(),
            });
        }

        let facts = CompactScipFacts {
            definitions: vec![DefinitionFact {
                symbol: "overloaded".to_string(),
                file_path: "src/lib.rs".to_string(),
                byte_range: Some((20, 30)),
                qualified_name: "overloaded".to_string(),
                signature: None,
            }],
            ..CompactScipFacts::default()
        };
        let report = merge_facts(&mut pdg, &facts);

        assert_eq!(report.definitions_matched, 1);
        assert!(pdg.is_precision_symbol("src/lib.rs:second:overloaded"));
        assert!(!pdg.is_precision_symbol("src/lib.rs:first:overloaded"));
    }

    #[test]
    fn test_merge_qualified_name_fallback_requires_exact_scope() {
        let mut pdg = ProgramDependenceGraph::new();
        for (id, name, byte_range) in [
            ("src/lib.rs:alpha::run", "run", (0, 10)),
            ("src/lib.rs:beta::run", "run", (20, 30)),
        ] {
            pdg.add_node(Node {
                id: id.to_string(),
                node_type: NodeType::Function,
                name: name.to_string(),
                file_path: Arc::from("src/lib.rs"),
                byte_range,
                complexity: 1,
                language: "rust".to_string(),
            });
        }

        let facts = CompactScipFacts {
            definitions: vec![DefinitionFact {
                symbol: "qualified-run".to_string(),
                file_path: "src/lib.rs".to_string(),
                byte_range: None,
                qualified_name: "beta::run".to_string(),
                signature: None,
            }],
            ..CompactScipFacts::default()
        };
        let report = merge_facts(&mut pdg, &facts);

        assert_eq!(report.definitions_matched, 1);
        assert!(pdg.is_precision_symbol("src/lib.rs:beta::run"));
        assert!(!pdg.is_precision_symbol("src/lib.rs:alpha::run"));
    }

    #[test]
    fn test_merge_rerun_clears_markers_only_for_identified_files() {
        let mut pdg = fixture();
        let other_file_node = pdg.add_node(Node {
            id: "src/other.rs:Other".to_string(),
            node_type: NodeType::Class,
            name: "Other".to_string(),
            file_path: Arc::from("src/other.rs"),
            byte_range: (0, 5),
            complexity: 1,
            language: "rust".to_string(),
        });
        pdg.mark_precision_symbol("src/lib.rs:Thing");
        pdg.mark_precision_symbol("src/other.rs:Other");
        pdg.mark_precision_symbol("src/lib.rs:removed");

        let facts = CompactScipFacts {
            files: vec!["src/lib.rs".to_string()],
            ..CompactScipFacts::default()
        };
        merge_facts(&mut pdg, &facts);

        assert!(!pdg.is_precision_symbol("src/lib.rs:Thing"));
        assert!(pdg.is_precision_symbol("src/lib.rs:removed"));
        assert!(pdg.is_precision_symbol(&pdg.get_node(other_file_node).unwrap().id));
    }
}
