// Rust language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

/// Rust language parser with full CodeIntelligence implementation
pub struct RustParser;

impl Default for RustParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RustParser {
    /// Create a new Rust parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and type definitions from Rust source
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
                "function_item" => {
                    if let Some(sig) = extract_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    // Don't recurse into function bodies
                }
                "mod_item" => {
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
                "impl_item" => {
                    // Extract methods from impl block
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        // Functions are inside declaration_list
                        if child.kind() == "declaration_list" {
                            let mut dcursor = child.walk();
                            for dc in child.children(&mut dcursor) {
                                if dc.kind() == "function_item" {
                                    if let Some(sig) = extract_function_signature(&dc, source, parent_path) {
                                        signatures.push(sig);
                                    }
                                }
                            }
                        } else if child.kind() == "function_item" {
                            // Direct function_item (shouldn't happen but handle it)
                            if let Some(sig) = extract_function_signature(&child, source, parent_path) {
                                signatures.push(sig);
                            }
                        }
                    }
                }
                "trait_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("trait".to_string()),
                            visibility: extract_visibility(node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });
                    }

                    // Recurse to extract trait methods
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "struct_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("::"), name)
                        };

                        // Extract type parameters
                        let type_params = node
                            .child_by_field_name("type_parameters")
                            .and_then(|tp| tp.utf8_text(source).ok())
                            .map(|s| s.trim().to_string());

                        let return_type = if let Some(tp) = type_params {
                            format!("struct{}", tp)
                        } else {
                            "struct".to_string()
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some(return_type),
                            visibility: extract_visibility(node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });
                    }
                }
                "enum_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("enum".to_string()),
                            visibility: extract_visibility(node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });
                    }
                }
                "use_declaration" => {
                    if let Some(sig) = extract_import_signature(node, source, parent_path) {
                        signatures.push(sig);
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

impl CodeIntelligence for RustParser {
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
            .set_language(&crate::traits::languages::rust::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Rust source".to_string()))?;

        let root_node = tree.root_node();

        let signatures = self.extract_all_definitions(source, root_node);

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::rust::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Rust source".to_string()))?;

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

/// Extract function signature from a function_item node
fn extract_function_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    parent_path: &[String],
) -> Option<SignatureInfo> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())?;

    let qualified_name = if parent_path.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", parent_path.join("::"), name)
    };

    let parameters = extract_rust_parameters(node, source);

    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|r| r.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    let is_async = node
        .children(&mut node.walk())
        .any(|c| {
            c.kind() == "function_modifiers"
                && c.children(&mut c.walk()).any(|cc| cc.kind() == "async")
        });

    let visibility = extract_visibility(node, source);

    // Check if this is a method (has self parameter)
    let is_method = parameters
        .first()
        .map(|p| p.name.contains("self"))
        .unwrap_or(false);

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility,
        is_async,
        is_method,
        docstring: extract_docstring(node, source),
    })
}

/// Extract import signature from a use_declaration node
fn extract_import_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    _parent_path: &[String],
) -> Option<SignatureInfo> {
    let import_arg = node
        .child_by_field_name("argument")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())?;

    // Extract just the last part as the "name"
    let name = import_arg
        .split("::")
        .last()
        .unwrap_or(&import_arg)
        .split('{')
        .next()
        .unwrap_or(&import_arg)
        .split(' ')
        .next()
        .unwrap_or(&import_arg)
        .to_string();

    Some(SignatureInfo {
        name: name.clone(),
        qualified_name: import_arg,
        parameters: vec![],
        return_type: Some("use".to_string()),
        visibility: Visibility::Public,
        is_async: false,
        is_method: false,
        docstring: None,
    })
}

/// Extract visibility modifier from a node
fn extract_visibility(node: &tree_sitter::Node, source: &[u8]) -> Visibility {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            if let Ok(text) = child.utf8_text(source) {
                if text.contains("pub") && !text.contains("pub(crate)") && !text.contains("pub(super)") {
                    return Visibility::Public;
                } else if text.contains("pub(crate)") || text.contains("pub(super)") {
                    return Visibility::Protected; // Use protected for restricted visibility
                }
            }
        }
    }
    Visibility::Private
}

/// Extract parameters from a Rust function
fn extract_rust_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "self_parameter" {
                // Self parameter (&self, &mut self, self)
                if let Ok(text) = child.utf8_text(source) {
                    parameters.push(Parameter {
                        name: text.trim().to_string(),
                        type_annotation: Some("self".to_string()),
                        default_value: None,
                    });
                }
            } else if child.kind() == "parameter" {
                // Regular parameter: name: Type
                let mut name = None;
                let mut type_annotation = None;

                let mut ccursor = child.walk();
                for param_child in child.children(&mut ccursor) {
                    match param_child.kind() {
                        "identifier" => {
                            if let Ok(text) = param_child.utf8_text(source) {
                                name = Some(text.to_string());
                            }
                        }
                        ":" | "," | "(" | ")" => {
                            // Skip punctuation
                        }
                        _ => {
                            // Everything else is likely a type annotation
                            if let Ok(text) = param_child.utf8_text(source) {
                                let text = text.trim();
                                if !text.is_empty() && text != ":" && text != "," {
                                    type_annotation = Some(text.to_string());
                                }
                            }
                        }
                    }
                }

                // Only add if we have a name
                if let Some(name_text) = name {
                    parameters.push(Parameter {
                        name: name_text,
                        type_annotation,
                        default_value: None,
                    });
                }
            }
        }
    }

    parameters
}

