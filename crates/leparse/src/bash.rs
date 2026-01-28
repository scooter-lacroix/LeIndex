// Bash language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, Visibility};
use tree_sitter::Parser;

/// Bash language parser with full CodeIntelligence implementation
pub struct BashParser;

impl Default for BashParser {
    fn default() -> Self {
        Self::new()
    }
}

impl BashParser {
    /// Create a new Bash parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function definitions from Bash source
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
        ) {
            match node.kind() {
                "function_definition" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(source) {
                            signatures.push(SignatureInfo {
                                name: name.to_string(),
                                qualified_name: name.to_string(),
                                parameters: vec![],
                                return_type: None,
                                visibility: Visibility::Public,
                                is_async: false,
                                is_method: false,
                                docstring: extract_docstring(node, source),
                                byte_range: (node.start_byte(), node.end_byte()),
                            });
                        }
                    }
                }
                _ => {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures);
                    }
                }
            }
        }

        visit_node(&root, source, &mut signatures);
        signatures
    }
}

impl CodeIntelligence for BashParser {
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
            .set_language(&crate::traits::languages::bash::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Bash source".to_string()))?;

        let root_node = tree.root_node();

        let signatures = self.extract_all_definitions(source, root_node);

        Ok(signatures)
    }

    fn compute_cfg(&self, _source: &[u8], _node_id: usize) -> Result<Graph<Block, Edge>> {
        Ok(Graph {
            blocks: vec![],
            edges: vec![],
            entry_block: 0,
            exit_blocks: vec![],
        })
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

fn calculate_complexity(node: &tree_sitter::Node, metrics: &mut ComplexityMetrics, depth: usize) {
    metrics.nesting_depth = metrics.nesting_depth.max(depth);
    metrics.line_count = std::cmp::max(metrics.line_count, 1);

    match node.kind() {
        "if_statement"
        | "while_statement"
        | "until_statement"
        | "for_statement"
        | "case_statement"
        | "elif_clause" => {
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

fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let prev_sibling = node.prev_sibling();
    if let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" {
            if let Ok(text) = sibling.utf8_text(source) {
                return Some(
                    text.trim()
                        .trim_start_matches('#')
                        .trim()
                        .to_string(),
                );
            }
        }
    }
    None
}
