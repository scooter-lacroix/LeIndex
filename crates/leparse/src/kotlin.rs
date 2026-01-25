// Kotlin language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

pub struct KotlinParser;

impl Default for KotlinParser {
    fn default() -> Self { Self::new() }
}

impl KotlinParser {
    pub fn new() -> Self { Self }
}

impl CodeIntelligence for KotlinParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        parser.set_language(&crate::traits::languages::kotlin::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        let tree = parser.parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Kotlin source".to_string()))?;
        let mut signatures = Vec::new();
        fn visit(node: &tree_sitter::Node, source: &[u8], sigs: &mut Vec<SignatureInfo>) {
            match node.kind() {
                "function_declaration" => {
                    if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                        sigs.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name: name.to_string(),
                            parameters: vec![],
                            return_type: node.child_by_field_name("return_type").and_then(|r| r.utf8_text(source).ok()).map(|s| s.trim().to_string()),
                            visibility: Visibility::Public,
                            is_async: {
                                let mut cursor = node.walk();
                                let mut found = false;
                                for child in node.children(&mut cursor) {
                                    if child.kind() == "suspend_modifier" {
                                        found = true;
                                        break;
                                    }
                                }
                                found
                            },
                            is_method: false,
                            docstring: None,
                        });
                    }
                }
                "class_declaration" => {
                    if let Some(name) = node.child_by_field_name("name").and_then(|n| n.utf8_text(source).ok()) {
                        sigs.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name: name.to_string(),
                            parameters: vec![],
                            return_type: Some("class".to_string()),
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
        parser.set_language(&crate::traits::languages::kotlin::language())
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
    fn test_kotlin_function() {
        let source = b"fun greet(name: String): String = \"Hello, $name\"";
        let parser = KotlinParser::new();
        assert!(parser.get_signatures(source).unwrap().len() > 0);
    }

    #[test]
    fn test_kotlin_suspend() {
        let source = b"suspend fun fetchData() {}";
        let parser = KotlinParser::new();
        let sigs = parser.get_signatures(source).unwrap();
        assert_eq!(sigs.len(), 1);
        assert!(sigs[0].is_async);
    }
}
