// PHP language parser implementation

use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use crate::traits::{
    CodeIntelligence, ComplexityMetrics, Error, Graph, ImportInfo, Result, SignatureInfo,
};
use tree_sitter::Parser;

/// PHP language parser with full CodeIntelligence implementation
pub struct PhpParser;

impl Default for PhpParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PhpParser {
    /// Create a new instance of the PHP parser.
    pub fn new() -> Self {
        Self
    }

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
                "function_definition" | "method_declaration" => {
                    if let Some(sig) = extract_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                }
                "class_declaration" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}\\{}", parent_path.join("\\"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("class".to_string()),
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: None,
                            calls: vec![],
                            imports: vec![],
                            byte_range: (0, 0),
                        });
                    }

                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "interface_declaration" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}\\{}", parent_path.join("\\"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("interface".to_string()),
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: None,
                            calls: vec![],
                            imports: vec![],
                            byte_range: (0, 0),
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

impl CodeIntelligence for PhpParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::php::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse PHP source".to_string()))?;

        let root_node = tree.root_node();
        let imports = extract_php_imports(root_node, source);
        let mut signatures = self.extract_all_definitions(source, root_node);

        for sig in &mut signatures {
            sig.imports = imports.clone();
        }

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::php::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse PHP source".to_string()))?;

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

fn extract_php_imports(root: tree_sitter::Node, source: &[u8]) -> Vec<ImportInfo> {
    let mut imports = Vec::new();

    fn add_import(imports: &mut Vec<ImportInfo>, path: &str, alias: Option<String>) {
        let path = path.trim().trim_end_matches(';').trim();
        if path.is_empty() {
            return;
        }
        imports.push(ImportInfo {
            path: path.to_string(),
            alias,
        });
    }

    fn parse_use_text(imports: &mut Vec<ImportInfo>, text: &str) {
        let text = text.trim().trim_end_matches(';');
        let text = text
            .trim_start_matches("use ")
            .trim_start_matches("use function ")
            .trim_start_matches("use const ");
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((path, alias)) = part.split_once(" as ") {
                add_import(imports, path.trim(), Some(alias.trim().to_string()));
            } else {
                add_import(
                    imports,
                    part,
                    part.split('\\').last().map(|s| s.to_string()),
                );
            }
        }
    }

    fn visit(node: &tree_sitter::Node, source: &[u8], imports: &mut Vec<ImportInfo>) {
        if node.kind().contains("namespace_use") || node.kind() == "namespace_use_declaration" {
            if let Ok(text) = node.utf8_text(source) {
                parse_use_text(imports, text);
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(&child, source, imports);
        }
    }

    visit(&root, source, &mut imports);
    imports
}

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
        format!("{}\\{}", parent_path.join("\\"), name)
    };

    let parameters = extract_php_parameters(node, source);

    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|t| t.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    let calls = extract_php_calls(node, source);

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility: Visibility::Public,
        is_async: false,
        is_method: node.kind() == "method_declaration",
        docstring: extract_docstring(node, source),
        calls,

        imports: vec![],
        byte_range: (0, 0),
    })
}

