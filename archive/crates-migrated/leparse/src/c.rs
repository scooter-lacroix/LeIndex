// C language parser implementation

use crate::traits::{Block, Edge, Visibility};
use crate::traits::{
    CodeIntelligence, ComplexityMetrics, Error, Graph, ImportInfo, Result, SignatureInfo,
};
use tree_sitter::Parser;

/// C language parser with full CodeIntelligence implementation
pub struct CParser;

impl Default for CParser {
    fn default() -> Self {
        Self::new()
    }
}

impl CParser {
    /// Create a new C parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and type definitions from C source
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
                "function_definition" => {
                    if let Some(sig) = extract_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                }
                "declaration" => {
                    // Check if it's a function declaration (prototype)
                    if let Some(sig) = extract_function_signature(node, source, parent_path) {
                        signatures.push(sig);
                    }
                }
                "struct_specifier" | "enum_specifier" | "union_specifier" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(source) {
                            let qualified_name = if parent_path.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}.{}", parent_path.join("."), name)
                            };

                            signatures.push(SignatureInfo {
                                name: name.to_string(),
                                qualified_name,
                                parameters: vec![],
                                return_type: Some(node.kind().replace("_specifier", "")),
                                visibility: Visibility::Public,
                                is_async: false,
                                is_method: false,
                                docstring: extract_docstring(node, source),
                                calls: vec![],
                                imports: vec![],
                                byte_range: (node.start_byte(), node.end_byte()),
                            });
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

impl CodeIntelligence for CParser {
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
            .set_language(&crate::traits::languages::c::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse C source".to_string()))?;

        let root_node = tree.root_node();

        let imports = extract_c_imports(root_node, source);
        let mut signatures = self.extract_all_definitions(source, root_node);

        for sig in &mut signatures {
            sig.imports = imports.clone();
        }

        Ok(signatures)
    }

    fn compute_cfg(&self, _source: &[u8], _node_id: usize) -> Result<Graph<Block, Edge>> {
        // CFG computation for C not yet implemented
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
        "if_statement" | "while_statement" | "for_statement" | "do_statement"
        | "switch_statement" | "case_statement" | "goto_statement" => {
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

fn extract_function_signature(
    node: &tree_sitter::Node,
    source: &[u8],
    parent_path: &[String],
) -> Option<SignatureInfo> {
    let declarator = node.child_by_field_name("declarator")?;

    // Find the function_declarator within the declarator
    let mut func_decl = declarator;
    while func_decl.kind() != "function_declarator" && func_decl.child_count() > 0 {
        if let Some(child) = func_decl.child_by_field_name("declarator") {
            func_decl = child;
        } else {
            break;
        }
    }

    if func_decl.kind() != "function_declarator" {
        return None;
    }

    let name_node = func_decl.child_by_field_name("declarator")?;
    let name = name_node.utf8_text(source).ok()?.to_string();

    let qualified_name = if parent_path.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", parent_path.join("."), name)
    };

    let return_type = node
        .child_by_field_name("type")
        .and_then(|t| t.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    let calls = extract_c_calls(node, source);

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters: vec![], // Parameter extraction for C can be complex, skipping for now
        return_type,
        visibility: Visibility::Public,
        is_async: false,
        is_method: false,
        docstring: extract_docstring(node, source),
        calls,

        imports: vec![],
        byte_range: (node.start_byte(), node.end_byte()),
    })
}

fn extract_c_imports(root: tree_sitter::Node, source: &[u8]) -> Vec<ImportInfo> {
    let mut imports = Vec::new();

    fn add_import(imports: &mut Vec<ImportInfo>, path: &str, alias: Option<String>) {
        let path = path
            .trim()
            .trim_matches('"')
            .trim_matches('<')
            .trim_matches('>')
            .trim();
        if path.is_empty() {
            return;
        }
        let alias = alias.or_else(|| path.split('/').last().map(|s| s.to_string()));
        imports.push(ImportInfo {
            path: path.to_string(),
            alias,
        });
    }

    fn visit(node: &tree_sitter::Node, source: &[u8], imports: &mut Vec<ImportInfo>) {
        if node.kind() == "preproc_include" {
            if let Ok(text) = node.utf8_text(source) {
                let parts: Vec<_> = text.split_whitespace().collect();
                if let Some(last) = parts.last() {
                    add_import(imports, last, None);
                }
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

fn extract_c_calls(node: &tree_sitter::Node, source: &[u8]) -> Vec<String> {
    let mut calls = Vec::new();

    fn clean_call_text(raw: &str) -> String {
        raw.split('(').next().unwrap_or(raw).trim().to_string()
    }

    fn find_calls(node: &tree_sitter::Node, source: &[u8], calls: &mut Vec<String>) {
        if node.kind() == "call_expression" {
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

fn extract_docstring(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let prev_sibling = node.prev_sibling();
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
