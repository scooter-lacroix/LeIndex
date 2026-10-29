//! Compact PDG persistence stage (WS6-9 Task 3).
//!
//! Builds per-file graph fragments, resolves cross-file edges in bounded
//! batches, writes node/edge segments + interned symbol table to CAS. No
//! whole-PDG clone during merge (spec §6.3, VAL-STREAM-004).

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::graph::pdg::ProgramDependenceGraph;
use crate::graph::pdg::{
    Edge as PDGEdge, EdgeType as PDGEdgeType, Node as PDGNode, NodeType as PDGNodeType,
};

/// A compact node record for CAS staging. Uses stable IDs and interned string
/// indices (no heap-heavy per-node allocations).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdgNodeRecord {
    /// Stable node ID (path:qualified_name).
    pub id: String,
    /// Node type string ("function", "class", etc.).
    pub node_type: String,
    /// Node name (interned in the symbol table; here as resolved string).
    pub name: String,
    /// File path.
    pub file_path: String,
    /// Byte range start.
    pub byte_start: usize,
    /// Byte range end.
    pub byte_end: usize,
    /// Complexity.
    pub complexity: u32,
    /// Language.
    pub language: String,
}

/// A compact edge record for CAS staging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdgEdgeRecord {
    /// Source node ID.
    pub source: String,
    /// Target node ID.
    pub target: String,
    /// Edge type ("call", "data_dependency", "inheritance", etc.).
    pub edge_type: String,
    /// Confidence score (0-100 as integer to avoid f32 serialization issues);
    /// `None` when the edge carries no confidence.
    #[serde(default)]
    pub confidence: Option<u8>,
    /// Call count for call edges.
    #[serde(default)]
    pub call_count: Option<usize>,
    /// Variable name for data-flow edges.
    #[serde(default)]
    pub variable_name: Option<String>,
    /// Flow channel (`argument`, `env`, `stdin`, etc.).
    #[serde(default)]
    pub channel: Option<String>,
    /// Argument ordinal for call/data-flow edges.
    #[serde(default)]
    pub position: Option<usize>,
}

/// A per-file PDG fragment: the nodes and edges extracted from one file
/// (before cross-file edge resolution).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdgFragment {
    /// Nodes from this file.
    pub nodes: Vec<PdgNodeRecord>,
    /// Edges where both endpoints are in this file (intra-file).
    pub intra_edges: Vec<PdgEdgeRecord>,
    /// Edges that reference nodes in other files (resolved in merge phase).
    pub cross_file_refs: Vec<PdgEdgeRecord>,
}

/// A merged PDG segment ready for CAS staging: all nodes, all edges, and
/// the interned symbol table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdgSegment {
    /// All nodes across all fragments.
    pub nodes: Vec<PdgNodeRecord>,
    /// All edges (intra-file + resolved cross-file).
    pub edges: Vec<PdgEdgeRecord>,
    /// Interned symbol table: each entry is a unique (name, file_path) pair.
    pub symbol_table: Vec<InternedSymbol>,
}

/// An entry in the interned symbol table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct InternedSymbol {
    /// Symbol name.
    pub name: String,
    /// File path where the symbol is defined.
    pub file_path: String,
    /// The stable node ID this symbol resolves to.
    pub node_id: String,
}

/// Statistics from the PDG build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdgStats {
    /// Number of fragments processed.
    pub fragments: usize,
    /// Total nodes.
    pub node_count: usize,
    /// Total edges.
    pub edge_count: usize,
    /// Number of cross-file edges resolved.
    pub cross_file_resolved: usize,
    /// Number of cross-file edges unresolved (external target not found).
    pub cross_file_unresolved: usize,
    /// Node records dropped by the one-record-per-id dedup. Legitimate for
    /// per-file `external::{target}` placeholders; a non-external count
    /// here means a real id collision was silently absorbed — diagnosable
    /// instead of invisible.
    pub duplicate_nodes_dropped: usize,
}

