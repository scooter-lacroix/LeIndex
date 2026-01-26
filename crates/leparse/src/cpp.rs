// C++ language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

/// C++ language parser with full CodeIntelligence implementation
pub struct CppParser;

impl Default for CppParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CppParser {
    /// Create a new C++ parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and type definitions from C++ source
    fn extract_all_definitions(
        &self,
        source: &[u8],
        root: tree_sitter::Node,
    ) -> Vec<SignatureInfo> {
        let mut signatures = Vec::new();

        fn visit_node(
            node: &tree_sitter::Node,
            source: &[u8],
            signatures: &mut Vec<SignatureInfo>,
            parent_path: &[String],
        ) {
            match node.kind() {
                "function_definition" | "function_declaration" => {
                    if let Some(sig) = extract_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    // Don't recurse into function bodies
                }
                "class_specifier" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}::{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("class".to_string()),
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });
                    }

                    // Recurse to extract class methods
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "struct_specifier" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}::{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("struct".to_string()),
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });
                    }
                }
                "enum_specifier" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}::{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("enum".to_string()),
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });
                    }
                }
                "namespace_definition" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let mut new_path = parent_path.to_vec();
                        new_path.push(name.to_string());

                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, &new_path);
                        }
                    }
                }
                _ => {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
            }
        }

        visit_node(&root, source, &mut signatures, &[]);
        signatures
    }
}

impl CodeIntelligence for CppParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        self.get_signatures_with_parser(source, &mut parser)
    }

    fn get_signatures_with_parser(
        &self,
        source: &[u8],
        parser: &mut tree_sitter::Parser,
    ) -> Result<Vec<SignatureInfo>> {
        parser
            .set_language(&crate::traits::languages::cpp::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse C++ source".to_string()))?;

        let root_node = tree.root_node();

        let signatures = self.extract_all_definitions(source, root_node);

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::cpp::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse C++ source".to_string()))?;

        let root_node = tree.root_node();

        let node = find_node_by_id(&root_node, node_id)
            .ok_or_else(|| Error::ParseFailed(format!("Node {} not found", node_id)))?;

        let mut cfg_builder = CfgBuilder::new(source);
        cfg_builder.build_from_node(&node)?;

        Ok(cfg_builder.finish())
    }

    fn extract_complexity(&self, node: &tree_sitter::Node) -> ComplexityMetrics {
        let mut complexity = ComplexityMetrics {
            cyclomatic: 1,
            nesting_depth: 0,
            line_count: 0,
            token_count: 0,
        };

        calculate_complexity(node, &mut complexity, 0);
        complexity
    }
}

/// Extract function signature from a function_definition or function_declaration node
fn extract_function_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    parent_path: &[String],
) -> Option<SignatureInfo> {
    let name = node
        .child_by_field_name("declarator")
        .and_then(|d| {
            d.child_by_field_name("name")
                .or_else(|| d.children(&mut d.walk()).find(|c| c.kind() == "identifier"))
        })
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string());

    let qualified_name = if let Some(ref name_str) = name {
        if parent_path.is_empty() {
            name_str.clone()
        } else {
            format!("{}::{}", parent_path.join("::"), name_str)
        }
    } else {
        return None;
    };

    let parameters = extract_cpp_parameters(node, source);

    let return_type = node
        .child_by_field_name("type")
        .and_then(|t| t.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    Some(SignatureInfo {
        name: name.unwrap_or_default(),
        qualified_name,
        parameters,
        return_type,
        visibility: Visibility::Public,
        is_async: false,
        is_method: false,
        docstring: extract_docstring(node, source),
    })
}

/// Extract parameters from a C++ function
fn extract_cpp_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    // Look for parameter list in the declarator
    let declarator = node.child_by_field_name("declarator");

    if let Some(decl) = declarator {
        let mut cursor = decl.walk();
        for child in decl.children(&mut cursor) {
            if child.kind() == "parameter_list" {
                let mut pcursor = child.walk();
                for param in child.children(&mut pcursor) {
                    if param.kind() == "parameter_declaration" {
                        let name = param
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok())
                            .map(|s| s.to_string());

                        let type_annotation = param
                            .child_by_field_name("type")
                            .and_then(|t| t.utf8_text(source).ok())
                            .map(|s| s.trim().to_string());

                        // If no name field, try to find identifier
                        let param_name = name.or_else(|| {
                            let mut ccursor = param.walk();
                            loop {
                                if !ccursor.goto_next_sibling() {
                                    break None;
                                }
                                let node = ccursor.node();
                                if node.kind() == "identifier" {
                                    if let Ok(text) = node.utf8_text(source) {
                                        break Some(text.to_string());
                                    }
                                }
                            }
                        });

                        if let Some(name_text) = param_name {
                            parameters.push(Parameter {
                                name: name_text,
                                type_annotation,
                                default_value: None,
                            });
                        }
                    }
                }
            }
        }
    }

    parameters
}

/// Extract docstring from a node
fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let mut prev_sibling = None;

    if let Some(parent) = node.parent() {
        let mut pcursor = parent.walk();
        for child in parent.children(&mut pcursor) {
            if child.id() == node.id() {
                break;
            }
            prev_sibling = Some(child);
        }
    }

    if let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" {
            if let Ok(text) = sibling.utf8_text(source) {
                return Some(
                    text.trim()
                        .trim_start_matches("/*")
                        .trim_start_matches("//")
                        .trim_start_matches("///")
                        .trim_end_matches("*/")
                        .trim()
                        .to_string(),
                );
            }
        }
    }

    None
}

/// Find a node by its ID
fn find_node_by_id<'a>(node: &tree_sitter::Node<'a>, id: usize) -> Option<tree_sitter::Node<'a>> {
    if node.id() == id {
        return Some(*node);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_node_by_id(&child, id) {
            return Some(found);
        }
    }

    None
}

