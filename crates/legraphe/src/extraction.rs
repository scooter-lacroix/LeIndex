// AST → PDG Extraction Module
//
// *L'Extraction* (The Extraction) - Transforms parsed signatures into Program Dependence Graphs
//
// # Overview
//
// This module bridges leparse (parsing) and legraphe (graph intelligence) by converting
// `SignatureInfo` structures into `ProgramDependenceGraph` instances.
//
// # Call Graph Extraction
//
// This module can extract **best-effort** call graph edges when parsers populate
// `SignatureInfo.calls`. The extraction is limited to what each language parser
// can recover from its AST (e.g., identifier/method names) and does not resolve
// dynamic dispatch or cross-module aliasing.
//
// # What IS Extracted
//
// 1. **Nodes**: Functions/methods from signatures with metadata
// 2. **Type Dependencies**: Edges between functions using similar parameter types
// 3. **Class Hierarchy**: Inheritance edges from qualified_name patterns (e.g., `Class::method`)
// 4. **Call Graph**: Edges from `SignatureInfo.calls` (best-effort)
//
// # Future Enhancement Path
//
// For richer call graph extraction:
// 1. Capture fully-qualified call targets in parsers
// 2. Add import/namespace resolution to reduce ambiguity
// 3. Track call sites with line/column information

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
    source_code: &[u8],
    file_path: &str,
    language: &str,
) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();

    // Track node IDs for edge creation
    let mut node_ids: HashMap<String, crate::pdg::NodeId> = HashMap::new();

    // Phase 1: Create nodes from signatures
    for signature in &signatures {
        let node = signature_to_node(signature, file_path, language);
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

    // Phase 4: Extract and add call graph edges
    let call_edges = extract_call_edges(&signatures, &node_ids);
    pdg.add_call_graph_edges(call_edges);

    // Phase 5: Extract and add import edges (with external module fallback)
    let import_edges = extract_import_edges(
        &signatures,
        &node_ids,
        &mut pdg,
        file_path,
        language,
        source_code,
    );
    pdg.add_import_edges(import_edges);

    pdg
}

/// Normalize symbols/import paths into a dotted comparable form used by extraction and relinking.
pub fn normalize_symbol(raw: &str) -> String {
    let trimmed = raw.split('(').next().unwrap_or(raw).trim();
    trimmed
        .replace("?.", ".")
        .replace("::", ".")
        .replace("->", ".")
        .replace('\\', ".")
        .replace('/', ".")
        .replace(':', ".")
        .replace("..", ".")
        .trim_matches('.')
        .to_string()
}

