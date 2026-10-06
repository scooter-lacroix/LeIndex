//! D1 round-trip tests for the lossless graph codec: graph → v2 payload →
//! CAS blob → `PdgReader` → graph, compared with the canonical-PDG helper.

use super::*;
use crate::graph::pdg::{EdgeMetadata, Node, NodeType as GraphNodeType, ProgramDependenceGraph};
use crate::storage::cas::blob::encode_blob;
use crate::storage::generation::reader::PdgReader;
use rusqlite::Connection;
use std::sync::Arc;

/// Canonical, order-stable serialization of a PDG so two graphs can be
/// compared byte-for-byte regardless of internal node-id ordering.
///
/// Shared helper (moved from `cli/leindex/generation_read_test.rs` per the
/// step-4 spec: one canonical comparison, no duplication).
pub(crate) fn canonical_pdg(pdg: &ProgramDependenceGraph) -> Vec<u8> {
    use petgraph::visit::{EdgeRef, IntoEdgeReferences};
    // Key nodes by their stable string id and edges by endpoint ids, so two
    // logically identical graphs compare equal regardless of petgraph's
    // internal NodeIndex assignment (SQL rowid order vs sorted layer order).
    let mut nodes: Vec<(&str, &str, String, String, (usize, usize), u32)> = Vec::new();
    for idx in pdg.node_indices() {
        if let Some(n) = pdg.get_node(idx) {
            nodes.push((
                n.id.as_str(),
                n.name.as_str(),
                format!("{:?}", n.node_type),
                n.file_path.to_string(),
                n.byte_range,
                n.complexity,
            ));
        }
    }
    nodes.sort();
    let mut edges: Vec<(&str, &str, String)> = Vec::new();
    for e in pdg.graph.edge_references() {
        if let (Some(source), Some(target)) = (
            pdg.get_node(e.source()).map(|n| n.id.as_str()),
            pdg.get_node(e.target()).map(|n| n.id.as_str()),
        ) {
            edges.push((source, target, format!("{:?}", e.weight().edge_type)));
        }
    }
    edges.sort();
    serde_json::to_vec(&(nodes, edges, pdg.node_count(), pdg.edge_count())).unwrap()
}

fn round_trip(pdg: &ProgramDependenceGraph) -> ProgramDependenceGraph {
    let (payload, assignment) = encode_pdg_v2_from_graph(pdg).expect("encode");
    // Duplicate stable ids collapse; otherwise the assignment is total.
    let distinct = {
        let mut ids: Vec<String> = pdg
            .node_indices()
            .filter_map(|idx| pdg.get_node(idx).map(|node| node.id.clone()))
            .collect();
        ids.sort();
        ids.dedup();
        ids.len()
    };
    assert_eq!(assignment.len(), distinct);
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join("pdg_v2.blob");
    std::fs::write(&blob_path, encode_blob(&payload)).expect("write blob");
    let reader = PdgReader::open(&blob_path).expect("open reader");
    reader.to_program_dependence_graph().expect("decode")
}

fn node(id: &str, name: &str, file: &str, node_type: GraphNodeType) -> Node {
    Node {
        id: id.to_string(),
        node_type,
        name: name.to_string(),
        file_path: Arc::from(file),
        byte_range: (0, 0),
        complexity: 0,
        language: "rust".to_string(),
    }
}

