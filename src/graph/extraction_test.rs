use super::*;
use crate::parse::prelude::{CodeIntelligence, ImportInfo, Parameter, SignatureInfo, Visibility};

fn sig(name: &str, qualified: &str, is_method: bool) -> SignatureInfo {
    SignatureInfo {
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        parameters: vec![],
        return_type: None,
        visibility: Visibility::Public,
        is_async: false,
        is_method,
        docstring: None,
        calls: vec![],
        imports: vec![],
        byte_range: (0, 100),
        flow_facts: vec![],

        cyclomatic_complexity: 0,
    }
}

#[test]
fn rust_flow_channels_are_first_class_pdg_edges() {
    let source = br#"
fn execute_native_command(password: &str, askpass: &str) {
    let mut command = std::process::Command::new("sudo");
    command.arg("-S").env("SUDO_ASKPASS", askpass).stdin(password);
    registry_record(password);
    verify_installation();
}
fn registry_record(value: &str) {}
fn verify_installation() {}
"#;
    let signatures = crate::parse::rust::RustParser::new()
        .get_signatures(source)
        .unwrap();
    let pdg = extract_pdg_from_signatures(signatures, source, "flows.rs", "rust");
    let from = pdg.find_by_name("execute_native_command").unwrap();

    let mut channels = HashSet::new();
    for edge_id in pdg.edge_indices() {
        let Some((source_id, target_id)) = pdg.edge_endpoints(edge_id) else {
            continue;
        };
        if source_id != from {
            continue;
        }
        let Some(edge) = pdg.get_edge(edge_id) else {
            continue;
        };
        if let Some(target) = pdg.get_node(target_id) {
            channels.insert((
                target.name.clone(),
                edge.edge_type.clone(),
                edge.metadata.channel.clone(),
            ));
        }
    }

    assert!(channels.contains(&(
        "sudo".to_string(),
        EdgeType::CommandArgument,
        Some("argv".to_string())
    )));
    assert!(channels.contains(&(
        "SUDO_ASKPASS".to_string(),
        EdgeType::Environment,
        Some("env".to_string())
    )));
    assert!(channels.contains(&(
        "password".to_string(),
        EdgeType::Stdin,
        Some("stdin".to_string())
    )));
    assert!(channels.iter().any(|(name, ty, channel)| {
        name == "registry_record"
            && *ty == EdgeType::StateTransition
            && channel.as_deref() == Some("state_write")
    }));
}

fn sig_with_types(
    name: &str,
    qualified: &str,
    params: Vec<(&str, &str)>,
    ret: Option<&str>,
) -> SignatureInfo {
    let parameters = params
        .into_iter()
        .map(|(pname, ptype)| Parameter {
            name: pname.to_string(),
            type_annotation: Some(ptype.to_string()),
            default_value: None,
        })
        .collect();
    SignatureInfo {
        name: name.to_string(),
        qualified_name: qualified.to_string(),
        parameters,
        return_type: ret.map(|s| s.to_string()),
        visibility: Visibility::Public,
        is_async: false,
        is_method: false,
        docstring: None,
        calls: vec![],
        imports: vec![],
        byte_range: (0, 100),
        flow_facts: vec![],

        cyclomatic_complexity: 0,
    }
}

#[test]
fn containment_edges_are_not_call_edges() {
    let sigs = vec![sig("speak", "Animal::speak", true)];
    let pdg = extract_pdg_from_signatures(sigs, b"", "f.py", "python");
    let call_count = pdg
        .edge_indices()
        .filter_map(|e| pdg.get_edge(e))
        .filter(|e| e.edge_type == crate::graph::pdg::EdgeType::Call)
        .count();
    let containment_count = pdg
        .edge_indices()
        .filter_map(|e| pdg.get_edge(e))
        .filter(|e| e.edge_type == crate::graph::pdg::EdgeType::Containment)
        .count();
    assert_eq!(call_count, 0, "Containment should not produce Call edges");
    assert_eq!(
        containment_count, 1,
        "Should have one Class→Method containment edge"
    );
}

