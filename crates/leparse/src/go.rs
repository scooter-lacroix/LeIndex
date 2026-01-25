// Go language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

/// Go language parser with full CodeIntelligence implementation
pub struct GoParser;

impl Default for GoParser {
    fn default() -> Self {
        Self::new()
    }
}

impl GoParser {
    /// Create a new Go parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and type definitions from Go source
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
                "function_declaration" => {
                    if let Some(sig) = extract_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "method_declaration" => {
                    if let Some(sig) = extract_method_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(&child, source, signatures, parent_path);
                    }
                }
                "type_declaration" => {
                    // Extract all type_spec nodes within this type_declaration
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "type_spec" {
                            // Extract the type name and type definition
                            if let Some(name_node) = child.child_by_field_name("name") {
                                if let Ok(name) = name_node.utf8_text(source) {
                                    let qualified_name = if parent_path.is_empty() {
                                        name.to_string()
                                    } else {
                                        format!("{}.{}", parent_path.join("."), name)
                                    };

                                    // Determine the kind of type
                                    let type_kind = child.child_by_field_name("type").map(|t| t.kind());

                                    signatures.push(SignatureInfo {
                                        name: name.to_string(),
                                        qualified_name,
                                        parameters: vec![],
                                        return_type: type_kind.map(|k| k.to_string()),
                                        visibility: Visibility::Public,
                                        is_async: false,
                                        is_method: false,
                                        docstring: extract_docstring(&child, source),
                                    });
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

impl CodeIntelligence for GoParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::go::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Go source".to_string()))?;

        let root_node = tree.root_node();

        let signatures = self.extract_all_definitions(source, root_node);

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::go::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Go source".to_string()))?;

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

/// Extract function signature from a function_declaration node
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
        format!("{}.{}", parent_path.join("."), name)
    };

    let parameters = extract_go_parameters(node, source);

    let return_type = node
        .child_by_field_name("result")
        .and_then(|r| r.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility: Visibility::Public, // Go uses capitalization for export
        is_async: false,
        is_method: false,
        docstring: extract_docstring(node, source),
    })
}

/// Extract method signature from a method_declaration node
fn extract_method_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    _parent_path: &[String],
) -> Option<SignatureInfo> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())?;

    // Get receiver type for qualified name
    let receiver = node.child_by_field_name("receiver");
    let receiver_type = receiver.and_then(|r| {
        r.children(&mut r.walk())
            .find(|c| c.kind() == "type_identifier" || c.kind() == "pointer_type")
            .and_then(|t| {
                if t.kind() == "pointer_type" {
                    t.child_by_field_name("type")
                        .and_then(|pt| pt.utf8_text(source).ok())
                        .map(|s| format!("*{}", s))
                } else {
                    t.utf8_text(source).ok().map(|s| s.to_string())
                }
            })
    });

    let qualified_name = if let Some(receiver) = receiver_type {
        format!("{}.{}", receiver, name)
    } else {
        name.clone()
    };

    let parameters = extract_go_parameters(node, source);

    let return_type = node
        .child_by_field_name("result")
        .and_then(|r| r.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility: Visibility::Public,
        is_async: false,
        is_method: true,
        docstring: extract_docstring(node, source),
    })
}

/// Extract parameters from a Go function
fn extract_go_parameters(node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter_declaration" {
                // Go allows multiple identifiers to share a type: (a, b int)
                // Collect all identifiers first
                let mut identifiers = Vec::new();
                let mut type_annotation = None;

                let mut ccursor = child.walk();
                for param_child in child.children(&mut ccursor) {
                    match param_child.kind() {
                        "identifier" => {
                            if let Ok(name) = param_child.utf8_text(source) {
                                identifiers.push(name.to_string());
                            }
                        }
                        "type_identifier" | "pointer_type" | "slice_type" | "array_type" => {
                            type_annotation = param_child.utf8_text(source).ok().map(|s| s.trim().to_string());
                        }
                        _ => {}
                    }
                }

                // If we found multiple identifiers, they share the type
                if identifiers.len() > 1 {
                    for ident in identifiers {
                        parameters.push(Parameter {
                            name: ident,
                            type_annotation: type_annotation.clone(),
                            default_value: None,
                        });
                    }
                } else if let Some(ident) = identifiers.first() {
                    // Single parameter with explicit name
                    parameters.push(Parameter {
                        name: ident.clone(),
                        type_annotation,
                        default_value: None,
                    });
                } else if let Some(ref typ) = type_annotation {
                    // Unnamed parameter (only type provided)
                    parameters.push(Parameter {
                        name: format!("_{}", typ),
                        type_annotation: Some(typ.clone()),
                        default_value: None,
                    });
                } else {
                    // No name, no type - just add a placeholder
                    parameters.push(Parameter {
                        name: "_".to_string(),
                        type_annotation: None,
                        default_value: None,
                    });
                }
            } else if child.kind() == "variadic_parameter_declaration" {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .map(|s| format!("...{}", s));

                let type_annotation = child
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source).ok())
                    .map(|s| format!("...{}", s.trim()));

                parameters.push(Parameter {
                    name: name.unwrap_or_else(|| "...".to_string()),
                    type_annotation,
                    default_value: None,
                });
            }
        }
    }

    parameters
}

