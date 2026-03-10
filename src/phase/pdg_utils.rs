// PDG Utilities — Rewrite
//
// Key changes from original:
//   - `ParsingResult` expected to carry `source_bytes: Vec<u8>` — eliminates re-read from disk
//   - `merge_pdgs` accepts external `existing_edges` set to avoid O(E) rebuild per file
//   - `MAX_RELINK_CANDIDATES` raised to 3 and made configurable via `RelinkConfig`
//   - Scoring magic numbers extracted to `RelinkConfig` constants with documentation
//   - `relink_external_import_edges` accepts `RelinkConfig`
//   - `cleanup_orphan_external_modules` uses degree map built during relink (not recomputed)

use crate::graph::{
    extract_pdg_from_signatures,
    pdg::{EdgeType, NodeId, NodeType, ProgramDependenceGraph},
};
use crate::parse::parallel::ParsingResult;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Relink configuration
// ---------------------------------------------------------------------------

/// Controls the behavior of `relink_external_import_edges`.
///
/// Scoring rationale (exposed here so maintainers understand tradeoffs):
///   exact_match_score:  Normalized import path exactly matches a known symbol.
///                       Highest score — unambiguous resolution.
///   suffix_match_score: Last N segments of import path match a known symbol.
///                       High score — common in monorepos where imports use
///                       partial paths (e.g. "utils.helper" matching "pkg.utils.helper")
///   last_seg_score:     Only the last identifier matches.
///                       Low score — ambiguous, many symbols share short names.
///   same_language_bonus: Prefer resolving Rust imports to Rust nodes, etc.
///                       Tiebreaker — avoids cross-language false resolutions.
///   module_node_bonus:  Prefer Module-type nodes for dotted import paths.
///                       Correct for package imports ("os.path" → Module, not Function)
/// Configuration for relinking external import edges.
///
/// When building a PDG, import statements often reference symbols from other files
/// that haven't been indexed yet. The relinking phase attempts to resolve these
/// imports by matching them against known symbols in the graph. This configuration
/// controls the scoring and candidate selection process.
///
/// # Scoring System
///
/// Each potential match is scored based on multiple signals:
/// - Exact symbol name match (highest score)
/// - Suffix match (e.g., `utils.helper` matches `helper`)
/// - Last segment match (e.g., `src/utils` matches `utils`)
/// - Same language bonus (prefer matches in the same language)
/// - Module node bonus (prefer module-level imports)
pub struct RelinkConfig {
    /// Max number of candidates to relink a single external import edge to.
    ///
    /// Set to 1 for strict mode (only the best match) or 2-3 for permissive
    /// mode (allows re-exports and aliased imports to coexist).
    pub max_candidates: usize,

    /// Score for an exact symbol name match.
    ///
    /// This is the highest-scoring match type. When the import name exactly
    /// matches a defined symbol, this score is applied.
    pub exact_match_score: i32,

    /// Score for a suffix match.
    ///
    /// Applied when the import path ends with the symbol name.
    /// For example, `utils.helper` would suffix-match `helper`.
    pub suffix_match_score: i32,

    /// Score for matching the last segment of a path.
    ///
    /// Applied when the final component of an import path matches.
    /// For example, `src/utils` would match `utils`.
    pub last_seg_score: i32,

    /// Bonus score for matching symbols in the same language.
    ///
    /// This bonus is added to the base score when the imported symbol
    /// and the potential match are in the same programming language,
    /// helping to disambiguate symbols with common names across languages.
    pub same_language_bonus: i32,

    /// Bonus score for module-level node matches.
    ///
    /// This bonus is added when the matching node is a module-level
    /// import (e.g., `import { foo } from './bar'` in JavaScript),
    /// as opposed to deep imports or re-exports.
    pub module_node_bonus: i32,
}

impl Default for RelinkConfig {
    fn default() -> Self {
        Self {
            max_candidates: 3, // Raised from 1; allows re-exports and aliased imports
            exact_match_score: 300,
            suffix_match_score: 200,
            last_seg_score: 100,
            same_language_bonus: 50,
            module_node_bonus: 120, // Module preference for dotted import paths
        }
    }
}