#[test]
fn test_round_trip_preserves_every_node_field() {
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node_without_trigrams(Node {
        id: "src/a.rs:alpha".to_string(),
        node_type: GraphNodeType::Function,
        name: "alpha".to_string(),
        file_path: Arc::from("src/a.rs"),
        byte_range: (120, 480),
        complexity: 7,
        language: "rust".to_string(),
    });
    let b = pdg.add_node_without_trigrams(Node {
        id: "src/b.rs:Beta".to_string(),
        node_type: GraphNodeType::Class,
        name: "Beta".to_string(),
        file_path: Arc::from("src/b.rs"),
        byte_range: (0, u32::MAX as usize),
        complexity: 0,
        language: "python".to_string(),
    });
    let c = pdg.add_node_without_trigrams(Node {
        id: "src/c.rst:Overview".to_string(),
        node_type: GraphNodeType::DocSection,
        name: "Overview".to_string(),
        file_path: Arc::from("src/c.rst"),
        byte_range: (10, 20),
        complexity: 2,
        language: "rst".to_string(),
    });
    pdg.mark_precision_symbol("src/a.rs:alpha");
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::Call,
            metadata: EdgeMetadata {
                call_count: Some(4),
                variable_name: Some("x".into()),
                confidence: Some(0.7),
                channel: Some("env".into()),
                position: Some(2),
            },
        },
    );
    pdg.add_edge(
        b,
        c,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::Containment,
            metadata: EdgeMetadata::empty(),
        },
    );

    let decoded = round_trip(&pdg);
    assert_eq!(decoded.node_count(), 3);
    assert_eq!(decoded.edge_count(), 2);
    assert_eq!(canonical_pdg(&decoded), canonical_pdg(&pdg));
    // Node fields survive individually.
    let alpha = decoded.find_by_id("src/a.rs:alpha").expect("alpha");
    let alpha_node = decoded.get_node(alpha).expect("alpha node");
    assert_eq!(alpha_node.name, "alpha");
    assert_eq!(alpha_node.file_path.to_string(), "src/a.rs");
    assert_eq!(alpha_node.byte_range, (120, 480));
    assert_eq!(alpha_node.complexity, 7);
    assert_eq!(alpha_node.language, "rust");
    // Precision markers survive.
    assert!(decoded.is_precision_symbol("src/a.rs:alpha"));
    assert!(!decoded.is_precision_symbol("src/b.rs:Beta"));
    // Edge metadata: all-present edge keeps every field.
    let alpha_to_beta = decoded
        .edge_indices()
        .find(|&edge| {
            decoded.edge_endpoints(edge).is_some_and(|(s, t)| {
                s == alpha && decoded.get_node(t).is_some_and(|n| n.id == "src/b.rs:Beta")
            })
        })
        .expect("alpha->beta edge");
    let edge = decoded.get_edge(alpha_to_beta).unwrap();
    assert_eq!(edge.metadata.call_count, Some(4));
    assert_eq!(edge.metadata.variable_name.as_deref(), Some("x"));
    assert_eq!(edge.metadata.confidence, Some(0.7));
    assert_eq!(edge.metadata.channel.as_deref(), Some("env"));
    assert_eq!(edge.metadata.position, Some(2));
}

#[test]
fn test_round_trip_all_absent_edge_metadata() {
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node_without_trigrams(node("f:a", "a", "f", GraphNodeType::Function));
    let b = pdg.add_node_without_trigrams(node("f:b", "b", "f", GraphNodeType::Function));
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::DataDependency,
            metadata: EdgeMetadata::empty(),
        },
    );
    let decoded = round_trip(&pdg);
    let edge = decoded
        .get_edge(decoded.edge_indices().next().unwrap())
        .unwrap();
    assert_eq!(edge.metadata.call_count, None);
    assert_eq!(edge.metadata.variable_name, None);
    assert_eq!(edge.metadata.confidence, None);
    assert_eq!(edge.metadata.channel, None);
    assert_eq!(edge.metadata.position, None);
}

#[test]
fn test_round_trip_confidence_nan_sentinel() {
    // NaN is the absence sentinel on the wire: a stored Some(NaN) decodes to
    // None, exactly like a stored None. NaN carries no confidence value
    // (NaN != NaN), so collapsing it to absence is the format's correct
    // behavior, not a silent drop.
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node_without_trigrams(node("f:a", "a", "f", GraphNodeType::Function));
    let b = pdg.add_node_without_trigrams(node("f:b", "b", "f", GraphNodeType::Function));
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::Inheritance,
            metadata: EdgeMetadata {
                confidence: Some(f32::NAN),
                ..EdgeMetadata::empty()
            },
        },
    );
    let decoded = round_trip(&pdg);
    let edge = decoded
        .get_edge(decoded.edge_indices().next().unwrap())
        .unwrap();
    assert_eq!(edge.metadata.confidence, None);
}

