// Java language parser implementation

use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use crate::traits::{
    CodeIntelligence, ComplexityMetrics, Error, Graph, ImportInfo, Result, SignatureInfo,
};
use tree_sitter::Parser;

/// Java language parser with full CodeIntelligence implementation
pub struct JavaParser;

impl Default for JavaParser {
    fn default() -> Self {
        Self::new()
    }
}

impl JavaParser {
    /// Create a new Java parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and type definitions from Java source
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
                "method_declaration" => {
                    if let Some(sig) = extract_method_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    // Don't recurse into method bodies
                }
                "constructor_declaration" => {
                    if let Some(sig) = extract_constructor_signature(node, source, parent_path) {
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
                            docstring: extract_docstring(node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                        });
                    }

                    // Recurse to extract class methods
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
                            docstring: extract_docstring(node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                        });
                    }

                    // Recurse to extract interface methods
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
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
                            docstring: extract_docstring(node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                        });
                    }
                }
                "field_declaration" => {
                    // Extract field declarations as signatures
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "variable_declarator" {
                            if let Some(name) = child
                                .child_by_field_name("name")
                                .and_then(|n| n.utf8_text(source).ok())
                            {
                                // Get type from parent
                                let type_annotation = node
                                    .child_by_field_name("type")
                                    .and_then(|t| t.utf8_text(source).ok())
                                    .map(|s| s.trim().to_string());

                                signatures.push(SignatureInfo {
                                    name: name.to_string(),
                                    qualified_name: name.to_string(),
                                    parameters: vec![],
                                    return_type: type_annotation,
                                    visibility: extract_visibility(node, source),
                                    is_async: false,
                                    is_method: false,
                                    docstring: None,
                                    calls: vec![],
                                    imports: vec![],
                                    byte_range: (0, 0),
                                });
                            }
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

impl CodeIntelligence for JavaParser {
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
            .set_language(&crate::traits::languages::java::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Java source".to_string()))?;

        let root_node = tree.root_node();

        let imports = extract_java_imports(root_node, source);
        let mut signatures = self.extract_all_definitions(source, root_node);

        for sig in &mut signatures {
            sig.imports = imports.clone();
        }

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::java::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Java source".to_string()))?;

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

fn extract_java_imports(root: tree_sitter::Node, source: &[u8]) -> Vec<ImportInfo> {
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

    fn visit(node: &tree_sitter::Node, source: &[u8], imports: &mut Vec<ImportInfo>) {
        if node.kind() == "import_declaration" {
            if let Ok(text) = node.utf8_text(source) {
                let text = text.trim().trim_end_matches(';').trim();
                let text = text
                    .trim_start_matches("import ")
                    .trim_start_matches("static ");
                let alias = text.split('.').last().map(|s| s.to_string());
                add_import(imports, text, alias);
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

/// Extract method signature from a method_declaration node
fn extract_method_signature(
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
        format!("{}.{}", parent_path.join("."), name)
    };

    let parameters = extract_java_parameters(node, source);

    let return_type = node
        .child_by_field_name("type")
        .and_then(|t| t.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    let visibility = extract_visibility(node, source);
    let calls = extract_java_calls(node, source);

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility,
        is_async: false, // Java doesn't have async in the same way
        is_method: true,
        docstring: extract_docstring(node, source),
        calls,

        imports: vec![],
        byte_range: (node.start_byte(), node.end_byte()),
    })
}

/// Extract constructor signature from a constructor_declaration node
fn extract_constructor_signature(
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
        format!("{}.{}", parent_path.join("."), name)
    };

    let parameters = extract_java_parameters(node, source);

    let visibility = extract_visibility(node, source);
    let calls = extract_java_calls(node, source);

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type: None, // Constructors don't have return types
        visibility,
        is_async: false,
        is_method: true,
        docstring: extract_docstring(node, source),
        calls,

        imports: vec![],
        byte_range: (node.start_byte(), node.end_byte()),
    })
}

/// Extract function calls from a Java node
fn extract_java_calls(node: &tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut calls = Vec::new();

    fn clean_call_text(raw: &str) -> String {
        raw.split('(').next().unwrap_or(raw).trim().to_string()
    }

    fn find_calls(node: &tree_sitter::Node, source: &[u8], calls: &mut Vec<String>) {
        match node.kind() {
            "method_invocation" => {
                let object = node
                    .child_by_field_name("object")
                    .or_else(|| node.child_by_field_name("scope"))
                    .and_then(|n| n.utf8_text(source).ok())
                    .map(|s| clean_call_text(s));
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .map(|s| clean_call_text(s));

                let call_name = match (object, name) {
                    (Some(obj), Some(method)) => format!("{}.{}", obj, method),
                    (_, Some(method)) => method,
                    _ => node
                        .utf8_text(source)
                        .ok()
                        .map(|s| clean_call_text(s))
                        .unwrap_or_default(),
                };

                if !call_name.is_empty() {
                    calls.push(call_name);
                }
            }
            "object_creation_expression" => {
                if let Some(typ) = node.child_by_field_name("type") {
                    if let Ok(text) = typ.utf8_text(source) {
                        let name = clean_call_text(text);
                        if !name.is_empty() {
                            calls.push(name);
                        }
                    }
                }
            }
            "explicit_constructor_invocation" | "constructor_invocation" => {
                if let Ok(text) = node.utf8_text(source) {
                    let name = clean_call_text(text);
                    if !name.is_empty() {
                        calls.push(name);
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

/// Extract parameters from a Java method
fn extract_java_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "formal_parameter" {
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

/// Extract visibility modifier from a node
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
                        _ => {}
                    }
                }
            }
        }
    }
    Visibility::Private
}

/// Extract docstring from a node
fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Look for javadoc comments before the node
    let prev_sibling = node.prev_sibling();

    // Check for javadoc comment
    if let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" {
            if let Ok(text) = sibling.utf8_text(source) {
                let is_javadoc = text.starts_with("/**");
                if is_javadoc {
                    return Some(
                        text.trim()
                            .trim_start_matches("/**")
                            .trim_end_matches("*/")
                            .trim()
                            .lines()
                            .map(|l| l.trim().trim_start_matches('*').trim())
                            .collect::<Vec<_>>()
                            .join("\n"),
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
        "if_statement"
        | "for_statement"
        | "enhanced_for_statement"
        | "while_statement"
        | "do_statement"
        | "switch_expression"
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
            "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" => {
                self.handle_loop_statement(node, current_block)?;
            }
            "switch_expression" => {
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

    fn handle_switch_statement(
        &mut self,
        _node: &tree_sitter::Node,
        current_block: usize,
    ) -> Result<()> {
        let merge_block = self.create_block();

        let mut cursor = _node.walk();
        let mut has_cases = false;
        for child in _node.children(&mut cursor) {
            if child.kind() == "switch_block" {
                has_cases = true;
                // Create a block for each case
                let mut ccursor = child.walk();
                for case_child in child.children(&mut ccursor) {
                    if case_child.kind() == "switch_block_statement_group" {
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
            }
        }

        if !has_cases {
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
    fn test_java_method_extraction() {
        let source = b"public class Test {
    public void greet(String name) {
        System.out.println(\"Hello, \" + name);
    }
}";

        let parser = JavaParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find the class and method
        assert!(signatures.len() >= 2);

        let class = signatures.iter().find(|s| s.name == "Test");
        assert!(class.is_some());

        let method = signatures.iter().find(|s| s.name == "greet");
        assert!(method.is_some());
    }

    #[test]
    fn test_java_class_extraction() {
        let source = b"public class Person {
    private String name;
    private int age;

    public Person(String name, int age) {
        this.name = name;
        this.age = age;
    }
}";

        let parser = JavaParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find the class, fields, and constructor
        assert!(signatures.len() >= 2);

        let person = signatures.iter().find(|s| s.name == "Person");
        assert!(person.is_some());
    }

    #[test]
    fn test_java_interface_extraction() {
        let source = b"public interface Runnable {
    void run();
    default void doNothing() {}
}";

        let parser = JavaParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find the interface
        assert!(!signatures.is_empty());

        let runnable = signatures.iter().find(|s| s.name == "Runnable");
        assert!(runnable.is_some());
        assert_eq!(runnable.unwrap().return_type, Some("interface".to_string()));
    }

    #[test]
    fn test_java_enum_extraction() {
        let source = b"public enum Color {
    RED, GREEN, BLUE;
}";

        let parser = JavaParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find the enum
        let color = signatures.iter().find(|s| s.name == "Color");
        assert!(color.is_some());
        assert_eq!(color.unwrap().return_type, Some("enum".to_string()));
    }

    #[test]
    fn test_java_visibility_modifiers() {
        let source = b"public class Test {
    public void publicMethod() {}
    private void privateMethod() {}
    protected void protectedMethod() {}
    void packagePrivateMethod() {}
}";

        let parser = JavaParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Check visibility modifiers
        let public = signatures.iter().find(|s| s.name == "publicMethod");
        assert_eq!(public.unwrap().visibility, Visibility::Public);

        let private = signatures.iter().find(|s| s.name == "privateMethod");
        assert_eq!(private.unwrap().visibility, Visibility::Private);

        let protected = signatures.iter().find(|s| s.name == "protectedMethod");
        assert_eq!(protected.unwrap().visibility, Visibility::Protected);
    }

    #[test]
    fn test_java_complexity_calculation() {
        let source = b"public void complex(int x) {
    if (x > 0) {
        for (int i = 0; i < x; i++) {
            if (i % 2 == 0) {
                System.out.println(i);
            }
        }
    }
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let java_parser = JavaParser::new();
        let metrics = java_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }
}
