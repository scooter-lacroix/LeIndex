use legraphe::{extract_pdg_from_signatures, pdg::EdgeType};
use leparse::prelude::{ImportInfo, Parameter, SignatureInfo, Visibility};

fn signature(name: &str, qualified: &str) -> SignatureInfo {
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
        imports: Vec::new(),
        byte_range: (1, 4),
    }
}

fn count_edges_of_type(pdg: &legraphe::pdg::ProgramDependenceGraph, edge_type: EdgeType) -> usize {
    pdg.edge_indices()
        .filter_map(|idx| pdg.get_edge(idx))
        .filter(|edge| edge.edge_type == edge_type)
        .count()
}

#[test]
fn rust_imports_local_resolution_and_external_fallback() {
    let mut signatures = vec![
        signature("main", "pkg::main"),
        signature("helper", "pkg::helper"),
    ];
    signatures[0].imports = vec![
        ImportInfo {
            path: "pkg.helper".to_string(),
            alias: None,
        },
        ImportInfo {
            path: "external.lib".to_string(),
            alias: None,
        },
    ];

    let pdg = extract_pdg_from_signatures(signatures, b"", "src/main.rs", "rust");
    let import_edges = count_edges_of_type(&pdg, EdgeType::Import);
    assert!(import_edges >= 2, "expected local + external import edges");

    let has_external_node = pdg.node_indices().any(|idx| {
        pdg.get_node(idx)
            .map(|node| node.language == "external")
            .unwrap_or(false)
    });
    assert!(
        has_external_node,
        "external module fallback node should exist"
    );
}

#[test]
fn python_imports_local_resolution_and_external_fallback() {
    let mut signatures = vec![
        signature("main", "pkg.main"),
        signature("helper", "pkg.helper"),
    ];
    signatures[0].imports = vec![
        ImportInfo {
            path: "pkg.helper".to_string(),
            alias: None,
        },
        ImportInfo {
            path: "third.party".to_string(),
            alias: None,
        },
    ];

    let pdg = extract_pdg_from_signatures(signatures, b"", "src/main.py", "python");
    let import_edges = count_edges_of_type(&pdg, EdgeType::Import);
    assert!(import_edges >= 2, "expected local + external import edges");
}

#[test]
fn import_edges_anchor_to_module_node_and_deduplicate() {
    let mut signatures = vec![
        signature("main", "pkg::main"),
        signature("helper", "pkg::helper"),
    ];

    signatures[0].imports = vec![
        ImportInfo {
            path: "pkg.helper".to_string(),
            alias: None,
        },
        ImportInfo {
            path: "pkg.helper".to_string(),
            alias: None,
        },
    ];

    let pdg = extract_pdg_from_signatures(signatures, b"", "src/main.rs", "rust");
    let module_node_id = pdg
        .find_by_symbol("src/main.rs:__module__")
        .expect("module anchor node");

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
        "duplicate imports should map to a single deduplicated edge"
    );
    assert_eq!(import_edges[0].0, module_node_id);
}

#[test]
fn regression_call_data_inheritance_edges_still_emitted() {
    let mut a = signature("process", "Base::process");
    a.is_method = true;
    a.calls.push("Derived::process".to_string());
    a.parameters = vec![Parameter {
        name: "x".to_string(),
        type_annotation: Some("User".to_string()),
        default_value: None,
    }];

    let mut b = signature("process", "Derived::process");
    b.is_method = true;
    b.parameters = vec![Parameter {
        name: "y".to_string(),
        type_annotation: Some("User".to_string()),
        default_value: None,
    }];

    let pdg = extract_pdg_from_signatures(vec![a, b], b"", "src/model.rs", "rust");

    assert!(count_edges_of_type(&pdg, EdgeType::Call) >= 1);
    assert!(count_edges_of_type(&pdg, EdgeType::DataDependency) >= 1);
    assert!(count_edges_of_type(&pdg, EdgeType::Inheritance) >= 1);
}
