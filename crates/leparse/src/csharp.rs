// C# language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

/// C# language parser with full CodeIntelligence implementation
pub struct CSharpParser;

impl Default for CSharpParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CSharpParser {
    /// Create a new C# parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and type definitions from C# source
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
                "method_declaration" | "local_function_statement" => {
                    if let Some(sig) = extract_method_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    // Don't recurse into method bodies
                }
                "class_declaration" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("."), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("class".to_string()),
                            visibility: extract_visibility(node, source),
                            is_async: false,
                            is_method: false,
                            docstring: None,
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
                            format!("{}.{}", parent_path.join("."), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("interface".to_string()),
                            visibility: extract_visibility(node, source),
                            is_async: false,
                            is_method: false,
                            docstring: None,
                        });
                    }
                }
                "struct_declaration" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("."), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("struct".to_string()),
                            visibility: extract_visibility(node, source),
                            is_async: false,
                            is_method: false,
                            docstring: None,
                        });
                    }
                }
                "enum_declaration" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("."), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("enum".to_string()),
                            visibility: extract_visibility(node, source),
                            is_async: false,
                            is_method: false,
                            docstring: None,
                        });
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

impl CodeIntelligence for CSharpParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::csharp::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse C# source".to_string()))?;

        let root_node = tree.root_node();
        let signatures = self.extract_all_definitions(source, root_node);

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::csharp::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse C# source".to_string()))?;

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

fn extract_method_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    parent_path: &[String],
) -> Option<SignatureInfo> {
    // Try to get name from "name" field first (for method_declaration)
    // If not found, look for an identifier child (for local_function_statement)
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())
        .or_else(|| {
            // For local_function_statement, find the identifier child
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    return child.utf8_text(source).ok().map(|s| s.to_string());
                }
            }
            None
        })?;

    let qualified_name = if parent_path.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", parent_path.join("."), name)
    };

    let parameters = extract_csharp_parameters(node, source);

    // For return type, try "type" field or look for generic_name
    let return_type = node
        .child_by_field_name("type")
        .and_then(|t| t.utf8_text(source).ok())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            // For local_function_statement, look for generic_name child
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "generic_name" || child.kind() == "predefined_type" || child.kind() == "identifier" {
                    return child.utf8_text(source).ok().map(|s| s.trim().to_string());
                }
            }
            None
        });

    let is_async = {
        let mut cursor = node.walk();
        let mut found = false;
        for child in node.children(&mut cursor) {
            if child.kind() == "modifier" {
                if let Ok(text) = child.utf8_text(source) {
                    if text.trim() == "async" {
                        found = true;
                        break;
                    }
                }
            }
        }
        found
    };

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility: extract_visibility(node, source),
        is_async,
        is_method: true,
        docstring: None,
    })
}

fn extract_csharp_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    // Try "parameters" field first, then look for parameter_list child
    let params_node = node.child_by_field_name("parameters")
        .or_else(|| {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "parameter_list" {
                    return Some(child);
                }
            }
            None
        });

    if let Some(params) = params_node {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter" {
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

fn extract_visibility(node: &tree_sitter::Node, source: &[u8]) -> Visibility {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mcursor = child.walk();
            for modifier in child.children(&mut mcursor) {
                if let Ok(text) = modifier.utf8_text(source) {
                    match text.trim() {
                        "public" => return Visibility::Public,
                        "protected" => return Visibility::Protected,
                        "private" => return Visibility::Private,
                        "internal" => return Visibility::Protected,
                        _ => {}
                    }
                }
            }
        }
    }
    Visibility::Private
}

fn extract_docstring(_node: &tree_sitter::Node, _source: &[u8]) -> Option<String> {
    None
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
        "if_statement" | "for_statement" | "foreach_statement" | "while_statement" | "switch_statement" => {
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
        Self { source, blocks: Vec::new(), edges: Vec::new(), next_block_id: 0 }
    }

    fn build_from_node(&mut self, node: &tree_sitter::Node) -> Result<()> {
        let entry_id = self.create_block();
        self.build_cfg_recursive(node, entry_id)?;
        Ok(())
    }

    fn build_cfg_recursive(&mut self, node: &tree_sitter::Node, current_block: usize) -> Result<()> {
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

    fn handle_if_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let true_block = self.create_block();
        let false_block = self.create_block();
        let merge_block = self.create_block();
        self.edges.push(Edge { from: current_block, to: true_block, edge_type: EdgeType::TrueBranch });
        self.edges.push(Edge { from: current_block, to: false_block, edge_type: EdgeType::FalseBranch });
        self.edges.push(Edge { from: true_block, to: merge_block, edge_type: EdgeType::Unconditional });
        self.edges.push(Edge { from: false_block, to: merge_block, edge_type: EdgeType::Unconditional });
        Ok(())
    }

    fn handle_loop_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let body_block = self.create_block();
        self.edges.push(Edge { from: current_block, to: body_block, edge_type: EdgeType::Unconditional });
        self.edges.push(Edge { from: body_block, to: current_block, edge_type: EdgeType::Loop });
        Ok(())
    }

    fn create_block(&mut self) -> usize {
        let id = self.next_block_id;
        self.next_block_id += 1;
        self.blocks.push(Block { id, statements: Vec::new() });
        id
    }

    fn add_statement_to_block(&mut self, block_id: usize, statement: String) {
        if let Some(block) = self.blocks.get_mut(block_id) {
            block.statements.push(statement);
        }
    }

    fn finish(self) -> Graph<Block, Edge> {
        Graph { blocks: self.blocks, edges: self.edges, entry_block: 0, exit_blocks: vec![self.next_block_id.saturating_sub(1)] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csharp_method_extraction() {
        let source = b"public class Test {
    public void Greet(string name) {
        Console.WriteLine(\"Hello, \" + name);
    }
}";

        let parser = CSharpParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find the class and method
        assert!(signatures.len() >= 2);

        let class = signatures.iter().find(|s| s.name == "Test");
        assert!(class.is_some());

        let method = signatures.iter().find(|s| s.name == "Greet");
        assert!(method.is_some());
    }

    #[test]
    fn test_csharp_async_method() {
        let source = b"public async Task<string> FetchData() {
    return await Task.FromResult(\"data\");
}";

        let parser = CSharpParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let method = signatures.iter().find(|s| s.name == "FetchData");
        assert!(method.is_some());
        assert!(method.unwrap().is_async);
    }

    #[test]
    fn test_csharp_complexity_calculation() {
        let source = b"public void Complex(int x) {
    if (x > 0) {
        for (int i = 0; i < x; i++) {
            if (i % 2 == 0) {
                Console.WriteLine(i);
            }
        }
    }
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let csharp_parser = CSharpParser::new();
        let metrics = csharp_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }
}
