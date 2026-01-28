// Scala language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

/// Scala language parser with full CodeIntelligence implementation
pub struct ScalaParser;

impl Default for ScalaParser {
    fn default() -> Self { Self::new() }
}

impl ScalaParser {
    /// Create a new instance of the Scala parser.
    pub fn new() -> Self { Self }
}

impl CodeIntelligence for ScalaParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        parser.set_language(&crate::traits::languages::scala::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        let tree = parser.parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Scala source".to_string()))?;
        let mut signatures = Vec::new();
        visit(&tree.root_node(), source, &mut signatures, &[]);
        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser.set_language(&crate::traits::languages::scala::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        let tree = parser.parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Scala source".to_string()))?;

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

fn visit(
    node: &tree_sitter::Node,
    source: &[u8],
    sigs: &mut Vec<SignatureInfo>,
    parent_path: &[String],
) {
    match node.kind() {
        "function_definition" => {
            if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                // Extract parameters
                let parameters = node
                    .child_by_field_name("parameters")
                    .map(|params| extract_parameters(&params, source))
                    .unwrap_or_default();

                // Extract return type
                let return_type = node
                    .child_by_field_name("return_type")
                    .and_then(|rt| rt.utf8_text(source).ok())
                    .map(|s| s.trim().to_string());

                // Build qualified name
                let mut qualified_path = parent_path.to_vec();
                qualified_path.push(name.to_string());
                let qualified_name = qualified_path.join(".");

                sigs.push(SignatureInfo {
                    name: name.to_string(),
                    qualified_name,
                    parameters,
                    return_type,
                    visibility: Visibility::Public,
                    is_async: false,
                    is_method: !parent_path.is_empty(),
                    docstring: extract_docstring(node, source), byte_range: (0, 0) });
            }
        }
        "class_definition" | "trait_definition" | "object_definition" => {
            if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                let entity_type = match node.kind() {
                    "trait_definition" => "trait",
                    "object_definition" => "object",
                    _ => "class",
                };

                let mut qualified_path = parent_path.to_vec();
                qualified_path.push(name.to_string());
                let qualified_name = qualified_path.join(".");

                sigs.push(SignatureInfo {
                    name: name.to_string(),
                    qualified_name,
                    parameters: vec![],
                    return_type: Some(entity_type.to_string()),
                    visibility: Visibility::Public,
                    is_async: false,
                    is_method: false,
                    docstring: extract_docstring(node, source), byte_range: (0, 0) });

                // Recurse into the body to find nested members
                let mut c = node.walk();
                for ch in node.children(&mut c) {
                    visit(&ch, source, sigs, &qualified_path);
                }
            }
        }
        _ => {
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                visit(&ch, source, sigs, parent_path);
            }
        }
    }
}

/// Extract parameters from a parameters node
fn extract_parameters(params_node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();
    let mut cursor = params_node.walk();

    for child in params_node.children(&mut cursor) {
        if child.kind() == "parameter" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    let type_annotation = child
                        .child_by_field_name("type")
                        .and_then(|t| t.utf8_text(source).ok())
                        .map(|s| s.trim().to_string());

                    parameters.push(Parameter {
                        name: name.to_string(),
                        type_annotation,
                        default_value: None,
                    });
                }
            }
        }
    }

    parameters
}

/// Extract docstring from a node (Scala uses /** */ comments)
fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "comment" {
            if let Ok(comment) = child.utf8_text(source) {
                let cleaned = comment
                    .trim_start_matches("/**")
                    .trim_start_matches("/*")
                    .trim_end_matches("*/")
                    .trim()
                    .to_string();
                if !cleaned.is_empty() {
                    return Some(cleaned);
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

/// Calculate complexity metrics for a node
fn calculate_complexity(node: &tree_sitter::Node, metrics: &mut ComplexityMetrics, depth: usize) {
    metrics.nesting_depth = metrics.nesting_depth.max(depth);
    metrics.line_count = std::cmp::max(metrics.line_count, 1);

    // Scala control flow structures
    match node.kind() {
        "if_expression"
        | "while_expression"
        | "for_expression"
        | "match_expression"
        | "try_expression" => {
            metrics.cyclomatic += 1;
        }
        "case_clause" => {
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

    fn build_cfg_recursive(&mut self, node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        match node.kind() {
            "if_expression" => {
                self.handle_if_statement(node, current_block)?;
            }
            "while_expression" => {
                self.handle_while_statement(node, current_block)?;
            }
            "for_expression" => {
                self.handle_for_statement(node, current_block)?;
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

    fn handle_while_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let body_block = self.create_block();

        self.edges.push(Edge {
            from: current_block,
            to: body_block,
            edge_type: EdgeType::TrueBranch,
        });
        self.edges.push(Edge {
            from: body_block,
            to: current_block,
            edge_type: EdgeType::Loop,
        });

        Ok(())
    }

    fn handle_for_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
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

        // For each case clause, create an edge
        let mut cursor = _node.walk();
        for child in _node.children(&mut cursor) {
            if child.kind() == "case_clause" {
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
    fn test_scala_function() {
        let source = b"def greet(name: String): String = s\"Hello, $name\"";
        let parser = ScalaParser::new();
        let sigs = parser.get_signatures(source).unwrap();
        assert!(!sigs.is_empty());
    }

    #[test]
    fn test_scala_class() {
        let source = b"class Person(name: String)";
        let parser = ScalaParser::new();
        let sigs = parser.get_signatures(source).unwrap();
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].return_type, Some("class".to_string()));
    }
}
