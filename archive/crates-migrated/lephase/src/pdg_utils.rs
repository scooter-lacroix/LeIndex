use legraphe::{
    extract_pdg_from_signatures,
    pdg::{EdgeType, NodeId, NodeType, ProgramDependenceGraph},
};
use leparse::parallel::ParsingResult;
use std::collections::{HashMap, HashSet};

const MAX_RELINK_CANDIDATES: usize = 1;

/// Build and merge per-file PDGs into a single project PDG.
pub fn merged_pdg_from_results(results: &[ParsingResult]) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();

    for result in results {
        if !result.is_success() {
            continue;
        }

        let file_path = result.file_path.display().to_string();
        let language = result
            .language
            .as_ref()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| "unknown".to_string());

        let source_bytes = std::fs::read(&result.file_path).unwrap_or_default();
        let file_pdg = extract_pdg_from_signatures(
            result.signatures.clone(),
            &source_bytes,
            &file_path,
            &language,
        );

        merge_pdgs(&mut pdg, &file_pdg);
    }

    relink_external_import_edges(&mut pdg);
    pdg
}

/// Merge source PDG into target PDG by symbol identity.
pub fn merge_pdgs(target: &mut ProgramDependenceGraph, source: &ProgramDependenceGraph) {
    for node_idx in source.node_indices() {
        if let Some(node) = source.get_node(node_idx) {
            if target.find_by_symbol(&node.id).is_none() {
                target.add_node(node.clone());
            }
        }
    }

    let mut existing_edges = collect_edge_keys(target);

    for edge_idx in source.edge_indices() {
        let Some(edge) = source.get_edge(edge_idx) else {
            continue;
        };
        let Some((from, to)) = source.edge_endpoints(edge_idx) else {
            continue;
        };
        let (Some(from_node), Some(to_node)) = (source.get_node(from), source.get_node(to)) else {
            continue;
        };
        let (Some(new_from), Some(new_to)) = (
            target.find_by_symbol(&from_node.id),
            target.find_by_symbol(&to_node.id),
        ) else {
            continue;
        };

        let key = edge_key(new_from, new_to, &edge.edge_type);
        if existing_edges.insert(key) {
            let _ = target.add_edge(new_from, new_to, edge.clone());
        }
    }
}