// `build_fragment_from_parsed` (the original streaming skeleton) was
// removed: it flattened signatures to name/kind/bytes, hardcoded
// complexity 0, and emitted no intra-file edges, which is why
// streaming-built indexes showed complexity-0 nodes and empty callee
// graphs. `fragment_from_pdg` — its documented production realization —
// is the only fragment builder now, fed by `extract_pdg_from_signatures`.

/// Merge per-file fragments into a compact CAS-ready segment.
///
/// Cross-file edges are resolved in a second pass against the full interned
/// node-id set, so resolution cannot depend on fragment iteration order. No
/// whole-PDG clone is performed during merge (VAL-STREAM-004).
///
/// Each fragment is consumed by value (moved into the segment), and the
/// symbol table is built incrementally — there is no intermediate `PDG::clone()`.
pub fn merge_fragments_to_segment(fragments: Vec<PdgFragment>) -> (PdgSegment, PdgStats) {
    let mut segment = PdgSegment {
        nodes: Vec::new(),
        edges: Vec::new(),
        symbol_table: Vec::new(),
    };
    let mut stats = PdgStats::default();
    // Node ids (`{file_path}:{qualified_name}` / `external::{target}`) — the
    // same namespace cross-file edge targets live in. The previous key
    // (`{name}:{file_path}`) could never match an edge target, so every
    // cross-file edge fell into the O(N) scan below, which only saw the
    // fragments merged so far and permanently dropped forward references.
    let mut node_ids: HashSet<String> = HashSet::new();
    let mut deferred_cross: Vec<PdgEdgeRecord> = Vec::new();

    for fragment in fragments {
        stats.fragments += 1;

        // Move nodes into segment (no clone), one record per node id:
        // per-file fragments each create their own `external::{target}`
        // placeholder, so concatenating them would leave the segment (and
        // every graph materialized from it) with duplicate ids that the
        // store's `(project_id, node_id)` key collapses onto a single row.
        // First occurrence wins;
        // edges resolve by id and stay attached to it.
        for node in fragment.nodes {
            if node_ids.insert(node.id.clone()) {
                let interned = InternedSymbol {
                    name: node.name.clone(),
                    file_path: node.file_path.clone(),
                    node_id: node.id.clone(),
                };
                segment.symbol_table.push(interned);
                segment.nodes.push(node);
            } else {
                stats.duplicate_nodes_dropped += 1;
                if node.node_type != "external" {
                    warn!(
                        id = %node.id,
                        "segment merge dropped a non-external duplicate node record (first wins)"
                    );
                }
            }
        }

        // Move intra-file edges
        segment.edges.extend(fragment.intra_edges);

        // Defer cross-file resolution until every fragment's nodes are known.
        deferred_cross.extend(fragment.cross_file_refs);
    }

    // Second pass: resolve against the complete node-id set. Order no longer
    // decides which cross-file edges survive.
    for edge in deferred_cross {
        if node_ids.contains(&edge.target) {
            segment.edges.push(edge);
            stats.cross_file_resolved += 1;
        } else {
            stats.cross_file_unresolved += 1;
        }
    }

    stats.node_count = segment.nodes.len();
    stats.edge_count = segment.edges.len();

    (segment, stats)
}

/// Map a PDG `NodeType` to its canonical string id.
pub fn node_type_to_str(node_type: &PDGNodeType) -> &'static str {
    match node_type {
        PDGNodeType::Function => "function",
        PDGNodeType::Class => "class",
        PDGNodeType::Method => "method",
        PDGNodeType::Variable => "variable",
        PDGNodeType::Module => "module",
        PDGNodeType::External => "external",
        PDGNodeType::DocSection => "doc_section",
        PDGNodeType::FileSummary => "file_summary",
    }
}

/// Parse a canonical `NodeType` string id back into a `PDGNodeType`.
pub fn node_type_from_str(s: &str) -> Option<PDGNodeType> {
    Some(match s {
        "function" => PDGNodeType::Function,
        "class" => PDGNodeType::Class,
        "method" => PDGNodeType::Method,
        "variable" => PDGNodeType::Variable,
        "module" => PDGNodeType::Module,
        "external" => PDGNodeType::External,
        "doc_section" => PDGNodeType::DocSection,
        "file_summary" => PDGNodeType::FileSummary,
        _ => return None,
    })
}

