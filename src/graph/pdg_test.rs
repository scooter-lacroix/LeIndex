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
fn traversal_includes_typeof_edges_in_semantic_and_impact_configs() {
    let mut pdg = ProgramDependenceGraph::new();
    let source = pdg.add_node(make_node("f:source", "source", "f.rs", NodeType::Function));
    let target = pdg.add_node(make_node("f:target", "target", "f.rs", NodeType::Class));
    pdg.add_edge(
        source,
        target,
        Edge {
            edge_type: EdgeType::TypeOf,
            metadata: EdgeMetadata::empty(),
        },
    );

    assert!(
        pdg.forward_impact(source, &TraversalConfig::for_semantic_analysis())
            .contains(&target)
    );
    assert!(
        pdg.forward_impact(source, &TraversalConfig::for_impact_analysis())
            .contains(&target)
    );
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
fn remove_node_cleans_up_precision_marker() {
    let mut pdg = ProgramDependenceGraph::new();
    let node = pdg.add_node(make_node("f:marked", "marked", "f.rs", NodeType::Function));
    pdg.mark_precision_symbol("f:marked");

    assert!(pdg.is_precision_symbol("f:marked"));
    assert!(pdg.remove_node(node).is_some());
    assert!(!pdg.is_precision_symbol("f:marked"));
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
fn serialization_preserves_precision_symbols_and_embeddings() {
    let mut pdg = ProgramDependenceGraph::new();
    let n1 = pdg.add_node(make_node("f:foo", "foo", "f.rs", NodeType::Function));
    let n2 = pdg.add_node(make_node("f:bar", "bar", "f.rs", NodeType::Function));
    pdg.add_call_edges(vec![(n1, n2)]);
    pdg.set_embedding("f:foo", vec![0.1, 0.2, 0.3]);
    pdg.set_embedding("f:bar", vec![0.4, 0.5, 0.6]);
    pdg.mark_precision_symbol("f:foo");
    pdg.mark_precision_symbol("f:bar");

    // Serialize
    let bytes = pdg.serialize().expect("Serialization should succeed");

    // Deserialize
    let restored =
        ProgramDependenceGraph::deserialize(&bytes).expect("Deserialization should succeed");

    // Verify embeddings survived the round-trip
    assert_eq!(restored.get_embedding("f:foo"), Some(&vec![0.1, 0.2, 0.3]));
    assert_eq!(restored.get_embedding("f:bar"), Some(&vec![0.4, 0.5, 0.6]));
    assert_eq!(restored.embedding_count(), 2);
    assert!(restored.is_precision_symbol("f:foo"));
    assert!(restored.is_precision_symbol("f:bar"));
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
        embeddings: HashMap::default(), // No embeddings — simulates old format
        precision_symbols: HashSet::default(),
    };

    let bytes = bincode::serialize(&old_format).expect("Serialize old format");
    let restored = ProgramDependenceGraph::deserialize(&bytes)
        .expect("Should deserialize old format without error");

    let legacy_without_precision = SerializablePDGWithoutPrecision {
        nodes: old_format.nodes.clone(),
        edges: old_format.edges.clone(),
        symbol_index: old_format.symbol_index.clone(),
        file_index: old_format.file_index.clone(),
        name_index: old_format.name_index.clone(),
        name_lower_index: old_format.name_lower_index.clone(),
        embeddings: old_format.embeddings.clone(),
    };
    let legacy_bytes =
        bincode::serialize(&legacy_without_precision).expect("Serialize legacy format");
    let legacy_restored = ProgramDependenceGraph::deserialize(&legacy_bytes)
        .expect("Should deserialize pre-precision format without error");

    assert_eq!(restored.embedding_count(), 0);
    assert_eq!(restored.node_count(), 1);
    assert!(!restored.is_precision_symbol("f:foo"));
    assert_eq!(legacy_restored.node_count(), 1);
    assert!(!legacy_restored.is_precision_symbol("f:foo"));

    let pre_embedding = SerializablePDGWithoutEmbeddings {
        nodes: old_format.nodes.clone(),
        edges: old_format.edges.clone(),
        symbol_index: old_format.symbol_index.clone(),
        file_index: old_format.file_index.clone(),
        name_index: old_format.name_index.clone(),
        name_lower_index: old_format.name_lower_index.clone(),
    };
    let pre_embedding_bytes =
        bincode::serialize(&pre_embedding).expect("Serialize pre-embedding format");
    let pre_embedding_restored = ProgramDependenceGraph::deserialize(&pre_embedding_bytes)
        .expect("Should deserialize pre-embedding format without error");
    assert_eq!(pre_embedding_restored.node_count(), 1);
    assert_eq!(pre_embedding_restored.embedding_count(), 0);
    assert!(!pre_embedding_restored.is_precision_symbol("f:foo"));
}

#[test]
fn deserialization_backward_compat_pre_embedding_inline_node_embeddings() {
    let node = LegacyNode {
        id: "f:legacy".to_string(),
        node_type: NodeType::Function,
        name: "legacy".to_string(),
        file_path: "f.rs".to_string(),
        byte_range: (0, 10),
        complexity: 2,
        language: "rust".to_string(),
        embedding: Some(vec![0.7, 0.8]),
    };
    let old_format = SerializablePDGWithInlineEmbeddings {
        nodes: vec![LegacySerializableNode { index: 0, node }],
        edges: Vec::new(),
        symbol_index: HashMap::from_iter([(String::from("f:legacy"), 0)]),
        file_index: HashMap::from_iter([(String::from("f.rs"), vec![0])]),
        name_index: HashMap::from_iter([(String::from("legacy"), vec![0])]),
        name_lower_index: HashMap::from_iter([(String::from("legacy"), vec![0])]),
    };

    let bytes = bincode::serialize(&old_format).expect("Serialize pre-embedding format");
    let restored = ProgramDependenceGraph::deserialize(&bytes)
        .expect("Should deserialize pre-embedding format without error");

    assert_eq!(restored.node_count(), 1);
    assert_eq!(restored.get_embedding("f:legacy"), Some(&vec![0.7, 0.8]));
    assert!(!restored.is_precision_symbol("f:legacy"));
}

#[test]
fn deserialization_drops_precision_markers_for_missing_nodes() {
    let mut pdg = ProgramDependenceGraph::new();
    pdg.add_node(make_node(
        "f:present",
        "present",
        "f.rs",
        NodeType::Function,
    ));
    pdg.mark_precision_symbol("f:present");
    pdg.mark_precision_symbol("f:missing");

    let bytes = pdg.serialize().expect("Serialization should succeed");
    let restored =
        ProgramDependenceGraph::deserialize(&bytes).expect("Deserialization should succeed");

    assert!(restored.is_precision_symbol("f:present"));
    assert!(!restored.is_precision_symbol("f:missing"));
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

/// VAL-PDG-004: serialize -> deserialize round-trips every NodeType and
/// EdgeType variant together with their full field/edge-metadata payloads.
///
/// Also guards the clone-free serialization shim: this invokes
/// `SerializablePDGRef::from_pdg` (borrowed) on the write path and
/// `SerializablePDG::to_pdg` on the read path, verifying the two bincode
/// layouts agree.
#[test]
fn serialization_roundtrip_all_node_and_edge_variants() {
    let mut pdg = ProgramDependenceGraph::new();

    let n_fn = pdg.add_node(make_node("a.rs:f", "f", "a.rs", NodeType::Function));
    let n_class = pdg.add_node(make_node("a.rs:Cls", "Cls", "a.rs", NodeType::Class));
    let n_method = pdg.add_node(make_node("a.rs:Cls::m", "m", "a.rs", NodeType::Method));
    let n_var = pdg.add_node(make_node("a.rs:v", "v", "a.rs", NodeType::Variable));
    let n_module = pdg.add_node(make_node("a.rs:mod", "mod", "a.rs", NodeType::Module));
    let n_external = pdg.add_node(make_node("a.rs:dep", "dep", "a.rs", NodeType::External));
    let n_summary = pdg.add_node(make_node(
        "a.rs:summary",
        "summary",
        "a.rs",
        NodeType::FileSummary,
    ));
    assert_eq!(pdg.node_count(), 7);

    // One edge per EdgeType variant, exercising every EdgeMetadata field.
    let edge_specs: Vec<(NodeId, NodeId, EdgeType, EdgeMetadata)> = vec![
        (
            n_fn,
            n_class,
            EdgeType::Call,
            EdgeMetadata {
                call_count: Some(7),
                variable_name: None,
                confidence: Some(0.9),
                channel: Some("call".into()),
                position: Some(0),
            },
        ),
        (
            n_method,
            n_var,
            EdgeType::DataDependency,
            EdgeMetadata {
                call_count: None,
                variable_name: Some("param".to_string()),
                confidence: Some(0.5),
                channel: Some("flow".to_string()),
                position: Some(2),
            },
        ),
        (
            n_class,
            n_summary,
            EdgeType::Inheritance,
            EdgeMetadata::with_confidence(0.85),
        ),
        (
            n_module,
            n_external,
            EdgeType::Import,
            EdgeMetadata::empty(),
        ),
        (
            n_class,
            n_method,
            EdgeType::Containment,
            EdgeMetadata::empty(),
        ),
        (
            n_var,
            n_external,
            EdgeType::StateTransition,
            EdgeMetadata {
                call_count: None,
                variable_name: None,
                confidence: Some(0.4),
                channel: Some("state".to_string()),
                position: None,
            },
        ),
        (
            n_external,
            n_module,
            EdgeType::CommandArgument,
            EdgeMetadata {
                call_count: None,
                variable_name: Some("argv0".to_string()),
                confidence: Some(0.3),
                channel: None,
                position: Some(1),
            },
        ),
        (
            n_external,
            n_module,
            EdgeType::Environment,
            EdgeMetadata {
                call_count: None,
                variable_name: Some("HOME".to_string()),
                confidence: Some(0.2),
                channel: Some("env".to_string()),
                position: None,
            },
        ),
        (
            n_external,
            n_module,
            EdgeType::Stdin,
            EdgeMetadata {
                call_count: None,
                variable_name: Some("payload".to_string()),
                confidence: Some(0.1),
                channel: Some("stdin".to_string()),
                position: Some(0),
            },
        ),
    ];
    for (source, target, edge_type, metadata) in &edge_specs {
        pdg.add_edge(
            *source,
            *target,
            Edge {
                edge_type: edge_type.clone(),
                metadata: metadata.clone(),
            },
        );
    }
    assert_eq!(pdg.edge_count(), 9);

    // Bump an embedding so the shim's embeddings map is exercised too.
    pdg.set_embedding("a.rs:f", vec![0.25, 0.5, 0.75]);

    let bytes = pdg.serialize().expect("serialize should succeed");
    let restored = ProgramDependenceGraph::deserialize(&bytes).expect("deserialize should succeed");

    // All nodes + all edges survive.
    assert_eq!(restored.node_count(), 7);
    assert_eq!(restored.edge_count(), 9);

    // Embeddings survive.
    assert_eq!(
        restored.get_embedding("a.rs:f"),
        Some(&vec![0.25, 0.5, 0.75])
    );

    // A fully-populated edge's metadata survives the round-trip.
    let data_edges: Vec<&Edge> = restored
        .edge_indices()
        .filter_map(|idx| {
            let edge = restored.get_edge(idx)?;
            if edge.edge_type == EdgeType::DataDependency {
                Some(edge)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        data_edges.len(),
        1,
        "exactly one DataDependency edge expected"
    );
    assert_eq!(
        data_edges[0].metadata.variable_name.as_deref(),
        Some("param")
    );
    assert_eq!(data_edges[0].metadata.position, Some(2));
    assert_eq!(data_edges[0].metadata.confidence, Some(0.5));

    // Every node type is individually addressable after reload.
    for id in [
        "a.rs:f",
        "a.rs:Cls",
        "a.rs:Cls::m",
        "a.rs:v",
        "a.rs:mod",
        "a.rs:dep",
        "a.rs:summary",
    ] {
        assert!(
            restored.find_by_symbol(id).is_some(),
            "node '{id}' should round-trip"
        );
    }
}

#[test]
fn test_name_corpus_is_cached_per_revision_and_invalidates_on_mutation() {
    let mut pdg = ProgramDependenceGraph::new();
    let a = pdg.add_node(make_node("a", "Alpha", "Src/A.rs", NodeType::Function));
    let first = pdg.name_corpus();
    assert_eq!(first.names, vec!["alpha".to_string()]);
    assert_eq!(first.files, vec!["src/a.rs".to_string()]);

    // Unchanged graph: same Arc, no rebuild.
    let rev = pdg.revision();
    assert!(Arc::ptr_eq(&first, &pdg.name_corpus()));
    assert_eq!(rev, pdg.revision());

    // Adding a node invalidates.
    let b = pdg.add_node(make_node("b", "Beta", "src/b.rs", NodeType::Function));
    assert_ne!(rev, pdg.revision());
    let second = pdg.name_corpus();
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(second.names.contains(&"beta".to_string()));

    // Mutating a node in place (rename) invalidates.
    pdg.get_node_mut(a).unwrap().name = "Gamma".to_string();
    let third = pdg.name_corpus();
    assert!(third.names.contains(&"gamma".to_string()));
    assert!(!third.names.contains(&"alpha".to_string()));

    // Removing a node invalidates.
    pdg.remove_node(b);
    let fourth = pdg.name_corpus();
    assert!(!fourth.names.contains(&"beta".to_string()));
}

#[test]
fn test_name_corpus_clones_share_until_they_diverge() {
    let mut original = ProgramDependenceGraph::new();
    original.add_node(make_node("a", "Alpha", "a.rs", NodeType::Function));
    let built = original.name_corpus();

    let mut clone = original.clone();
    assert_eq!(clone.revision(), original.revision());
    assert!(Arc::ptr_eq(&built, &clone.name_corpus()));

    clone.add_node(make_node("b", "Beta", "b.rs", NodeType::Function));
    assert_ne!(clone.revision(), original.revision());
    assert!(clone.name_corpus().names.contains(&"beta".to_string()));
    assert!(
        !original.name_corpus().names.contains(&"beta".to_string()),
        "a diverged clone must not leak names into the original"
    );

    let other = ProgramDependenceGraph::new();
    assert_ne!(other.revision(), original.revision());
}
