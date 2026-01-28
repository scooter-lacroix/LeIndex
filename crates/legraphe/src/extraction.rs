// AST → PDG Extraction Module
//
// *L'Extraction* (The Extraction) - Transforms parsed signatures into Program Dependence Graphs
//
// # Overview
//
// This module bridges leparse (parsing) and legraphe (graph intelligence) by converting
// `SignatureInfo` structures into `ProgramDependenceGraph` instances.
//
// # Critical Limitations
//
// **Call Graph Extraction**: This module CANNOT extract true call graphs because:
// - `SignatureInfo` contains only function signatures, not bodies
// - Call relationships require AST-level analysis of function bodies
// - Future enhancement: Extend `CodeIntelligence` trait to extract full AST with body nodes
//
// # What IS Extracted
//
// 1. **Nodes**: Functions/methods from signatures with metadata
// 2. **Type Dependencies**: Edges between functions using similar parameter types
// 3. **Class Hierarchy**: Inheritance edges from qualified_name patterns (e.g., `Class::method`)
//
// # Future Enhancement Path
//
// For full call graph extraction:
// 1. Extend `SignatureInfo` to include AST node references
// 2. Add `get_function_body()` method to `CodeIntelligence` trait
// 3. Implement AST traversal in this module to find call expressions
// 4. Extract call sites with line/column information

#![warn(missing_docs)]

use crate::pdg::{Node, NodeType, ProgramDependenceGraph};
use leparse::prelude::SignatureInfo;
use std::collections::{HashMap, HashSet};

/// Extract a Program Dependence Graph from function signatures
///
/// This function transforms a vector of `SignatureInfo` structures into a PDG by:
/// 1. Creating nodes for each function/method signature
/// 2. Extracting type-based data dependency edges
/// 3. Parsing class hierarchy for inheritance edges
///
/// # Arguments
///
/// * `signatures` - Vector of function/method signatures from leparse
/// * `source_code` - Raw source code bytes (currently unused, reserved for future AST extraction)
/// * `file_path` - Path to the source file for node metadata
///
/// # Returns
///
/// A `ProgramDependenceGraph` containing nodes and edges extracted from signatures
///
/// # Example
///
/// ```ignore
/// use legraphe::extraction::extract_pdg_from_signatures;
/// use leparse::SignatureInfo;
///
/// let signatures = vec![
///     SignatureInfo {
///         name: "process_data".to_string(),
///         qualified_name: "MyClass::process_data".to_string(),
///         parameters: vec![],
///         return_type: Some("Result".to_string()),
///         visibility: leparse::Visibility::Public,
///         is_async: false,
///         is_method: true,
///         docstring: None,
///     },
/// ];
///
/// let pdg = extract_pdg_from_signatures(signatures, b"...", "src/my_file.rs");
/// assert_eq!(pdg.node_count(), 1);
/// ```
///
/// # Limitations
///
/// - **No Call Graph**: Cannot extract function call relationships without AST bodies
/// - **Type Heuristics**: Data dependencies based on type similarity may have false positives
/// - **Default Metadata**: byte_range and complexity use estimated values
pub fn extract_pdg_from_signatures(
    signatures: Vec<SignatureInfo>,
    _source_code: &[u8],
    file_path: &str,
) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();

    // Return empty PDG for empty input (graceful degradation)
    if signatures.is_empty() {
        return pdg;
    }

    // Track node IDs for edge creation
    let mut node_ids: HashMap<String, crate::pdg::NodeId> = HashMap::new();

    // Phase 1: Create nodes from signatures
    for signature in &signatures {
        let node = signature_to_node(signature, file_path);
        let node_id = pdg.add_node(node);
        node_ids.insert(signature.qualified_name.clone(), node_id);
    }

    // Phase 2: Extract and add type dependency edges
    let type_deps = extract_type_dependencies(&signatures);
    let data_edges: Vec<(crate::pdg::NodeId, crate::pdg::NodeId, String)> = type_deps
        .into_iter()
        .filter_map(|(from_sig, to_sig, type_name)| {
            let from_id = node_ids.get(&from_sig)?;
            let to_id = node_ids.get(&to_sig)?;
            Some((*from_id, *to_id, type_name))
        })
        .collect();

    pdg.add_data_flow_edges(data_edges);

    // Phase 3: Extract and add inheritance edges
    let inheritance_edges = extract_inheritance_edges(&signatures, &node_ids);
    pdg.add_inheritance_edges(inheritance_edges);

    // Note: Call graph extraction is NOT possible with SignatureInfo alone
    // See module documentation for future enhancement path

    pdg
}