/// Map a `PDGEdgeType` to its canonical string id.
pub fn edge_type_to_str(edge_type: &PDGEdgeType) -> &'static str {
    match edge_type {
        PDGEdgeType::Call => "call",
        PDGEdgeType::DataDependency => "data_dependency",
        PDGEdgeType::Inheritance => "inheritance",
        PDGEdgeType::Import => "import",
        PDGEdgeType::Containment => "containment",
        PDGEdgeType::TypeOf => "type_of",
        PDGEdgeType::StateTransition => "state_transition",
        PDGEdgeType::CommandArgument => "command_argument",
        PDGEdgeType::Environment => "environment",
        PDGEdgeType::Stdin => "stdin",
    }
}

/// Parse a canonical `EdgeType` string id back into a `PDGEdgeType`.
pub fn edge_type_from_str(s: &str) -> Option<PDGEdgeType> {
    Some(match s {
        "call" => PDGEdgeType::Call,
        "data_dependency" => PDGEdgeType::DataDependency,
        "inheritance" => PDGEdgeType::Inheritance,
        "import" => PDGEdgeType::Import,
        "containment" => PDGEdgeType::Containment,
        "type_of" => PDGEdgeType::TypeOf,
        "state_transition" => PDGEdgeType::StateTransition,
        "command_argument" => PDGEdgeType::CommandArgument,
        "environment" => PDGEdgeType::Environment,
        "stdin" => PDGEdgeType::Stdin,
        _ => return None,
    })
}

/// Lossless conversion of a `PDGNode` into a compact `PdgNodeRecord`.
///
/// Every `Node` field (`id`, `name`, `file_path`, `byte_range`, `complexity`,
/// `language`, `node_type`) maps onto `PdgNodeRecord`, so no information is
/// dropped when a graph is routed through the streaming fragment/segment path.
fn node_record(node: &PDGNode) -> PdgNodeRecord {
    PdgNodeRecord {
        id: node.id.clone(),
        node_type: node_type_to_str(&node.node_type).to_string(),
        name: node.name.clone(),
        file_path: node.file_path.to_string(),
        byte_start: node.byte_range.0,
        byte_end: node.byte_range.1,
        complexity: node.complexity,
        language: node.language.clone(),
    }
}

/// Convert a `PDGEdge` into a compact `PdgEdgeRecord`, preserving all
/// `EdgeMetadata` (confidence quantized to 0-100, matching the existing
/// compact-edge contract).
fn edge_record(edge: &PDGEdge) -> PdgEdgeRecord {
    PdgEdgeRecord {
        source: String::new(), // filled in by the caller with resolved node ids
        target: String::new(),
        edge_type: edge_type_to_str(&edge.edge_type).to_string(),
        confidence: edge
            .metadata
            .confidence
            .map(|c| (c.clamp(0.0, 1.0) * 100.0).round() as u8),
        call_count: edge.metadata.call_count,
        variable_name: edge.metadata.variable_name.clone(),
        channel: edge.metadata.channel.clone(),
        position: edge.metadata.position,
    }
}

/// Reconstruct a `PDGEdge` (with full `EdgeMetadata`) from a compact record.
fn edge_from_record(record: &PdgEdgeRecord) -> Result<PDGEdge> {
    let edge_type = edge_type_from_str(&record.edge_type)
        .ok_or_else(|| anyhow::anyhow!("unknown edge type '{}'", record.edge_type))?;
    Ok(PDGEdge {
        edge_type,
        metadata: crate::graph::pdg::EdgeMetadata {
            call_count: record.call_count,
            variable_name: record.variable_name.clone(),
            confidence: record.confidence.map(|c| c as f32 / 100.0),
            channel: record.channel.clone(),
            position: record.position,
        },
    })
}