#[test]
fn test_local_call_resolution_preserves_alias_namespace_suffix_and_type_edge() {
    let mut caller = sig("caller", "app::service::caller", false);
    caller.imports.push(ImportInfo {
        path: "external::package".to_string(),
        alias: Some("alias".to_string()),
    });
    caller.calls = vec![
        "alias::aliased_target".to_string(),
        "sibling".to_string(),
        "utility::suffix_target".to_string(),
        "Widget::new".to_string(),
    ];

    let pdg = extract_pdg_from_signatures(
        vec![
            caller,
            sig("aliased_target", "external::package::aliased_target", false),
            sig("sibling", "app::service::sibling", false),
            sig("suffix_target", "lib::utility::suffix_target", false),
            sig("new", "Widget::new", true),
        ],
        b"",
        "local.rs",
        "rust",
    );

    for callee in [
        "aliased_target",
        "sibling",
        "suffix_target",
        "new",
        "Widget",
    ] {
        assert!(
            has_call_edge(&pdg, "caller", callee),
            "caller should resolve {callee}"
        );
    }
}

#[test]
fn test_same_file_calls_reach_all_duplicate_qualified_names() {
    let mut caller = sig("caller", "caller", false);
    caller.calls.push("target".to_string());
    let mut first = sig("target", "target", false);
    first.byte_range = (10, 20);
    let mut second = sig("target", "target", false);
    second.byte_range = (30, 40);

    let pdg = extract_pdg_from_signatures(vec![caller, first, second], b"", "duplicate.rs", "rust");
    let target_ids: HashSet<_> = pdg
        .edge_indices()
        .filter_map(|edge| {
            let data = pdg.get_edge(edge)?;
            let (source, target) = pdg.edge_endpoints(edge)?;
            (data.edge_type == EdgeType::Call
                && pdg.get_node(source)?.name == "caller"
                && pdg.get_node(target)?.name == "target")
                .then_some(target)
        })
        .collect();

    assert_eq!(target_ids.len(), 2);
}

#[test]
fn data_flow_signal_a_produces_directed_edge() {
    let producer = sig_with_types("make_user", "make_user", vec![], Some("User"));
    let consumer = sig_with_types("save_user", "save_user", vec![("u", "User")], None);
    let mut nids = HashMap::new();
    let mut pdg = ProgramDependenceGraph::new();
    let p = pdg.add_node(signature_to_node(&producer, "f.rs", "rust"));
    let c = pdg.add_node(signature_to_node(&consumer, "f.rs", "rust"));
    nids.insert("make_user".to_string(), p);
    nids.insert("save_user".to_string(), c);

    let edges = extract_data_flow_edges(&[producer, consumer], &nids);
    assert!(
        !edges.is_empty(),
        "Signal A should produce a data flow edge"
    );
    let (from, to, _, conf) = &edges[0];
    assert_eq!((*from, *to), (p, c), "Edge should be producer → consumer");
    assert!(*conf >= 0.8, "Signal A confidence should be >= 0.8");
}

#[test]
fn data_flow_clique_not_generated() {
    // 10 functions all taking String — old code would produce 45 edges
    let sigs: Vec<SignatureInfo> = (0..10)
        .map(|i| {
            sig_with_types(
                &format!("f{i}"),
                &format!("f{i}"),
                vec![("s", "String")],
                None,
            )
        })
        .collect();
    let mut nids = HashMap::new();
    let mut pdg = ProgramDependenceGraph::new();
    for s in &sigs {
        let nid = pdg.add_node(signature_to_node(s, "f.rs", "rust"));
        nids.insert(s.qualified_name.clone(), nid);
    }
    let edges = extract_data_flow_edges(&sigs, &nids);
    // Signal A requires one to produce and another to consume — none return String
    // Signal B requires call relationship — none call each other
    // Signal C same
    assert_eq!(
        edges.len(),
        0,
        "Shared param type without call or return relationship should not produce edges"
    );
}

#[test]
fn inheritance_super_call_signal() {
    let parent_speak = sig("speak", "Animal::speak", true);
    let mut child_speak = sig("speak", "Dog::speak", true);
    child_speak.calls.push("super.speak".to_string());

    let sigs = vec![parent_speak, child_speak];
    let pdg = extract_pdg_from_signatures(sigs, b"", "f.py", "python");

    let inheritance_edges: Vec<_> = pdg
        .edge_indices()
        .filter_map(|e| {
            let edge = pdg.get_edge(e)?;
            if edge.edge_type == crate::graph::pdg::EdgeType::Inheritance {
                Some(edge.metadata.confidence.unwrap_or(0.0))
            } else {
                None
            }
        })
        .collect();

    assert!(
        !inheritance_edges.is_empty(),
        "Super call should produce inheritance edge"
    );
    assert!(
        inheritance_edges[0] >= 0.85,
        "Super call confidence should be high"
    );
}