/// Convert a SignatureInfo to a PDG Node
///
/// This function maps signature metadata to node properties:
/// - Uses `qualified_name` as unique node ID
/// - Determines node type from `is_method` flag
/// - Estimates complexity from parameter count
/// - Sets byte_range to default (0, 0) since not available in signature
///
/// # Arguments
///
/// * `sig` - Function/method signature from leparse
/// * `file_path` - Source file path for node metadata
///
/// # Returns
///
/// A PDG `Node` populated with signature information
fn signature_to_node(sig: &SignatureInfo, file_path: &str) -> Node {
    // Determine node type based on is_method flag
    let node_type = if sig.is_method {
        NodeType::Method
    } else {
        NodeType::Function
    };

    // Estimate complexity: base complexity + parameter count
    // This is a heuristic since true cyclomatic complexity requires AST analysis
    let base_complexity = 1u32;
    let param_complexity = sig.parameters.len() as u32;
    let complexity = base_complexity + param_complexity;

    // byte_range is now available in SignatureInfo
    let byte_range = sig.byte_range;

    Node {
        id: format!("{}:{}", file_path, sig.qualified_name),
        node_type,
        name: sig.name.clone(),
        file_path: file_path.to_string(),
        byte_range,
        complexity,
        embedding: None, // Embeddings added separately by embedding module
    }
}

/// Extract type-based data dependencies between functions
///
/// This function analyzes parameter types to find potential data flow relationships.
/// Functions that accept similar types are likely to be part of the same data pipeline.
///
/// # Heuristic
///
/// Two functions have a data dependency if they share parameter types.
/// This catches real connections like:
/// - `process_user(user: User)` → `validate_user(user: User)`
/// - `parse_config(data: &str)` → `load_config(path: &str)`
///
/// # Arguments
///
/// * `signatures` - Slice of signatures to analyze
///
/// # Returns
///
/// Vector of (from_qualified_name, to_qualified_name, type_name) tuples representing edges
fn extract_type_dependencies(signatures: &[SignatureInfo]) -> Vec<(String, String, String)> {
    let mut edges = Vec::new();
    let mut type_to_signatures: HashMap<String, Vec<String>> = HashMap::new();

    // Build type → signatures mapping
    for sig in signatures {
        for param in &sig.parameters {
            if let Some(type_name) = &param.type_annotation {
                type_to_signatures
                    .entry(type_name.clone())
                    .or_default()
                    .push(sig.qualified_name.clone());
            }
        }
    }

    // Create edges between functions sharing types
    for (_type_name, sig_names) in type_to_signatures {
        // Create edges between all pairs of functions using this type
        // This creates a clique - all functions using type T are connected
        for i in 0..sig_names.len() {
            for j in (i + 1)..sig_names.len() {
                let from = sig_names[i].clone();
                let to = sig_names[j].clone();
                let type_name = _type_name.clone();
                edges.push((from, to, type_name));
            }
        }
    }

    edges
}