#[test]
fn test_round_trip_duplicate_node_id_last_wins() {
    let mut pdg = ProgramDependenceGraph::new();
    let first = pdg.add_node_without_trigrams(Node {
        id: "f:dup".to_string(),
        node_type: GraphNodeType::Function,
        name: "first".to_string(),
        file_path: Arc::from("f.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });
    let second = pdg.add_node_without_trigrams(Node {
        id: "f:dup".to_string(),
        node_type: GraphNodeType::Method,
        name: "second".to_string(),
        file_path: Arc::from("f.rs"),
        byte_range: (20, 30),
        complexity: 9,
        language: "rust".to_string(),
    });
    pdg.mark_precision_symbol("f:dup");
    pdg.add_edge(
        first,
        second,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::Call,
            metadata: EdgeMetadata::empty(),
        },
    );

    let (payload, assignment) = encode_pdg_v2_from_graph(&pdg).expect("encode");
    // One collapsed record, not two.
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join("dup.blob");
    std::fs::write(&blob_path, encode_blob(&payload)).expect("write blob");
    let reader = PdgReader::open(&blob_path).expect("open reader");
    assert_eq!(reader.num_nodes(), 1, "duplicate stable id collapses");
    assert_eq!(assignment.len(), 1);
    let decoded = reader.to_program_dependence_graph().expect("decode");
    // Last record wins (petgraph insertion order = iteration order here).
    let only = decoded
        .get_node(decoded.node_indices().next().unwrap())
        .unwrap();
    assert_eq!(only.name, "second");
    assert_eq!(only.node_type, GraphNodeType::Method);
    assert_eq!(only.complexity, 9);
    // The surviving record keeps the precision marker keyed by stable id.
    assert!(decoded.is_precision_symbol("f:dup"));
    // Both duplicates' edges survive, pointing at the surviving record.
    assert_eq!(decoded.edge_count(), 1);
}

#[test]
fn test_round_trip_rejects_unknown_node_type_code() {
    // Hand-craft a v2 payload with an out-of-vocabulary node type code and
    // assert decode refuses instead of guessing.
    let mut pdg = ProgramDependenceGraph::new();
    pdg.add_node_without_trigrams(node("f:a", "a", "f", GraphNodeType::Function));
    let (mut payload, _) = encode_pdg_v2_from_graph(&pdg).expect("encode");
    // node record 0 begins at PDG_HEADER_LEN; node_type is the 5th u32.
    let type_offset = PDG_HEADER_LEN + 4 * 4;
    let mut code = [0u8; 4];
    code.copy_from_slice(&payload[type_offset..type_offset + 4]);
    assert_eq!(u32::from_le_bytes(code), 1);
    payload[type_offset..type_offset + 4].copy_from_slice(&99u32.to_le_bytes());
    // Re-hash: the reader validates the content hash over the data region.
    let data = payload[PDG_HEADER_LEN..].to_vec();
    let hash = crate::storage::cas::blob::blob_hash(&data);
    payload[29..61].copy_from_slice(&hash);

    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join("bad_type.blob");
    std::fs::write(&blob_path, encode_blob(&payload)).expect("write blob");
    let reader = PdgReader::open(&blob_path).expect("open reader");
    let error = match reader.to_program_dependence_graph() {
        Ok(_) => panic!("unknown type code must error"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("unknown node type code"),
        "got: {error}"
    );
}

#[test]
fn test_collapse_for_layer_matches_round_trip() {
    // collapse_for_layer is the in-memory form of what a layer round-trip
    // reconstructs; persisted_search_identity fingerprints it, so the two
    // must agree exactly.
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node_without_trigrams(node("f:a", "a", "f", GraphNodeType::Function));
    let b = pdg.add_node_without_trigrams(node("f:b", "b", "f", GraphNodeType::Function));
    // Duplicate id: last wins.
    pdg.add_node_without_trigrams(Node {
        id: "f:a".to_string(),
        node_type: GraphNodeType::Method,
        name: "a-later".to_string(),
        file_path: Arc::from("f.rs"),
        byte_range: (5, 9),
        complexity: 3,
        language: "rust".to_string(),
    });
    pdg.mark_precision_symbol("f:b");
    // Parallel same-type edges: last wins. Different type survives.
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::DataDependency,
            metadata: EdgeMetadata {
                call_count: Some(1),
                ..EdgeMetadata::empty()
            },
        },
    );
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::DataDependency,
            metadata: EdgeMetadata {
                call_count: Some(7),
                variable_name: Some("v".into()),
                ..EdgeMetadata::empty()
            },
        },
    );

    let decoded = round_trip(&pdg);
    let collapsed = super::super::graph_codec::collapse_for_layer(&pdg);
    assert_eq!(
        canonical_pdg(&collapsed),
        canonical_pdg(&decoded),
        "collapse_for_layer must match the layer round-trip form"
    );
    assert_eq!(collapsed.node_count(), decoded.node_count());
    assert_eq!(collapsed.edge_count(), decoded.edge_count());
    assert!(collapsed.is_precision_symbol("f:b"));
}