#[test]
fn cross_file_flow_argument_is_resolved_after_merge() {
    let mut caller = sig("dispatch", "dispatch", false);
    caller.flow_facts.push(crate::parse::traits::FlowFact {
        channel: FlowChannel::Argument,
        source: "password".to_string(),
        target: "resolve_password".to_string(),
        position: Some(0),
        byte_range: (0, 8),
    });
    let callee = sig("resolve_password", "resolve_password", false);
    let mut merged = ProgramDependenceGraph::new();
    crate::cli::index_builder::merge_pdgs(
        &mut merged,
        extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust"),
    );
    crate::cli::index_builder::merge_pdgs(
        &mut merged,
        extract_pdg_from_signatures(vec![callee.clone()], b"", "b.rs", "rust"),
    );

    resolve_cross_file_flow_edges_for_files(
        &mut merged,
        &[("a.rs".to_string(), caller), ("b.rs".to_string(), callee)],
    );

    let caller_id = merged
        .node_indices()
        .find(|&id| {
            merged
                .get_node(id)
                .is_some_and(|node| node.id == "a.rs:dispatch")
        })
        .unwrap();
    let callee_id = merged
        .node_indices()
        .find(|&id| {
            merged
                .get_node(id)
                .is_some_and(|node| node.id == "b.rs:resolve_password")
        })
        .unwrap();
    assert!(merged.edge_indices().any(|edge_id| {
        merged.edge_endpoints(edge_id) == Some((caller_id, callee_id))
            && merged
                .get_edge(edge_id)
                .is_some_and(|edge| edge.edge_type == EdgeType::DataDependency)
    }));
}

#[test]
fn python_multiline_import_parsed() {
    let source = b"from os.path import (\n    join,\n    exists,\n    dirname\n)\n";
    let imports = extract_import_paths_from_source(source, "python");
    assert!(imports.contains("os.path"), "Module should be captured");
    assert!(imports.contains("os.path.join") || imports.iter().any(|s| s.contains("join")));
}

#[test]
fn rust_brace_import_expanded() {
    let source = b"use std::{\n    collections::HashMap,\n    sync::Arc,\n};\n";
    let imports = extract_import_paths_from_source(source, "rust");
    assert!(imports.iter().any(|s| s.contains("HashMap")));
    assert!(imports.iter().any(|s| s.contains("Arc")));
}

#[test]
fn typescript_multiline_import_parsed() {
    let source = b"import {\n  useState,\n  useEffect,\n  useCallback\n} from 'react';\n";
    let imports = extract_import_paths_from_source(source, "typescript");
    assert!(imports.contains("react"));
}

#[test]
fn cyclomatic_complexity_wiring_from_signature_to_node() {
    use crate::parse::traits::{Parameter, SignatureInfo, Visibility};

    // Test 1: cyclomatic_complexity = 0 should use parameter count fallback
    let sig_simple = SignatureInfo {
        name: "simple".to_string(),
        qualified_name: "simple".to_string(),
        parameters: vec![],
        return_type: None,
        visibility: Visibility::Public,
        is_async: false,
        is_method: false,
        docstring: None,
        calls: vec![],
        imports: vec![],
        byte_range: (0, 10),
        flow_facts: vec![],

        cyclomatic_complexity: 0,
    };

    let node = signature_to_node(&sig_simple, "test.rs", "rust");
    assert_eq!(node.complexity, 1, "Simple: no params → complexity 1");

    // Test 2: cyclomatic_complexity > 0 should use that value
    let sig_complex = SignatureInfo {
        flow_facts: vec![],

        cyclomatic_complexity: 5,
        ..sig_simple.clone()
    };

    let node = signature_to_node(&sig_complex, "test.rs", "rust");
    assert_eq!(node.complexity, 5, "Complex: cyclomatic=5 → complexity 5");

    // Test 3: parameters without cyclomatic should use 1 + param_count
    let sig_params = SignatureInfo {
        name: "with_params".to_string(),
        qualified_name: "with_params".to_string(),
        parameters: vec![
            Parameter {
                name: "a".into(),
                type_annotation: None,
                default_value: None,
            },
            Parameter {
                name: "b".into(),
                type_annotation: None,
                default_value: None,
            },
        ],
        flow_facts: vec![],

        cyclomatic_complexity: 0,
        ..sig_simple
    };

    let node = signature_to_node(&sig_params, "test.rs", "rust");
    assert_eq!(node.complexity, 3, "Params: 2 params → complexity 3");

    // Test 4: cyclomatic should override parameter count
    let sig_both = SignatureInfo {
        flow_facts: vec![],

        cyclomatic_complexity: 10,
        ..sig_params
    };

    let node = signature_to_node(&sig_both, "test.rs", "rust");
    assert_eq!(
        node.complexity, 10,
        "Both: cyclomatic=10 overrides param count"
    );
}