/// Materialize a per-file `PdgFragment` from an already-extracted per-file
/// `ProgramDependenceGraph` (see `crate::graph::extraction`).
///
/// This is the production realization of `build_fragment_from_parsed`: it
/// delegates node/edge extraction to the real extraction pipeline, then emits
/// compact records. All edges within this file's graph are emitted as
/// `intra_edges` (a per-file extracted PDG only contains its own nodes, so
/// everything is intra-file at this point). No whole-graph clone occurs.
pub fn fragment_from_pdg(pdg: &ProgramDependenceGraph) -> PdgFragment {
    let mut nodes = Vec::with_capacity(pdg.node_count());
    for idx in pdg.node_indices() {
        // `node_indices()` only yields live nodes; `get_node` cannot be None here.
        if let Some(node) = pdg.get_node(idx) {
            nodes.push(node_record(node));
        }
    }

    let mut intra_edges = Vec::with_capacity(pdg.edge_count());
    for eidx in pdg.edge_indices() {
        if let (Some(edge), Some((source, target))) = (pdg.get_edge(eidx), pdg.edge_endpoints(eidx))
        {
            if let (Some(sn), Some(tn)) = (pdg.get_node(source), pdg.get_node(target)) {
                let mut rec = edge_record(edge);
                rec.source = sn.id.clone();
                rec.target = tn.id.clone();
                intra_edges.push(rec);
            }
        }
    }

    PdgFragment {
        nodes,
        intra_edges,
        cross_file_refs: Vec::new(),
    }
}

/// Materialize a `ProgramDependenceGraph` from a merged `PdgSegment`.
///
/// This is the inverse of `fragment_from_pdg`/`merge_fragments_to_segment`:
/// all node and edge records (with their full metadata) are reconstructed
/// into a live graph, rebuilding every index via `add_node`/`add_edge`.
pub fn pdg_from_segment(segment: &PdgSegment) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();
    let mut node_ids: HashMap<String, petgraph::stable_graph::NodeIndex> =
        HashMap::with_capacity(segment.nodes.len());

    // Nodes of one file share a single path allocation.
    let mut file_paths: HashMap<&str, std::sync::Arc<str>> = HashMap::new();
    for record in &segment.nodes {
        // One node per id: per-file fragments each create their own
        // `external::{target}` placeholder, so the merged segment carries
        // duplicates that would otherwise all upsert onto the single
        // `intel_nodes` row `(project_id, node_id)` and collapse on reload.
        // Keep the first occurrence and let edges resolve to it by id.
        if node_ids.contains_key(&record.id) {
            continue;
        }
        let node_type = node_type_from_str(&record.node_type).unwrap_or(PDGNodeType::External);
        // Shared external placeholders are graph-level vocabulary, not file
        // content: canonicalize their path so per-file removal never deletes
        // a node other files' edges still point at (mirrors `merge_pdgs`).
        // Keyed on the RAW record string, not the parsed type: the parser
        // maps any unknown type string to External, and re-pathing a real
        // file's node to `<external>` would strand it outside `file_index`,
        // making it unpurgeable by remove_file/delete_file_data.
        let file_path = if record.node_type == "external" {
            std::sync::Arc::from(crate::graph::pdg::EXTERNAL_NODE_FILE_PATH)
        } else {
            match file_paths.get(record.file_path.as_str()) {
                Some(shared) => std::sync::Arc::clone(shared),
                None => {
                    let shared: std::sync::Arc<str> =
                        std::sync::Arc::from(record.file_path.as_str());
                    file_paths.insert(record.file_path.as_str(), std::sync::Arc::clone(&shared));
                    shared
                }
            }
        };
        let node = PDGNode {
            id: record.id.clone(),
            node_type,
            name: record.name.clone(),
            file_path,
            byte_range: (record.byte_start, record.byte_end),
            complexity: record.complexity,
            language: record.language.clone(),
        };
        let nid = pdg.add_node(node);
        node_ids.insert(record.id.clone(), nid);
    }

    for record in &segment.edges {
        if let (Some(&from), Some(&to)) =
            (node_ids.get(&record.source), node_ids.get(&record.target))
        {
            if let Ok(edge) = edge_from_record(record) {
                pdg.add_edge(from, to, edge);
            }
        }
    }

    pdg
}

