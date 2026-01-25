// Scala language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

pub struct ScalaParser;

impl Default for ScalaParser {
    fn default() -> Self { Self::new() }
}

impl ScalaParser {
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
        fn visit(node: &tree_sitter::Node, source: &[u8], sigs: &mut Vec<SignatureInfo>) {
            match node.kind() {
                "function_definition" => {
                    if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                        sigs.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name: name.to_string(),
                            parameters: vec![],
                            return_type: None,
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: None,
                        });
                    }
                }
                "class_definition" | "trait_definition" | "object_definition" => {
                    if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                        let return_type = match node.kind() {
                            "trait_definition" => Some("trait".to_string()),
                            "object_definition" => Some("object".to_string()),
                            _ => Some("class".to_string()),
                        };
                        sigs.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name: name.to_string(),
                            parameters: vec![],
                            return_type,
                            visibility: Visibility::Public,
                            is_async: false,
                            is_method: false,
                            docstring: None,
                        });
                    }
                }
                _ => { let mut c = node.walk(); for ch in node.children(&mut c) { visit(&ch, source, sigs); } }
            }
        }
        visit(&tree.root_node(), source, &mut signatures);
        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], _node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser.set_language(&crate::traits::languages::scala::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        parser.parse(source, None).ok_or_else(|| Error::ParseFailed("Failed to parse".to_string()))?;
        Ok(Graph { blocks: vec![], edges: vec![], entry_block: 0, exit_blocks: vec![] })
    }

    fn extract_complexity(&self, node: &tree_sitter::Node) -> ComplexityMetrics {
        ComplexityMetrics { cyclomatic: 1, nesting_depth: 0, line_count: 1, token_count: node.child_count() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scala_function() {
        let source = b"def greet(name: String): String = s\"Hello, $name\"";
        let parser = ScalaParser::new();
        assert!(parser.get_signatures(source).unwrap().len() > 0);
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