/// Resolve import edges that currently point to synthetic external-module nodes
/// to internal symbols when merged project context makes that possible.
pub fn relink_external_import_edges(pdg: &mut ProgramDependenceGraph) {
    let mut symbol_map: HashMap<String, Vec<NodeId>> = HashMap::new();

    for node_idx in pdg.node_indices() {
        let Some(node) = pdg.get_node(node_idx) else {
            continue;
        };

        if node.node_type == NodeType::Module && node.language == "external" {
            continue;
        }

        for key in candidate_keys_for_node(node) {
            symbol_map.entry(key).or_default().push(node_idx);
        }
    }

    let edge_indices = pdg.edge_indices().collect::<Vec<_>>();
    let mut to_remove = Vec::new();
    let mut to_add = Vec::new();
    let mut existing_edges = collect_edge_keys(pdg);

    for edge_idx in edge_indices {
        let Some(edge) = pdg.get_edge(edge_idx) else {
            continue;
        };
        if edge.edge_type != EdgeType::Import {
            continue;
        }

        let Some((from, to)) = pdg.edge_endpoints(edge_idx) else {
            continue;
        };
        let Some(target) = pdg.get_node(to) else {
            continue;
        };

        if !(target.node_type == NodeType::Module && target.language == "external") {
            continue;
        }

        let importer_language = pdg
            .get_node(from)
            .map(|node| node.language.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let candidates =
            resolve_import_candidates_ranked(&target.name, &symbol_map, pdg, &importer_language)
                .into_iter()
                .filter(|candidate| *candidate != from)
                .take(MAX_RELINK_CANDIDATES)
                .collect::<Vec<_>>();

        if candidates.is_empty() {
            continue;
        }

        to_remove.push(edge_idx);
        for candidate in candidates {
            to_add.push((from, candidate));
        }
    }

    for edge_idx in to_remove {
        if let Some((from, to)) = pdg.edge_endpoints(edge_idx) {
            if let Some(edge) = pdg.get_edge(edge_idx) {
                existing_edges.remove(&edge_key(from, to, &edge.edge_type));
            }
        }
        let _ = pdg.remove_edge(edge_idx);
    }

    for (from, to) in to_add {
        let key = edge_key(from, to, &EdgeType::Import);
        if existing_edges.insert(key) {
            pdg.add_import_edges(vec![(from, to)]);
        }
    }

    cleanup_orphan_external_modules(pdg);
}

fn collect_edge_keys(pdg: &ProgramDependenceGraph) -> HashSet<(usize, usize, u8)> {
    let mut keys = HashSet::new();
    for edge_idx in pdg.edge_indices() {
        let Some(edge) = pdg.get_edge(edge_idx) else {
            continue;
        };
        let Some((from, to)) = pdg.edge_endpoints(edge_idx) else {
            continue;
        };
        keys.insert(edge_key(from, to, &edge.edge_type));
    }
    keys
}

fn edge_key(from: NodeId, to: NodeId, edge_type: &EdgeType) -> (usize, usize, u8) {
    (from.index(), to.index(), edge_type_code(edge_type))
}

fn edge_type_code(edge_type: &EdgeType) -> u8 {
    match edge_type {
        EdgeType::Call => 1,
        EdgeType::DataDependency => 2,
        EdgeType::Inheritance => 3,
        EdgeType::Import => 4,
    }
}

fn cleanup_orphan_external_modules(pdg: &mut ProgramDependenceGraph) {
    let mut degree: HashMap<NodeId, usize> = HashMap::new();
    for edge_idx in pdg.edge_indices() {
        let Some((from, to)) = pdg.edge_endpoints(edge_idx) else {
            continue;
        };
        *degree.entry(from).or_insert(0) += 1;
        *degree.entry(to).or_insert(0) += 1;
    }

    let orphan_external = pdg
        .node_indices()
        .filter(|idx| {
            let Some(node) = pdg.get_node(*idx) else {
                return false;
            };
            node.node_type == NodeType::Module
                && node.language == "external"
                && degree.get(idx).copied().unwrap_or(0) == 0
        })
        .collect::<Vec<_>>();

    for node_id in orphan_external {
        let _ = pdg.remove_node(node_id);
    }
}

fn suffix_keys(normalized: &str, max_len: usize) -> Vec<String> {
    let parts = normalized.split('.').collect::<Vec<_>>();
    if parts.len() < 2 {
        return Vec::new();
    }

    let mut out = Vec::new();
    for len in 2..=parts.len().min(max_len) {
        out.push(parts[parts.len() - len..].join("."));
    }
    out
}

fn candidate_keys_for_node(node: &legraphe::pdg::Node) -> Vec<String> {
    let mut keys = HashSet::new();

    let normalized_id = node
        .id
        .split_once(':')
        .map(|(_, qualified)| legraphe::extraction::normalize_symbol(qualified))
        .unwrap_or_else(|| legraphe::extraction::normalize_symbol(&node.id));
    let normalized_name = legraphe::extraction::normalize_symbol(&node.name);

    if !normalized_id.is_empty() {
        keys.insert(normalized_id.clone());
        for suffix in suffix_keys(&normalized_id, 3) {
            keys.insert(suffix);
        }
        if let Some(last) = normalized_id.split('.').next_back() {
            keys.insert(last.to_string());
        }
    }

    if !normalized_name.is_empty() {
        keys.insert(normalized_name);
    }

    keys.into_iter().collect()
}

fn resolve_import_candidates_ranked(
    import_name: &str,
    symbol_map: &HashMap<String, Vec<NodeId>>,
    pdg: &ProgramDependenceGraph,
    importer_language: &str,
) -> Vec<NodeId> {
    let normalized = legraphe::extraction::normalize_symbol(import_name);
    let mut scored: HashMap<NodeId, i32> = HashMap::new();

    if let Some(exact) = symbol_map.get(&normalized) {
        for id in exact {
            *scored.entry(*id).or_insert(0) += 300;
        }
    }

    for suffix in suffix_keys(&normalized, 3) {
        if let Some(values) = symbol_map.get(&suffix) {
            for id in values {
                *scored.entry(*id).or_insert(0) += 200;
            }
        }
    }

    if let Some(last) = normalized.split('.').next_back() {
        if let Some(values) = symbol_map.get(last) {
            for id in values {
                *scored.entry(*id).or_insert(0) += 100;
            }
        }
    }

    let looks_module_like = normalized.contains('.');

    let mut ranked = scored.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_id, left_score), (right_id, right_score)| {
        let left_bonus = node_bonus(pdg, *left_id, importer_language, looks_module_like);
        let right_bonus = node_bonus(pdg, *right_id, importer_language, looks_module_like);

        (right_score + right_bonus)
            .cmp(&(left_score + left_bonus))
            .then_with(|| left_id.index().cmp(&right_id.index()))
    });

    let ranked_ids = ranked.into_iter().map(|(id, _)| id).collect::<Vec<_>>();

    if looks_module_like {
        let module_ids = ranked_ids
            .iter()
            .copied()
            .filter(|id| {
                pdg.get_node(*id)
                    .map(|n| n.node_type == NodeType::Module)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();

        if !module_ids.is_empty() {
            return module_ids;
        }
    }

    ranked_ids
}

fn node_bonus(
    pdg: &ProgramDependenceGraph,
    node_id: NodeId,
    importer_language: &str,
    looks_module_like: bool,
) -> i32 {
    let Some(node) = pdg.get_node(node_id) else {
        return 0;
    };

    let mut bonus = 0;
    if node.language == importer_language {
        bonus += 50;
    }

    if looks_module_like && node.node_type == NodeType::Module {
        bonus += 120;
    }

    bonus
}

#[cfg(test)]
mod tests {
    use super::*;
    use leparse::traits::{ImportInfo, SignatureInfo, Visibility};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_signature(name: &str, qualified: &str, imports: Vec<ImportInfo>) -> SignatureInfo {
        SignatureInfo {
            name: name.to_string(),
            qualified_name: qualified.to_string(),
            parameters: Vec::new(),
            return_type: None,
            visibility: Visibility::Public,
            is_async: false,
            is_method: false,
            docstring: None,
            calls: Vec::new(),
            imports,
            byte_range: (0, 10),
        }
    }

    fn make_result(path: &str, language: &str, signatures: Vec<SignatureInfo>) -> ParsingResult {
        ParsingResult {
            file_path: PathBuf::from(path),
            language: Some(language.to_string()),
            signatures,
            error: None,
            parse_time_ms: 1,
        }
    }

    #[test]
    fn relinks_external_imports_when_internal_symbol_exists_in_merged_graph() {
        let file_a = make_result(
            "src/a.rs",
            "rust",
            vec![make_signature(
                "main",
                "pkg::main",
                vec![ImportInfo {
                    path: "pkg.helper".to_string(),
                    alias: None,
                }],
            )],
        );

        let file_b = make_result(
            "src/b.rs",
            "rust",
            vec![make_signature("helper", "pkg::helper", Vec::new())],
        );

        let pdg = merged_pdg_from_results(&[file_a, file_b]);

        let mut external_import_edges = 0usize;
        let mut internal_import_edges = 0usize;

        for edge_idx in pdg.edge_indices() {
            let Some(edge) = pdg.get_edge(edge_idx) else {
                continue;
            };
            if edge.edge_type != EdgeType::Import {
                continue;
            }

            let Some((_, to)) = pdg.edge_endpoints(edge_idx) else {
                continue;
            };
            let Some(target) = pdg.get_node(to) else {
                continue;
            };

            if target.node_type == NodeType::Module && target.language == "external" {
                external_import_edges += 1;
            } else {
                internal_import_edges += 1;
            }
        }

        assert!(
            internal_import_edges >= 1,
            "expected at least one relinked internal import edge"
        );
        assert_eq!(
            external_import_edges, 0,
            "external import edge should be removed when internal symbol is found"
        );
    }

    #[test]
    fn relink_is_bounded_to_single_best_candidate() {
        let file_a = make_result(
            "src/a.rs",
            "rust",
            vec![make_signature(
                "main",
                "pkg::main",
                vec![ImportInfo {
                    path: "common.util".to_string(),
                    alias: None,
                }],
            )],
        );
        let file_b = make_result(
            "src/b.rs",
            "rust",
            vec![make_signature("util", "alpha::util", Vec::new())],
        );
        let file_c = make_result(
            "src/c.rs",
            "rust",
            vec![make_signature("util", "beta::util", Vec::new())],
        );

        let pdg = merged_pdg_from_results(&[file_a, file_b, file_c]);

        let import_edges = pdg
            .edge_indices()
            .filter_map(|idx| {
                let edge = pdg.get_edge(idx)?;
                if edge.edge_type != EdgeType::Import {
                    return None;
                }
                pdg.edge_endpoints(idx)
            })
            .collect::<Vec<_>>();

        assert_eq!(
            import_edges.len(),
            1,
            "relink should keep import set bounded to one best candidate"
        );
    }

    #[test]
    fn relink_prefers_same_language_candidate_when_scores_tie() {
        let importer = make_result(
            "src/a.rs",
            "rust",
            vec![make_signature(
                "main",
                "pkg::main",
                vec![ImportInfo {
                    path: "pkg.helper".to_string(),
                    alias: None,
                }],
            )],
        );

        let rust_target = make_result(
            "src/rust_mod.rs",
            "rust",
            vec![make_signature("helper", "pkg::helper", Vec::new())],
        );

        let python_target = make_result(
            "src/py_mod.py",
            "python",
            vec![make_signature("helper", "pkg.helper", Vec::new())],
        );

        let pdg = merged_pdg_from_results(&[importer, rust_target, python_target]);

        let (_, to) = pdg
            .edge_indices()
            .filter_map(|idx| {
                let edge = pdg.get_edge(idx)?;
                if edge.edge_type != EdgeType::Import {
                    return None;
                }
                pdg.edge_endpoints(idx)
            })
            .next()
            .expect("expected one import edge");

        let target = pdg.get_node(to).expect("import target node");
        assert_eq!(
            target.language, "rust",
            "same-language candidate should win tie-break"
        );
    }

    #[test]
    fn merged_pdg_uses_source_fallback_for_empty_signature_import_files() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("__init__.py");
        std::fs::write(&file, "import third.party\n").expect("write");

        let result = ParsingResult {
            file_path: file,
            language: Some("python".to_string()),
            signatures: Vec::new(),
            error: None,
            parse_time_ms: 1,
        };

        let pdg = merged_pdg_from_results(&[result]);
        let import_edges = pdg
            .edge_indices()
            .filter_map(|idx| pdg.get_edge(idx))
            .filter(|edge| edge.edge_type == EdgeType::Import)
            .count();

        assert!(import_edges >= 1, "expected source-fallback import edge");
    }

    #[test]
    fn module_candidates_are_preferred_for_module_like_imports() {
        let mut pdg = ProgramDependenceGraph::new();
        let module = pdg.add_node(legraphe::pdg::Node {
            id: "src/mod.rs:pkg::helper".to_string(),
            node_type: NodeType::Module,
            name: "helper".to_string(),
            file_path: "src/mod.rs".to_string(),
            byte_range: (0, 1),
            complexity: 1,
            language: "rust".to_string(),
            embedding: None,
        });
        let function = pdg.add_node(legraphe::pdg::Node {
            id: "src/lib.rs:pkg::helper".to_string(),
            node_type: NodeType::Function,
            name: "helper".to_string(),
            file_path: "src/lib.rs".to_string(),
            byte_range: (0, 1),
            complexity: 1,
            language: "rust".to_string(),
            embedding: None,
        });

        let mut symbol_map: HashMap<String, Vec<NodeId>> = HashMap::new();
        for node_id in [module, function] {
            let node = pdg.get_node(node_id).expect("node");
            for key in candidate_keys_for_node(node) {
                symbol_map.entry(key).or_default().push(node_id);
            }
        }

        let ranked = resolve_import_candidates_ranked("pkg.helper", &symbol_map, &pdg, "rust");
        assert_eq!(ranked.first().copied(), Some(module));
    }

    #[test]
    fn orphan_external_modules_are_removed_after_relink() {
        let mut pdg = ProgramDependenceGraph::new();
        let orphan = pdg.add_node(legraphe::pdg::Node {
            id: "src/a.rs:__external__:x.y".to_string(),
            node_type: NodeType::Module,
            name: "x.y".to_string(),
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 0),
            complexity: 1,
            language: "external".to_string(),
            embedding: None,
        });

        cleanup_orphan_external_modules(&mut pdg);
        assert!(pdg.get_node(orphan).is_none());
    }
}