#[test]
fn test_parallel_edges_collapse_last_wins() {
    // The legacy intel_edges PRIMARY KEY (caller, callee, edge_type) dedups
    // parallel same-type edges with last-write-wins metadata; the layer must
    // match so layer-hydrated and SQL-hydrated graphs stay identical.
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node_without_trigrams(node("f:a", "a", "f", GraphNodeType::Function));
    let b = pdg.add_node_without_trigrams(node("f:b", "b", "f", GraphNodeType::Function));
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::DataDependency,
            metadata: EdgeMetadata {
                call_count: Some(1),
                ..EdgeMetadata::empty()
            },
        },
    );
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::DataDependency,
            metadata: EdgeMetadata {
                call_count: Some(9),
                variable_name: Some("last".into()),
                ..EdgeMetadata::empty()
            },
        },
    );
    // A different type between the same pair survives as its own edge.
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::Call,
            metadata: EdgeMetadata::empty(),
        },
    );

    let (payload, _) = encode_pdg_v2_from_graph(&pdg).expect("encode");
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join("parallel.blob");
    std::fs::write(&blob_path, encode_blob(&payload)).expect("write blob");
    let decoded = PdgReader::open(&blob_path)
        .expect("open reader")
        .to_program_dependence_graph()
        .expect("decode");
    assert_eq!(decoded.edge_count(), 2, "parallel same-type edges collapse");
    for edge_id in decoded.edge_indices() {
        let edge = decoded.get_edge(edge_id).unwrap();
        if edge.edge_type == crate::graph::pdg::EdgeType::DataDependency {
            assert_eq!(edge.metadata.call_count, Some(9), "last write wins");
            assert_eq!(edge.metadata.variable_name.as_deref(), Some("last"));
        }
    }
}

#[test]
fn test_assignment_is_deterministic_and_sorted() {
    let mut pdg = ProgramDependenceGraph::new();
    pdg.add_node_without_trigrams(node("zz.rs:z", "z", "zz.rs", GraphNodeType::Module));
    pdg.add_node_without_trigrams(node("aa.rs:a", "a", "aa.rs", GraphNodeType::Module));
    pdg.add_node_without_trigrams(node("mm.rs:m", "m", "mm.rs", GraphNodeType::Module));
    let (_, assignment) = encode_pdg_v2_from_graph(&pdg).expect("encode");
    let ids = assignment.node_ids();
    assert_eq!(ids, ["aa.rs:a", "mm.rs:m", "zz.rs:z"]);
    assert_eq!(assignment.get("aa.rs:a"), Some(0));
    assert_eq!(assignment.get("mm.rs:m"), Some(1));
    assert_eq!(assignment.get("zz.rs:z"), Some(2));
    assert_eq!(assignment.as_map().len(), 3);
}

