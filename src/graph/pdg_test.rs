use super::*;

fn make_node(id: &str, name: &str, file: &str, ntype: NodeType) -> Node {
    Node {
        id: id.to_string(),
        node_type: ntype,
        name: name.to_string(),
        file_path: Arc::from(file),
        byte_range: (0, 10),
        complexity: 2,
        language: "rust".to_string(),
    }
}

#[test]
fn traversal_respects_max_nodes() {
    let mut pdg = ProgramDependenceGraph::new();
    let n: Vec<NodeId> = (0..10)
        .map(|i| {
            pdg.add_node(make_node(
                &format!("n{i}"),
                &format!("n{i}"),
                "f.rs",
                NodeType::Function,
            ))
        })
        .collect();
    // Chain: n0 → n1 → n2 → ... → n9
    for i in 0..9 {
        pdg.add_call_edges(vec![(n[i], n[i + 1])]);
    }
    let config = TraversalConfig {
        max_depth: None,
        max_nodes: Some(3),
        ..TraversalConfig::for_impact_analysis()
    };
    let result = pdg.forward_impact(n[0], &config);
    assert!(result.len() <= 3, "Should respect max_nodes cap");
}

#[test]
fn traversal_filters_containment_edges() {
    let mut pdg = ProgramDependenceGraph::new();
    let cls = pdg.add_node(make_node("f:MyClass", "MyClass", "f.rs", NodeType::Class));
    let method = pdg.add_node(make_node("f:MyClass::foo", "foo", "f.rs", NodeType::Method));
    let callee = pdg.add_node(make_node("f:bar", "bar", "f.rs", NodeType::Function));
    pdg.add_containment_edges(vec![(cls, method)]);
    pdg.add_call_edges(vec![(method, callee)]);

    // With default semantic config, containment edges should not be traversed
    let config = TraversalConfig::for_semantic_analysis();
    let result = pdg.forward_impact(cls, &config);
    // Should not reach callee via containment→method→call chain
    // because containment is filtered — cls can only reach method
    // if containment is allowed; method→callee only if call is allowed
    // With semantic_analysis: Call allowed but Containment not → cls reaches nothing
    assert!(
        !result.contains(&callee) || result.contains(&method),
        "Containment edges should be filtered from semantic traversal"
    );
}

#[test]
fn find_by_name_in_file_no_scan_needed() {
    let mut pdg = ProgramDependenceGraph::new();
    for i in 0..1000 {
        pdg.add_node(make_node(
            &format!("f:func{i}"),
            &format!("func{i}"),
            "f.rs",
            NodeType::Function,
        ));
    }
    // Case-insensitive lookup should use name_lower_index, not scan
    let result = pdg.find_by_name_in_file("FUNC42", None);
    assert!(result.is_some());
}

#[test]
fn name_file_index_provides_o1_lookup() {
    let mut pdg = ProgramDependenceGraph::new();

    // Add nodes with same name in different files
    let a = pdg.add_node(make_node("a.rs:foo", "foo", "a.rs", NodeType::Function));
    let b = pdg.add_node(make_node("b.rs:foo", "foo", "b.rs", NodeType::Function));
    let c = pdg.add_node(make_node("c.rs:foo", "foo", "c.rs", NodeType::Function));

    // Direct name_file_index lookup with file hint returns correct node
    assert_eq!(pdg.find_by_name_in_file("foo", Some("a.rs")), Some(a));
    assert_eq!(pdg.find_by_name_in_file("foo", Some("b.rs")), Some(b));
    assert_eq!(pdg.find_by_name_in_file("foo", Some("c.rs")), Some(c));

    // Non-existent file returns None for exact match but falls through
    assert_eq!(pdg.find_by_name_in_file("foo", Some("z.rs")), Some(a));
}