/// Calculate complexity metrics
fn calculate_complexity(node: &tree_sitter::Node, metrics: &mut ComplexityMetrics, depth: usize) {
    metrics.nesting_depth = metrics.nesting_depth.max(depth);
    metrics.line_count = std::cmp::max(metrics.line_count, 1);

    match node.kind() {
        "if_statement"
        | "for_statement"
        | "while_statement"
        | "do_statement"
        | "switch_statement"
        | "case_statement" => {
            metrics.cyclomatic += 1;
        }
        _ => {}
    }

    metrics.token_count += node.child_count();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calculate_complexity(&child, metrics, depth + 1);
    }
}

/// Control flow graph builder
struct CfgBuilder<'a> {
    source: &'a [u8],
    blocks: Vec<Block>,
    edges: Vec<Edge>,
    next_block_id: usize,
}

impl<'a> CfgBuilder<'a> {
    fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            blocks: Vec::new(),
            edges: Vec::new(),
            next_block_id: 0,
        }
    }

    fn build_from_node(&mut self, node: &tree_sitter::Node) -> Result<()> {
        let entry_id = self.create_block();
        self.build_cfg_recursive(node, entry_id)?;
        Ok(())
    }

    fn build_cfg_recursive(
        &mut self,
        node: &tree_sitter::Node,
        current_block: usize,
    ) -> Result<()> {
        match node.kind() {
            "if_statement" => {
                self.handle_if_statement(node, current_block)?;
            }
            "for_statement" | "while_statement" | "do_statement" => {
                self.handle_loop_statement(node, current_block)?;
            }
            "switch_statement" => {
                self.handle_switch_statement(node, current_block)?;
            }
            _ => {
                if let Ok(text) = node.utf8_text(self.source) {
                    self.add_statement_to_block(current_block, text.to_string());
                }

                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    self.build_cfg_recursive(&child, current_block)?;
                }
            }
        }

        Ok(())
    }

    fn handle_if_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let true_block = self.create_block();
        let false_block = self.create_block();
        let merge_block = self.create_block();

        self.edges.push(Edge {
            from: current_block,
            to: true_block,
            edge_type: EdgeType::TrueBranch,
        });
        self.edges.push(Edge {
            from: current_block,
            to: false_block,
            edge_type: EdgeType::FalseBranch,
        });
        self.edges.push(Edge {
            from: true_block,
            to: merge_block,
            edge_type: EdgeType::Unconditional,
        });
        self.edges.push(Edge {
            from: false_block,
            to: merge_block,
            edge_type: EdgeType::Unconditional,
        });

        Ok(())
    }

    fn handle_loop_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let body_block = self.create_block();

        self.edges.push(Edge {
            from: current_block,
            to: body_block,
            edge_type: EdgeType::Unconditional,
        });
        self.edges.push(Edge {
            from: body_block,
            to: current_block,
            edge_type: EdgeType::Loop,
        });

        Ok(())
    }

    fn handle_switch_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let merge_block = self.create_block();

        let mut cursor = _node.walk();
        for child in _node.children(&mut cursor) {
            if child.kind() == "case_statement" || child.kind() == "default_statement" {
                let case_block = self.create_block();
                self.edges.push(Edge {
                    from: current_block,
                    to: case_block,
                    edge_type: EdgeType::TrueBranch,
                });
                self.edges.push(Edge {
                    from: case_block,
                    to: merge_block,
                    edge_type: EdgeType::Unconditional,
                });
            }
        }

        Ok(())
    }

    fn create_block(&mut self) -> usize {
        let id = self.next_block_id;
        self.next_block_id += 1;
        self.blocks.push(Block {
            id,
            statements: Vec::new(),
        });
        id
    }

    fn add_statement_to_block(&mut self, block_id: usize, statement: String) {
        if let Some(block) = self.blocks.get_mut(block_id) {
            block.statements.push(statement);
        }
    }

    fn finish(self) -> Graph<Block, Edge> {
        Graph {
            blocks: self.blocks,
            edges: self.edges,
            entry_block: 0,
            exit_blocks: vec![self.next_block_id.saturating_sub(1)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpp_function_extraction() {
        let source = b"void greet(std::string name) {
    std::cout << \"Hello, \" << name << std::endl;
}";

        let parser = CppParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(!signatures.is_empty());
        let func = signatures.iter().find(|s| s.name == "greet");
        assert!(func.is_some());
    }

    #[test]
    fn test_cpp_class_extraction() {
        let source = b"class MyClass {
public:
    void method();
private:
    int value;
};";

        let parser = CppParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let my_class = signatures.iter().find(|s| s.name == "MyClass");
        assert!(my_class.is_some());
    }

    #[test]
    fn test_cpp_struct_extraction() {
        let source = b"struct Point {
    double x;
    double y;
};";

        let parser = CppParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let point = signatures.iter().find(|s| s.name == "Point");
        assert!(point.is_some());
        assert_eq!(point.unwrap().return_type, Some("struct".to_string()));
    }

    #[test]
    fn test_cpp_namespace_extraction() {
        let source = b"namespace MyNamespace {
    void function() {}
}";

        let parser = CppParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let func = signatures.iter().find(|s| s.qualified_name.contains("MyNamespace"));
        assert!(func.is_some());
    }

    #[test]
    fn test_cpp_complexity_calculation() {
        let source = b"void complex(int x) {
    if (x > 0) {
        for (int i = 0; i < x; i++) {
            if (i % 2 == 0) {
                std::cout << i << std::endl;
            }
        }
    }
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let cpp_parser = CppParser::new();
        let metrics = cpp_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }
}