// -----------------------------------------------------------------
// Tests for resolve_cross_file_call_edges
// -----------------------------------------------------------------

/// Helper: count Call edges in a PDG
fn count_call_edges(pdg: &ProgramDependenceGraph) -> usize {
    pdg.edge_indices()
        .filter_map(|e| pdg.get_edge(e))
        .filter(|e| e.edge_type == crate::graph::pdg::EdgeType::Call)
        .count()
}

/// Helper: check if a Call edge exists from caller to callee
fn has_call_edge(pdg: &ProgramDependenceGraph, caller_name: &str, callee_name: &str) -> bool {
    let caller_nid = pdg.node_indices().find(|&n| {
        pdg.get_node(n)
            .map(|node| &*node.name == caller_name)
            .unwrap_or(false)
    });
    let callee_nid = pdg.node_indices().find(|&n| {
        pdg.get_node(n)
            .map(|node| &*node.name == callee_name)
            .unwrap_or(false)
    });

    match (caller_nid, callee_nid) {
        (Some(c), Some(d)) => {
            // Check if there's a Call edge from c to d
            pdg.neighbors(c).contains(&d)
        }
        _ => false,
    }
}

fn has_call_edge_from_file(
    pdg: &ProgramDependenceGraph,
    caller_file: &str,
    caller_name: &str,
    callee_name: &str,
) -> bool {
    let caller_nid = pdg.node_indices().find(|&n| {
        pdg.get_node(n)
            .map(|node| node.file_path.as_ref() == caller_file && &*node.name == caller_name)
            .unwrap_or(false)
    });
    let callee_nid = pdg.node_indices().find(|&n| {
        pdg.get_node(n)
            .map(|node| &*node.name == callee_name)
            .unwrap_or(false)
    });

    match (caller_nid, callee_nid) {
        (Some(c), Some(d)) => pdg.neighbors(c).contains(&d),
        _ => false,
    }
}

fn has_call_edge_between_files(
    pdg: &ProgramDependenceGraph,
    caller_file: &str,
    caller_name: &str,
    callee_file: &str,
    callee_name: &str,
) -> bool {
    let caller_nid = pdg.node_indices().find(|&n| {
        pdg.get_node(n)
            .map(|node| node.file_path.as_ref() == caller_file && &*node.name == caller_name)
            .unwrap_or(false)
    });
    let callee_nid = pdg.node_indices().find(|&n| {
        pdg.get_node(n)
            .map(|node| node.file_path.as_ref() == callee_file && &*node.name == callee_name)
            .unwrap_or(false)
    });

    match (caller_nid, callee_nid) {
        (Some(c), Some(d)) => pdg.neighbors(c).contains(&d),
        _ => false,
    }
}