/// Extract call edges from signatures
fn extract_call_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::pdg::NodeId>,
) -> Vec<(crate::pdg::NodeId, crate::pdg::NodeId)> {
    let mut edges = Vec::new();
    let mut seen_edges: HashSet<(crate::pdg::NodeId, crate::pdg::NodeId)> = HashSet::new();

    fn symbol_segments(raw: &str) -> Vec<String> {
        normalize_symbol(raw)
            .split('.')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    fn suffix_keys(segments: &[String], max_len: usize) -> Vec<String> {
        let mut keys = Vec::new();
        let len = segments.len();
        if len < 2 {
            return keys;
        }

        for suffix_len in 2..=max_len.min(len) {
            let start = len - suffix_len;
            let key = segments[start..].join(".");
            keys.push(key);
        }

        keys
    }

    fn namespace_prefix(qualified: &str) -> Option<String> {
        let segments = symbol_segments(qualified);
        if segments.len() <= 1 {
            None
        } else {
            Some(segments[..segments.len() - 1].join("."))
        }
    }

    let mut alias_map: HashMap<String, String> = HashMap::new();
    for signature in signatures {
        for import in &signature.imports {
            let alias = import.alias.clone().or_else(|| {
                import
                    .path
                    .split(|c| c == '.' || c == ':' || c == '\\' || c == '/')
                    .last()
                    .map(|s| s.to_string())
            });
            if let Some(alias) = alias {
                alias_map
                    .entry(alias)
                    .or_insert_with(|| import.path.clone());
            }
        }
    }

    let mut exact_map: HashMap<String, Vec<crate::pdg::NodeId>> = HashMap::new();
    let mut last_map: HashMap<String, Vec<crate::pdg::NodeId>> = HashMap::new();
    let mut suffix_map: HashMap<String, Vec<crate::pdg::NodeId>> = HashMap::new();
    let mut namespace_map: HashMap<String, Vec<crate::pdg::NodeId>> = HashMap::new();

    for sig in signatures {
        if let Some(&id) = node_ids.get(&sig.qualified_name) {
            let normalized = normalize_symbol(&sig.qualified_name);
            let segments = symbol_segments(&sig.qualified_name);

            exact_map.entry(normalized).or_default().push(id);

            if let Some(last) = segments.last() {
                last_map.entry(last.clone()).or_default().push(id);
            }

            if let Some(namespace) = namespace_prefix(&sig.qualified_name) {
                namespace_map.entry(namespace).or_default().push(id);
            }

            for suffix in suffix_keys(&segments, 3) {
                suffix_map.entry(suffix).or_default().push(id);
            }
        }
    }

    for sig in signatures {
        let Some(&caller_id) = node_ids.get(&sig.qualified_name) else {
            continue;
        };

        let caller_namespace = namespace_prefix(&sig.qualified_name);

        for call_target in &sig.calls {
            let mut candidates = Vec::new();
            candidates.push(call_target.clone());

            let segments = symbol_segments(call_target);
            if let Some(first) = segments.first() {
                if let Some(import_path) = alias_map.get(first) {
                    if segments.len() == 1 {
                        candidates.push(import_path.clone());
                    } else {
                        candidates.push(format!("{}.{}", import_path, segments[1..].join(".")));
                    }
                }
            }

            if let Some(namespace) = &caller_namespace {
                if let Some(first) = segments.first() {
                    if matches!(first.as_str(), "self" | "this" | "super" | "Self" | "crate") {
                        let rest = segments
                            .iter()
                            .skip(1)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(".");
                        if !rest.is_empty() {
                            candidates.push(format!("{}.{}", namespace, rest));
                        }
                    } else if segments.len() == 1 {
                        candidates.push(format!("{}.{}", namespace, first));
                    }
                }
            }

            let mut targets: Vec<crate::pdg::NodeId> = Vec::new();
            for candidate in candidates {
                let normalized_call = normalize_symbol(&candidate);
                if normalized_call.is_empty() {
                    continue;
                }
                let candidate_segments = symbol_segments(&candidate);

                if let Some(ids) = exact_map.get(&normalized_call) {
                    targets.extend(ids.iter().copied());
                }

                if let Some(last) = candidate_segments.last() {
                    if let Some(ids) = last_map.get(last) {
                        targets.extend(ids.iter().copied());
                    }
                }

                if candidate_segments.len() > 1 {
                    for suffix in suffix_keys(&candidate_segments, 3) {
                        if let Some(ids) = suffix_map.get(&suffix) {
                            targets.extend(ids.iter().copied());
                        }
                    }
                } else if let Some(namespace) = &caller_namespace {
                    if let Some(ids) = namespace_map.get(namespace) {
                        targets.extend(ids.iter().copied());
                    }
                }
            }

            for target_id in targets {
                if caller_id != target_id && seen_edges.insert((caller_id, target_id)) {
                    edges.push((caller_id, target_id));
                }
            }
        }
    }

    edges
}

/// Extract import edges from signatures.
fn extract_import_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::pdg::NodeId>,
    pdg: &mut ProgramDependenceGraph,
    file_path: &str,
    language: &str,
    source_code: &[u8],
) -> Vec<(crate::pdg::NodeId, crate::pdg::NodeId)> {
    let mut edges = Vec::new();
    let mut seen_edges: HashSet<(crate::pdg::NodeId, crate::pdg::NodeId)> = HashSet::new();

    let mut unique_import_paths = signatures
        .iter()
        .flat_map(|signature| signature.imports.iter().map(|import| import.path.clone()))
        .collect::<HashSet<_>>();

    unique_import_paths.extend(extract_import_paths_from_source(source_code, language));

    if unique_import_paths.is_empty() {
        return edges;
    }

    let importer_module_id = pdg
        .find_by_symbol(&format!("{}:__module__", file_path))
        .unwrap_or_else(|| {
            pdg.add_node(Node {
                id: format!("{}:__module__", file_path),
                node_type: NodeType::Module,
                name: "__module__".to_string(),
                file_path: file_path.to_string(),
                byte_range: (0, 0),
                complexity: 1,
                language: language.to_string(),
                embedding: None,
            })
        });

    let mut symbol_map: HashMap<String, Vec<crate::pdg::NodeId>> = HashMap::new();
    for signature in signatures {
        if let Some(node_id) = node_ids.get(&signature.qualified_name) {
            let normalized = normalize_import_symbol(&signature.qualified_name);
            if !normalized.is_empty() {
                symbol_map
                    .entry(normalized.clone())
                    .or_default()
                    .push(*node_id);
            }

            if let Some(last) = normalized.split('.').last() {
                symbol_map
                    .entry(last.to_string())
                    .or_default()
                    .push(*node_id);
            }
        }
    }

    let mut external_nodes: HashMap<String, crate::pdg::NodeId> = HashMap::new();

    for import_path in unique_import_paths {
        let mut targets = resolve_import_targets(&import_path, &symbol_map);

        if targets.is_empty() {
            let external_id = external_nodes
                .entry(import_path.clone())
                .or_insert_with(|| {
                    pdg.add_node(Node {
                        id: format!("{}:__external__:{}", file_path, import_path),
                        node_type: NodeType::Module,
                        name: import_path.clone(),
                        file_path: file_path.to_string(),
                        byte_range: (0, 0),
                        complexity: 1,
                        language: "external".to_string(),
                        embedding: None,
                    })
                });
            targets.push(*external_id);
        }

        for target in &targets {
            if importer_module_id == *target {
                continue;
            }
            if seen_edges.insert((importer_module_id, *target)) {
                edges.push((importer_module_id, *target));
            }
        }
    }

    edges
}