/// Serialize a PDG segment for CAS staging.
pub fn serialize_pdg_segment(segment: &PdgSegment) -> Result<Vec<u8>> {
    Ok(bincode::serialize(segment)?)
}

/// Deserialize a PDG segment from a CAS blob.
pub fn deserialize_pdg_segment(data: &[u8]) -> Result<PdgSegment> {
    Ok(bincode::deserialize(data)?)
}

#[cfg(all(test, feature = "full"))]
mod test {
    use super::*;

    /// VAL-STREAM-004: No whole-PDG clone during merge.
    #[test]
    fn test_merge_no_clone() {
        let fragments: Vec<PdgFragment> = (0..5)
            .map(|i| PdgFragment {
                nodes: vec![PdgNodeRecord {
                    id: format!("file{i}:func"),
                    node_type: "function".into(),
                    name: "func".into(),
                    file_path: format!("file{i}"),
                    byte_start: 0,
                    byte_end: 100,
                    complexity: 1,
                    language: "rust".into(),
                }],
                intra_edges: vec![],
                cross_file_refs: vec![PdgEdgeRecord {
                    source: format!("file{i}:func"),
                    target: format!("file{}:func", (i + 1) % 5),
                    edge_type: "call".into(),
                    confidence: Some(80),
                    call_count: None,
                    variable_name: None,
                    channel: None,
                    position: None,
                }],
            })
            .collect();

        let (segment, stats) = merge_fragments_to_segment(fragments);

        // All nodes merged
        assert_eq!(segment.nodes.len(), 5);
        assert_eq!(stats.fragments, 5);
        // Symbol table has unique entries per (name, file_path)
        assert_eq!(segment.symbol_table.len(), 5);
        // Cross-file edges resolved against the full node-id set: four of
        // these target fragments that merge LATER in the iteration, which
        // the old single-pass resolution dropped as unresolved.
        assert_eq!(stats.cross_file_resolved, 5);
        assert_eq!(stats.cross_file_unresolved, 0);
        assert_eq!(segment.edges.len(), 5);
    }

    /// Cross-file resolution must not depend on fragment order: every edge
    /// targets a node id that exists somewhere in the merge, so *all* of
    /// them resolve regardless of which file is merged first. The previous
    /// keyed lookup used a `{name}:{file_path}` namespace that never matched
    /// an edge target, and the fallback scan only saw fragments merged so
    /// far — forward references were permanently dropped.
    #[test]
    fn test_cross_file_resolution_is_order_independent() {
        let make_fragment = |file: &str, target: &str| PdgFragment {
            nodes: vec![PdgNodeRecord {
                id: format!("{file}:func"),
                node_type: "function".into(),
                name: "func".into(),
                file_path: file.to_string(),
                byte_start: 0,
                byte_end: 100,
                complexity: 1,
                language: "rust".into(),
            }],
            intra_edges: vec![],
            cross_file_refs: vec![PdgEdgeRecord {
                source: format!("{file}:func"),
                target: target.to_string(),
                edge_type: "call".into(),
                confidence: Some(80),
                call_count: None,
                variable_name: None,
                channel: None,
                position: None,
            }],
        };

        // Caller in a.rs targets a node defined in z.rs (a "forward"
        // reference across the merge order), and vice versa.
        let fragments = vec![
            make_fragment("a.rs", "z.rs:func"),
            make_fragment("z.rs", "a.rs:func"),
        ];
        let (segment, stats) = merge_fragments_to_segment(fragments);
        assert_eq!(stats.cross_file_resolved, 2, "both directions resolve");
        assert_eq!(stats.cross_file_unresolved, 0);
        assert_eq!(segment.edges.len(), 2);

        // Reversed merge order: identical outcome.
        let fragments = vec![
            make_fragment("z.rs", "a.rs:func"),
            make_fragment("a.rs", "z.rs:func"),
        ];
        let (_, stats) = merge_fragments_to_segment(fragments);
        assert_eq!(stats.cross_file_resolved, 2);
        assert_eq!(stats.cross_file_unresolved, 0);
    }

