// JavaScript and TypeScript language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

/// JavaScript language parser with full CodeIntelligence implementation
pub struct JavaScriptParser;

impl Default for JavaScriptParser {
    fn default() -> Self {
        Self::new()
    }
}

impl JavaScriptParser {
    /// Create a new JavaScript parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and class definitions from JavaScript source
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
                "function_declaration" | "method_definition" | "generator_function_declaration" => {
                    if let Some(sig) = extract_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "class_declaration" | "class_expression" => {
                    let class_name = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    if let Some(name) = class_name {
                        let mut class_path = parent_path.to_vec();
                        class_path.push(name.clone());

                        // Extract class as a signature info
                        signatures.push(SignatureInfo {
                            name: name.clone(),
                            qualified_name: if parent_path.is_empty() {
                                name.clone()
                            } else {
                                format!("{}.{}", parent_path.join("."), name)
                            },
                            parameters: vec![],
                            return_type: None,
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });

                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, &class_path);
                        }
                    } else {
                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, parent_path);
                        }
                    }
                }
                "lexical_declaration" | "variable_declaration" => {
                    // Check for arrow functions or function expressions
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "variable_declarator" {
                            if let Some(name) = child
                                .child_by_field_name("name")
                                .and_then(|n| n.utf8_text(source).ok())
                            {
                                if let Some(value) = child.child_by_field_name("value") {
                                    if value.kind() == "arrow_function"
                                        || value.kind() == "function_expression"
                                    {
                                        if let Some(sig) =
                                            extract_function_signature(&value, source, parent_path)
                                        {
                                            // Override name with variable name
                                            let mut sig = sig;
                                            sig.name = name.to_string();
                                            sig.qualified_name = if parent_path.is_empty() {
                                                name.to_string()
                                            } else {
                                                format!("{}.{}", parent_path.join("."), name)
                                            };
                                            signatures.push(sig);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Continue recursion
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
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

impl CodeIntelligence for JavaScriptParser {
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
            .set_language(&crate::traits::languages::javascript::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse JavaScript source".to_string()))?;

        let root_node = tree.root_node();

        let signatures = self.extract_all_definitions(source, root_node);

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::javascript::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse JavaScript source".to_string()))?;

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

/// TypeScript language parser with full CodeIntelligence implementation
pub struct TypeScriptParser;

impl Default for TypeScriptParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeScriptParser {
    /// Create a new TypeScript parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all definitions from TypeScript source
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
                "function_declaration"
                | "method_definition"
                | "generator_function_declaration" => {
                    if let Some(sig) = extract_ts_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "interface_declaration" | "type_alias_declaration" => {
                    // Extract interface/type declarations
                    let name = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    if let Some(type_name) = name {
                        signatures.push(SignatureInfo {
                            name: type_name.clone(),
                            qualified_name: if parent_path.is_empty() {
                                type_name.clone()
                            } else {
                                format!("{}.{}", parent_path.join("."), type_name)
                            },
                            parameters: vec![],
                            return_type: None,
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });
                    }

                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "class_declaration" | "class_expression" => {
                    let class_name = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    if let Some(name) = class_name {
                        let mut class_path = parent_path.to_vec();
                        class_path.push(name.clone());

                        signatures.push(SignatureInfo {
                            name: name.clone(),
                            qualified_name: if parent_path.is_empty() {
                                name.clone()
                            } else {
                                format!("{}.{}", parent_path.join("."), name)
                            },
                            parameters: vec![],
                            return_type: None,
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(node, source),
                        });

                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, &class_path);
                        }
                    } else {
                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, parent_path);
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

impl CodeIntelligence for TypeScriptParser {
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
            .set_language(&crate::traits::languages::typescript::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse TypeScript source".to_string()))?;

        let root_node = tree.root_node();

        let signatures = self.extract_all_definitions(source, root_node);

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::typescript::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse TypeScript source".to_string()))?;

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

/// Extract function signature for JavaScript
fn extract_function_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    parent_path: &[String],
) -> Option<SignatureInfo> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "[anonymous]".to_string());

    let qualified_name = if parent_path.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", parent_path.join("."), name)
    };

    let parameters = extract_parameters(node, source);

    let is_async = node
        .children(&mut node.walk())
        .any(|child| child.kind() == "async");

    let is_method = node.kind() == "method_definition" || !parent_path.is_empty();

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type: None, // JS doesn't have explicit return types
        visibility: Visibility::Public,
        is_async,
        is_method,
        docstring: extract_docstring(node, source),
    })
}

/// Extract function signature for TypeScript with type annotations
fn extract_ts_function_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    parent_path: &[String],
) -> Option<SignatureInfo> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "[anonymous]".to_string());

    let qualified_name = if parent_path.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", parent_path.join("."), name)
    };

    let parameters = extract_ts_parameters(node, source);

    // Extract return type annotation
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|rt| rt.utf8_text(source).ok())
        .map(|s| s.trim().trim_start_matches(':').trim().to_string());

    let is_async = node
        .children(&mut node.walk())
        .any(|child| child.kind() == "async");

    let is_method = node.kind() == "method_definition" || !parent_path.is_empty();

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility: Visibility::Public,
        is_async,
        is_method,
        docstring: extract_docstring(node, source),
    })
}