fn normalize_import_symbol(raw: &str) -> String {
    normalize_symbol(raw)
}

fn resolve_import_targets(
    import_path: &str,
    symbol_map: &HashMap<String, Vec<crate::pdg::NodeId>>,
) -> Vec<crate::pdg::NodeId> {
    let mut targets = Vec::new();
    let normalized = normalize_import_symbol(import_path);

    if let Some(ids) = symbol_map.get(&normalized) {
        targets.extend(ids.iter().copied());
    }

    if normalized.contains('.') {
        for suffix in suffix_candidates(&normalized, 3) {
            if let Some(ids) = symbol_map.get(&suffix) {
                targets.extend(ids.iter().copied());
            }
        }
    }

    if targets.is_empty() {
        if let Some(last) = normalized.split('.').last() {
            if let Some(ids) = symbol_map.get(last) {
                targets.extend(ids.iter().copied());
            }
        }
    }

    targets.sort_by_key(|id| id.index());
    targets.dedup();
    targets
}

fn suffix_candidates(value: &str, max_len: usize) -> Vec<String> {
    let segments = value.split('.').collect::<Vec<_>>();
    let mut out = Vec::new();

    for len in 2..=segments.len().min(max_len) {
        let start = segments.len() - len;
        out.push(segments[start..].join("."));
    }

    out
}

fn extract_import_paths_from_source(source_code: &[u8], language: &str) -> HashSet<String> {
    let mut imports = HashSet::new();
    let Ok(source_text) = std::str::from_utf8(source_code) else {
        return imports;
    };

    let lang = language.to_ascii_lowercase();

    for raw_line in source_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }

        if lang == "python" || lang == "py" {
            if let Some(rest) = line.strip_prefix("import ") {
                for segment in rest.split(',') {
                    let name = segment
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .trim_matches(',');
                    if !name.is_empty() {
                        imports.insert(name.to_string());
                    }
                }
            }

            if let Some(rest) = line.strip_prefix("from ") {
                let module = rest.split(" import ").next().unwrap_or_default().trim();
                if !module.is_empty() {
                    imports.insert(module.to_string());
                }
            }
        }

        if (lang == "javascript"
            || lang == "typescript"
            || lang == "js"
            || lang == "ts"
            || lang == "jsx"
            || lang == "tsx")
            && (line.starts_with("import ") || line.starts_with("export "))
        {
            if let Some(path) = extract_quoted_path(line) {
                imports.insert(path);
            }
        }

        if (lang == "rust" || lang == "rs") && line.starts_with("use ") {
            let use_stmt = line.trim_start_matches("use ").trim_end_matches(';').trim();
            if let Some((base, rest)) = use_stmt.split_once('{') {
                let base = base.trim().trim_end_matches("::");
                let members = rest.trim_end_matches('}');
                for member in members.split(',') {
                    let member = member.trim();
                    if member.is_empty() || member == "self" {
                        continue;
                    }
                    imports.insert(format!("{}.{}", base.replace("::", "."), member));
                }
            } else if !use_stmt.is_empty() {
                imports.insert(use_stmt.replace("::", "."));
            }
        }

        if (lang == "go" || lang == "golang") && line.starts_with("import") {
            if let Some(path) = extract_quoted_path(line) {
                imports.insert(path);
            }
        }
    }

    imports
}