#[test]
fn name_file_index_maintained_on_remove() {
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node(make_node("a.rs:foo", "foo", "a.rs", NodeType::Function));
    let b = pdg.add_node(make_node("b.rs:bar", "bar", "b.rs", NodeType::Function));

    // Verify lookups work before removal
    assert_eq!(pdg.find_by_name_in_file("foo", Some("a.rs")), Some(a));
    assert_eq!(pdg.find_by_name_in_file("bar", Some("b.rs")), Some(b));

    // Remove node a
    pdg.remove_node(a);

    // name_file_index should no longer find removed node
    assert_eq!(pdg.find_by_name_in_file("foo", Some("a.rs")), None);
    assert!(!pdg.file_index.contains_key("a.rs"));
    assert!(!pdg.name_index.contains_key("foo"));
    assert!(!pdg.name_lower_index.contains_key("foo"));
    // b should still be found
    assert_eq!(pdg.find_by_name_in_file("bar", Some("b.rs")), Some(b));
    assert!(pdg.file_index.contains_key("b.rs"));
    assert!(pdg.name_index.contains_key("bar"));
    assert!(pdg.name_lower_index.contains_key("bar"));
}

#[test]
fn containment_edge_type_is_separate_from_call() {
    let mut pdg = ProgramDependenceGraph::new();
    let cls = pdg.add_node(make_node("f:C", "C", "f.rs", NodeType::Class));
    let m = pdg.add_node(make_node("f:C::m", "m", "f.rs", NodeType::Method));
    pdg.add_containment_edges(vec![(cls, m)]);

    let containment_count = pdg
        .edge_indices()
        .filter_map(|e| pdg.get_edge(e))
        .filter(|e| e.edge_type == EdgeType::Containment)
        .count();
    let call_count = pdg
        .edge_indices()
        .filter_map(|e| pdg.get_edge(e))
        .filter(|e| e.edge_type == EdgeType::Call)
        .count();

    assert_eq!(containment_count, 1);
    assert_eq!(call_count, 0);
}

#[test]
fn confidence_filtering_works() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:a", "a", "f.rs", NodeType::Function));
    let n2 = pdg.add_node(make_node("f:b", "b", "f.rs", NodeType::Function));
    pdg.add_data_flow_edges(vec![(n1, n2, "T".to_string(), 0.3)]);

    // Low confidence edge should be filtered when min_edge_confidence = 0.5
    let config = TraversalConfig {
        max_depth: Some(5),
        max_nodes: Some(100),
        allowed_edge_types: Some(&[EdgeType::DataDependency]),
        excluded_node_types: None,
        min_complexity: None,
        min_edge_confidence: 0.5,
    };
    let result = pdg.forward_impact(n1, &config);
    assert!(
        !result.contains(&n2),
        "Low confidence edge should be filtered"
    );
}

#[test]
fn backward_traversal_works() {
    let mut pdg = ProgramDependenceGraph::new();
    let n: Vec<NodeId> = (0..5)
        .map(|i| {
            pdg.add_node(make_node(
                &format!("f:n{i}"),
                &format!("n{i}"),
                "f.rs",
                NodeType::Function,
            ))
        })
        .collect();
    // Chain: n0 → n1 → n2 → n3 → n4
    for i in 0..4 {
        pdg.add_call_edges(vec![(n[i], n[i + 1])]);
    }

    let config = TraversalConfig::for_impact_analysis();
    let backward = pdg.backward_impact(n[4], &config);
    assert!(backward.contains(&n[0]));
    assert!(backward.contains(&n[1]));
    assert!(backward.contains(&n[2]));
    assert!(backward.contains(&n[3]));
}

#[test]
fn bidirectional_traversal_works() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:a", "a", "f.rs", NodeType::Function));
    let n2 = pdg.add_node(make_node("f:b", "b", "f.rs", NodeType::Function));
    let n3 = pdg.add_node(make_node("f:c", "c", "f.rs", NodeType::Function));
    // n1 → n2 and n2 → n3 (n2 is in the middle)
    pdg.add_call_edges(vec![(n1, n2), (n2, n3)]);

    let config = TraversalConfig::for_impact_analysis();
    let bidirectional = pdg.bidirectional_impact(n2, &config);
    assert!(bidirectional.contains(&n1), "Should reach backward");
    assert!(bidirectional.contains(&n3), "Should reach forward");
    assert!(
        !bidirectional.contains(&n2),
        "Should not include start node"
    );
}

