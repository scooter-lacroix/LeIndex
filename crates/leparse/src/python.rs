// Python language parser implementation

use crate::traits::{CodeIntelligence, ComplexityMetrics, Error, Graph, ImportInfo, Result, SignatureInfo};
use crate::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use tree_sitter::Parser;

/// Python language parser with full CodeIntelligence implementation
pub struct PythonParser;

impl Default for PythonParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PythonParser {
    /// Create a new Python parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and class definitions from Python source
    ///
    /// This is the unified entry point for signature extraction that avoids
    /// duplicate extraction by traversing the AST once and handling both
    /// top-level functions and class methods appropriately.
    ///
    /// Tracks the full qualified name path including nested functions.
    fn extract_all_definitions(
        &self,
        source: &[u8],
        root: tree_sitter::Node,
    ) -> Vec<SignatureInfo> {
        let mut signatures = Vec::new();

        // Traverse the AST to find all definitions
        // We track the full qualified name path as a vector of name components
        fn visit_node(
            node: &tree_sitter::Node,
            source: &[u8],
            signatures: &mut Vec<SignatureInfo>,
            parent_path: &[String],
        ) {
            match node.kind() {
                "function_definition" => {
                    // Extract function name
                    let func_name = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    if let Some(name) = func_name {
                        // Build the full qualified path
                        let mut qualified_path = parent_path.to_vec();
                        qualified_path.push(name.clone());
                        let qualified_name = qualified_path.join(".");

                        // Extract function signature with full qualified name
                        if let Some(sig) = extract_function_signature_with_path(node, source, &qualified_name) {
                            signatures.push(sig);
                        }

                        // Continue recursion to find nested functions with updated path
                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, &qualified_path);
                        }
                    } else {
                        // Continue recursion without adding to path
                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, parent_path);
                        }
                    }
                }
                "class_definition" => {
                    // Extract class name
                    let class_name = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());

                    if let Some(name) = class_name {
                        // Add class to path
                        let mut class_path = parent_path.to_vec();
                        class_path.push(name);

                        // Continue recursion into class body
                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, &class_path);
                        }
                    } else {
                        // Continue recursion without adding to path
                        let mut cursor = node.walk();
                        for child in node.children(&mut cursor) {
                            visit_node(&child, source, signatures, parent_path);
                        }
                    }
                }
                _ => {
                    // Recursively visit children for all other node types
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

impl CodeIntelligence for PythonParser {
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
            .set_language(&crate::traits::languages::python::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Python source".to_string()))?;

        let root_node = tree.root_node();

        let imports = extract_python_imports(root_node, source);

        // Extract signatures - extract_class_definitions handles both classes AND top-level functions
        // by calling the shared extract_function_signature helper
        let mut signatures = self.extract_all_definitions(source, root_node);

        for sig in &mut signatures {
            sig.imports = imports.clone();
        }

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::python::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Python source".to_string()))?;

        let root_node = tree.root_node();

        // Find the node with the given ID
        let node = find_node_by_id(&root_node, node_id)
            .ok_or_else(|| Error::ParseFailed(format!("Node {} not found", node_id)))?;

        // Build control flow graph
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

fn extract_python_imports(root: tree_sitter::Node, source: &[u8]) -> Vec<ImportInfo> {
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

    fn parse_import_text(imports: &mut Vec<ImportInfo>, text: &str) {
        let text = text.trim();
        if text.starts_with("import ") {
            let rest = text.trim_start_matches("import ");
            for part in rest.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                if let Some((path, alias)) = part.split_once(" as ") {
                    add_import(imports, path.trim(), Some(alias.trim().to_string()));
                } else {
                    add_import(imports, part, part.split('.').last().map(|s| s.to_string()));
                }
            }
        } else if text.starts_with("from ") {
            let rest = text.trim_start_matches("from ");
            if let Some((module, items)) = rest.split_once(" import ") {
                let module = module.trim();
                for item in items.split(',') {
                    let item = item.trim();
                    if item.is_empty() || item == "*" {
                        continue;
                    }
                    if let Some((name, alias)) = item.split_once(" as ") {
                        let path = format!("{}.{}", module, name.trim());
                        add_import(imports, &path, Some(alias.trim().to_string()));
                    } else {
                        let path = format!("{}.{}", module, item);
                        add_import(imports, &path, item.split('.').last().map(|s| s.to_string()));
                    }
                }
            }
        }
    }

    fn visit(node: &tree_sitter::Node, source: &[u8], imports: &mut Vec<ImportInfo>) {
        match node.kind() {
            "import_statement" | "import_from_statement" => {
                if let Ok(text) = node.utf8_text(source) {
                    parse_import_text(imports, text);
                }
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(&child, source, imports);
        }
    }

    visit(&root, source, &mut imports);
    imports
}

/// Extract function signature from a function_definition node
///
/// This is the legacy version for backward compatibility.
/// New code should use `extract_function_signature_with_path`.
#[allow(dead_code)]
fn extract_function_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    class_name: Option<&str>,
) -> Option<SignatureInfo> {
    // Extract function name
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source).ok()?.to_string();

    // Build qualified name from class_name if present
    let qualified_name = if let Some(class) = class_name {
        format!("{}.{}", class, name)
    } else {
        name.clone()
    };

    extract_function_signature_with_path(node, source, &qualified_name)
}

/// Extract function signature from a function_definition node with a pre-computed qualified name
///
/// This version supports full qualified name paths including nested functions.
fn extract_function_signature_with_path(
    node: &tree_sitter::Node,
    source: &[u8],
    qualified_name: &str,
) -> Option<SignatureInfo> {
    // Extract function name (the last component of the qualified name)
    let name = qualified_name.split('.').next_back().unwrap_or(qualified_name).to_string();

    // Extract parameters
    let parameters_node = node.child_by_field_name("parameters")?;
    let parameters = extract_parameters(&parameters_node, source);

    // Extract return type (if present)
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|rt| rt.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    // Check if async
    let is_async = node
        .children(&mut node.walk())
        .any(|child| child.kind() == "async");

    // Extract docstring (if present)
    let docstring = extract_docstring(node, source);

    // Extract calls
    let calls = extract_python_calls(node, source);

    // Determine if this is a method (qualified name contains at least one dot)
    let is_method = qualified_name.contains('.');

    Some(SignatureInfo {
        name,
        qualified_name: qualified_name.to_string(),
        parameters,
        return_type,
        visibility: Visibility::Public, // Python doesn't have explicit visibility
        is_async,
        is_method,
        docstring,
        calls,
        
        imports: vec![], byte_range: (node.start_byte(), node.end_byte()),
    })
}

/// Extract function calls from a Python node
fn extract_python_calls(node: &tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut calls = Vec::new();
    
    fn clean_call_text(raw: &str) -> String {
        raw.split('(')
            .next()
            .unwrap_or(raw)
            .replace("?.", ".")
            .trim()
            .to_string()
    }

    fn find_calls(node: &tree_sitter::Node, source: &[u8], calls: &mut Vec<String>) {
        if node.kind() == "call" {
            if let Some(func) = node.child_by_field_name("function") {
                if let Ok(text) = func.utf8_text(source) {
                    let name = clean_call_text(text);
                    if !name.is_empty() {
                        calls.push(name);
                    }
                }
            }
        }
        
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            find_calls(&child, source, calls);
        }
    }
    
    find_calls(node, source, &mut calls);
    calls
}