fn extract_quoted_path(line: &str) -> Option<String> {
    let first_quote = line.find(['\'', '"'])?;
    let quote = line.chars().nth(first_quote)?;
    let remaining = &line[first_quote + 1..];
    let second = remaining.find(quote)?;
    let path = remaining[..second].trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
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
fn signature_to_node(sig: &SignatureInfo, file_path: &str, language: &str) -> Node {
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
        language: language.to_string(),
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
                                if let (Some(&child_id), Some(&parent_id)) =
                                    (node_ids.get(child_method), node_ids.get(parent_method))
                                {
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
            let names_a: HashSet<_> = a.iter().filter_map(|q| q.split("::").last()).collect();

            let names_b: HashSet<_> = b.iter().filter_map(|q| q.split("::").last()).collect();

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
            calls: Vec::new(),
            imports: Vec::new(),
            byte_range: (0, 0),
        }
    }

    #[test]
    fn test_extract_pdg_from_signatures_empty() {
        let signatures = vec![];
        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py", "python");

        assert_eq!(pdg.node_count(), 0);
        assert_eq!(pdg.edge_count(), 0);
    }

    #[test]
    fn test_extract_pdg_from_signatures_single_function() {
        let signatures = vec![create_test_signature("my_func", "my_func", vec![], false)];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py", "python");

        assert_eq!(pdg.node_count(), 1);
        assert_eq!(pdg.edge_count(), 0);

        let node_id = pdg
            .find_by_symbol("test.py:my_func")
            .expect("Node not found");
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

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py", "python");

        assert_eq!(pdg.node_count(), 1);

        let node_id = pdg
            .find_by_symbol("test.py:MyClass::process")
            .expect("Node not found");
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

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py", "python");

        let node_id = pdg
            .find_by_symbol("test.py:complex_func")
            .expect("Node not found");
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

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py", "python");

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

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py", "python");

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
            create_test_signature("process", "Base::process", vec![], true),
            create_test_signature("process", "DerivedClass::process", vec![], true),
        ];

        let pdg = extract_pdg_from_signatures(signatures, b"", "test.py", "python");

        // Should have 2 nodes and 1 inheritance edge
        assert_eq!(pdg.node_count(), 2);
        assert_eq!(pdg.edge_count(), 1);

        // Verify edge exists from DerivedClass to Base (longer name → shorter name)
        // The heuristic assumes shorter names are base classes
        let derived_id = pdg.find_by_symbol("test.py:DerivedClass::process").unwrap();
        let neighbors = pdg.neighbors(derived_id);
        assert_eq!(neighbors.len(), 1);

        // Verify the neighbor is Base
        let base_id = pdg.find_by_symbol("test.py:Base::process").unwrap();
        assert_eq!(neighbors[0], base_id);
    }

    #[test]
    fn test_multiple_signatures_same_file() {
        let signatures = vec![
            create_test_signature("func1", "func1", vec![], false),
            create_test_signature("func2", "func2", vec![], false),
            create_test_signature("func3", "func3", vec![], false),
        ];

        let pdg = extract_pdg_from_signatures(signatures, b"", "src/my_file.rs", "rust");

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
        let node = signature_to_node(&sig, "test.py", "python");

        assert_eq!(node.node_type, NodeType::Function);
        assert_eq!(node.id, "test.py:my_func");
        assert_eq!(node.name, "my_func");
    }

    #[test]
    fn test_signature_to_node_method_type() {
        let sig = create_test_signature("my_method", "Class::my_method", vec![], true);
        let node = signature_to_node(&sig, "test.py", "python");

        assert_eq!(node.node_type, NodeType::Method);
        assert_eq!(node.id, "test.py:Class::my_method");
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

        let node = signature_to_node(&sig_with_params, "test.py", "python");

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

    #[test]
    fn test_extract_import_edges_local_resolution() {
        let mut signatures = vec![
            create_test_signature("main", "pkg::main", vec![], false),
            create_test_signature("helper", "pkg::helper", vec![], false),
        ];
        signatures[0].imports.push(leparse::prelude::ImportInfo {
            path: "pkg.helper".to_string(),
            alias: None,
        });

        let pdg = extract_pdg_from_signatures(signatures, b"", "src/main.rs", "rust");

        let import_edges = pdg
            .edge_indices()
            .filter_map(|idx| pdg.get_edge(idx))
            .filter(|edge| edge.edge_type == crate::pdg::EdgeType::Import)
            .count();

        assert!(import_edges >= 1, "expected at least one import edge");
    }

    #[test]
    fn test_import_edges_are_anchored_to_file_module_node() {
        let mut signatures = vec![
            create_test_signature("main", "pkg::main", vec![], false),
            create_test_signature("helper", "pkg::helper", vec![], false),
        ];
        signatures[0].imports.push(leparse::prelude::ImportInfo {
            path: "pkg.helper".to_string(),
            alias: None,
        });

        let pdg = extract_pdg_from_signatures(signatures, b"", "src/main.rs", "rust");

        let module_node = pdg
            .node_indices()
            .find(|&idx| {
                pdg.get_node(idx)
                    .map(|node| {
                        node.node_type == NodeType::Module
                            && node.file_path == "src/main.rs"
                            && node.language == "rust"
                    })
                    .unwrap_or(false)
            })
            .expect("expected module-level importer node");

        let mut module_import_edges = 0usize;
        let mut non_module_import_edges = 0usize;

        for edge_idx in pdg.edge_indices() {
            let Some(edge) = pdg.get_edge(edge_idx) else {
                continue;
            };
            if edge.edge_type != crate::pdg::EdgeType::Import {
                continue;
            }

            let Some((from, _)) = pdg.edge_endpoints(edge_idx) else {
                continue;
            };

            if from == module_node {
                module_import_edges += 1;
            } else {
                non_module_import_edges += 1;
            }
        }

        assert!(
            module_import_edges >= 1,
            "expected import edge from module node"
        );
        assert_eq!(
            non_module_import_edges, 0,
            "function/method nodes should not be direct import sources"
        );
    }

    #[test]
    fn test_extract_import_edges_external_fallback() {
        let mut signatures = vec![create_test_signature("run", "pkg::run", vec![], false)];
        signatures[0].imports.push(leparse::prelude::ImportInfo {
            path: "third.party.lib".to_string(),
            alias: None,
        });

        let pdg = extract_pdg_from_signatures(signatures, b"", "src/run.rs", "rust");

        let external_module_exists = pdg.node_indices().any(|idx| {
            pdg.get_node(idx)
                .map(|n| n.node_type == NodeType::Module && n.language == "external")
                .unwrap_or(false)
        });

        let import_edges = pdg
            .edge_indices()
            .filter_map(|idx| pdg.get_edge(idx))
            .filter(|edge| edge.edge_type == crate::pdg::EdgeType::Import)
            .count();

        assert!(
            external_module_exists,
            "expected synthetic external module node"
        );
        assert!(import_edges >= 1, "expected import edge to external module");
    }

    #[test]
    fn test_empty_signatures_can_still_emit_import_edges_from_source() {
        let source = b"import third.party\n";
        let pdg = extract_pdg_from_signatures(vec![], source, "src/__init__.py", "python");

        let import_edges = pdg
            .edge_indices()
            .filter_map(|idx| pdg.get_edge(idx))
            .filter(|edge| edge.edge_type == crate::pdg::EdgeType::Import)
            .count();

        let has_module_anchor = pdg.find_by_symbol("src/__init__.py:__module__").is_some();

        assert!(has_module_anchor, "expected module anchor node");
        assert!(
            import_edges >= 1,
            "expected import edge for empty-signature import-only file"
        );
    }

    #[test]
    fn test_call_alias_resolution_unions_imports_from_all_signatures() {
        let mut first = create_test_signature("first", "pkg::first", vec![], false);
        first.calls.push("alias.run".to_string());

        let mut second = create_test_signature("run", "real::module::run", vec![], false);
        second.imports.push(leparse::prelude::ImportInfo {
            path: "real.module".to_string(),
            alias: Some("alias".to_string()),
        });

        let pdg = extract_pdg_from_signatures(vec![first, second], b"", "src/main.rs", "rust");

        let call_edges = pdg
            .edge_indices()
            .filter_map(|idx| {
                let edge = pdg.get_edge(idx)?;
                (edge.edge_type == crate::pdg::EdgeType::Call).then_some(idx)
            })
            .count();

        assert!(
            call_edges >= 1,
            "expected alias-based call edge even when alias is only present in non-first signature"
        );
    }
}