fn extract_php_calls(node: &tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut calls = Vec::new();

    fn clean_call_text(raw: &str) -> String {
        raw.split('(').next().unwrap_or(raw).trim().to_string()
    }

    fn find_calls(node: &tree_sitter::Node, source: &[u8], calls: &mut Vec<String>) {
        match node.kind() {
            "function_call_expression" | "method_call_expression" | "scoped_call_expression" => {
                if let Some(name_node) = node
                    .child_by_field_name("name")
                    .or_else(|| node.child_by_field_name("function"))
                {
                    if let Ok(text) = name_node.utf8_text(source) {
                        let name = clean_call_text(text);
                        if !name.is_empty() {
                            calls.push(name);
                        }
                    }
                }
            }
            "object_creation_expression" => {
                if let Some(name_node) = node
                    .child_by_field_name("class")
                    .or_else(|| node.child_by_field_name("type"))
                {
                    if let Ok(text) = name_node.utf8_text(source) {
                        let name = clean_call_text(text);
                        if !name.is_empty() {
                            calls.push(name);
                        }
                    }
                }
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            find_calls(&child, source, calls);
        }
    }

    find_calls(node, source, &mut calls);
    calls
}

fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    // PHP uses PHPDoc comments: /** Description */
    // Look for comment nodes before the function
    let node_start = node.byte_range().start;

    // Walk up to find the root, then search for comments
    let mut current = *node;
    let root = loop {
        let parent = current.parent();
        match parent {
            Some(p) => current = p,
            None => break current,
        }
    };

    // Find the closest comment before this node (within 500 bytes)
    let mut closest_comment: Option<(usize, String)> = None;
    find_closest_comment(&root, source, node_start, &mut closest_comment, 500);

    closest_comment.map(|(_, comment)| {
        // Clean up the PHPDoc comment
        comment
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_end_matches("*/")
            .lines()
            .map(|line| line.trim().trim_start_matches('*').trim())
            .filter(|line| !line.is_empty() && !line.starts_with('@'))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn find_closest_comment(
    node: &tree_sitter::Node,
    source: &[u8],
    target_byte: usize,
    closest: &mut Option<(usize, String)>,
    max_distance: usize,
) {
    // Check if this node is a comment before our target
    if node.kind() == "comment" {
        let byte_range = node.byte_range();
        if byte_range.end <= target_byte {
            let distance = target_byte.saturating_sub(byte_range.start);
            if distance <= max_distance {
                if let Ok(comment) = node.utf8_text(source) {
                    match closest {
                        Some((existing_dist, _)) if distance < *existing_dist => {
                            *closest = Some((distance, comment.to_string()));
                        }
                        None => *closest = Some((distance, comment.to_string())),
                        _ => {}
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_closest_comment(&child, source, target_byte, closest, max_distance);
    }
}

fn extract_php_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "simple_parameter" || child.kind() == "variadic_parameter" {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .map(|s| s.to_string());

                let type_annotation = child
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source).ok())
                    .map(|s| s.trim().to_string());

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

fn calculate_complexity(node: &tree_sitter::Node, metrics: &mut ComplexityMetrics, depth: usize) {
    metrics.nesting_depth = metrics.nesting_depth.max(depth);
    metrics.line_count = std::cmp::max(metrics.line_count, 1);
    match node.kind() {
        "if_statement" | "for_statement" | "foreach_statement" | "while_statement"
        | "switch_statement" => {
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
            "for_statement" | "foreach_statement" | "while_statement" => {
                self.handle_loop_statement(node, current_block)?;
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

    fn handle_if_statement(
        &mut self,
        _node: &tree_sitter::Node,
        current_block: usize,
    ) -> Result<()> {
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
    fn test_php_function_extraction() {
        let source = b"<?php
function greet($name) {
    echo \"Hello, \" . $name;
}";

        let parser = PhpParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let func = &signatures[0];
        assert_eq!(func.name, "greet");
        assert_eq!(func.parameters.len(), 1);
    }

    #[test]
    fn test_php_class_extraction() {
        let source = b"<?php
class MyClass {
    public function myMethod($param) {
        return $param;
    }
}";

        let parser = PhpParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find class and method
        assert!(signatures.len() >= 2);
        let class = signatures.iter().find(|s| s.name == "MyClass");
        assert!(class.is_some());
    }

    #[test]
    fn test_php_complexity_calculation() {
        let source = b"<?php
function complex($x) {
    if ($x > 0) {
        for ($i = 0; $i < $x; $i++) {
            if ($i % 2 == 0) {
                echo $i;
            }
        }
    }
}";

        let mut parser = Parser::new();
        // Use the language function from traits
        parser
            .set_language(&crate::traits::languages::php::language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let php_parser = PhpParser::new();
        let metrics = php_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }
}