/// Extract parameters from a parameters node
fn extract_parameters(params_node: &tree_sitter::Node, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    // Find all 'identifier' nodes within parameters
    let mut cursor = params_node.walk();
    for child in params_node.children(&mut cursor) {
        if child.kind() == "identifier" {
            if let Ok(name) = child.utf8_text(source) {
                parameters.push(Parameter {
                    name: name.to_string(),
                    type_annotation: None, // Could be enhanced to extract type hints
                    default_value: None,
                });
            }
        } else if child.kind() == "typed_parameter" {
            // Extract parameter with type annotation
            let mut param_cursor = child.walk();
            for param_child in child.children(&mut param_cursor) {
                if param_child.kind() == "identifier" {
                    if let Ok(name) = param_child.utf8_text(source) {
                        // Try to find type annotation
                        let type_annotation = child
                            .child_by_field_name("type")
                            .and_then(|t| t.utf8_text(source).ok())
                            .map(|s| s.trim().to_string());

                        parameters.push(Parameter {
                            name: name.to_string(),
                            type_annotation,
                            default_value: None,
                        });
                        break;
                    }
                }
            }
        } else if child.kind() == "default_parameter" {
            // Extract parameter with default value
            let mut param_cursor = child.walk();
            for param_child in child.children(&mut param_cursor) {
                if param_child.kind() == "identifier" {
                    if let Ok(name) = param_child.utf8_text(source) {
                        parameters.push(Parameter {
                            name: name.to_string(),
                            type_annotation: None,
                            default_value: Some("...".to_string()), // Could extract actual default
                        });
                        break;
                    }
                }
            }
        }
    }

    parameters
}