    /// Per-file fragments each create their own `external::{target}`
    /// placeholder; the merged segment must keep exactly one record per node
    /// id (first wins) so the materialized graph cannot carry duplicate ids
    /// that the store's `(project_id, node_id)` key would collapse onto a
    /// single row. Edges from every duplicate-holder must survive, resolved to the
    /// one shared node.
    #[test]
    fn test_merge_dedupes_external_placeholders_by_node_id() {
        let make_fragment = |file: &str| PdgFragment {
            nodes: vec![
                PdgNodeRecord {
                    id: format!("{file}:main"),
                    node_type: "function".into(),
                    name: "main".into(),
                    file_path: file.to_string(),
                    byte_start: 0,
                    byte_end: 100,
                    complexity: 1,
                    language: "rust".into(),
                },
                PdgNodeRecord {
                    id: "external::String".to_string(),
                    node_type: "external".into(),
                    name: "String".into(),
                    file_path: file.to_string(),
                    byte_start: 0,
                    byte_end: 0,
                    complexity: 0,
                    language: "external".into(),
                },
            ],
            intra_edges: vec![PdgEdgeRecord {
                source: format!("{file}:main"),
                target: "external::String".to_string(),
                edge_type: "call".into(),
                confidence: None,
                call_count: None,
                variable_name: None,
                channel: None,
                position: None,
            }],
            cross_file_refs: vec![],
        };

        let fragments = vec![make_fragment("a.rs"), make_fragment("b.rs")];
        let (segment, stats) = merge_fragments_to_segment(fragments);

        assert_eq!(segment.nodes.len(), 3, "two callers + one shared external");
        assert_eq!(segment.symbol_table.len(), 3);
        assert_eq!(stats.node_count, 3);
        assert_eq!(
            stats.duplicate_nodes_dropped, 1,
            "b.rs's external placeholder folded onto a.rs's"
        );
        assert_eq!(segment.edges.len(), 2, "both callers' edges survive");

        let pdg = pdg_from_segment(&segment);
        assert_eq!(pdg.node_count(), 3);
        let external = pdg
            .find_by_id("external::String")
            .expect("shared external materialized once");
        assert_eq!(
            pdg.get_node(external).unwrap().file_path.as_ref(),
            "<external>",
            "shared placeholder is not owned by any file"
        );
    }

    #[test]
    fn test_pdg_segment_roundtrip() {
        let segment = PdgSegment {
            nodes: vec![PdgNodeRecord {
                id: "a:foo".into(),
                node_type: "function".into(),
                name: "foo".into(),
                file_path: "a.rs".into(),
                byte_start: 0,
                byte_end: 10,
                complexity: 2,
                language: "rust".into(),
            }],
            edges: vec![PdgEdgeRecord {
                source: "a:foo".into(),
                target: "a:bar".into(),
                edge_type: "call".into(),
                confidence: Some(90),
                call_count: None,
                variable_name: None,
                channel: None,
                position: None,
            }],
            symbol_table: vec![InternedSymbol {
                name: "foo".into(),
                file_path: "a.rs".into(),
                node_id: "a:foo".into(),
            }],
        };
        let bytes = serialize_pdg_segment(&segment).unwrap();
        let back = deserialize_pdg_segment(&bytes).unwrap();
        assert_eq!(back, segment);
    }

    #[test]
    fn test_merge_empty_fragments() {
        let (segment, _stats) = merge_fragments_to_segment(Vec::new());
        assert!(segment.nodes.is_empty());
        assert!(segment.edges.is_empty());
        assert!(segment.symbol_table.is_empty());
    }

