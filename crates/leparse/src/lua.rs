// Lua language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

pub struct LuaParser;

impl Default for LuaParser {
    fn default() -> Self { Self::new() }
}

impl LuaParser {
    pub fn new() -> Self { Self }
}

impl CodeIntelligence for LuaParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        parser.set_language(&crate::traits::languages::lua::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        let tree = parser.parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Lua source".to_string()))?;
        let mut signatures = Vec::new();
        fn visit(node: &tree_sitter::Node, source: &[u8], sigs: &mut Vec<SignatureInfo>) {
            match node.kind() {
                "function_declaration" => {
                    // Lua function name is in an identifier child, not a "name" field
                    let name = node.children(&mut node.walk())
                        .find(|c| c.kind() == "identifier")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    if let Some(name) = name {
                        sigs.push(SignatureInfo {
                            name: name.clone(),
                            qualified_name: name,
                            parameters: vec![],
                            return_type: None,
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
        parser.set_language(&crate::traits::languages::lua::language())
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
    fn test_lua_function() {
        let source = b"function greet(name)\n  return \"Hello, \" .. name\nend";
        let parser = LuaParser::new();
        assert!(parser.get_signatures(source).unwrap().len() > 0);
    }
}