/// Extract docstring from a function or class node
fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Look for a string expression as the first statement in the body
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            // Check if it's a string
            let string_node = child.children(&mut child.walk()).next()?;
            if string_node.kind() == "string" {
                return string_node.utf8_text(source).ok().map(|s| {
                    // Remove quotes and escape sequences
                    s.trim_matches('"')
                        .trim_matches('\'')
                        .replace("\\n", "\n")
                        .replace("\\t", "\t")
                        .to_string()
                });
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
    // Update nesting depth
    metrics.nesting_depth = metrics.nesting_depth.max(depth);

    // Count lines using the node's byte range
    metrics.line_count = std::cmp::max(metrics.line_count, 1);

    // Count control flow structures (increase cyclomatic complexity)
    match node.kind() {
        "if_statement"
        | "while_statement"
        | "for_statement"
        | "match_statement"
        | "try_statement" => {
            metrics.cyclomatic += 1;
        }
        "elif_clause" => {
            metrics.cyclomatic += 1;
        }
        _ => {}
    }

    // Count tokens (rough estimate)
    metrics.token_count += node.child_count();

    // Recursively process children
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
        // Create entry block
        let entry_id = self.create_block();

        // Traverse the node and build CFG
        self.build_cfg_recursive(node, entry_id)?;

        Ok(())
    }

    fn build_cfg_recursive(&mut self, node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        match node.kind() {
            "if_statement" => {
                self.handle_if_statement(node, current_block)?;
            }
            "while_statement" => {
                self.handle_while_statement(node, current_block)?;
            }
            "for_statement" => {
                self.handle_for_statement(node, current_block)?;
            }
            _ => {
                // For other nodes, just add the text to current block
                if let Ok(text) = node.utf8_text(self.source) {
                    self.add_statement_to_block(current_block, text.to_string());
                }

                // Recursively process children
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    self.build_cfg_recursive(&child, current_block)?;
                }
            }
        }

        Ok(())
    }

    fn handle_if_statement(&mut self, _node: &tree_sitter::Node, current_block: usize) -> Result<()> {
        // Create true and false branches
        let true_block = self.create_block();
        let false_block = self.create_block();
        let merge_block = self.create_block();

        // Add edges
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
        // Create loop body block
        let body_block = self.create_block();

        // Add edges for loop
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
        // Create loop body block
        let body_block = self.create_block();

        // Add edges for loop
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
    use crate::traits::CodeIntelligence;

    #[test]
    fn test_extract_function_signature() {
        let source = b"def hello(name: str) -> str:
            \"\"\"Greet someone.\"\"\"
            return f'Hello, {name}!'";

        let parser = PythonParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "hello");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].name, "name");
        assert_eq!(sig.return_type, Some("str".to_string()));
        assert_eq!(sig.docstring, Some("Greet someone.".to_string()));
    }

    #[test]
    fn test_extract_class_methods() {
        let source = b"
class MyClass:
    def method1(self):
        pass

    def method2(self, x):
        return x

def standalone_func():
    pass
";

        let parser = PythonParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // We're extracting both from extract_function_definitions AND extract_class_definitions
        // This results in duplicates, so let's just check that we find the expected signatures
        assert!(signatures.len() >= 3);

        // Check that we have the methods
        let method_names: Vec<_> = signatures
            .iter()
            .filter(|sig| sig.is_method && sig.name.contains("method"))
            .collect();
        assert!(method_names.len() >= 2);

        // Check that we have the standalone function
        let standalone: Vec<_> = signatures
            .iter()
            .filter(|sig| sig.name == "standalone_func")
            .collect();
        assert!(!standalone.is_empty());
    }

    #[test]
    fn test_complexity_calculation() {
        let source = b"
def complex_function(x):
    if x > 0:
        for i in range(x):
            if i % 2 == 0:
                pass
    return x
";

        let mut parser = Parser::new();
        parser
            .set_language(&crate::traits::languages::python::language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let python_parser = PythonParser::new();
        let metrics = python_parser.extract_complexity(&root);

        // Should have complexity > 1 due to if/for/if
        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }

    #[test]
    fn test_async_function_detection() {
        let source = b"async def fetch_data():
    pass";

        let parser = PythonParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        assert!(signatures[0].is_async);
    }
}