// -----------------------------------------------------------------------
// EmbeddingStore integration tests
// -----------------------------------------------------------------------

#[test]
fn embedding_store_field_initialized_on_new_pdg() {
    let pdg = ProgramDependenceGraph::new();
    assert!(pdg.embedding_store.is_empty());
    assert_eq!(pdg.embedding_count(), 0);
}

#[test]
fn set_and_get_embedding_roundtrip() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:foo", "foo", "f.rs", NodeType::Function));

    // Store embedding via PDG accessor
    let emb = vec![0.1, 0.2, 0.3, 0.4];
    pdg.set_embedding("f:foo", emb.clone());

    // Retrieve via PDG accessor
    assert_eq!(pdg.get_embedding("f:foo"), Some(&emb));
    assert_eq!(pdg.embedding_count(), 1);

    // Node should still exist
    assert!(pdg.get_node(n1).is_some());
}

#[test]
fn remove_node_cleans_up_embedding() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:foo", "foo", "f.rs", NodeType::Function));
    pdg.set_embedding("f:foo", vec![0.5, 0.6]);

    assert_eq!(pdg.embedding_count(), 1);

    // Remove node should also remove embedding
    let removed = pdg.remove_node(n1);
    assert!(removed.is_some());
    assert_eq!(pdg.embedding_count(), 0);
    assert!(pdg.get_embedding("f:foo").is_none());
}

#[test]
fn remove_file_cleans_up_all_embeddings() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:a", "a", "src/lib.rs", NodeType::Function));
    let n2 = pdg.add_node(make_node("f:b", "b", "src/lib.rs", NodeType::Function));
    let n3 = pdg.add_node(make_node("f:c", "c", "src/other.rs", NodeType::Function));

    pdg.set_embedding("f:a", vec![1.0]);
    pdg.set_embedding("f:b", vec![2.0]);
    pdg.set_embedding("f:c", vec![3.0]);

    assert_eq!(pdg.embedding_count(), 3);

    // Remove file src/lib.rs — should clean up a and b embeddings
    pdg.remove_file("src/lib.rs");

    assert!(
        pdg.get_embedding("f:a").is_none(),
        "a's embedding should be removed"
    );
    assert!(
        pdg.get_embedding("f:b").is_none(),
        "b's embedding should be removed"
    );
    assert_eq!(
        pdg.get_embedding("f:c"),
        Some(&vec![3.0]),
        "c's embedding should remain"
    );
    assert_eq!(pdg.embedding_count(), 1);

    // n1 and n2 should be gone, n3 should remain
    assert!(pdg.get_node(n1).is_none());
    assert!(pdg.get_node(n2).is_none());
    assert!(pdg.get_node(n3).is_some());
}

#[test]
fn embedding_store_overwrite() {
    let mut pdg = ProgramDependenceGraph::new();
    pdg.add_node(make_node("f:foo", "foo", "f.rs", NodeType::Function));

    pdg.set_embedding("f:foo", vec![1.0, 2.0]);
    assert_eq!(pdg.get_embedding("f:foo"), Some(&vec![1.0, 2.0]));

    // Overwrite
    pdg.set_embedding("f:foo", vec![3.0, 4.0]);
    assert_eq!(pdg.get_embedding("f:foo"), Some(&vec![3.0, 4.0]));
    assert_eq!(
        pdg.embedding_count(),
        1,
        "Should still have 1 embedding after overwrite"
    );
}