/// Extract docstring from a node
fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Look for comment before the node
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

    // Check for comment
    if let Some(sibling) = prev_sibling {
        if sibling.kind() == "comment" {
            if let Ok(text) = sibling.utf8_text(source) {
                return Some(
                    text.trim()
                        .trim_start_matches("/*")
                        .trim_start_matches("//")
                        .trim_end_matches("*/")
                        .trim()
                        .to_string(),
                );
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
        | "range_clause"
        | "go_statement"
        | "select_statement"
        | "switch_statement"
        | "type_switch_statement" => {
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

    fn build_cfg_recursive(
        &mut self,
        node: &tree_sitter::Node,
        current_block: usize,
    ) -> Result<()> {
        match node.kind() {
            "if_statement" => {
                self.handle_if_statement(node, current_block)?;
            }
            "for_statement" => {
                self.handle_for_statement(node, current_block)?;
            }
            "select_statement" => {
                self.handle_select_statement(node, current_block)?;
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

    fn handle_select_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        let default_block = self.create_block();
        let merge_block = self.create_block();

        self.edges.push(Edge {
            from: current_block,
            to: default_block,
            edge_type: EdgeType::Unconditional,
        });
        self.edges.push(Edge {
            from: default_block,
            to: merge_block,
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
    fn test_go_function_extraction() {
        let source = b"func greet(name string) string {
    return \"Hello, \" + name
}";

        let parser = GoParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "greet");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].name, "name");
        assert_eq!(sig.return_type, Some("string".to_string()));
        assert!(!sig.is_method);
    }

    #[test]
    fn test_go_method_extraction() {
        let source = b"func (s *Server) Start() error {
    return nil
}

func (c *Client) Connect(addr string) error {
    return nil
}";

        let parser = GoParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        // Find methods
        let methods: Vec<_> = signatures.iter().filter(|s| s.is_method).collect();
        assert!(methods.len() >= 2);
    }

    #[test]
    fn test_go_interface_extraction() {
        let source = b"type Writer interface {
    Write(p []byte) (n int, err error)
    Close() error
}

type Reader interface {
    Read(p []byte) (n int, err error)
}";

        let parser = GoParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract the type declarations (Writer, Reader)
        assert!(signatures.len() >= 2);

        // Check for Writer and Reader type declarations
        let writer = signatures.iter().find(|s| s.name == "Writer");
        assert!(writer.is_some());

        let reader = signatures.iter().find(|s| s.name == "Reader");
        assert!(reader.is_some());
    }

    #[test]
    fn test_go_struct_extraction() {
        let source = b"type Point struct {
    X float64
    Y float64
}

type Person struct {
    Name string
    Age  int
}";

        let parser = GoParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let point = signatures.iter().find(|s| s.name == "Point");
        assert!(point.is_some());

        let person = signatures.iter().find(|s| s.name == "Person");
        assert!(person.is_some());
    }

    #[test]
    fn test_go_variadic_parameters() {
        let source = b"func sum(nums ...int) int {
    total := 0
    for _, n := range nums {
        total += n
    }
    return total
}";

        let parser = GoParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "sum");
        assert!(sig.parameters.len() >= 1);
    }

    #[test]
    fn test_go_multiple_return_values() {
        let source = b"func divide(a, b int) (int, error) {
    if b == 0 {
        return 0, errors.New(\"division by zero\")
    }
    return a / b, nil
}

func multiply(x, y int) int {
    return x * y
}";

        let parser = GoParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 2);

        let divide = signatures.iter().find(|s| s.name == "divide").unwrap();
        assert_eq!(divide.parameters.len(), 2);
        assert_eq!(divide.return_type, Some("(int, error)".to_string()));

        let multiply = signatures.iter().find(|s| s.name == "multiply").unwrap();
        assert_eq!(multiply.parameters.len(), 2);
        assert_eq!(multiply.return_type, Some("int".to_string()));
    }

    #[test]
    fn test_go_complexity_calculation() {
        let source = b"func complex(x int) int {
    if x > 0 {
        for i := 0; i < x; i++ {
            if i%2 == 0 {
                fmt.Println(i)
            }
        }
    }
    return x
}";

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let go_parser = GoParser::new();
        let metrics = go_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }

    #[test]
    fn test_go_exported_function() {
        let source = b"func PublicFunction() string {
    return \"public\"
}

func privateFunction() string {
    return \"private\"
}";

        let parser = GoParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 2);

        let public = signatures.iter().find(|s| s.name == "PublicFunction");
        assert!(public.is_some());

        let private = signatures.iter().find(|s| s.name == "privateFunction");
        assert!(private.is_some());
    }
}