/// Tier 1: Exact match - caller references callee by exact qualified name
#[test]
fn cross_file_exact_match() {
    // File A has a function that calls a function in File B by exact name
    let callee = sig("target_func", "target_func", false);
    let mut caller = sig("caller_func", "caller_func", false);
    caller.calls.push("target_func".to_string());

    // Build PDG with nodes from two different files
    let pdg_a = extract_pdg_from_signatures(vec![caller], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee], b"", "b.rs", "rust");

    // Merge into a single PDG
    let mut merged = ProgramDependenceGraph::new();
    for nid in pdg_a.node_indices() {
        if let Some(node) = pdg_a.get_node(nid) {
            merged.add_node(node.clone());
        }
    }
    for nid in pdg_b.node_indices() {
        if let Some(node) = pdg_b.get_node(nid) {
            merged.add_node(node.clone());
        }
    }

    let edges_before = count_call_edges(&merged);
    resolve_cross_file_call_edges(
        &mut merged,
        &[
            SignatureInfo {
                name: "caller_func".to_string(),
                qualified_name: "caller_func".to_string(),
                calls: vec!["target_func".to_string()],
                ..sig("caller_func", "caller_func", false)
            },
            SignatureInfo {
                name: "target_func".to_string(),
                qualified_name: "target_func".to_string(),
                calls: vec![],
                ..sig("target_func", "target_func", false)
            },
        ],
    );
    let edges_after = count_call_edges(&merged);

    assert!(
        edges_after > edges_before,
        "Exact match should create a new call edge (before={}, after={})",
        edges_before,
        edges_after
    );
    assert!(
        has_call_edge(&merged, "caller_func", "target_func"),
        "Should have call edge from caller_func to target_func"
    );
}

/// Tier 2: Suffix match - caller references callee via module-qualified path
#[test]
fn cross_file_suffix_match() {
    // callee is defined as `my_module.helper_func` in file B
    // caller calls `my_module.helper_func` - should match via exact normalized name
    let callee = sig("helper_func", "my_module.helper_func", false);
    let mut caller = sig("do_work", "do_work", false);
    caller.calls.push("my_module.helper_func".to_string());

    let pdg_a = extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee], b"", "b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for nid in pdg_a.node_indices() {
        if let Some(node) = pdg_a.get_node(nid) {
            merged.add_node(node.clone());
        }
    }
    for nid in pdg_b.node_indices() {
        if let Some(node) = pdg_b.get_node(nid) {
            merged.add_node(node.clone());
        }
    }

    resolve_cross_file_call_edges(
        &mut merged,
        &[caller, sig("helper_func", "my_module.helper_func", false)],
    );

    assert!(
        has_call_edge(&merged, "do_work", "helper_func"),
        "Suffix match: should have call edge from do_work to helper_func"
    );
}

/// Tier 2b: Suffix match with 2+ segments from the end
#[test]
fn cross_file_suffix_match_multi_segment() {
    // callee is `crate::network::send_request` in file B
    // caller calls `network.send_request` - should match via suffix
    let callee = sig("send_request", "crate.network.send_request", false);
    let mut caller = sig("handle_request", "handle_request", false);
    caller.calls.push("network.send_request".to_string());

    let pdg_a = extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee], b"", "b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for nid in pdg_a.node_indices() {
        if let Some(node) = pdg_a.get_node(nid) {
            merged.add_node(node.clone());
        }
    }
    for nid in pdg_b.node_indices() {
        if let Some(node) = pdg_b.get_node(nid) {
            merged.add_node(node.clone());
        }
    }

    resolve_cross_file_call_edges(
        &mut merged,
        &[
            caller,
            sig("send_request", "crate.network.send_request", false),
        ],
    );

    assert!(
        has_call_edge(&merged, "handle_request", "send_request"),
        "Multi-segment suffix match: should have call edge from handle_request to send_request"
    );
}

/// Tier 3: Last-segment fallback - fully qualified call resolved via last segment
#[test]
fn cross_file_last_segment_fallback() {
    // callee is defined as `process_data` in file B
    // caller calls `some.long.path.process_data` - no exact/suffix match,
    // but last-segment fallback should find it
    let callee = sig("process_data", "process_data", false);
    let mut caller = sig("main_func", "main_func", false);
    caller.calls.push("some.long.path.process_data".to_string());

    let pdg_a = extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee], b"", "b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for nid in pdg_a.node_indices() {
        if let Some(node) = pdg_a.get_node(nid) {
            merged.add_node(node.clone());
        }
    }
    for nid in pdg_b.node_indices() {
        if let Some(node) = pdg_b.get_node(nid) {
            merged.add_node(node.clone());
        }
    }

    resolve_cross_file_call_edges(
        &mut merged,
        &[caller, sig("process_data", "process_data", false)],
    );

    assert!(
        has_call_edge(&merged, "main_func", "process_data"),
        "Last-segment fallback: should have call edge from main_func to process_data"
    );
}