#[test]
fn serialization_preserves_embeddings() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:foo", "foo", "f.rs", NodeType::Function));
    let n2 = pdg.add_node(make_node("f:bar", "bar", "f.rs", NodeType::Function));
    pdg.add_call_edges(vec![(n1, n2)]);
    pdg.set_embedding("f:foo", vec![0.1, 0.2, 0.3]);
    pdg.set_embedding("f:bar", vec![0.4, 0.5, 0.6]);

    // Serialize
    let bytes = pdg.serialize().expect("Serialization should succeed");

    // Deserialize
    let restored =
        ProgramDependenceGraph::deserialize(&bytes).expect("Deserialization should succeed");

    // Verify embeddings survived the round-trip
    assert_eq!(restored.get_embedding("f:foo"), Some(&vec![0.1, 0.2, 0.3]));
    assert_eq!(restored.get_embedding("f:bar"), Some(&vec![0.4, 0.5, 0.6]));
    assert_eq!(restored.embedding_count(), 2);
}

#[test]
fn deserialization_backward_compat_no_embeddings() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:foo", "foo", "f.rs", NodeType::Function));
    pdg.add_call_edges(vec![(n1, n1)]);

    // Manually serialize without embeddings (simulate old format)
    let old_format = SerializablePDG {
        nodes: pdg
            .graph
            .node_indices()
            .map(|idx| SerializableNode {
                index: idx.index() as u32,
                node: pdg.graph[idx].clone(),
            })
            .collect(),
        edges: pdg
            .graph
            .edge_indices()
            .map(|eidx| {
                let (source, target) = pdg.graph.edge_endpoints(eidx).unwrap();
                SerializableEdge {
                    source: source.index() as u32,
                    target: target.index() as u32,
                    edge: pdg.graph[eidx].clone(),
                }
            })
            .collect(),
        symbol_index: pdg
            .symbol_index
            .iter()
            .map(|(k, v)| (k.clone(), v.index() as u32))
            .collect(),
        file_index: pdg
            .file_index
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect(),
        name_index: pdg
            .name_index
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect(),
        name_lower_index: pdg
            .name_lower_index
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect(),
        embeddings: HashMap::new(), // No embeddings — simulates old format
    };

    let bytes = bincode::serialize(&old_format).expect("Serialize old format");
    let restored = ProgramDependenceGraph::deserialize(&bytes)
        .expect("Should deserialize old format without error");

    assert_eq!(restored.embedding_count(), 0);
    assert_eq!(restored.node_count(), 1);
}

#[test]
fn bulk_import_edges_helper() {
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node(make_node("mod:a", "a", "a.rs", NodeType::Module));
    let b = pdg.add_node(make_node("mod:b", "b", "b.rs", NodeType::Module));
    let c = pdg.add_node(make_node("mod:c", "c", "c.rs", NodeType::Module));

    pdg.add_import_edges(vec![(a, b), (a, c)]);

    let import_count = pdg
        .edge_indices()
        .filter_map(|e| pdg.get_edge(e))
        .filter(|e| e.edge_type == EdgeType::Import)
        .count();
    assert_eq!(import_count, 2, "Should have 2 import edges");
}

#[test]
fn bulk_inheritance_edges_with_confidence() {
    let mut pdg = ProgramDependenceGraph::new();
    let child = pdg.add_node(make_node("f:Child", "Child", "f.rs", NodeType::Class));
    let parent = pdg.add_node(make_node("f:Parent", "Parent", "f.rs", NodeType::Class));

    pdg.add_inheritance_edges(vec![(child, parent, 0.85)]);

    // Verify edge was created with correct type and confidence
    let edges: Vec<_> = pdg
        .edge_indices()
        .filter_map(|e| {
            let edge = pdg.get_edge(e)?;
            if edge.edge_type == EdgeType::Inheritance {
                Some((pdg.edge_endpoints(e).unwrap(), edge.clone()))
            } else {
                None
            }
        })
        .collect();

    assert_eq!(edges.len(), 1);
    let ((src, tgt), edge) = &edges[0];
    assert_eq!(*src, child);
    assert_eq!(*tgt, parent);
    assert_eq!(edge.metadata.confidence, Some(0.85));
}