#[test]
fn test_edge_endpoint_convention_matches_migration_encoder() {
    // A graph encoded from memory and a catalog encoded by the migration
    // encoder must agree on the endpoint convention (interned node-id ids),
    // so layers are interchangeable across producers.
    let tmp = tempfile::tempdir().expect("tempdir");
    let catalog = tmp.path().join("leindex.db");
    write_minimal_catalog(&catalog);
    let conn = Connection::open(&catalog).expect("open");
    let migrated =
        crate::storage::generation::migrate::encode_pdg_layer_v2(&conn).expect("migration encode");
    // Same logical graph from memory.
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node_without_trigrams(node("f:a", "a", "f", GraphNodeType::Function));
    let b = pdg.add_node_without_trigrams(node("f:b", "b", "f", GraphNodeType::Function));
    pdg.add_edge(
        a,
        b,
        crate::graph::pdg::Edge {
            edge_type: crate::graph::pdg::EdgeType::Call,
            metadata: EdgeMetadata::empty(),
        },
    );
    let (from_graph, _) = encode_pdg_v2_from_graph(&pdg).expect("graph encode");
    // Both payloads must decode through the same reader without error; the
    // endpoint convention agreement is what makes the migration blob's edges
    // resolvable by the graph-side decoder.
    let migrated_graph = {
        let path = tmp.path().join("migrated.blob");
        std::fs::write(&path, encode_blob(&migrated)).expect("write");
        PdgReader::open(&path)
            .expect("open migrated")
            .to_program_dependence_graph()
            .expect("decode migrated")
    };
    assert_eq!(migrated_graph.node_count(), 2);
    assert_eq!(migrated_graph.edge_count(), 1);
    assert!(!canonical_pdg(&migrated_graph).is_empty());
    // And the graph-side payload round-trips through the shared decoder.
    let path = tmp.path().join("from_graph.blob");
    std::fs::write(&path, encode_blob(&from_graph)).expect("write");
    let decoded = PdgReader::open(&path)
        .expect("open graph-encoded")
        .to_program_dependence_graph()
        .expect("decode graph-encoded");
    assert_eq!(decoded.node_count(), 2);
    assert_eq!(decoded.edge_count(), 1);
    assert_eq!(canonical_pdg(&decoded), canonical_pdg(&pdg));
}

#[test]
fn test_symbols_layer_from_graph_round_trip() {
    use crate::storage::generation::reader::SymbolReader;
    let mut pdg = ProgramDependenceGraph::new();
    pdg.add_node_without_trigrams(node("f:a", "alpha", "f.rs", GraphNodeType::Function));
    pdg.add_node_without_trigrams(node("f:b", "beta", "f.rs", GraphNodeType::Class));
    let payload = encode_symbols_layer_from_graph(&pdg).expect("encode symbols");
    let tmp = tempfile::tempdir().expect("tempdir");
    let blob_path = tmp.path().join("symbols.blob");
    std::fs::write(&blob_path, encode_blob(&payload)).expect("write blob");
    let reader = SymbolReader::open(&blob_path).expect("open symbols");
    assert_eq!(reader.num_symbols(), 2);
    let s0 = reader.symbol(0).expect("symbol 0");
    assert_eq!(reader.resolve_string(s0.name_id), Some("alpha"));
    assert_eq!(reader.resolve_string(s0.file_path_id), Some("f.rs"));
    let s1 = reader.symbol(1).expect("symbol 1");
    assert_eq!(reader.resolve_string(s1.name_id), Some("beta"));
}

/// Minimal legacy catalog with two nodes and one edge, matching the graph
/// fixture in [`test_edge_endpoint_convention_matches_migration_encoder`].
fn write_minimal_catalog(path: &std::path::Path) {
    let conn = Connection::open(path).expect("open catalog");
    conn.execute_batch(
        r#"
        CREATE TABLE intel_nodes (
            id INTEGER PRIMARY KEY,
            project_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            symbol_name TEXT,
            file_path TEXT,
            language TEXT,
            node_type TEXT,
            complexity INTEGER,
            byte_range_start INTEGER,
            byte_range_end INTEGER,
            precision INTEGER DEFAULT 0
        );
        CREATE TABLE intel_edges (
            id INTEGER PRIMARY KEY,
            project_id TEXT NOT NULL,
            caller_id INTEGER NOT NULL,
            callee_id INTEGER NOT NULL,
            edge_type TEXT NOT NULL,
            metadata TEXT
        );
        INSERT INTO intel_nodes VALUES
            (1, 'p', 'f:a', 'a', 'f', 'rust', 'function', 1, 0, 10, 0),
            (2, 'p', 'f:b', 'b', 'f', 'rust', 'function', 1, 20, 30, 0);
        INSERT INTO intel_edges VALUES
            (1, 'p', 1, 2, 'call', NULL);
        "#,
    )
    .expect("seed catalog");
}