/// Tier 4: COMMON_NAMES exclusion - common names should NOT be resolved
/// via last-segment fallback to avoid false positives
#[test]
fn cross_file_common_names_excluded() {
    // callee is named "clone" (a common name) in file B
    // caller calls `some.path.clone` - should NOT match via last-segment
    // fallback because "clone" is in COMMON_NAMES
    let callee = sig("clone", "clone", false);
    let mut caller = sig("caller", "caller", false);
    caller.calls.push("some.path.clone".to_string());

    let pdg_a = extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee], b"", "b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for nid in pdg_a.node_indices() {
        if let Some(node) = pdg_a.get_node(nid) {
            merged.add_node(node.clone());
        }
    }
    for nid in pdg_b.node_indices() {
        if let Some(node) = pdg_b.get_node(nid) {
            merged.add_node(node.clone());
        }
    }

    let edges_before = count_call_edges(&merged);
    resolve_cross_file_call_edges(&mut merged, &[caller, sig("clone", "clone", false)]);
    let edges_after = count_call_edges(&merged);

    assert_eq!(
        edges_before, edges_after,
        "COMMON_NAMES exclusion: 'clone' should NOT be resolved via last-segment fallback"
    );
    assert!(
        !has_call_edge(&merged, "caller", "clone"),
        "Should NOT have call edge from caller to clone (common name excluded)"
    );
}

/// Verify that COMMON_NAMES exclusion also applies to other common names
#[test]
fn cross_file_common_names_excluded_execute() {
    let callee = sig("execute", "execute", false);
    let mut caller = sig("runner", "runner", false);
    caller.calls.push("module.sub.execute".to_string());

    let pdg_a = extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee], b"", "b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for nid in pdg_a.node_indices() {
        if let Some(node) = pdg_a.get_node(nid) {
            merged.add_node(node.clone());
        }
    }
    for nid in pdg_b.node_indices() {
        if let Some(node) = pdg_b.get_node(nid) {
            merged.add_node(node.clone());
        }
    }

    let edges_before = count_call_edges(&merged);
    resolve_cross_file_call_edges(&mut merged, &[caller, sig("execute", "execute", false)]);
    let edges_after = count_call_edges(&merged);

    assert_eq!(
        edges_before, edges_after,
        "COMMON_NAMES exclusion: 'execute' should NOT be resolved via last-segment fallback"
    );
}

/// Verify that short names (len <= 2) are excluded from last-segment fallback
#[test]
fn cross_file_short_names_excluded_from_fallback() {
    let callee = sig("fn", "fn", false);
    let mut caller = sig("caller", "caller", false);
    caller.calls.push("some.path.fn".to_string());

    let pdg_a = extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee], b"", "b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for nid in pdg_a.node_indices() {
        if let Some(node) = pdg_a.get_node(nid) {
            merged.add_node(node.clone());
        }
    }
    for nid in pdg_b.node_indices() {
        if let Some(node) = pdg_b.get_node(nid) {
            merged.add_node(node.clone());
        }
    }

    let edges_before = count_call_edges(&merged);
    resolve_cross_file_call_edges(&mut merged, &[caller, sig("fn", "fn", false)]);
    let edges_after = count_call_edges(&merged);

    assert_eq!(
        edges_before, edges_after,
        "Short name 'fn' (len=2) should NOT be resolved via last-segment fallback"
    );
}