/// Extract docstring from a node
fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Look for doc comments before the node
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

    // Check for doc comment (line or block)
    if let Some(sibling) = prev_sibling {
        if sibling.kind() == "line_comment" || sibling.kind() == "block_comment" {
            if let Ok(text) = sibling.utf8_text(source) {
                let is_doc = text.starts_with("///") || text.starts_with("//!")
                    || text.starts_with("/**") || text.starts_with("/*!");
                if is_doc {
                    return Some(
                        text.trim()
                            .trim_start_matches("///")
                            .trim_start_matches("//!")
                            .trim_start_matches("/**")
                            .trim_start_matches("/*!")
                            .trim_end_matches("*/")
                            .trim()
                            .to_string(),
                    );
                }
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
        "if_expression"
        | "if_let_expression"
        | "while_expression"
        | "while_let_expression"
        | "for_expression"
        | "loop_expression"
        | "match_expression"
        | "match_arm"
        | "if_expression_else" => {
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
            "if_expression" | "if_let_expression" => {
                self.handle_if_statement(node, current_block)?;
            }
            "while_expression"
            | "while_let_expression"
            | "for_expression"
            | "loop_expression" => {
                self.handle_loop_statement(node, current_block)?;
            }
            "match_expression" => {
                self.handle_match_statement(node, current_block)?;
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

    fn handle_match_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let merge_block = self.create_block();

        // Create a block for each match arm
        let mut cursor = _node.walk();
        let mut has_arms = false;
        for child in _node.children(&mut cursor) {
            if child.kind() == "match_arm" {
                has_arms = true;
                let arm_block = self.create_block();
                self.edges.push(Edge {
                    from: current_block,
                    to: arm_block,
                    edge_type: EdgeType::TrueBranch,
                });
                self.edges.push(Edge {
                    from: arm_block,
                    to: merge_block,
                    edge_type: EdgeType::Unconditional,
                });
            }
        }

        if !has_arms {
            self.edges.push(Edge {
                from: current_block,
                to: merge_block,
                edge_type: EdgeType::Unconditional,
            });
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
    fn test_rust_function_extraction() {
        let source = b"fn greet(name: &str) -> String {
    format!(\"Hello, {}\", name)
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "greet");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].name, "name");
        assert_eq!(sig.return_type, Some("String".to_string()));
        assert!(!sig.is_method);
    }

    #[test]
    fn test_rust_async_function() {
        let source = b"async fn fetch_data(url: &str) -> Result<String, Error> {
    Ok(String::new())
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "fetch_data");
        assert!(sig.is_async);
    }

    #[test]
    fn test_rust_method_extraction() {
        let source = b"impl Server {
    fn new() -> Self {
        Server {}
    }

    pub fn start(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Client for Server {
    fn connect(&self) -> bool {
        true
    }
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find methods from impl blocks
        let methods: Vec<_> = signatures.iter().filter(|s| s.is_method).collect();
        assert!(!methods.is_empty());
    }

    #[test]
    fn test_rust_struct_extraction() {
        let source = b"struct Point {
    x: f64,
    y: f64,
}

pub struct Person {
    pub name: String,
    pub age: u32,
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let point = signatures.iter().find(|s| s.name == "Point");
        assert!(point.is_some());

        let person = signatures.iter().find(|s| s.name == "Person");
        assert!(person.is_some());
    }

    #[test]
    fn test_rust_enum_extraction() {
        let source = b"enum Option<T> {
    Some(T),
    None,
}

pub enum Result<T, E> {
    Ok(T),
    Err(E),
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let option = signatures.iter().find(|s| s.name == "Option");
        assert!(option.is_some());

        let result = signatures.iter().find(|s| s.name == "Result");
        assert!(result.is_some());
    }

    #[test]
    fn test_rust_trait_extraction() {
        let source = b"trait Display {
    fn fmt(&self, f: &mut Formatter) -> Result;
}

trait Iterator {
    type Item;
    fn next(&mut self) -> Option<Self::Item>;
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract trait declarations
        assert!(signatures.len() >= 2);

        let display = signatures.iter().find(|s| s.name == "Display");
        assert!(display.is_some());

        let iterator = signatures.iter().find(|s| s.name == "Iterator");
        assert!(iterator.is_some());
    }

    #[test]
    fn test_rust_visibility_modifiers() {
        let source = b"pub fn public_function() {}

fn private_function() {}

pub(crate) fn crate_function() {}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 3);

        let public = signatures.iter().find(|s| s.name == "public_function");
        assert_eq!(public.unwrap().visibility, Visibility::Public);

        let private = signatures.iter().find(|s| s.name == "private_function");
        assert_eq!(private.unwrap().visibility, Visibility::Private);

        let crate_fn = signatures.iter().find(|s| s.name == "crate_function");
        assert!(crate_fn.is_some());
    }

    #[test]
    fn test_rust_import_extraction() {
        let source = b"use std::collections::HashMap;
use crate::module::Item;

fn main() {}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract use declarations
        let imports: Vec<_> = signatures
            .iter()
            .filter(|s| s.return_type.as_deref() == Some("use"))
            .collect();

        assert!(imports.len() >= 2);
    }

    #[test]
    fn test_rust_self_parameter() {
        let source = b"impl Foo {
    fn by_ref(&self) -> i32 { 0 }
    fn by_mut_ref(&mut self) -> i32 { 0 }
    fn by_value(self) -> i32 { 0 }
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 3);

        for sig in &signatures {
            assert!(sig.is_method);
            assert!(!sig.parameters.is_empty());
            assert!(sig.parameters[0].name.contains("self"));
        }
    }

    #[test]
    fn test_rust_complexity_calculation() {
        let source = b"fn complex(x: i32) -> i32 {
    if x > 0 {
        for i in 0..x {
            if i % 2 == 0 {
                println!(\"{}\", i);
            }
        }
    }
    x
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }
}