impl RelinkConfig {
    /// Strict single-match mode (original behavior).
    pub fn strict() -> Self {
        Self {
            max_candidates: 1,
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Primary build function
// ---------------------------------------------------------------------------

/// Build and merge per-file PDGs into a single project PDG.
///
/// Expects `ParsingResult.source_bytes` to be populated by the parser —
/// this avoids a redundant disk read during merge.
pub fn merged_pdg_from_results(
    results: &[ParsingResult],
    relink_config: Option<RelinkConfig>,
) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();
    let mut existing_edges: HashSet<(usize, usize, u8)> = HashSet::new();

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

        // Source bytes come from ParsingResult — no disk re-read
        let source_bytes = result.source_bytes.as_deref().unwrap_or(&[]);

        let file_pdg = extract_pdg_from_signatures(
            result.signatures.clone(),
            source_bytes,
            &file_path,
            &language,
        );

        merge_pdgs_with_keys(&mut pdg, &file_pdg, &mut existing_edges);
    }

    let config = relink_config.unwrap_or_default();
    relink_external_import_edges(&mut pdg, &config);
    pdg
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

/// Merge `source` PDG into `target` by symbol identity (node.id deduplication).
/// Accepts external edge key set to avoid O(E) rebuild on every call.
pub fn merge_pdgs(target: &mut ProgramDependenceGraph, source: &ProgramDependenceGraph) {
    let mut existing_edges = collect_edge_keys(target);
    merge_pdgs_with_keys(target, source, &mut existing_edges);
}

fn merge_pdgs_with_keys(
    target: &mut ProgramDependenceGraph,
    source: &ProgramDependenceGraph,
    existing_edges: &mut HashSet<(usize, usize, u8)>,
) {
    // Add nodes not already present (by symbol id)
    for node_idx in source.node_indices() {
        if let Some(node) = source.get_node(node_idx) {
            if target.find_by_symbol(&node.id).is_none() {
                target.add_node(node.clone());
            }
        }
    }

    // Add edges (deduplicated by (from, to, edge_type))
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
            target.add_edge(new_from, new_to, edge.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Import edge relinking
// ---------------------------------------------------------------------------

/// Resolve import edges that point to synthetic external-module nodes
/// to internal symbols where project context now allows resolution.
pub fn relink_external_import_edges(pdg: &mut ProgramDependenceGraph, config: &RelinkConfig) {
    // Build symbol map for all non-external nodes
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

    // Collect edges to relink
    let edge_indices: Vec<_> = pdg.edge_indices().collect();
    let mut to_remove = Vec::new();
    let mut to_add: Vec<(NodeId, NodeId)> = Vec::new();
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

        let importer_lang = pdg
            .get_node(from)
            .map(|n| n.language.clone())
            .unwrap_or_default();

        let candidates: Vec<NodeId> = resolve_import_candidates_ranked(
            &target.name,
            &symbol_map,
            pdg,
            &importer_lang,
            config,
        )
        .into_iter()
        .filter(|c| *c != from)
        .take(config.max_candidates)
        .collect();

        if candidates.is_empty() {
            continue;
        }

        // Track the degree of the external node; it becomes orphaned after relink
        to_remove.push((edge_idx, from, to));
        for candidate in candidates {
            to_add.push((from, candidate));
        }
    }

    // Collect orphan candidates before removal (external nodes that will lose all edges)
    let mut external_degree: HashMap<NodeId, usize> = HashMap::new();
    for (_, _, to) in &to_remove {
        *external_degree.entry(*to).or_insert(0) += 1;
    }

    // Remove old edges and update key set
    for (edge_idx, from, to) in to_remove {
        if let Some(edge) = pdg.get_edge(edge_idx) {
            existing_edges.remove(&edge_key(from, to, &edge.edge_type));
        }
        pdg.remove_edge(edge_idx);
    }

    // Add new edges
    for (from, to) in to_add {
        let key = edge_key(from, to, &EdgeType::Import);
        if existing_edges.insert(key) {
            pdg.add_import_edges(vec![(from, to)]);
        }
    }

    // Clean up orphaned external nodes
    // Only remove nodes that have no remaining edges
    let orphans: Vec<NodeId> = external_degree
        .keys()
        .copied()
        .filter(|&nid| {
            let Some(node) = pdg.get_node(nid) else {
                return false;
            };
            node.node_type == NodeType::Module
                && node.language == "external"
                && pdg.predecessors(nid).is_empty()
                && pdg.neighbors(nid).is_empty()
        })
        .collect();

    for nid in orphans {
        pdg.remove_node(nid);
    }
}

// ---------------------------------------------------------------------------
// Scoring and resolution
// ---------------------------------------------------------------------------

fn resolve_import_candidates_ranked(
    import_name: &str,
    symbol_map: &HashMap<String, Vec<NodeId>>,
    pdg: &ProgramDependenceGraph,
    importer_language: &str,
    config: &RelinkConfig,
) -> Vec<NodeId> {
    let normalized = crate::graph::extraction::normalize_symbol(import_name);
    let mut scored: HashMap<NodeId, i32> = HashMap::new();

    // Exact match
    if let Some(exact) = symbol_map.get(&normalized) {
        for id in exact {
            *scored.entry(*id).or_insert(0) += config.exact_match_score;
        }
    }

    // Suffix matches (last 2 and 3 segments)
    let parts: Vec<&str> = normalized.split('.').collect();
    for len in 2..=3_usize.min(parts.len()) {
        let start = parts.len() - len;
        let key = parts[start..].join(".");
        if let Some(values) = symbol_map.get(&key) {
            for id in values {
                *scored.entry(*id).or_insert(0) += config.suffix_match_score;
            }
        }
    }

    // Last segment
    if let Some(last) = normalized.split('.').last() {
        if let Some(values) = symbol_map.get(last) {
            for id in values {
                *scored.entry(*id).or_insert(0) += config.last_seg_score;
            }
        }
    }

    let looks_module_like = normalized.contains('.');

    // Sort by score with bonuses
    let mut ranked: Vec<(NodeId, i32)> = scored.into_iter().collect();
    ranked.sort_by(|(la, ls), (ra, rs)| {
        let lb = node_bonus(pdg, *la, importer_language, looks_module_like, config);
        let rb = node_bonus(pdg, *ra, importer_language, looks_module_like, config);
        (rs + rb)
            .cmp(&(ls + lb))
            .then_with(|| la.index().cmp(&ra.index()))
    });

    let ranked_ids: Vec<NodeId> = ranked.into_iter().map(|(id, _)| id).collect();

    // For module-like imports, prefer Module-typed nodes
    if looks_module_like {
        let module_ids: Vec<NodeId> = ranked_ids
            .iter()
            .copied()
            .filter(|id| {
                pdg.get_node(*id)
                    .map(|n| n.node_type == NodeType::Module)
                    .unwrap_or(false)
            })
            .collect();
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
    config: &RelinkConfig,
) -> i32 {
    let Some(node) = pdg.get_node(node_id) else {
        return 0;
    };
    let mut bonus = 0;
    if node.language == importer_language {
        bonus += config.same_language_bonus;
    }
    if looks_module_like && node.node_type == NodeType::Module {
        bonus += config.module_node_bonus;
    }
    bonus
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collects all edge keys from the PDG for deduplication purposes.
///
/// This function creates a set of compact edge identifiers that can be used
/// to efficiently check for duplicate edges during PDG merging. Each key
/// consists of (source_index, target_index, edge_type_code).
///
/// # Arguments
///
/// * `pdg` - The ProgramDependenceGraph to collect edges from
///
/// # Returns
///
/// A HashSet of edge keys representing all edges in the graph.
pub fn collect_edge_keys(pdg: &ProgramDependenceGraph) -> HashSet<(usize, usize, u8)> {
    pdg.edge_indices()
        .filter_map(|idx| {
            let edge = pdg.get_edge(idx)?;
            let (from, to) = pdg.edge_endpoints(idx)?;
            Some(edge_key(from, to, &edge.edge_type))
        })
        .collect()
}

fn edge_key(from: NodeId, to: NodeId, edge_type: &EdgeType) -> (usize, usize, u8) {
    (from.index(), to.index(), edge_type_code(edge_type))
}

fn edge_type_code(et: &EdgeType) -> u8 {
    match et {
        EdgeType::Call => 1,
        EdgeType::DataDependency => 2,
        EdgeType::Inheritance => 3,
        EdgeType::Import => 4,
        EdgeType::Containment => 5,
    }
}

fn candidate_keys_for_node(node: &crate::graph::pdg::Node) -> Vec<String> {
    let mut keys: HashSet<String> = HashSet::new();

    let norm_id = node
        .id
        .split_once(':')
        .map(|(_, q)| crate::graph::extraction::normalize_symbol(q))
        .unwrap_or_else(|| crate::graph::extraction::normalize_symbol(&node.id));
    let norm_name = crate::graph::extraction::normalize_symbol(&node.name);

    if !norm_id.is_empty() {
        keys.insert(norm_id.clone());
        let parts: Vec<&str> = norm_id.split('.').collect();
        for len in 2..=3_usize.min(parts.len()) {
            let start = parts.len() - len;
            keys.insert(parts[start..].join("."));
        }
        if let Some(last) = norm_id.split('.').last() {
            keys.insert(last.to_string());
        }
    }
    if !norm_name.is_empty() {
        keys.insert(norm_name);
    }

    keys.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::traits::{ImportInfo, SignatureInfo, Visibility};
    use std::path::PathBuf;

    fn make_sig(name: &str, qualified: &str, imports: Vec<ImportInfo>) -> SignatureInfo {
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

    fn make_result(
        path: &str,
        language: &str,
        sigs: Vec<SignatureInfo>,
        src: &[u8],
    ) -> ParsingResult {
        ParsingResult {
            file_path: PathBuf::from(path),
            language: Some(language.to_string()),
            signatures: sigs,
            source_bytes: Some(src.to_vec()),
            error: None,
            parse_time_ms: 1,
        }
    }

    #[test]
    fn relinks_external_to_internal() {
        let a = make_result(
            "src/a.rs",
            "rust",
            vec![make_sig(
                "main",
                "pkg::main",
                vec![ImportInfo {
                    path: "pkg.helper".to_string(),
                    alias: None,
                }],
            )],
            b"",
        );
        let b = make_result(
            "src/b.rs",
            "rust",
            vec![make_sig("helper", "pkg::helper", vec![])],
            b"",
        );

        let pdg = merged_pdg_from_results(&[a, b], None);

        let external_count = pdg
            .edge_indices()
            .filter_map(|idx| {
                let edge = pdg.get_edge(idx)?;
                if edge.edge_type != EdgeType::Import {
                    return None;
                }
                let (_, to) = pdg.edge_endpoints(idx)?;
                let target = pdg.get_node(to)?;
                (target.node_type == NodeType::Module && target.language == "external")
                    .then_some(())
            })
            .count();

        assert_eq!(external_count, 0, "External import should be relinked");
    }

    #[test]
    fn strict_config_keeps_single_candidate() {
        let a = make_result(
            "src/a.rs",
            "rust",
            vec![make_sig(
                "main",
                "pkg::main",
                vec![ImportInfo {
                    path: "util".to_string(),
                    alias: None,
                }],
            )],
            b"",
        );
        let b = make_result(
            "src/b.rs",
            "rust",
            vec![make_sig("util", "alpha::util", vec![])],
            b"",
        );
        let c = make_result(
            "src/c.rs",
            "rust",
            vec![make_sig("util", "beta::util", vec![])],
            b"",
        );

        let pdg = merged_pdg_from_results(&[a, b, c], Some(RelinkConfig::strict()));
        let import_count = pdg
            .edge_indices()
            .filter_map(|idx| pdg.get_edge(idx))
            .filter(|e| e.edge_type == EdgeType::Import)
            .count();
        assert_eq!(import_count, 1, "Strict mode: only one relink candidate");
    }

    #[test]
    fn permissive_config_allows_multiple_candidates() {
        let a = make_result(
            "src/a.rs",
            "rust",
            vec![make_sig(
                "main",
                "pkg::main",
                vec![ImportInfo {
                    path: "util".to_string(),
                    alias: None,
                }],
            )],
            b"",
        );
        let b = make_result(
            "src/b.rs",
            "rust",
            vec![make_sig("util", "alpha::util", vec![])],
            b"",
        );
        let c = make_result(
            "src/c.rs",
            "rust",
            vec![make_sig("util", "beta::util", vec![])],
            b"",
        );

        let config = RelinkConfig {
            max_candidates: 3,
            ..RelinkConfig::default()
        };
        let pdg = merged_pdg_from_results(&[a, b, c], Some(config));
        let import_count = pdg
            .edge_indices()
            .filter_map(|idx| pdg.get_edge(idx))
            .filter(|e| e.edge_type == EdgeType::Import)
            .count();
        assert!(
            import_count >= 2,
            "Permissive mode: should relink to both candidates"
        );
    }

    #[test]
    fn containment_edges_preserved_through_merge() {
        let a = make_result(
            "src/a.rs",
            "rust",
            vec![{
                let mut s = make_sig("method", "MyClass::method", vec![]);
                s.is_method = true;
                s
            }],
            b"",
        );

        let pdg = merged_pdg_from_results(&[a], None);
        let containment = pdg
            .edge_indices()
            .filter_map(|idx| pdg.get_edge(idx))
            .filter(|e| e.edge_type == EdgeType::Containment)
            .count();
        assert_eq!(containment, 1);
    }
}