/// Extract parameters from a JavaScript function
fn extract_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "identifier" {
                if let Ok(name) = child.utf8_text(source) {
                    parameters.push(Parameter {
                        name: name.to_string(),
                        type_annotation: None,
                        default_value: None,
                    });
                }
            } else if child.kind() == "rest_parameter" {
                if let Some(name) = child.child_by_field_name("name") {
                    if let Ok(n) = name.utf8_text(source) {
                        parameters.push(Parameter {
                            name: format!("...{}", n),
                            type_annotation: None,
                            default_value: None,
                        });
                    }
                }
            }
        }
    }

    parameters
}

/// Extract parameters from a TypeScript function with type annotations
fn extract_ts_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            match child.kind() {
                "identifier" => {
                    if let Ok(name) = child.utf8_text(source) {
                        parameters.push(Parameter {
                            name: name.to_string(),
                            type_annotation: None,
                            default_value: None,
                        });
                    }
                }
                "required_parameter" | "optional_parameter" => {
                    // Get the parameter name - try name field first, then look for identifier child
                    let param_name = child
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    // If no name field found, look for identifier child
                    let param_name = if param_name.is_none() {
                        let mut ccursor = child.walk();
                        let result = child.children(&mut ccursor)
                            .find(|c| c.kind() == "identifier")
                            .and_then(|c| c.utf8_text(source).ok())
                            .map(|s| s.to_string());
                        result
                    } else {
                        param_name
                    };

                    if let Some(name) = param_name {
                        let type_annotation = child
                            .child_by_field_name("type")
                            .and_then(|t| t.utf8_text(source).ok())
                            .map(|s| s.trim().to_string());

                        parameters.push(Parameter {
                            name,
                            type_annotation,
                            default_value: None,
                        });
                    }
                }
                "rest_parameter" => {
                    let param_name = child
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    // If no name field found, look for identifier child
                    let param_name = if param_name.is_none() {
                        let mut ccursor = child.walk();
                        let result = child.children(&mut ccursor)
                            .find(|c| c.kind() == "identifier")
                            .and_then(|c| c.utf8_text(source).ok())
                            .map(|s| s.to_string());
                        result
                    } else {
                        param_name
                    };

                    if let Some(name) = param_name {
                        let type_annotation = child
                            .child_by_field_name("type")
                            .and_then(|t| t.utf8_text(source).ok())
                            .map(|s| s.trim().to_string());

                        parameters.push(Parameter {
                            name: format!("...{}", name),
                            type_annotation,
                            default_value: None,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    parameters
}

/// Extract docstring from a node
fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Look for comment before the node
    let mut prev_sibling = None;

    // Get the previous sibling or parent's previous child
    if let Some(parent) = node.parent() {
        let mut pcursor = parent.walk();
        for child in parent.children(&mut pcursor) {
            if child.id() == node.id() {
                break;
            }
            prev_sibling = Some(child);
        }
    }

    // Check for comment_block or comment_line
    if let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" || sibling.kind() == "comment_block" {
            if let Ok(text) = sibling.utf8_text(source) {
                return Some(
                    text.trim()
                        .trim_start_matches("/*")
                        .trim_start_matches("//")
                        .trim_start_matches("/**")
                        .trim_start_matches("*")
                        .trim_end_matches("*/")
                        .trim()
                        .to_string(),
                );
            }
        }
    }

    // Check for JSDoc comment
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "comment" {
            if let Ok(text) = child.utf8_text(source) {
                let cleaned = text
                    .trim()
                    .trim_start_matches("/**")
                    .trim_start_matches("/*")
                    .trim_start_matches("//")
                    .trim_end_matches("*/")
                    .trim();
                return Some(cleaned.to_string());
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
        | "while_statement"
        | "for_statement"
        | "for_in_statement"
        | "for_of_statement"
        | "try_statement"
        | "switch_statement"
        | "catch_clause" => {
            metrics.cyclomatic += 1;
        }
        "else" | "case" => {
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
            "while_statement" | "for_statement" | "for_in_statement" | "for_of_statement" => {
                self.handle_loop_statement(node, current_block)?;
            }
            "try_statement" => {
                self.handle_try_statement(node, current_block)?;
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

    fn handle_loop_statement(
        &mut self,
        _node: &tree_sitter::Node,
        current_block: usize,
    ) -> Result<()> {
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

    fn handle_try_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let try_block = self.create_block();
        let catch_block = self.create_block();
        let finally_block = self.create_block();

        self.edges.push(Edge {
            from: current_block,
            to: try_block,
            edge_type: EdgeType::Unconditional,
        });
        self.edges.push(Edge {
            from: try_block,
            to: catch_block,
            edge_type: EdgeType::Exception,
        });
        self.edges.push(Edge {
            from: catch_block,
            to: finally_block,
            edge_type: EdgeType::Unconditional,
        });

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
    fn test_javascript_function_extraction() {
        let source = b"function greet(name) {
    return 'Hello, ' + name;
}";

        let parser = JavaScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "greet");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].name, "name");
        assert!(!sig.is_async);
        assert!(!sig.is_method);
    }

    #[test]
    fn test_javascript_async_function() {
        let source = b"async function fetchData(url) {
    return fetch(url);
}";

        let parser = JavaScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        assert!(signatures[0].is_async);
        assert_eq!(signatures[0].name, "fetchData");
    }

    #[test]
    fn test_javascript_class_methods() {
        let source = b"class MyClass {
    method1() {
        return 1;
    }

    async method2(x) {
        return x;
    }
}";

        let parser = JavaScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract class + methods
        assert!(signatures.len() >= 2);

        let methods: Vec<_> = signatures.iter().filter(|s| s.is_method).collect();
        assert!(methods.len() >= 2);
    }

    #[test]
    fn test_javascript_arrow_function() {
        let source = b"const add = (a, b) => a + b;";

        let parser = JavaScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 1);
        let add_sig = signatures.iter().find(|s| s.name == "add");
        assert!(add_sig.is_some());
        if let Some(sig) = add_sig {
            assert_eq!(sig.parameters.len(), 2);
        }
    }

    #[test]
    fn test_typescript_function_with_types() {
        let source = b"function greet(name: string): string {
    return 'Hello, ' + name;
}";

        let parser = TypeScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "greet");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].name, "name");
        assert_eq!(sig.return_type, Some("string".to_string()));
    }

    #[test]
    fn test_typescript_interface_extraction() {
        let source = b"interface User {
    name: string;
    age: number;
}

interface Admin extends User {
    permissions: string[];
}";

        let parser = TypeScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let user_interface = signatures.iter().find(|s| s.name == "User");
        assert!(user_interface.is_some());

        let admin_interface = signatures.iter().find(|s| s.name == "Admin");
        assert!(admin_interface.is_some());
    }

    #[test]
    fn test_typescript_type_alias() {
        let source = b"type ID = string | number;
type JsonObject = Record<string, unknown>;";

        let parser = TypeScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);
    }

    #[test]
    fn test_typescript_class_with_types() {
        let source = b"class Calculator {
    add(a: number, b: number): number {
        return a + b;
    }

    private secret: number = 42;
}";

        let parser = TypeScriptParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 1);
    }

    #[test]
    fn test_javascript_complexity_calculation() {
        let source = b"function complex(x) {
    if (x > 0) {
        for (let i = 0; i < x; i++) {
            if (i % 2 === 0) {
                console.log(i);
            }
        }
    }
    return x;
}";

        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::javascript::language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let js_parser = JavaScriptParser::new();
        let metrics = js_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }
}