/// Extract inheritance edges from qualified names
///
/// This function parses qualified names to detect class hierarchy patterns.
/// Common patterns:
/// - `ChildClass::method` extends `BaseClass` if types are related
/// - Namespaces: `module::Submodule::Class`
///
/// # Current Implementation
///
/// Detects implicit inheritance through:
/// 1. Method names with class qualifiers (e.g., `Child::method` may inherit from `Parent::method`)
/// 2. Shared base classes in qualified name paths
///
/// # Arguments
///
/// * `signatures` - Slice of signatures to analyze
/// * `node_ids` - Mapping from qualified names to node IDs
///
/// # Returns
///
/// Vector of (child_id, parent_id) tuples representing inheritance edges
fn extract_inheritance_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::pdg::NodeId>,
) -> Vec<(crate::pdg::NodeId, crate::pdg::NodeId)> {
    let mut edges = Vec::new();
    let mut class_methods: HashMap<String, Vec<String>> = HashMap::new();

    // Group methods by their class
    for sig in signatures {
        if sig.is_method {
            if let Some(class_name) = parse_class_hierarchy(&sig.qualified_name) {
                class_methods
                    .entry(class_name)
                    .or_default()
                    .push(sig.qualified_name.clone());
            }
        }
    }

    // Find potential inheritance relationships
    // If two classes have methods with identical names, they might be related
    let class_names: Vec<_> = class_methods.keys().collect();
    for i in 0..class_names.len() {
        for j in (i + 1)..class_names.len() {
            let class_a = class_names[i];
            let class_b = class_names[j];

            // Check if these classes share method names
            if classes_share_methods(&class_methods, class_a, class_b) {
                // Potential inheritance: add edge from child to parent
                // Heuristic: shorter name is likely the parent/base class
                let (child, parent) = if class_a.len() < class_b.len() {
                    (class_b, class_a)
                } else {
                    (class_a, class_b)
                };

                // Connect all methods from child to parent
                if let Some(child_methods) = class_methods.get(child) {
                    if let Some(parent_methods) = class_methods.get(parent) {
                        // Connect first method from each class as representative
                        if let Some(child_method) = child_methods.first() {
                            if let Some(parent_method) = parent_methods.first() {
                                if let (Some(&child_id), Some(&parent_id)) = (
                                    node_ids.get(child_method),
                                    node_ids.get(parent_method),
                                ) {
                                    edges.push((child_id, parent_id));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    edges
}

/// Parse class hierarchy from a qualified name
///
/// Extracts the class name from a qualified function name.
/// Examples:
/// - `MyClass::method_name` → `Some("MyClass")`
/// - `module::submodule::Class::method` → `Some("Class")`
/// - `standalone_function` → `None`
///
/// # Arguments
///
/// * `qualified_name` - Fully qualified function/method name
///
/// # Returns
///
/// `Some(class_name)` if a class pattern is found, `None` otherwise
fn parse_class_hierarchy(qualified_name: &str) -> Option<String> {
    // Split by common namespace separators
    let parts: Vec<&str> = qualified_name.split("::").collect();

    if parts.len() >= 2 {
        // Last part is method name, second-to-last is class name
        let class_name = parts[parts.len() - 2].to_string();
        Some(class_name)
    } else {
        None
    }
}

/// Check if two classes share method names (potential inheritance)
///
/// # Arguments
///
/// * `class_methods` - Mapping of class names to their method qualified names
/// * `class_a` - First class name
/// * `class_b` - Second class name
///
/// # Returns
///
/// `true` if the classes share at least one method name
fn classes_share_methods(
    class_methods: &HashMap<String, Vec<String>>,
    class_a: &str,
    class_b: &str,
) -> bool {
    let methods_a = class_methods.get(class_a);
    let methods_b = class_methods.get(class_b);

    match (methods_a, methods_b) {
        (Some(a), Some(b)) => {
            // Extract just the method names (last part after ::)
            let names_a: HashSet<_> = a
                .iter()
                .filter_map(|q| q.split("::").last())
                .collect();

            let names_b: HashSet<_> = b
                .iter()
                .filter_map(|q| q.split("::").last())
                .collect();

            // Check for intersection
            !names_a.is_disjoint(&names_b)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leparse::prelude::{Parameter, SignatureInfo, Visibility};

    fn create_test_signature(
        name: &str,
        qualified_name: &str,
        parameters: Vec<Parameter>,
        is_method: bool,
    ) -> SignatureInfo {
        SignatureInfo {
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            parameters,
            return_type: None,
            visibility: Visibility::Public,
            is_async: false,
            is_method,
            docstring: None,
            byte_range: (0, 0),
        }
    }

    #[test]
    fn test_extract_pdg_from_signatures_empty() {
        let signatures = vec![];
        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py");

        assert_eq!(pdg.node_count(), 0);
        assert_eq!(pdg.edge_count(), 0);
    }

    #[test]
    fn test_extract_pdg_from_signatures_single_function() {
        let signatures = vec![create_test_signature(
            "my_func",
            "my_func",
            vec![],
            false,
        )];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py");

        assert_eq!(pdg.node_count(), 1);
        assert_eq!(pdg.edge_count(), 0);

        let node_id = pdg.find_by_symbol("my_func").expect("Node not found");
        let node = pdg.get_node(node_id).expect("Node weight not found");

        assert_eq!(node.name, "my_func");
        assert_eq!(node.node_type, NodeType::Function);
        assert_eq!(node.file_path, "test.py");
        assert_eq!(node.complexity, 1); // Base complexity
    }

    #[test]
    fn test_extract_pdg_from_signatures_method() {
        let signatures = vec![create_test_signature(
            "process",
            "MyClass::process",
            vec![],
            true,
        )];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py");

        assert_eq!(pdg.node_count(), 1);

        let node_id = pdg.find_by_symbol("MyClass::process").expect("Node not found");
        let node = pdg.get_node(node_id).expect("Node weight not found");

        assert_eq!(node.node_type, NodeType::Method);
        assert_eq!(node.name, "process");
    }

    #[test]
    fn test_extract_pdg_from_signatures_with_parameters() {
        let signatures = vec![create_test_signature(
            "complex_func",
            "complex_func",
            vec![
                Parameter {
                    name: "x".to_string(),
                    type_annotation: Some("i32".to_string()),
                    default_value: None,
                },
                Parameter {
                    name: "y".to_string(),
                    type_annotation: Some("String".to_string()),
                    default_value: None,
                },
            ],
            false,
        )];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py");

        let node_id = pdg.find_by_symbol("complex_func").expect("Node not found");
        let node = pdg.get_node(node_id).expect("Node weight not found");

        // Complexity = base (1) + param count (2) = 3
        assert_eq!(node.complexity, 3);
    }

    #[test]
    fn test_extract_type_dependencies() {
        let signatures = vec![
            create_test_signature(
                "process_user",
                "process_user",
                vec![Parameter {
                    name: "user".to_string(),
                    type_annotation: Some("User".to_string()),
                    default_value: None,
                }],
                false,
            ),
            create_test_signature(
                "validate_user",
                "validate_user",
                vec![Parameter {
                    name: "user".to_string(),
                    type_annotation: Some("User".to_string()),
                    default_value: None,
                }],
                false,
            ),
        ];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py");

        // Should have 2 nodes and 1 data dependency edge
        assert_eq!(pdg.node_count(), 2);
        assert_eq!(pdg.edge_count(), 1);
    }

    #[test]
    fn test_extract_type_dependencies_multiple_types() {
        let signatures = vec![
            create_test_signature(
                "func_a",
                "func_a",
                vec![
                    Parameter {
                        name: "x".to_string(),
                        type_annotation: Some("Type1".to_string()),
                        default_value: None,
                    },
                    Parameter {
                        name: "y".to_string(),
                        type_annotation: Some("Type2".to_string()),
                        default_value: None,
                    },
                ],
                false,
            ),
            create_test_signature(
                "func_b",
                "func_b",
                vec![Parameter {
                    name: "x".to_string(),
                    type_annotation: Some("Type1".to_string()),
                    default_value: None,
                }],
                false,
            ),
            create_test_signature(
                "func_c",
                "func_c",
                vec![Parameter {
                    name: "y".to_string(),
                    type_annotation: Some("Type2".to_string()),
                    default_value: None,
                }],
                false,
            ),
        ];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py");

        // Should have 3 nodes
        assert_eq!(pdg.node_count(), 3);

        // Should have edges: func_a↔func_b (Type1), func_a↔func_c (Type2)
        // That's 2 edges total
        assert_eq!(pdg.edge_count(), 2);
    }

    #[test]
    fn test_parse_class_hierarchy() {
        assert_eq!(
            parse_class_hierarchy("MyClass::method"),
            Some("MyClass".to_string())
        );

        assert_eq!(
            parse_class_hierarchy("module::submodule::Class::method"),
            Some("Class".to_string())
        );

        assert_eq!(parse_class_hierarchy("standalone_function"), None);

        assert_eq!(parse_class_hierarchy("JustClass"), None);
    }

    #[test]
    fn test_extract_inheritance_edges() {
        let signatures = vec![
            create_test_signature(
                "process",
                "Base::process",
                vec![],
                true,
            ),
            create_test_signature(
                "process",
                "DerivedClass::process",
                vec![],
                true,
            ),
        ];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py");

        // Should have 2 nodes and 1 inheritance edge
        assert_eq!(pdg.node_count(), 2);
        assert_eq!(pdg.edge_count(), 1);

        // Verify edge exists from DerivedClass to Base (longer name → shorter name)
        // The heuristic assumes shorter names are base classes
        let derived_id = pdg.find_by_symbol("DerivedClass::process").unwrap();
        let neighbors = pdg.neighbors(derived_id);
        assert_eq!(neighbors.len(), 1);

        // Verify the neighbor is Base
        let base_id = pdg.find_by_symbol("Base::process").unwrap();
        assert_eq!(neighbors[0], base_id);
    }

    #[test]
    fn test_multiple_signatures_same_file() {
        let signatures = vec![
            create_test_signature("func1", "func1", vec![], false),
            create_test_signature("func2", "func2", vec![], false),
            create_test_signature("func3", "func3", vec![], false),
        ];

        let pdg = extract_pdg_from_signatures(signatures, b"", "src/my_file.rs");

        assert_eq!(pdg.node_count(), 3);

        // All nodes should have the same file path
        for node_id in 0..pdg.node_count() {
            let node = pdg.get_node(crate::pdg::NodeId::new(node_id));
            if let Some(node) = node {
                assert_eq!(node.file_path, "src/my_file.rs");
            }
        }
    }

    #[test]
    fn test_signature_to_node_function_type() {
        let sig = create_test_signature("my_func", "my_func", vec![], false);
        let node = signature_to_node(&sig, "test.py");

        assert_eq!(node.node_type, NodeType::Function);
        assert_eq!(node.id, "my_func");
        assert_eq!(node.name, "my_func");
    }

    #[test]
    fn test_signature_to_node_method_type() {
        let sig = create_test_signature("my_method", "Class::my_method", vec![], true);
        let node = signature_to_node(&sig, "test.py");

        assert_eq!(node.node_type, NodeType::Method);
        assert_eq!(node.id, "Class::my_method");
        assert_eq!(node.name, "my_method");
    }

    #[test]
    fn test_signature_to_node_complexity_estimation() {
        let sig_with_params = create_test_signature(
            "func",
            "func",
            vec![
                Parameter {
                    name: "a".to_string(),
                    type_annotation: Some("A".to_string()),
                    default_value: None,
                },
                Parameter {
                    name: "b".to_string(),
                    type_annotation: Some("B".to_string()),
                    default_value: None,
                },
                Parameter {
                    name: "c".to_string(),
                    type_annotation: Some("C".to_string()),
                    default_value: None,
                },
            ],
            false,
        );

        let node = signature_to_node(&sig_with_params, "test.py");

        // Base complexity (1) + 3 parameters = 4
        assert_eq!(node.complexity, 4);
    }

    #[test]
    fn test_classes_share_methods() {
        let mut class_methods: HashMap<String, Vec<String>> = HashMap::new();

        class_methods.insert(
            "ClassA".to_string(),
            vec!["ClassA::method1".to_string(), "ClassA::method2".to_string()],
        );

        class_methods.insert(
            "ClassB".to_string(),
            vec!["ClassB::method1".to_string(), "ClassB::method3".to_string()],
        );

        // ClassA and ClassB share "method1"
        assert!(classes_share_methods(&class_methods, "ClassA", "ClassB"));

        class_methods.insert(
            "ClassC".to_string(),
            vec!["ClassC::unique_method".to_string()],
        );

        // ClassC shares no methods with ClassA
        assert!(!classes_share_methods(&class_methods, "ClassA", "ClassC"));
    }
}
