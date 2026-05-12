// Kotlin language parser implementation

#[cfg(feature = "parse")]
use crate::parse::traits::{Block, Edge, Parameter, Visibility};
#[cfg(feature = "parse")]
use crate::parse::traits::{
    CodeIntelligence, Error, Graph, ImportInfo, Result, SignatureInfo,
};
#[cfg(feature = "parse")]
use tree_sitter::Parser;

#[cfg(feature = "parse")]
/// Kotlin language parser with full CodeIntelligence implementation
pub struct KotlinParser;

#[cfg(feature = "parse")]
impl Default for KotlinParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "parse")]
impl KotlinParser {
    /// Create a new instance of the Kotlin parser.
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "parse")]
impl CodeIntelligence for KotlinParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::parse::traits::languages::kotlin::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Kotlin source".to_string()))?;
        let root_node = tree.root_node();
        let imports = extract_kotlin_imports(root_node, source);
        let mut signatures = Vec::new();
        visit_kotlin(&root_node, source, &mut signatures, &[]);
        for sig in &mut signatures {
            sig.imports = imports.clone();
        }
        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::parse::traits::languages::kotlin::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Kotlin source".to_string()))?;
        
        // Find the function/method with the given node_id
        fn find_node_by_id<'a>(root: &'a tree_sitter::Node<'a>, target_id: usize) -> Option<tree_sitter::Node<'a>> {
            let mut queue: std::collections::VecDeque<tree_sitter::Node<'a>> = std::collections::VecDeque::new();
            queue.push_back(*root);

            while let Some(current) = queue.pop_front() {
                if current.id() == target_id {
                    return Some(current);
                }

                let mut child_cursor = current.walk();
                for child in current.children(&mut child_cursor) {
                    queue.push_back(child);
                }
            }

            None
        }
        
        if let Some(found) = find_node_by_id(&tree.root_node(), node_id) {
            return extract_kotlin_cfg(&found, source);
        }

        Err(Error::ParseFailed("Node not found".to_string()))
    }

    fn extract_complexity(&self, node: &tree_sitter::Node<'_>) -> crate::parse::traits::ComplexityMetrics {
        let mut complexity = crate::parse::traits::ComplexityMetrics {
            cyclomatic: 1,
            nesting_depth: 0,
            line_count: 0,
            token_count: 0,
        };

        calculate_kotlin_complexity(node, &mut complexity, 0);
        complexity
    }
}

#[cfg(feature = "parse")]
fn extract_kotlin_imports(node: tree_sitter::Node<'_>, source: &[u8]) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    if node.kind() == "import_list" {
        for child in node.children(&mut node.walk()) {
            if child.kind() == "import_header" {
                let text = child.utf8_text(source).unwrap_or("");
                imports.push(ImportInfo {
                    path: text.to_string(),
                    alias: None,
                });
            }
        }
    } else {
        for child in node.children(&mut node.walk()) {
            imports.extend(extract_kotlin_imports(child, source));
        }
    }
    imports
}

#[cfg(feature = "parse")]
fn visit_kotlin(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    signatures: &mut Vec<SignatureInfo>,
    parent_path: &[String],
) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let qualified_name = if parent_path.is_empty() {
                    name.clone()
                } else {
                    format!("{}::{}", parent_path.join("::"), name)
                };

                signatures.push(SignatureInfo {
                    name: name.clone(),
                    qualified_name,
                    parameters: extract_kotlin_parameters(*node, source),
                    return_type: extract_kotlin_return_type(*node, source),
                    visibility: extract_kotlin_visibility(*node, source),
                    is_async: extract_kotlin_async(*node, source),
                    is_method: false,
                    docstring: None,
                    calls: vec![],
                    imports: Vec::new(),
                    byte_range: (node.start_byte(), node.end_byte()),
                    cyclomatic_complexity: 0,
                });
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let mut new_path = parent_path.to_vec();
                new_path.push(name.clone());
                
                for child in node.children(&mut node.walk()) {
                    visit_kotlin(&child, source, signatures, &new_path);
                }
            }
        }
        _ => {
            for child in node.children(&mut node.walk()) {
                visit_kotlin(&child, source, signatures, parent_path);
            }
        }
    }
}

#[cfg(feature = "parse")]
fn extract_kotlin_parameters(node: tree_sitter::Node<'_>, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        for child in params_node.children(&mut params_node.walk()) {
            if child.kind() == "parameter" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    parameters.push(Parameter {
                        name: name_node.utf8_text(source).unwrap_or("unknown").to_string(),
                        type_annotation: None,
                        default_value: None,
                    });
                }
            }
        }
    }
    parameters
}

#[cfg(feature = "parse")]
fn extract_kotlin_return_type(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(return_node) = node.child_by_field_name("return_type") {
        Some(return_node.utf8_text(source).unwrap_or("Unit").to_string())
    } else {
        None
    }
}

#[cfg(feature = "parse")]
fn extract_kotlin_visibility(node: tree_sitter::Node<'_>, source: &[u8]) -> Visibility {
    for child in node.children(&mut node.walk()) {
        let kind = child.kind();
        let text = child.utf8_text(source).unwrap_or("");
        if kind == "public" || kind == "private" || kind == "internal" || kind == "protected" {
            return match text {
                "public" => Visibility::Public,
                "private" => Visibility::Private,
                "internal" => Visibility::Internal,
                "protected" => Visibility::Protected,
                _ => Visibility::Private,
            };
        }
    }
    Visibility::Public
}

#[cfg(feature = "parse")]
fn extract_kotlin_async(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    for child in node.children(&mut node.walk()) {
        if child.kind() == "modifier" {
            let text = child.utf8_text(source).unwrap_or("");
            if text == "suspend" {
                return true;
            }
        }
    }
    false
}

#[cfg(feature = "parse")]
fn calculate_kotlin_complexity(
    node: &tree_sitter::Node<'_>,
    metrics: &mut crate::parse::traits::ComplexityMetrics,
    depth: usize,
) {
    let mut stack: Vec<(tree_sitter::Node<'_>, usize)> = Vec::new();
    stack.push((*node, depth));

    while let Some((current_node, current_depth)) = stack.pop() {
        metrics.nesting_depth = metrics.nesting_depth.max(current_depth);
        metrics.line_count = std::cmp::max(metrics.line_count, 1);

        match current_node.kind() {
            "if_expression" => metrics.cyclomatic += 1,
            "when_expression" => metrics.cyclomatic += 1,
            "when_entry" => metrics.cyclomatic += 1,
            "for_statement" => metrics.cyclomatic += 1,
            "while_statement" => metrics.cyclomatic += 1,
            "do_while_statement" => metrics.cyclomatic += 1,
            _ => {}
        }

        metrics.token_count += 1;

        let mut child_cursor = current_node.walk();
        for child in current_node.children(&mut child_cursor) {
            stack.push((child, current_depth + 1));
        }
    }
}

#[cfg(feature = "parse")]
fn extract_kotlin_cfg(node: &tree_sitter::Node<'_>, _source: &[u8]) -> Result<Graph<Block, Edge>> {
    let entry_block = Block {
        id: node.id(),
        statements: vec![],
    };
    let graph = Graph {
        blocks: vec![entry_block],
        edges: vec![],
        entry_block: 0,
        exit_blocks: vec![],
    };
    Ok(graph)
}