    #[test]
    fn test_symbol_table_dedup() {
        // Same symbol name in same file should produce one entry
        let fragments = vec![
            PdgFragment {
                nodes: vec![PdgNodeRecord {
                    id: "a:foo".into(),
                    node_type: "function".into(),
                    name: "foo".into(),
                    file_path: "a.rs".into(),
                    byte_start: 0,
                    byte_end: 10,
                    complexity: 0,
                    language: "rust".into(),
                }],
                intra_edges: vec![],
                cross_file_refs: vec![],
            },
            PdgFragment {
                nodes: vec![PdgNodeRecord {
                    id: "a:foo".into(),
                    node_type: "function".into(),
                    name: "foo".into(),
                    file_path: "a.rs".into(),
                    byte_start: 0,
                    byte_end: 10,
                    complexity: 0,
                    language: "rust".into(),
                }],
                intra_edges: vec![],
                cross_file_refs: vec![],
            },
        ];
        let (segment, _) = merge_fragments_to_segment(fragments);
        // Same (name, file_path) → one interned entry
        assert_eq!(segment.symbol_table.len(), 1);
    }

    /// VAL-PDG-005/006: `fragment_from_pdg` → `merge_fragments_to_segment` →
    /// `pdg_from_segment` round-trips a graph without losing nodes, edges,
    /// edge metadata, or internal indexing, and does so without cloning graph
    /// weights into a `SerializablePDG` intermediate.
    #[test]
    fn test_pdg_segment_roundtrip_preserves_graph_and_metadata() {
        use crate::graph::pdg::{Edge as GEdge, EdgeMetadata, EdgeType, Node, NodeType};

        let mut pdg = ProgramDependenceGraph::new();
        let a = pdg.add_node(Node {
            id: "a.rs:foo".into(),
            node_type: NodeType::Function,
            name: "foo".into(),
            file_path: std::sync::Arc::from("a.rs"),
            byte_range: (0, 10),
            complexity: 2,
            language: "rust".into(),
        });
        let b = pdg.add_node(Node {
            id: "a.rs:Bar".into(),
            node_type: NodeType::Class,
            name: "Bar".into(),
            file_path: std::sync::Arc::from("a.rs"),
            byte_range: (11, 20),
            complexity: 0,
            language: "rust".into(),
        });
        pdg.add_edge(
            a,
            b,
            GEdge {
                edge_type: EdgeType::Call,
                metadata: EdgeMetadata {
                    call_count: Some(3),
                    variable_name: Some("x".into()),
                    confidence: Some(0.75),
                    channel: Some("arg".into()),
                    position: Some(1),
                },
            },
        );

        let fragment = fragment_from_pdg(&pdg);
        assert_eq!(fragment.nodes.len(), 2);
        assert_eq!(fragment.intra_edges.len(), 1);

        let (segment, stats) = merge_fragments_to_segment(vec![fragment]);
        assert_eq!(stats.node_count, 2);
        assert_eq!(stats.edge_count, 1);

        let restored = pdg_from_segment(&segment);
        assert_eq!(restored.node_count(), 2);
        assert_eq!(restored.edge_count(), 1);

        // Nodes are fully preserved (incl. non-id fields).
        let foo = restored.find_by_symbol("a.rs:foo").expect("foo present");
        let node = restored.get_node(foo).unwrap();
        assert_eq!(node.node_type, NodeType::Function);
        assert_eq!(node.complexity, 2);
        assert_eq!(node.file_path.as_ref(), "a.rs");

        // Edge + full metadata preserved (confidence quantized to 0-100, back).
        let edges: Vec<_> = restored
            .edge_indices()
            .filter_map(|idx| restored.get_edge(idx))
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].edge_type, EdgeType::Call);
        assert_eq!(edges[0].metadata.call_count, Some(3));
        assert_eq!(edges[0].metadata.variable_name.as_deref(), Some("x"));
        assert_eq!(edges[0].metadata.channel.as_deref(), Some("arg"));
        assert_eq!(edges[0].metadata.position, Some(1));
        assert_eq!(edges[0].metadata.confidence, Some(0.75));
    }
}