#[test]
fn cross_file_duplicate_qnames_do_not_overwrite_callers() {
    let mut caller_a = sig("caller", "caller", false);
    caller_a.calls.push("target".to_string());
    let mut caller_b = sig("caller", "caller", false);
    caller_b.calls.push("target".to_string());
    let callee = sig("target", "target", false);

    let pdg_a = extract_pdg_from_signatures(vec![caller_a.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![caller_b.clone()], b"", "b.rs", "rust");
    let pdg_c = extract_pdg_from_signatures(vec![callee.clone()], b"", "c.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for source in [&pdg_a, &pdg_b, &pdg_c] {
        for nid in source.node_indices() {
            if let Some(node) = source.get_node(nid) {
                merged.add_node(node.clone());
            }
        }
    }

    resolve_cross_file_call_edges(&mut merged, &[caller_a, caller_b, callee]);

    assert!(has_call_edge_from_file(&merged, "a.rs", "caller", "target"));
    assert!(has_call_edge_from_file(&merged, "b.rs", "caller", "target"));
}

#[test]
fn cross_file_file_owned_signatures_do_not_cross_apply_duplicate_qnames() {
    let mut caller_a = sig("handler", "handler", false);
    caller_a.calls.push("target_a".to_string());
    let mut caller_b = sig("handler", "handler", false);
    caller_b.calls.push("target_b".to_string());
    let target_a = sig("target_a", "target_a", false);
    let target_b = sig("target_b", "target_b", false);

    let pdg_a = extract_pdg_from_signatures(vec![caller_a.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![caller_b.clone()], b"", "b.rs", "rust");
    let pdg_ta = extract_pdg_from_signatures(vec![target_a.clone()], b"", "target_a.rs", "rust");
    let pdg_tb = extract_pdg_from_signatures(vec![target_b.clone()], b"", "target_b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for source in [&pdg_a, &pdg_b, &pdg_ta, &pdg_tb] {
        for nid in source.node_indices() {
            if let Some(node) = source.get_node(nid) {
                merged.add_node(node.clone());
            }
        }
    }

    resolve_cross_file_call_edges_for_files(
        &mut merged,
        &[
            ("a.rs".to_string(), caller_a),
            ("b.rs".to_string(), caller_b),
            ("target_a.rs".to_string(), target_a),
            ("target_b.rs".to_string(), target_b),
        ],
    );

    assert!(has_call_edge_between_files(
        &merged,
        "a.rs",
        "handler",
        "target_a.rs",
        "target_a"
    ));
    assert!(has_call_edge_between_files(
        &merged,
        "b.rs",
        "handler",
        "target_b.rs",
        "target_b"
    ));
    assert!(!has_call_edge_between_files(
        &merged,
        "a.rs",
        "handler",
        "target_b.rs",
        "target_b"
    ));
    assert!(!has_call_edge_between_files(
        &merged,
        "b.rs",
        "handler",
        "target_a.rs",
        "target_a"
    ));
}

#[test]
fn test_same_file_duplicate_qnames_keep_graph_and_search_ids_and_call_resolution() {
    use crate::search::search::{NodeInfo, SearchEngine};

    let mut first = sig("run", "Service::run", true);
    first.byte_range = (10, 20);
    let mut second = sig("run", "Service::run", true);
    second.byte_range = (30, 40);
    let mut caller = sig("dispatch", "dispatch", false);
    caller.calls.push("Service::run".to_string());

    let dupes =
        extract_pdg_from_signatures(vec![first.clone(), second.clone()], b"", "dupes.rs", "rust");
    let duplicate_nodes: Vec<Node> = dupes
        .node_indices()
        .filter_map(|id| dupes.get_node(id))
        .filter(|node| node.name == "run")
        .cloned()
        .collect();

    assert_eq!(duplicate_nodes.len(), 2);
    assert_ne!(duplicate_nodes[0].id, duplicate_nodes[1].id);
    assert_eq!(
        duplicate_nodes
            .iter()
            .map(|node| qualified_name_from_node(node))
            .collect::<Vec<_>>(),
        vec![Some("Service::run"), Some("Service::run")]
    );

    let caller_pdg = extract_pdg_from_signatures(vec![caller.clone()], b"", "caller.rs", "rust");
    let mut merged = ProgramDependenceGraph::new();
    for source in [&caller_pdg, &dupes] {
        for id in source.node_indices() {
            if let Some(node) = source.get_node(id) {
                merged.add_node(node.clone());
            }
        }
    }

    resolve_cross_file_call_edges_for_files(
        &mut merged,
        &[
            ("caller.rs".to_string(), caller),
            ("dupes.rs".to_string(), first),
            ("dupes.rs".to_string(), second),
        ],
    );

    let caller_id = merged
        .node_indices()
        .find(|&id| {
            merged.get_node(id).is_some_and(|node| {
                node.file_path.as_ref() == "caller.rs" && node.name == "dispatch"
            })
        })
        .unwrap();
    let mut resolved_ids: Vec<String> = merged
        .edge_indices()
        .filter_map(|edge_id| {
            let edge = merged.get_edge(edge_id)?;
            let (source, target) = merged.edge_endpoints(edge_id)?;
            (edge.edge_type == EdgeType::Call && source == caller_id)
                .then(|| merged.get_node(target))
                .flatten()
                .filter(|node| {
                    node.file_path.as_ref() == "dupes.rs"
                        && qualified_name_from_node(node) == Some("Service::run")
                })
                .map(|node| node.id.clone())
        })
        .collect();
    resolved_ids.sort();
    resolved_ids.dedup();
    assert_eq!(resolved_ids.len(), 2);

    let node_ids: Vec<String> = duplicate_nodes.iter().map(|node| node.id.clone()).collect();
    let mut engine = SearchEngine::new();
    engine.index_nodes(
        duplicate_nodes
            .iter()
            .map(|node| NodeInfo {
                node_id: node.id.clone(),
                file_path: node.file_path.to_string(),
                symbol_name: node.name.clone(),
                language: node.language.clone(),
                content: "fn run() {}".to_string(),
                byte_range: node.byte_range,
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: node.complexity,
                signature: None,
                pre_tokenized: None,
            })
            .collect(),
    );
    assert_eq!(engine.node_count(), 2);
    assert_ne!(
        engine.node_index(&node_ids[0]),
        engine.node_index(&node_ids[1])
    );
}

#[test]
fn cross_file_rust_qualified_names_preserve_colon_segments() {
    let mut caller = sig("handler", "my_mod::handler", false);
    caller.calls.push("other_mod::target".to_string());
    let callee = sig("target", "other_mod::target", false);

    let pdg_a = extract_pdg_from_signatures(vec![caller.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![callee.clone()], b"", "b.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for source in [&pdg_a, &pdg_b] {
        for nid in source.node_indices() {
            if let Some(node) = source.get_node(nid) {
                merged.add_node(node.clone());
            }
        }
    }

    resolve_cross_file_call_edges_for_files(
        &mut merged,
        &[("a.rs".to_string(), caller), ("b.rs".to_string(), callee)],
    );

    assert!(has_call_edge_between_files(
        &merged, "a.rs", "handler", "b.rs", "target"
    ));
}

#[test]
fn cross_file_import_aliases_are_scoped_to_the_calling_signature() {
    let mut caller_a = sig("handler_a", "handler_a", false);
    caller_a.imports.push(ImportInfo {
        path: "real.module.target".to_string(),
        alias: Some("alias".to_string()),
    });
    caller_a.calls.push("alias".to_string());

    let mut caller_b = sig("handler_b", "handler_b", false);
    caller_b.calls.push("alias".to_string());

    let callee = sig("target", "real.module.target", false);

    let pdg_a = extract_pdg_from_signatures(vec![caller_a.clone()], b"", "a.rs", "rust");
    let pdg_b = extract_pdg_from_signatures(vec![caller_b.clone()], b"", "b.rs", "rust");
    let pdg_c = extract_pdg_from_signatures(vec![callee.clone()], b"", "c.rs", "rust");

    let mut merged = ProgramDependenceGraph::new();
    for source in [&pdg_a, &pdg_b, &pdg_c] {
        for nid in source.node_indices() {
            if let Some(node) = source.get_node(nid) {
                merged.add_node(node.clone());
            }
        }
    }

    resolve_cross_file_call_edges_for_files(
        &mut merged,
        &[
            ("a.rs".to_string(), caller_a),
            ("b.rs".to_string(), caller_b),
            ("c.rs".to_string(), callee),
        ],
    );

    assert!(has_call_edge_between_files(
        &merged,
        "a.rs",
        "handler_a",
        "c.rs",
        "target"
    ));
    assert!(!has_call_edge_between_files(
        &merged,
        "b.rs",
        "handler_b",
        "c.rs",
        "target"
    ));
}

#[test]
fn qualified_name_from_node_handles_equivalent_path_suffixes() {
    let node = Node {
        id: "relative.rs:my_mod::handler".to_string(),
        node_type: NodeType::Function,
        name: "handler".to_string(),
        file_path: Arc::from("/abs/relative.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    };

    assert_eq!(qualified_name_from_node(&node), Some("my_mod::handler"));
}

#[test]
fn qualified_name_from_node_rejects_different_path_suffixes() {
    let node = Node {
        id: "other.rs:my_mod::handler".to_string(),
        node_type: NodeType::Function,
        name: "handler".to_string(),
        file_path: Arc::from("/abs/relative.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    };

    assert_eq!(qualified_name_from_node(&node), None);
}
